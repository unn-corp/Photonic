//! Formal §6 seek-budget harness for 24-preview-media-load.
//!
//! Asserts product-relevant contracts that can be measured headlessly without
//! a full interactive GUI / GPU play path:
//!
//! | Metric | Budget | Hardness |
//! |--------|--------|----------|
//! | Scrub seek coalesce | Drop intermediate seeks (latest-wins) | **Hard** |
//! | Warm keyframe index lookup (CPU) | ≤ 1 ms p95 (100× random) | **Hard** (pure CPU) |
//! | Exact Draft frame after seek (decode) | ≤ 150 ms p95 product | Soft — integration-only |
//! | Draft quality → proxy compile flag | PREVIEW.proxy=true, FULL=false | **Hard** |
//! | Play start / ring hit | ring API surface present | Smoke (API exists) |
//!
//! See `docs/specs/video-editor/24-preview-media-load.md` §6 and
//! `11-testing-phasing.md` §4.1.

#![expect(clippy::assertions_on_constants)]

use std::path::PathBuf;
use std::time::Instant;

use photonic_core::timeline::{
    AssetKind, MediaAsset, Sequence, Tick, TimelineProject, TICKS_PER_SECOND,
};
use photonic_video::decode::{FrameRing, SharedRing};
use photonic_video::graph::compile::{compile, Quality};
use photonic_video::media::KeyframeIndex;
use photonic_video::{coalesce_commands, EngineCmd, PreviewQuality, ProxyMode};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn counter_mp4() -> PathBuf {
    fixtures_dir().join("counter.mp4")
}

fn tools_or_skip() -> Option<photonic_video::media::FfmpegTools> {
    photonic_video::media::locate().ok()
}

// ── 1. ScrubSeek + Seek latest-wins coalesce ────────────────────────────────

/// Batch of many ScrubSeek + Seek collapses to a single position command = last.
/// Non-position cmds (SetProxyMode / SetPreviewQuality) are preserved.
#[test]
fn scrub_seek_coalesce_latest_wins() {
    let batch = vec![
        EngineCmd::SetPreviewQuality(PreviewQuality::Draft),
        EngineCmd::ScrubSeek(Tick(1_000)),
        EngineCmd::ScrubSeek(Tick(2_000)),
        EngineCmd::ScrubSeek(Tick(3_000)),
        EngineCmd::Seek(Tick(4_000)),
        EngineCmd::ScrubSeek(Tick(5_000)),
        EngineCmd::SetProxyMode(ProxyMode::Auto),
        EngineCmd::Seek(Tick(9_999)), // last position — wins
        EngineCmd::SetPreviewQuality(PreviewQuality::Full),
    ];
    let out = coalesce_commands(batch);

    // quality Draft, proxy Auto, last Seek(9999), quality Full — 4 cmds.
    // First SetPreviewQuality kept; SetProxyMode kept; last position kept;
    // trailing SetPreviewQuality kept.
    assert_eq!(
        out.len(),
        4,
        "expected 2 quality + 1 proxy + 1 position, got {out:?}"
    );
    assert!(matches!(
        out[0],
        EngineCmd::SetPreviewQuality(PreviewQuality::Draft)
    ));
    assert!(matches!(out[1], EngineCmd::SetProxyMode(ProxyMode::Auto)));
    assert!(
        matches!(out[2], EngineCmd::Seek(Tick(9_999))),
        "last position cmd must be Seek(9999), got {:?}",
        out[2]
    );
    assert!(matches!(
        out[3],
        EngineCmd::SetPreviewQuality(PreviewQuality::Full)
    ));

    // Only one position command survives.
    let pos_count = out
        .iter()
        .filter(|c| matches!(c, EngineCmd::Seek(_) | EngineCmd::ScrubSeek(_)))
        .count();
    assert_eq!(pos_count, 1);
}

// ── Optional: drag-simulation coalesce ──────────────────────────────────────

