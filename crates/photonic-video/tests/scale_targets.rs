//! Scale-budget acceptance test (37 §3.2, acceptance test 7).
//!
//! The `photonic_core::timeline::scale` constants declare the project sizes the
//! editor is meant to stay responsive at. These are budget *targets*, not hard
//! limits — exceeding one degrades speed, never correctness — so this test does
//! not assert any rejection. It asserts the opposite: a project built at a
//! target (a) constructs, (b) compiles a frame to non-empty IR, and (c) compiles
//! under the same generous 50 ms CI ceiling `perf_hotpath.rs` uses.
//!
//! Coverage note: the structurally load-bearing targets (tracks, clips, assets,
//! sequences) are exercised here with real project construction + compile. The
//! keyframe / caption-cue / graph-node targets are pinned as constants by the
//! `scale` module's own unit test; a per-target build for those awaits the
//! richer builders in 35 (groups / effect scopes) and is intentionally out of
//! scope for this file.

use std::time::Instant;

use photonic_core::timeline::scale::{
    TARGET_ASSETS_PER_PROJECT, TARGET_CLIPS_PER_SEQUENCE, TARGET_SEQUENCES_PER_PROJECT,
    TARGET_TRACKS_PER_SEQUENCE,
};
use photonic_core::timeline::{
    AssetKind, Clip, ClipSource, FrameRate, MediaAsset, Sequence, SequenceId, Tick,
    TimelineProject, Track, TrackKind, TICKS_PER_SECOND,
};
use photonic_core::Color;
use photonic_video::graph::compile::{compile, Quality};

/// One 2-second solid clip starting at frame span `k` (2 s apart, so a track's
/// clips are sorted and non-overlapping — the `Track::clips` invariant).
fn solid_clip_at(k: usize) -> Clip {
    let start = Tick((k as i64) * 2 * TICKS_PER_SECOND);
    Clip::new(
        ClipSource::SolidColor {
            color: Color::BLACK,
        },
        start,
        Tick(2 * TICKS_PER_SECOND),
    )
}

/// Compile `sequence` 20 times at spread ticks and assert: every frame is
/// non-empty IR, and the p95 sample stays under the 50 ms CI ceiling.
fn assert_compiles_under_budget(project: &TimelineProject, seq_id: SequenceId, label: &str) {
    // Warm the first compile (path allocation, caches) out of the sample set.
    let warm = compile(project, seq_id, 0, Tick::ZERO, Quality::PREVIEW, None);
    assert!(
        !warm.graph.nodes.is_empty(),
        "{label}: compile must emit IR nodes"
    );

    let mut samples = Vec::with_capacity(20);
    for i in 0..20 {
        let t = Tick(i * 10_000);
        let start = Instant::now();
        let compiled = compile(project, seq_id, 0, t, Quality::PREVIEW, None);
        samples.push(start.elapsed().as_micros());
        assert!(
            !compiled.graph.nodes.is_empty(),
            "{label}: compile at tick {t:?} produced empty IR"
        );
    }
    samples.sort_unstable();
    let p95 = samples[(samples.len() * 95) / 100];
    eprintln!("{label}: compile p95={p95} µs (ceiling 50 ms)");
    assert!(p95 < 50_000, "{label}: compile p95 {p95} µs exceeds 50 ms");
}

#[test]
fn tracks_and_clips_at_target_open_compile_and_stay_under_budget() {
    // 100 tracks × 100 clips = 10 000 clips in one sequence (the headline
    // §3.2 target). Bottom→top solid tracks so compile has real work to fold.
    let per_track = TARGET_CLIPS_PER_SEQUENCE / TARGET_TRACKS_PER_SEQUENCE;
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("scale", FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;
    for t in 0..TARGET_TRACKS_PER_SEQUENCE {
        let mut track = Track::new(TrackKind::Video, format!("V{t}"));
        for k in 0..per_track {
            track.clips.push(solid_clip_at(k));
        }
        seq.video_tracks.push(track);
    }

    // (a) constructs at target.
    assert_eq!(seq.video_tracks.len(), TARGET_TRACKS_PER_SEQUENCE);
    let total_clips: usize = seq.video_tracks.iter().map(|t| t.clips.len()).sum();
    assert_eq!(total_clips, TARGET_CLIPS_PER_SEQUENCE);

    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);
    project.sequence_order.push(seq_id);

    // (b) + (c) compiles to non-empty IR under budget.
    assert_compiles_under_budget(&project, seq_id, "10k-clips/100-tracks");
}

#[test]
fn assets_at_target_open_and_compile() {
    let mut project = TimelineProject::new();
    for i in 0..TARGET_ASSETS_PER_PROJECT {
        // No file access — `from_file` only records the path; nothing here reads
        // the asset's bytes, so a synthetic path is sufficient to hit the count.
        let asset = MediaAsset::from_file(AssetKind::Video, format!("/nonexistent/asset_{i}.mp4"));
        project.media.insert(asset);
    }
    assert_eq!(project.media.assets.len(), TARGET_ASSETS_PER_PROJECT);

    // A trivial compilable sequence alongside the large pool.
    let mut seq = Sequence::new("assets", FrameRate::FPS_30, 1280, 720);
    let seq_id = seq.id;
    let mut track = Track::new(TrackKind::Video, "V0");
    track.clips.push(solid_clip_at(0));
    seq.video_tracks.push(track);
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);
    project.sequence_order.push(seq_id);

    assert_compiles_under_budget(&project, seq_id, "5k-assets");
}

#[test]
fn sequences_at_target_open_and_compile_the_active_one() {
    let mut project = TimelineProject::new();
    let mut active = None;
    for s in 0..TARGET_SEQUENCES_PER_PROJECT {
        let mut seq = Sequence::new(format!("S{s}"), FrameRate::FPS_30, 1280, 720);
        let seq_id = seq.id;
        // Give each sequence one solid clip so the active one compiles non-empty.
        let mut track = Track::new(TrackKind::Video, "V0");
        track.clips.push(solid_clip_at(0));
        seq.video_tracks.push(track);
        project.sequences.insert(seq_id, seq);
        project.sequence_order.push(seq_id);
        if active.is_none() {
            active = Some(seq_id);
        }
    }
    assert_eq!(project.sequences.len(), TARGET_SEQUENCES_PER_PROJECT);
    let seq_id = active.unwrap();
    project.active_sequence = Some(seq_id);

    assert_compiles_under_budget(&project, seq_id, "200-sequences");
}
