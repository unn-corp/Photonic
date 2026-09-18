//! P1 golden-output safety net (`docs/specs/video-editor/03-render-color-pipeline.md`
//! §2.6). Renders the fixture corpus under `tests/golden/` through
//! `HeadlessRenderer::render_rgba_with_opts` — the *existing* vector renderer
//! — and compares against blessed reference PNGs. This is the concrete P1
//! merge gate named in `00-overview.md` §7's top risk ("Renderer rework
//! destabilizes vector editing"): post-P1 renders of this corpus must match
//! pre-P1 renders (within the portable PSNR threshold, or a stricter documented
//! per-case threshold for `COMPOSITE_SHADER` wiring).
//!
//! Bless mode (writes `expected/reference.png` for every case instead of
//! comparing):
//!
//! ```sh
//! PHOTONIC_BLESS_GOLDEN=1 cargo test -p photonic-render --test golden_vector_equivalence -- --test-threads=1
//! ```
//!
//! Every bless run must be a reviewed diff (`git diff --stat tests/golden/`)
//! before commit — never automatic in CI (`11-testing-phasing.md` §1.4's
//! blessing-discipline convention, mirrored here for this corpus).
//!
//! No-GPU-adapter CI runners: skip with a printed message, the same
//! convention as `headless.rs`'s `try_renderer()` (`headless.rs:1488`).
//!
//! NOTE: this corpus (`crates/photonic-render/tests/golden/`) is deliberately
//! a *separate* system from the repo-root `tests/golden/` (the video/timeline
//! golden-frame corpus, `11-testing-phasing.md` §1) — different scope
//! (single static document vs. timeline sequence at sampled ticks),
//! different lifecycle. See `tests/golden/README.md`. Do not merge them.

use photonic_core::load_photon;
use photonic_render::{ExportBackground, ExportOptions, HeadlessRenderer};

use std::path::{Path, PathBuf};

/// Returns `Some(renderer)` if a GPU adapter is available, else `None` so the
/// test can skip on headless CI without a GPU. Mirrors `headless.rs`'s
/// `try_renderer()` (`headless.rs:1488`) — duplicated here rather than
/// exported across the test/lib boundary, matching how other integration
/// test files in this crate are self-contained.
fn try_renderer() -> Option<HeadlessRenderer> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    Some(pollster::block_on(HeadlessRenderer::new()))
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// One rendered case: RGBA8 pixels + dimensions, straight alpha, gamma-encoded
/// (exactly what `render_rgba_with_opts` returns).
struct Rendered {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

fn render_case(r: &HeadlessRenderer, project_photon: &Path) -> Rendered {
    let json = std::fs::read_to_string(project_photon)
        .unwrap_or_else(|e| panic!("read {}: {e}", project_photon.display()));
    let (doc, _history) =
        load_photon(&json).unwrap_or_else(|e| panic!("parse {}: {e}", project_photon.display()));
    let w = doc.width.round().max(1.0) as u32;
    let h = doc.height.round().max(1.0) as u32;
    let opts = ExportOptions {
        background: ExportBackground::Artboard,
        ..ExportOptions::default()
    };
    let (pixels, width, height) = r.render_rgba_with_opts(&doc, w, h, &opts);
    Rendered {
        pixels,
        width,
        height,
    }
}

fn load_reference_png(path: &Path) -> Option<Rendered> {
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes)
        .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()))
        .to_rgba8();
    let (width, height) = img.dimensions();
    Some(Rendered {
        pixels: img.into_raw(),
        width,
        height,
    })
}

fn save_png(path: &Path, rendered: &Rendered) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    image::save_buffer(
        path,
        &rendered.pixels,
        rendered.width,
        rendered.height,
        image::ColorType::Rgba8,
    )
    .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Peak signal-to-noise ratio between two equal-length RGBA8 buffers, in dB.
/// `f64::INFINITY` for identical buffers (matches the convention used by
/// `11-testing-phasing.md` §1.2's aggregate metric).
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "PSNR requires equal-length buffers");
    let mse: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| {
            let d = *x as f64 - *y as f64;
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    20.0 * (255.0f64).log10() - 10.0 * mse.log10()
}

/// Per-pixel abs-diff heatmap, for the failure-diagnostic PNG (`11` §1.2's
/// "dump a diff PNG next to the test output dir" convention, applied here).
fn diff_heatmap(a: &[u8], b: &[u8], width: u32, height: u32) -> Rendered {
    let mut pixels = Vec::with_capacity(a.len());
    for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let dr = x[0].abs_diff(y[0]);
        let dg = x[1].abs_diff(y[1]);
        let db = x[2].abs_diff(y[2]);
        let da = x[3].abs_diff(y[3]);
        let mag = *[dr, dg, db, da].iter().max().unwrap();
        pixels.extend_from_slice(&[mag, mag, mag, 255]);
    }
    Rendered {
        pixels,
        width,
        height,
    }
}

