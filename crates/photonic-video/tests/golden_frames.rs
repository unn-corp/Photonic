//! Golden-frame corpus harness (11-testing-phasing.md §1) — the net-new
//! snapshot/reference layer this gate finding calls out as absent for video.
//!
//! For every case under repo-root `tests/golden/video/{case}/` this loads the
//! `.photon` project fixture, compiles it at each tick named in `meta.toml`,
//! evaluates the frame graph on the **`eval_cpu` CPU reference** (02 §2 — the
//! canonical golden source, 11 §1.3), and compares the result against the
//! blessed PNG in `expected/cpu/frame_{n:04}.png` using 11 §1.2's two-layer
//! metric (BOTH must pass):
//!
//! | Layer       | Metric              | Threshold (CPU-vs-CPU)  |
//! |-------------|---------------------|-------------------------|
//! | Per-channel | max abs diff        | ≤ 0.02 (linear-light)   |
//! | Aggregate   | PSNR                | ≥ 40 dB                 |
//!
//! Bless mode: `PHOTONIC_BLESS_GOLDEN=1 cargo test -p photonic-video --test \
//! golden_frames -- --test-threads=1` writes `expected/cpu/*.png` instead of
//! comparing (11 §1.4, option B — env-gated bless inside the test binary, no
//! xtask). Every bless is a reviewed `git diff` before commit.
//!
//! Scope: this is the CPU-reference corpus (whole-frame promotion of the
//! `headless.rs` single-pixel CPU-vs-CPU pattern). GPU-vs-CPU parity (the
//! looser 35 dB / SSIM ≥ 0.98 tier, 11 §1.2) is deferred to when the wgpu
//! evaluator (`graph::eval`) gains a headless readback path; that variant will
//! self-skip on adapter-less runners exactly as `headless.rs::try_renderer`
//! does. `eval_cpu` needs no GPU adapter, so these cases run everywhere.
//!
//! This corpus (repo-root `tests/golden/video/`, video/timeline scope) is
//! deliberately DISTINCT from `crates/photonic-render/tests/golden/` (03 §2.6's
//! P1 vector-renderer-equivalence corpus). Do not merge them.

use std::path::{Path, PathBuf};

use photonic_core::timeline::{AssetId, Tick, TimelineProject, VectorRef, VectorStateKey};
use photonic_video::graph::eval_cpu::{evaluate, FrameProvider};
use photonic_video::graph::ops::Image;
use photonic_video::graph::{compile, Quality};
// 11 §1.2's two-layer metric + its sRGB PNG round-trip and diff-heatmap now live
// in the always-compiled `photonic_video::testing::frame_compare` module so this
// harness and the cross-crate acceptance-story harness (29 §3) share one source.
use photonic_video::testing::frame_compare::{
    decode_png, diff_heatmap, encode_png, measure, MAX_ABS_TOL, MIN_PSNR_DB,
};
use serde::Deserialize;

/// Repo-root `tests/golden/video/` (two levels up from this crate's manifest).
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/video")
        .canonicalize()
        .unwrap_or_else(|e| {
            panic!(
                "golden corpus dir tests/golden/video is missing ({e}); run the \
                 generator: cargo test -p photonic-video --test gen_video_golden \
                 -- --ignored, then bless with PHOTONIC_BLESS_GOLDEN=1"
            )
        })
}

// ── meta.toml (11 §1.1) ──────────────────────────────────────────────────────

/// One case's `meta.toml`: which ticks to sample, the format to compile, and a
/// human note. Frames are named so a failure log points at "frame at tick X in
/// case Y", not an opaque index (11 §1.1).
#[derive(Debug, Deserialize)]
struct Meta {
    #[serde(default)]
    format_index: usize,
    frames: Vec<FrameSpec>,
    // `notes` / `doc_generation` also live in meta.toml (human-facing / in-file
    // provenance); serde ignores the extra keys here.
}