/// Simulate a continuous scrub drag: 50 ScrubSeek drained via coalesce_commands.
#[test]
fn seek_coalesce_under_drag_simulation() {
    let mut batch: Vec<EngineCmd> = Vec::with_capacity(52);
    batch.push(EngineCmd::SetPreviewQuality(PreviewQuality::Draft));
    for i in 0..50 {
        // Ascending scrub ticks during a drag.
        batch.push(EngineCmd::ScrubSeek(Tick(i * 10_000)));
    }
    batch.push(EngineCmd::SetProxyMode(ProxyMode::ForceProxy));

    let out = coalesce_commands(batch);
    assert_eq!(out.len(), 3, "quality + last scrub + proxy; got {out:?}");
    assert!(matches!(
        out[0],
        EngineCmd::SetPreviewQuality(PreviewQuality::Draft)
    ));
    let final_tick = match &out[1] {
        EngineCmd::ScrubSeek(t) => *t,
        other => panic!("expected final ScrubSeek, got {other:?}"),
    };
    assert_eq!(final_tick, Tick(49 * 10_000), "latest-wins scrub tick");
    assert!(matches!(
        out[2],
        EngineCmd::SetProxyMode(ProxyMode::ForceProxy)
    ));
}

// ── 2. Warm keyframe index lookup budget (CPU, hard ≤ 1 ms p95) ─────────────