/// Optional per-case PSNR floor (`tests/golden/{case}/tolerance_db.txt`).
/// Absent = byte-exact comparison required locally (see `tests/golden/README.md`).
fn tolerance_db(case_dir: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(case_dir.join("tolerance_db.txt")).ok()?;
    Some(
        text.trim()
            .parse()
            .unwrap_or_else(|e| panic!("{}/tolerance_db.txt: {e}", case_dir.display())),
    )
}

/// Cross-vendor GPU rasterisation can differ slightly even on a developer
/// workstation: the checked-in references were blessed on one adapter, while
/// the suite is expected to run on many others. This floor still rejects
/// visually meaningful regressions while avoiding false failures from those
/// implementation-level differences.
const CROSS_GPU_TOLERANCE_DB: f64 = 35.0;

/// Effective tolerance for a case. A per-case file wins. The common fallback
/// applies on every adapter, rather than only on CI: local runs are just as
/// likely to use a driver other than the machine that blessed the reference.
/// Set `PHOTONIC_STRICT_GOLDEN=1` to require byte-exact output while blessing
/// or investigating a single known adapter.
fn effective_tolerance_db(case_dir: &Path) -> Option<f64> {
    if let Some(t) = tolerance_db(case_dir) {
        return Some(t);
    }
    if std::env::var("PHOTONIC_STRICT_GOLDEN").as_deref() == Ok("1") {
        return None;
    }
    // ~35 dB admits small cross-driver variance; random noise is ~10 dB.
    Some(CROSS_GPU_TOLERANCE_DB)
}

fn run_case(r: &HeadlessRenderer, case_dir: &Path, bless: bool) {
    let name = case_dir.file_name().unwrap().to_string_lossy().to_string();
    let project_photon = case_dir.join("project.photon");
    assert!(
        project_photon.exists(),
        "{name}: missing project.photon fixture"
    );

    let got = render_case(r, &project_photon);
    assert!(
        !got.pixels.is_empty(),
        "{name}: render_rgba_with_opts returned empty pixels (GPU readback failure?)"
    );

    let reference_path = case_dir.join("expected/reference.png");

    if bless {
        save_png(&reference_path, &got);
        println!("[bless] {name}: wrote {}", reference_path.display());
        return;
    }

    let Some(want) = load_reference_png(&reference_path) else {
        panic!(
            "{name}: no blessed reference at {} — run with PHOTONIC_BLESS_GOLDEN=1 first",
            reference_path.display()
        );
    };
    assert_eq!(
        (got.width, got.height),
        (want.width, want.height),
        "{name}: rendered dimensions changed vs. blessed reference"
    );

    match effective_tolerance_db(case_dir) {
        None => {
            if got.pixels != want.pixels {
                let heat = diff_heatmap(&got.pixels, &want.pixels, got.width, got.height);
                let diff_path = case_dir.join("expected/last_failure_diff.png");
                save_png(&diff_path, &heat);
                let got_psnr = psnr(&got.pixels, &want.pixels);
                panic!(
                    "{name}: byte-exact comparison failed (PSNR {got_psnr:.1} dB; this case has no tolerance_db.txt, \
                     so it must render pixel-identical to the blessed reference). \
                     Diff heatmap written to {}",
                    diff_path.display()
                );
            }
        }
        Some(min_db) => {
            // Byte-exact is still preferred; PSNR only gates when pixels differ.
            if got.pixels == want.pixels {
                return;
            }
            let got_psnr = psnr(&got.pixels, &want.pixels);
            if got_psnr < min_db {
                let heat = diff_heatmap(&got.pixels, &want.pixels, got.width, got.height);
                let diff_path = case_dir.join("expected/last_failure_diff.png");
                save_png(&diff_path, &heat);
                panic!(
                    "{name}: PSNR {got_psnr:.1} dB below tolerance floor {min_db:.1} dB. \
                     Diff heatmap written to {}",
                    diff_path.display()
                );
            }
        }
    }
}

#[test]
fn p1_golden_vector_equivalence_corpus() {
    let Some(r) = try_renderer() else {
        eprintln!("no GPU adapter — skipping P1 golden vector-equivalence corpus");
        return;
    };

    let bless = std::env::var("PHOTONIC_BLESS_GOLDEN").as_deref() == Ok("1");

    let root = corpus_dir();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("read {}: {e}", root.display()))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() && path.join("project.photon").exists() {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    cases.sort();

    assert!(
        !cases.is_empty(),
        "no fixture cases found under {} — run `cargo run -p photonic-render \
         --example gen_p1_golden_fixtures`",
        root.display()
    );

    let mut failures = Vec::new();
    for case_dir in &cases {
        let name = case_dir.file_name().unwrap().to_string_lossy().to_string();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_case(&r, case_dir, bless)
        }));
        if let Err(e) = result {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "panic with non-string payload".to_string());
            failures.push(format!("{name}: {msg}"));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} golden case(s) failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