#[derive(Debug, Deserialize)]
struct FrameSpec {
    /// Frame number at the sequence's frame rate; the tick is derived.
    n: i64,
    #[serde(default)]
    name: String,
}

// ── source provider (injects known pixels; 02 §2 eval_cpu source hook) ───────

/// Deterministic four-quadrant test pattern for any decode/vector source op —
/// TL red, TR green, BL blue, BR yellow — so Transform2D/Crop/Resize cases have
/// non-uniform, eyeball-able content (a solid can't reveal a resample). Values
/// are authored directly in premultiplied scene-linear Rec.709 (D-09, alpha 1),
/// so they encode losslessly through the PNG round-trip's un/premultiply.
///
/// Cases whose sources are all `SolidColor` never call this (that op is a
/// generator, not a provider source) — one provider serves every case.
struct PatternProvider;

impl PatternProvider {
    fn quadrants(w: u32, h: u32) -> Image {
        let (w, h) = (w.max(1), h.max(1));
        const COLORS: [[f32; 4]; 4] = [
            [0.75, 0.10, 0.10, 1.0], // TL
            [0.10, 0.55, 0.15, 1.0], // TR
            [0.10, 0.15, 0.75, 1.0], // BL
            [0.70, 0.65, 0.10, 1.0], // BR
        ];
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let q = ((y >= h / 2) as usize) * 2 + (x >= w / 2) as usize;
                pixels.push(COLORS[q]);
            }
        }
        Image {
            width: w,
            height: h,
            pixels,
        }
    }
}

impl FrameProvider for PatternProvider {
    fn decode_video(&mut self, _: AssetId, _: Tick, _: bool, w: u32, h: u32) -> Image {
        Self::quadrants(w, h)
    }
    fn decode_still(&mut self, _: AssetId, w: u32, h: u32) -> Image {
        Self::quadrants(w, h)
    }
    fn raster_vector(&mut self, _: VectorRef, _: VectorStateKey, w: u32, h: u32) -> Image {
        Self::quadrants(w, h)
    }
}

// ── harness ──────────────────────────────────────────────────────────────────

fn discover_cases(root: &Path) -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap_or_else(|e| panic!("read golden corpus dir {root:?}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_dir() && p.join("meta.toml").is_file() && p.join("project.photon").is_file()
        })
        .collect();
    cases.sort();
    cases
}

fn load_project(case: &Path) -> TimelineProject {
    let text = std::fs::read_to_string(case.join("project.photon"))
        .unwrap_or_else(|e| panic!("read {:?}/project.photon: {e}", case));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse {:?}/project.photon as TimelineProject: {e}", case))
}

fn load_meta(case: &Path) -> Meta {
    let text = std::fs::read_to_string(case.join("meta.toml"))
        .unwrap_or_else(|e| panic!("read {:?}/meta.toml: {e}", case));
    toml::from_str(&text).unwrap_or_else(|e| panic!("parse {:?}/meta.toml: {e}", case))
}

