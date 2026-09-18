//! Engine perf budgets (02 §8, gated by 11 §6 P3): `criterion` benches for
//! `graph::compile`, `graph::eval`, and decode cold-seek, each printing a
//! measured-vs-budget verdict so a human can read the result straight off
//! `cargo bench` output without cross-referencing 02 §8 by hand.
//!
//! **Advisory, not gating** (11 §6 P3 / §4): these numbers are not asserted —
//! a regression prints `FAIL (advisory)` but the bench still exits 0. Perf
//! benches are noisy on shared/CI hardware (11 §4's rationale for the same
//! policy applied to `cargo bench` in CI); a human reviews the trend at phase
//! exit rather than every run gating on a noisy number.
//!
//! Each bench also runs its own small `Instant`-based sample pass before
//! registering with `criterion` proper, because `criterion::Bencher::iter`
//! only reports to its own (optional) HTML/console summary, not to a value
//! this file can compare against a budget inline.
//!
//! GPU (`bench_eval`) and ffmpeg (`bench_cold_seek`) benches use the same
//! skip-with-message convention as the crate's tests (`GpuContext -> None`,
//! `locate_for_test() -> None`) rather than a new cargo feature — consistent
//! with `graph::eval`'s existing GPU-adapter-skip tests and
//! `tests/decode_media.rs`'s ffmpeg-skip tests, and it keeps `cargo bench`
//! runnable (skipping just the unavailable benches) on a machine missing
//! either toolchain.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion};

use photonic_core::timeline::{
    Clip, ClipSource, FrameRate, Sequence, TimelineProject, Track, TrackKind,
};
use photonic_core::Color;
use photonic_video::contract::Tick;
use photonic_video::decode::scheduler::{PtsKind, SourceParams};
use photonic_video::decode::{DecodeSource, PixFmt, SharedRing};
use photonic_video::graph::compile::{compile, Quality};
use photonic_video::graph::eval::{Evaluator, GpuContext, NullFrameSource};
use photonic_video::media::ffmpeg_locate::locate_for_test;
use photonic_video::media::keyframe_index::KeyframeIndex;

// ── 02 §8 budgets ─────────────────────────────────────────────────────────

const COMPILE_BUDGET: Duration = Duration::from_micros(500); // < 0.5 ms
const EVAL_BUDGET: Duration = Duration::from_millis(8); // < 8 ms GPU
const COLD_SEEK_BUDGET: Duration = Duration::from_millis(150); // < 150 ms

const COMPILE_SAMPLES: usize = 50;
const EVAL_SAMPLES: usize = 10;
const COLD_SEEK_SAMPLES: usize = 5;

/// Print `measured (median of N) vs budget — PASS/FAIL` for one bench. Never
/// panics on a `FAIL` (11 §4: advisory, not gating).
fn report(label: &str, samples: &mut [Duration], budget: Duration) {
    samples.sort();
    let median = samples[samples.len() / 2];
    let verdict = if median <= budget {
        "PASS"
    } else {
        "FAIL (advisory — does not gate CI, 02 §8 / 11 §6 P3)"
    };
    println!(
        "[perf] {label}: measured {median:?} (median of {} samples) vs budget {budget:?} — {verdict}",
        samples.len()
    );
}

// ── graph::compile — 10-track/3-active synthetic project ───────────────────