/// Build/load KeyframeIndex for counter.mp4, 100× keyframe_before at random
/// ticks within duration, assert p95 pure-CPU lookup ≤ 1 ms (O(log n) warm).
///
/// Full decode-to-frame p95 (product ≤ 150 ms with warm index + proxy) remains
/// integration-only and is **not** hard-asserted here — see soft note below.
#[test]
fn warm_keyframe_index_lookup_budget() {
    let Some(tools) = tools_or_skip() else {
        eprintln!("skip warm_keyframe_index_lookup_budget: no ffmpeg/ffprobe");
        return;
    };
    let path = counter_mp4();
    assert!(path.is_file(), "fixture missing: {}", path.display());

    let cache =
        std::env::temp_dir().join(format!("photonic-seek-budget-kf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();

    let hash = photonic_video::media::content_hash(&path).expect("hash");
    let idx = KeyframeIndex::load_or_build(&tools, &path, &cache, &hash).expect("keyframes");
    assert!(
        !idx.keyframes.is_empty(),
        "counter.mp4 must have at least one keyframe"
    );
    // Warm load path: second load is cache-only (no ffprobe).
    let warm = KeyframeIndex::load(&cache, &hash).expect("warm load from sidecar");
    assert_eq!(warm.keyframes, idx.keyframes);

    // counter.mp4: 10s @ 30fps (frame_truth.json).
    let duration = Tick(10 * TICKS_PER_SECOND);
    const N: usize = 100;
    // Deterministic LCG for reproducible "random" ticks (no rand dep).
    let mut state: u64 = 0xC0FFEE_u64;
    let mut times_ns: Vec<u128> = Vec::with_capacity(N);
    for _ in 0..N {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let r = (state >> 33) as i64; // 31 bits
        let tick = Tick((r % duration.0.max(1)).max(0));

        let t0 = Instant::now();
        let _kf = warm.keyframe_before(tick);
        times_ns.push(t0.elapsed().as_nanos());
    }

    times_ns.sort_unstable();
    // p95 index: ceil(0.95 * N) - 1
    let p95_idx = ((N as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(N - 1);
    let p95_ns = times_ns[p95_idx];
    let p95_us = p95_ns as f64 / 1_000.0;
    let p95_ms = p95_ns as f64 / 1_000_000.0;

    // Hard: pure CPU binary search must be well under 1 ms p95.
    assert!(
        p95_ms <= 1.0,
        "warm keyframe_before p95 {p95_ms:.4} ms > 1 ms (index not O(log n) / not warm?)"
    );

    eprintln!(
        "warm keyframe_before: n={N} p95={p95_us:.3} µs ({p95_ms:.6} ms) \
         max={:.3} µs keyframes={}",
        times_ns[N - 1] as f64 / 1_000.0,
        warm.keyframes.len()
    );
    eprintln!(
        "NOTE: full decode-to-frame Draft seek p95 (≤ 150 ms product budget) is \
         integration-only; this test only proves warm index lookup is O(log n)."
    );

    // Soft wall for load_or_build of tiny fixture (not product p95).
    let t_build = Instant::now();
    let _ = KeyframeIndex::load(&cache, &hash);
    let load_ms = t_build.elapsed().as_millis();
    if load_ms > 150 {
        eprintln!(
            "SOFT: sidecar keyframe load took {load_ms} ms (product Draft seek \
             budget is ≤ 150 ms including decode — load alone is soft)"
        );
    }

    let _ = std::fs::remove_dir_all(&cache);
}

// ── 3. Draft quality selects proxy flag in compile ──────────────────────────

/// Quality::PREVIEW.proxy == true; Quality::FULL.proxy == false.
/// Also verified on a minimal sequence compile DecodeVideo node.
#[test]
fn draft_quality_selects_proxy_flag_in_compile() {
    assert!(
        Quality::PREVIEW.proxy,
        "Draft/PREVIEW must request proxy decode inputs"
    );
    assert!(
        !Quality::FULL.proxy,
        "Full must request original decode inputs"
    );

    let mut project = TimelineProject::new();
    let asset = MediaAsset::from_file(AssetKind::Video, counter_mp4());
    let asset_id = asset.id;
    project.media.insert(asset);
    let seq = Sequence::new("S", photonic_core::timeline::FrameRate::FPS_30, 320, 180);
    let seq_id = seq.id;
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);

    // Empty sequence still compiles; DecodeVideo appears once a clip is present.
    use photonic_core::timeline::{Clip, ClipSource, Track, TrackKind};
    let mut track = Track::new(TrackKind::Video, "V1");
    track.clips.push(Clip::new(
        ClipSource::Asset { asset: asset_id },
        Tick::ZERO,
        Tick(TICKS_PER_SECOND),
    ));
    project
        .sequences
        .get_mut(&seq_id)
        .unwrap()
        .video_tracks
        .push(track);

    let draft = compile(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW, None);
    let full = compile(&project, seq_id, 0, Tick::ZERO, Quality::FULL, None);

    let draft_proxy = draft.graph.nodes.iter().find_map(|n| match n.op {
        photonic_video::graph::ir::IrOp::DecodeVideo { proxy, .. } => Some(proxy),
        _ => None,
    });
    let full_proxy = full.graph.nodes.iter().find_map(|n| match n.op {
        photonic_video::graph::ir::IrOp::DecodeVideo { proxy, .. } => Some(proxy),
        _ => None,
    });
    assert_eq!(
        draft_proxy,
        Some(true),
        "PREVIEW compile must set proxy=true"
    );
    assert_eq!(full_proxy, Some(false), "FULL compile must set proxy=false");
}

// ── Ring / play-start smoke (API existence; no GPU) ─────────────────────────

/// Assert decode ring API used by play-start / ring-hit path exists and behaves
/// for latest-wins covering semantics. Full GPU play start budget is skipped.
#[test]
fn ring_api_existence_for_play_start_path() {
    // Defaults match 02 §3 preview window (perf-tuned depths).
    assert_eq!(photonic_video::decode::ring::DEFAULT_FWD, 24);
    assert_eq!(photonic_video::decode::ring::DEFAULT_BACK, 6);

    let ring = FrameRing::preview();
    assert!(ring.is_empty());
    assert!(ring.frame_covering(Tick::ZERO).is_none());

    // SharedRing is the engine-thread facing type (session media sources).
    let shared = SharedRing::preview();
    assert!(shared.frame_covering(Tick::ZERO).is_none());

    // Coalesce + ScrubSeek surface used when GUI starts play after scrub.
    let out = coalesce_commands(vec![
        EngineCmd::ScrubSeek(Tick(100)),
        EngineCmd::ScrubSeek(Tick(200)),
        EngineCmd::Play,
    ]);
    assert_eq!(out.len(), 2);
    assert!(matches!(out[0], EngineCmd::ScrubSeek(Tick(200))));
    assert!(matches!(out[1], EngineCmd::Play));
}

// ── Cut-ahead / prefetch contract (02 §3 / 24 §5) ───────────────────────────

#[test]
fn prefetch_ahead_horizon_is_contractual() {
    use photonic_video::playback::prefetch::{
        CUT_AHEAD_LEAD_FRAMES, PREFETCH_AHEAD_FRAMES, PREFETCH_BATCH,
    };
    // Look-ahead must cover multiple frames so cut-adjacent clips can warm.
    assert!(PREFETCH_AHEAD_FRAMES >= 4);
    assert!(PREFETCH_BATCH >= 1);
    // ≥ 500 ms spirit at 30fps ≈ 15 frames — v1 ships 8 frames (~267 ms at 30fps)
    // as incremental ring top-up; cut-ahead next-clip open remains session-owned.
    assert!(PREFETCH_AHEAD_FRAMES <= 32);
    // Cut-ahead lead frames × 30fps ticks ≥ 500 ms (500_000 µs).
    let tpf = TICKS_PER_SECOND / 30;
    let lead_us = CUT_AHEAD_LEAD_FRAMES * tpf;
    assert!(
        lead_us >= 500_000,
        "CUT_AHEAD_LEAD_FRAMES must cover ≥500 ms at 30fps (got {lead_us} µs)"
    );
}

/// Pure cut-ahead scan: next clip within lead is selected; no dual always-on
/// decoder implied (scan only returns asset ids + source times).
#[test]
fn cut_ahead_scan_next_clip_within_lead() {
    use photonic_core::timeline::{
        AssetId, Clip, ClipSource, FrameRate, Sequence, Track, TrackKind,
    };
    use photonic_video::playback::prefetch::cut_ahead_targets;

    let mut seq = Sequence::new("S", FrameRate::FPS_30, 320, 180);
    let a_cur = AssetId::new();
    let a_next = AssetId::new();
    let mut track = Track::new(TrackKind::Video, "V1");
    // Clip A: [0, 1s); Clip B starts at 1s (same layout as unit tests).
    track.clips.push(Clip::new(
        ClipSource::Asset { asset: a_cur },
        Tick::ZERO,
        Tick(1_000_000),
    ));
    let mut next = Clip::new(
        ClipSource::Asset { asset: a_next },
        Tick(1_000_000),
        Tick(1_000_000),
    );
    next.source_in = Tick(50);
    track.clips.push(next);
    seq.video_tracks.push(track);

    let lead = Tick(1_000_000);
    let targets = cut_ahead_targets(&seq, Tick::ZERO, lead);
    assert_eq!(
        targets.len(),
        1,
        "expected one cut-ahead target, got {targets:?}"
    );
    assert_eq!(targets[0].asset, a_next);
    assert_eq!(targets[0].source_time, Tick(50));
    // Past the cut: nothing further to warm.
    assert!(cut_ahead_targets(&seq, Tick(1_000_000), lead).is_empty());
}

/// Soft product budget: warm keyframe_before + optional index build for
/// counter.mp4. Hard-fail only on extreme multi-second stalls; p95 product
/// 150 ms is logged (24 §6 soft when machine variance is high).
#[test]
fn soft_draft_seek_budget_with_warm_index() {
    let Some(tools) = tools_or_skip() else {
        eprintln!("skip soft_draft_seek_budget: no ffmpeg");
        return;
    };
    let path = counter_mp4();
    if !path.is_file() {
        eprintln!("skip soft_draft_seek_budget: missing fixture");
        return;
    }
    let cache = std::env::temp_dir().join(format!("photonic-seek-soft-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();
    let hash = photonic_video::media::content_hash(&path).expect("hash");
    let idx = KeyframeIndex::load_or_build(&tools, &path, &cache, &hash).expect("index");
    let mut samples = Vec::with_capacity(50);
    for i in 0..50 {
        let t = Tick((i as i64 * 100_000) % (TICKS_PER_SECOND * 5).max(1));
        let start = Instant::now();
        let _ = idx.keyframe_before(t);
        samples.push(start.elapsed().as_micros());
    }
    samples.sort_unstable();
    let p95 = samples[(samples.len() * 95) / 100];
    eprintln!(
        "soft seek: warm keyframe_before p95={p95} µs (product Draft decode p95 ≤150 ms is full path)"
    );
    // Hard structural: warm index lookup is sub-millisecond class on SSD.
    assert!(p95 < 50_000, "warm index lookup p95 too slow: {p95} µs");
    let _ = std::fs::remove_dir_all(&cache);
}