#[test]
fn golden_frames() {
    let bless = std::env::var("PHOTONIC_BLESS_GOLDEN").as_deref() == Ok("1");
    let root = corpus_dir();
    let cases = discover_cases(&root);
    assert!(
        !cases.is_empty(),
        "no golden cases found under {root:?} — the video golden corpus is not \
         seeded. Generate it: cargo test -p photonic-video --test gen_video_golden \
         -- --ignored, then bless with PHOTONIC_BLESS_GOLDEN=1."
    );

    let mut failures: Vec<String> = Vec::new();
    let mut compared = 0usize;
    let mut blessed = 0usize;

    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let project = load_project(case);
        let meta = load_meta(case);

        let seq_id = match project.active_sequence {
            Some(id) => id,
            None => {
                failures.push(format!("[{name}] project.photon has no active_sequence"));
                continue;
            }
        };
        let seq = project
            .sequences
            .get(&seq_id)
            .unwrap_or_else(|| panic!("[{name}] active_sequence not present in project"));
        let format_index = meta.format_index.min(seq.formats.len().saturating_sub(1));
        let format = &seq.formats[format_index];
        let canvas = (format.width, format.height);
        let frame_rate = seq.frame_rate;

        let out_dir = case.join("expected/cpu");
        if bless {
            std::fs::create_dir_all(&out_dir)
                .unwrap_or_else(|e| panic!("[{name}] create {out_dir:?}: {e}"));
        }

        for fs in &meta.frames {
            let tick = frame_rate.frame_start(fs.n);
            let compiled = compile(&project, seq_id, format_index, tick, Quality::FULL, None);
            for d in &compiled.diagnostics {
                eprintln!(
                    "[{name}] frame {} ({}) compile diagnostic: {}",
                    fs.n, fs.name, d.message
                );
            }
            let actual = evaluate(&compiled.graph, canvas, &mut PatternProvider);
            let png_path = out_dir.join(format!("frame_{:04}.png", fs.n));

            if bless {
                // Guard against silently blessing a blank (fully transparent)
                // frame — that is almost always a mis-built case, not intent.
                let opaque = actual.pixels.iter().any(|p| p[3] > 1e-3);
                assert!(
                    opaque,
                    "[{name}] frame {} ({}) is fully transparent — refusing to \
                     bless a blank reference; check the case in gen_video_golden.rs",
                    fs.n, fs.name
                );
                encode_png(&actual)
                    .save(&png_path)
                    .unwrap_or_else(|e| panic!("[{name}] write {png_path:?}: {e}"));
                blessed += 1;
                continue;
            }

            if !png_path.is_file() {
                failures.push(format!(
                    "[{name}] frame {} ({}) has no blessed reference {png_path:?} — run \
                     PHOTONIC_BLESS_GOLDEN=1 to create it",
                    fs.n, fs.name
                ));
                continue;
            }
            let reference = decode_png(
                &image::open(&png_path)
                    .unwrap_or_else(|e| panic!("[{name}] open {png_path:?}: {e}"))
                    .to_rgba8(),
            );
            compared += 1;
            match measure(&reference, &actual) {
                Ok(m) if m.max_abs <= MAX_ABS_TOL && m.psnr_db >= MIN_PSNR_DB => {}
                Ok(m) => {
                    let dump =
                        std::env::temp_dir().join(format!("golden-video-{name}-frame_{:04}", fs.n));
                    let _ = encode_png(&actual).save(dump.with_extension("actual.png"));
                    let _ = diff_heatmap(&reference, &actual).save(dump.with_extension("diff.png"));
                    failures.push(format!(
                        "[{name}] frame {} ({}) at tick {}: max_abs {:.5} (≤{:.2}? {}), \
                         PSNR {:.2}dB (≥{:.0}? {}) — actual+diff dumped to {}.{{actual,diff}}.png",
                        fs.n,
                        fs.name,
                        tick.0,
                        m.max_abs,
                        MAX_ABS_TOL,
                        m.max_abs <= MAX_ABS_TOL,
                        m.psnr_db,
                        MIN_PSNR_DB,
                        m.psnr_db >= MIN_PSNR_DB,
                        dump.display()
                    ));
                }
                Err(e) => failures.push(format!("[{name}] frame {} ({}): {e}", fs.n, fs.name)),
            }
        }
    }

    if bless {
        eprintln!(
            "blessed {blessed} golden frame(s) across {} case(s)",
            cases.len()
        );
        return;
    }

    assert!(
        compared > 0,
        "golden corpus has cases but compared zero frames — check meta.toml `frames` \
         lists and that expected/cpu/*.png references exist"
    );
    assert!(
        failures.is_empty(),
        "{} golden-frame mismatch(es):\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!(
        "golden-frames: {compared} frame(s) matched across {} case(s)",
        cases.len()
    );
}
