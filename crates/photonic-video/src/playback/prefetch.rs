//! Prefetch v1 (02 §3): keep the active clip's decode ring pumped ahead of the
//! playhead along the play direction.
//!
//! **Cut-ahead (02 §3 / §8 / 24 §5):** pure scan of the sequence for the next
//! clip starting within a lead window, plus ring pump for active sources.
//! Session `MediaSources::prefetch_upcoming` warms at most one new sidecar per
//! present — never dual always-on decoders for every clip.

use photonic_core::timeline::{AssetId, ClipSource, FrameRate, Sequence, Tick, TrackKind};

use crate::decode::scheduler::DecodeSource;
use crate::graph::source_range::FrameRange;

// ── Cut-ahead scan (pure) ───────────────────────────────────────────────────

/// One decode target to warm before the playhead reaches a cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CutAheadTarget {
    pub asset: AssetId,
    /// Source-clock time at the clip's in-point (sequence time `clip.start`).
    pub source_time: Tick,
}

/// Scan enabled video tracks for the **next** clip that starts in `(t, t+lead]`.
///
/// Pure / allocation-light: no I/O, no decoder open. The engine applies results
/// through the existing sidecar pool + LRU (at most one open per present).
/// Disabled tracks/clips and non-asset sources are skipped.
pub fn cut_ahead_targets(seq: &Sequence, t: Tick, lead: Tick) -> Vec<CutAheadTarget> {
    let horizon = Tick(t.0.saturating_add(lead.0));
    let mut targets = Vec::new();
    for track in &seq.video_tracks {
        if track.kind != TrackKind::Video || !track.enabled {
            continue;
        }
        let Some(clip) = track
            .clips
            .iter()
            .find(|c| c.enabled && c.start > t && c.start <= horizon)
        else {
            continue;
        };
        if let ClipSource::Asset { asset } = clip.source {
            targets.push(CutAheadTarget {
                asset,
                source_time: clip.source_in,
            });
        }
    }
    targets
}

/// Default cut-ahead lead in frames (session uses this × ticks_per_frame).
/// At 30 fps, 24 frames ≈ 800 ms — meets the ≥500 ms spirit of 02 §3.
pub const CUT_AHEAD_LEAD_FRAMES: i64 = 24;

/// E-1 / 32 §1: derive a decode look-ahead from the compiled graph's source
/// range union. Temporal nodes (deinterlace, future motion blur) expand the
/// window; pure graphs return zero extra lead so cut-ahead stays the default.
///
/// Returns the max distance from `playhead` to either end of `range`, in ticks
/// (never negative). Callers `max` this with the cut-ahead lead.
pub fn lead_from_source_range(range: FrameRange, playhead: Tick) -> Tick {
    let back = playhead.0.saturating_sub(range.first.0);
    let fwd = range.last.0.saturating_sub(playhead.0);
    Tick(back.max(fwd).max(0))
}

/// Combine the fixed cut-ahead lead with the E-1 graph-driven window.
pub fn combined_prefetch_lead(cut_ahead: Tick, source_range: FrameRange, playhead: Tick) -> Tick {
    let from_graph = lead_from_source_range(source_range, playhead);
    Tick(cut_ahead.0.max(from_graph.0))
}

/// Frames decoded per pump call — sized so one engine-loop iteration tops the
/// ring toward capacity without monopolizing the present thread (poll ≈ 4 ms).
pub const PREFETCH_BATCH: usize = 6;

/// How far ahead of the playhead (in frames) the ring should already have a
/// frame before we stop pumping. 12 frames ≈ 400 ms @ 30 fps — smoother cuts
/// without decoding a full second of backlog.
pub const PREFETCH_AHEAD_FRAMES: i64 = 12;

/// Top up `source`'s ring ahead of `playhead` (source-time ticks). Returns how
/// many frames were decoded (0 = ring already ahead / end of stream / no
/// active process).
pub fn pump_ahead(source: &mut DecodeSource, playhead: Tick, rate: FrameRate) -> usize {
    if !source.is_active() {
        // Nothing decoded from this source yet this session (first frame comes
        // from the evaluator's on-demand seek); nothing to pump sequentially.
        return 0;
    }
    let target = Tick(rate.snap(playhead).0 + PREFETCH_AHEAD_FRAMES * rate.ticks_per_frame().0);
    // Caught up only when the decode *frontier* (newest resident pts) has
    // actually reached the look-ahead horizon. Using `frame_covering` here was
    // wrong: it returns any frame at/before `target`, so the single frame left
    // by a seek made this report "pumped ahead" and forward decode never ran.
    if source.ring().newest().is_some_and(|n| n.0 >= target.0) {
        return 0; // ring already reaches the look-ahead horizon
    }
    // Keep the ring's eviction window tracking the consumer.
    source.ring().set_playhead(playhead);
    source.pump(PREFETCH_BATCH).unwrap_or(0)
}

// ── Sidecar pool LRU (pure) ─────────────────────────────────────────────────

/// Cap on concurrent live decode sidecars (session `MediaSources`).
pub const MAX_LIVE_SOURCES: usize = 8;

