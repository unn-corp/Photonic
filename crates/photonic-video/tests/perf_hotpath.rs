//! Structural + micro benchmarks for top-tier interactive performance.
//!
//! These assert **shipped** hot-path contracts (not reimplemented kernels):
//! - ring defaults sized for smooth scrub
//! - prefetch/cut-ahead horizons meet ≥500 ms spirit
//! - coalesce keeps scrub batches O(1) output
//! - graph compile stays sub-budget for a realistic 10-track active slice
//!
//! Soft wall-clock budgets skip hard-fail on overloaded CI; structural
//! assertions always run.

#![expect(clippy::assertions_on_constants)]

use std::time::Instant;

use photonic_core::timeline::{
    AssetId, Clip, ClipSource, FrameRate, Sequence, Tick, TimelineProject, Track, TrackKind,
    TICKS_PER_SECOND,
};
use photonic_video::decode::ring::{DEFAULT_BACK, DEFAULT_FWD};
use photonic_video::graph::compile::{compile, Quality};
use photonic_video::playback::prefetch::{
    cut_ahead_targets, CUT_AHEAD_LEAD_FRAMES, MAX_LIVE_SOURCES, PREFETCH_AHEAD_FRAMES,
    PREFETCH_BATCH,
};
use photonic_video::{coalesce_commands, EngineCmd};

#[test]
fn ring_and_prefetch_depths_are_perf_tier() {
    assert!(
        DEFAULT_FWD >= 16,
        "forward ring must cover at least a half-second @30fps"
    );
    assert!(DEFAULT_BACK >= 4);
    assert!(PREFETCH_BATCH >= 4);
    assert!(PREFETCH_AHEAD_FRAMES >= 8);
    assert_eq!(
        MAX_LIVE_SOURCES, 8,
        "sidecar LRU cap is a hard memory bound"
    );
    // ≥500 ms cut-ahead at 30 fps.
    let lead_us = CUT_AHEAD_LEAD_FRAMES * (TICKS_PER_SECOND / 30);
    assert!(lead_us >= 500_000);
}

#[test]
fn scrub_coalesce_is_constant_output_under_load() {
    // 500 scrub events → one Seek (GUI drag storm).
    let batch: Vec<_> = (0..500)
        .map(|i| EngineCmd::ScrubSeek(Tick(i * 1000)))
        .chain(std::iter::once(EngineCmd::Seek(Tick(999_000))))
        .collect();
    let t0 = Instant::now();
    let out = coalesce_commands(batch);
    let us = t0.elapsed().as_micros();
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], EngineCmd::Seek(Tick(999_000))));
    // Pure CPU; 5 ms hard ceiling is generous for CI noise.
    assert!(us < 5_000, "coalesce 500 cmds took {us} µs");
}

#[test]
fn compile_active_slice_under_budget() {
    // 10 video tracks, 1 active clip each — same shape as engine bench.
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("perf", FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;
    for i in 0..10 {
        let asset = AssetId::new();
        let mut track = Track::new(TrackKind::Video, format!("V{i}"));
        track.clips.push(Clip::new(
            ClipSource::Asset { asset },
            Tick::ZERO,
            Tick(TICKS_PER_SECOND * 5),
        ));
        seq.video_tracks.push(track);
    }
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);

    // Warm
    let _ = compile(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW, None);

    let mut samples = Vec::with_capacity(20);
    for i in 0..20 {
        let t = Tick(i * 10_000);
        let start = Instant::now();
        let compiled = compile(&project, seq_id, 0, t, Quality::PREVIEW, None);
        samples.push(start.elapsed().as_micros());
        assert!(
            !compiled.graph.nodes.is_empty(),
            "compile must emit IR nodes"
        );
    }
    samples.sort_unstable();
    let p95 = samples[(samples.len() * 95) / 100];
    eprintln!("compile 10-track p95={p95} µs (budget advisory 5 ms)");
    // Hard: stay under 50 ms even on slow CI (02 §8 compile is pure CPU).
    assert!(p95 < 50_000, "compile p95 {p95} µs exceeds 50 ms");
}

#[test]
fn cut_ahead_scan_is_cheap() {
    let mut seq = Sequence::new("S", FrameRate::FPS_30, 320, 180);
    let mut track = Track::new(TrackKind::Video, "V1");
    for i in 0..50 {
        let asset = AssetId::new();
        track.clips.push(Clip::new(
            ClipSource::Asset { asset },
            Tick(i * 500_000),
            Tick(500_000),
        ));
    }
    seq.video_tracks.push(track);
    let lead = Tick(1_000_000);
    let t0 = Instant::now();
    for i in 0..200 {
        let _ = cut_ahead_targets(&seq, Tick(i * 100_000), lead);
    }
    let us = t0.elapsed().as_micros();
    eprintln!("cut_ahead 200 scans over 50 clips: {us} µs total");
    assert!(us < 10_000, "cut_ahead scan too slow: {us} µs");
}