/// Ten video tracks, three of which have a clip covering `tick 0` (the
/// "active" clips the fold step actually composites); the other seven carry a
/// clip that does *not* cover `tick 0`, so the per-track "find the covering
/// clip" scan (02 §2 step 1) still runs its full width, matching the "10
/// tracks, 3 active clips" budget-table wording (02 §8) rather than trivially
/// skipping empty tracks.
fn synthetic_compile_project() -> (TimelineProject, photonic_core::timeline::SequenceId) {
    let mut project = TimelineProject::new();
    let seq = Sequence::new("bench-compile", FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;
    project.insert_sequence(seq);

    for i in 0..10u32 {
        let s = project.sequences.get_mut(&seq_id).unwrap();
        s.video_tracks
            .push(Track::new(TrackKind::Video, format!("V{i}")));
        let tk = s.video_tracks.len() - 1;
        let clip = if i < 3 {
            // Active at tick 0: contributes to the fold.
            Clip::new(
                ClipSource::SolidColor {
                    color: Color {
                        r: 0.2,
                        g: 0.4,
                        b: 0.6,
                        a: 1.0,
                    },
                },
                Tick(0),
                Tick::from_seconds(2),
            )
        } else {
            // Present, but starts after tick 0 — exercised by the scan, not
            // the fold.
            Clip::new(
                ClipSource::SolidColor {
                    color: Color {
                        r: 0.1,
                        g: 0.1,
                        b: 0.1,
                        a: 1.0,
                    },
                },
                Tick::from_seconds(5),
                Tick::from_seconds(2),
            )
        };
        s.video_tracks[tk].clips.push(clip);
    }
    (project, seq_id)
}

fn bench_compile(c: &mut Criterion) {
    let (project, seq_id) = synthetic_compile_project();
    let tick = Tick(0);

    // Warm up (first call pays one-time allocator/branch-predictor cost).
    for _ in 0..10 {
        black_box(compile(&project, seq_id, 0, tick, Quality::PREVIEW, None));
    }
    let mut samples: Vec<Duration> = (0..COMPILE_SAMPLES)
        .map(|_| {
            let t0 = Instant::now();
            black_box(compile(&project, seq_id, 0, tick, Quality::PREVIEW, None));
            t0.elapsed()
        })
        .collect();
    report(
        "graph::compile (10-track/3-active)",
        &mut samples,
        COMPILE_BUDGET,
    );

    c.bench_function("graph_compile_10track_3active", |b| {
        b.iter(|| {
            black_box(compile(
                black_box(&project),
                seq_id,
                0,
                tick,
                Quality::PREVIEW,
                None,
            ))
        })
    });
}

// ── graph::eval — 1080p, 3 layers ───────────────────────────────────────────

/// Three video tracks, each a full-duration solid clip covering `tick 0` —
/// folds to a 3-layer composite (2 `Merge`s) at 1080p, the "eval 1080p, 3
/// layers" budget-table row (02 §8). P3's evaluator renders `Grade`/caption
/// overlay nodes as an identity blit (module docs, `graph/eval.rs`), the same
/// pass cost as the `Transform2D` blits this graph already exercises, so a
/// plain 3-layer solid/merge composite is the representative P3 GPU workload.
fn eval_project_1080p_3layer() -> (TimelineProject, photonic_core::timeline::SequenceId) {
    let mut project = TimelineProject::new();
    let seq = Sequence::new("bench-eval", FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;
    project.insert_sequence(seq);

    let colors = [
        Color {
            r: 0.8,
            g: 0.1,
            b: 0.1,
            a: 1.0,
        },
        Color {
            r: 0.1,
            g: 0.8,
            b: 0.1,
            a: 0.6,
        },
        Color {
            r: 0.1,
            g: 0.1,
            b: 0.8,
            a: 0.6,
        },
    ];
    for (i, color) in colors.into_iter().enumerate() {
        let s = project.sequences.get_mut(&seq_id).unwrap();
        s.video_tracks
            .push(Track::new(TrackKind::Video, format!("V{i}")));
        let tk = s.video_tracks.len() - 1;
        s.video_tracks[tk].clips.push(Clip::new(
            ClipSource::SolidColor { color },
            Tick(0),
            Tick::from_seconds(2),
        ));
    }
    (project, seq_id)
}

fn bench_eval(c: &mut Criterion) {
    let Some(gpu) = GpuContext::request_blocking() else {
        println!("[perf] graph::eval (1080p, 3 layers) — no GPU adapter available, skipping");
        return;
    };

    let (project, seq_id) = eval_project_1080p_3layer();
    let compiled = compile(&project, seq_id, 0, Tick(0), Quality::FULL, None);
    let canvas = (1920, 1080);
    let mut evaluator = Evaluator::new(gpu.clone());

    // Warm up: compiles pipelines and allocates the first round of cache
    // textures, none of which is part of the steady-state per-frame cost.
    evaluator.evaluate(&compiled.graph, canvas, &mut NullFrameSource);
    gpu.device().poll(wgpu::Maintain::Wait);

    // The budget is the per-frame (cold) GPU cost, not the near-zero
    // content-hash cache-hit path — invalidate everything before each sample
    // so every iteration does a full recompute.
    let mut samples: Vec<Duration> = (0..EVAL_SAMPLES)
        .map(|_| {
            evaluator.invalidate_matching(|_| true);
            let t0 = Instant::now();
            let out = evaluator.evaluate(&compiled.graph, canvas, &mut NullFrameSource);
            gpu.device().poll(wgpu::Maintain::Wait);
            black_box(out);
            t0.elapsed()
        })
        .collect();
    report("graph::eval (1080p, 3 layers)", &mut samples, EVAL_BUDGET);

    c.bench_function("graph_eval_1080p_3layer", |b| {
        b.iter(|| {
            evaluator.invalidate_matching(|_| true);
            let out = evaluator.evaluate(black_box(&compiled.graph), canvas, &mut NullFrameSource);
            gpu.device().poll(wgpu::Maintain::Wait);
            black_box(out)
        })
    });
}

// ── decode cold seek — counter.mp4, mid-GOP ─────────────────────────────────

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// One fully cold seek: build the keyframe index from scratch, open a fresh
/// decode source, and seek to a mid-GOP tick — mirrors
/// `tests/decode_media.rs`'s `decode_counter_mid_gop_seek_is_pts_exact` (the
/// "already ~82ms ad-hoc" measurement, 11 §4), but as a repeatable
/// `criterion`-registered bench. "Index + 1 GOP decode" (02 §8) means the
/// index build is counted in every sample, not amortized across seeks.
fn cold_seek_once(
    tools: &photonic_video::media::ffmpeg_locate::FfmpegTools,
    path: &std::path::Path,
    rate: FrameRate,
    target: Tick,
) -> Duration {
    let idx = KeyframeIndex::build(tools, path).expect("keyframe index");
    let params = SourceParams {
        input: path.to_path_buf(),
        width: 320,
        height: 180,
        pix_fmt: PixFmt::Yuv420p,
        pts_kind: PtsKind::Cfr(rate),
        keyframes: idx,
    };
    let ring = SharedRing::preview();
    let mut src = DecodeSource::new(tools.clone(), params, ring);

    let t0 = Instant::now();
    let frame = src.seek(target).expect("mid-GOP seek to frame 75");
    let elapsed = t0.elapsed();
    black_box(frame);
    elapsed
}

fn bench_cold_seek(c: &mut Criterion) {
    let Some(tools) = locate_for_test() else {
        println!(
            "[perf] decode cold seek (counter.mp4, mid-GOP) — ffmpeg/ffprobe not found, \
             skipping (set PHOTONIC_FFMPEG_DIR or install ffmpeg)"
        );
        return;
    };
    let path = fixtures_dir().join("counter.mp4");
    let rate = FrameRate::FPS_30;
    let tpf = rate.ticks_per_frame().0;
    let target = Tick(75 * tpf); // mid-GOP: keyframe_before is frame 60 (README: GOP=60)

    let mut samples: Vec<Duration> = (0..COLD_SEEK_SAMPLES)
        .map(|_| cold_seek_once(&tools, &path, rate, target))
        .collect();
    report(
        "decode cold seek (counter.mp4, mid-GOP)",
        &mut samples,
        COLD_SEEK_BUDGET,
    );

    c.bench_function("decode_cold_seek_mid_gop", |b| {
        b.iter(|| cold_seek_once(&tools, &path, rate, target))
    });
}

// ── registration ─────────────────────────────────────────────────────────

criterion_group!(engine_benches, bench_compile, bench_eval);

// Cold seek forks a real ffmpeg process per iteration — a small sample size
// and short measurement window keep `cargo bench` from running hundreds of
// subprocess spawns for one bench (11 §4: benches stay advisory-cheap, not a
// CI-blocking suite).
criterion_group! {
    name = decode_benches;
    config = Criterion::default().sample_size(10).measurement_time(Duration::from_secs(3));
    targets = bench_cold_seek
}

criterion_main!(engine_benches, decode_benches);