/// Choose LRU victims so `live_count - victims.len() <= max_live`.
///
/// `entries` is `(id, last_used)` for currently live sources. Never selects an
/// entry with `last_used >= protected_min` (sources touched this present /
/// on-screen). Pure — no HashMap mutation.
pub fn lru_eviction_victims<K: Copy + Eq>(
    entries: &[(K, u64)],
    max_live: usize,
    protected_min: u64,
) -> Vec<K> {
    if entries.len() <= max_live {
        return Vec::new();
    }
    let mut candidates: Vec<(K, u64)> = entries
        .iter()
        .copied()
        .filter(|(_, used)| *used < protected_min)
        .collect();
    candidates.sort_by_key(|(_, used)| *used);
    let need = entries.len().saturating_sub(max_live);
    candidates.into_iter().take(need).map(|(k, _)| k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        AssetKind, Clip, ClipSource, FrameRate, MediaAsset, Sequence, Track, TrackKind,
    };

    #[test]
    fn lead_from_source_range_is_zero_for_identity() {
        let ph = Tick(1000);
        let r = FrameRange::identity(ph);
        assert_eq!(lead_from_source_range(r, ph), Tick(0));
    }

    #[test]
    fn lead_from_source_range_widens_for_deinterlace_window() {
        let ph = Tick(100);
        let r = FrameRange::span(Tick(99), Tick(101));
        assert_eq!(lead_from_source_range(r, ph), Tick(1));
        let combined = combined_prefetch_lead(Tick(24), r, ph);
        assert_eq!(combined, Tick(24)); // cut-ahead still wins
        let wide = FrameRange::span(Tick(0), Tick(200));
        assert_eq!(combined_prefetch_lead(Tick(10), wide, ph), Tick(100));
    }

    #[test]
    fn cut_ahead_finds_next_clip_within_lead() {
        let mut seq = Sequence::new("S", FrameRate::FPS_30, 320, 180);
        let a1 = AssetId::new();
        let a2 = AssetId::new();
        let mut track = Track::new(TrackKind::Video, "V1");
        // Clip A covers [0, 1s); clip B starts at 1s.
        let mut c1 = Clip::new(ClipSource::Asset { asset: a1 }, Tick::ZERO, Tick(1_000_000));
        c1.source_in = Tick(0);
        let mut c2 = Clip::new(
            ClipSource::Asset { asset: a2 },
            Tick(1_000_000),
            Tick(1_000_000),
        );
        c2.source_in = Tick(100);
        track.clips.push(c1);
        track.clips.push(c2);
        seq.video_tracks.push(track);

        // At t=0 with lead covering 1s → should warm B.
        let lead = Tick(1_000_000);
        let targets = cut_ahead_targets(&seq, Tick::ZERO, lead);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].asset, a2);
        assert_eq!(targets[0].source_time, Tick(100));

        // After B starts, no next clip in lead.
        let later = cut_ahead_targets(&seq, Tick(1_000_000), lead);
        assert!(later.is_empty());

        // Lead too short to reach B.
        let short = cut_ahead_targets(&seq, Tick::ZERO, Tick(100));
        assert!(short.is_empty());
    }

    #[test]
    fn cut_ahead_skips_disabled_track_and_clip() {
        let mut seq = Sequence::new("S", FrameRate::FPS_30, 320, 180);
        let a = AssetId::new();
        let mut track = Track::new(TrackKind::Video, "V1");
        track.enabled = false;
        track.clips.push(Clip::new(
            ClipSource::Asset { asset: a },
            Tick(500_000),
            Tick(1_000_000),
        ));
        seq.video_tracks.push(track);
        assert!(cut_ahead_targets(&seq, Tick::ZERO, Tick(1_000_000)).is_empty());

        let mut seq2 = Sequence::new("S2", FrameRate::FPS_30, 320, 180);
        let mut track2 = Track::new(TrackKind::Video, "V1");
        let mut c = Clip::new(
            ClipSource::Asset { asset: a },
            Tick(500_000),
            Tick(1_000_000),
        );
        c.enabled = false;
        track2.clips.push(c);
        seq2.video_tracks.push(track2);
        assert!(cut_ahead_targets(&seq2, Tick::ZERO, Tick(1_000_000)).is_empty());
    }

    #[test]
    fn lru_evicts_coldest_unprotected_only() {
        // 10 live, max 8, protect stamp >= 100 → evict two coldest below 100.
        let entries = [
            (1u32, 10u64),
            (2, 20),
            (3, 30),
            (4, 40),
            (5, 50),
            (6, 60),
            (7, 70),
            (8, 80),
            (9, 100),  // protected
            (10, 110), // protected
        ];
        let victims = lru_eviction_victims(&entries, MAX_LIVE_SOURCES, 100);
        assert_eq!(victims, vec![1, 2]);
        // Under cap: no victims.
        assert!(lru_eviction_victims(&entries[..5], MAX_LIVE_SOURCES, 0).is_empty());
    }

    #[test]
    fn cut_ahead_does_not_require_media_asset_in_pool() {
        // Pure scan only looks at clip sources — no dual always-on open.
        let _ = MediaAsset::from_file(AssetKind::Video, "/x.mp4");
        let mut seq = Sequence::new("S", FrameRate::FPS_30, 64, 64);
        let asset = AssetId::new();
        let mut track = Track::new(TrackKind::Video, "V");
        track
            .clips
            .push(Clip::new(ClipSource::Asset { asset }, Tick(100), Tick(50)));
        seq.video_tracks.push(track);
        let t = cut_ahead_targets(&seq, Tick(0), Tick(200));
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].asset, asset);
    }
}
