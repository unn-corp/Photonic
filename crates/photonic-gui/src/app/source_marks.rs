//! G-10 source marks for the **single** context-driven monitor (24 §3.3).
//!
//! Session-only, non-undoable (ROADMAP S7). Marks are source-clock ticks on the
//! armed media-pool asset; sequence work range stays separate document state.

use photonic_core::timeline::{AssetId, AssetKind, ClipSource, MediaAsset, Tick, TrackKind};

use super::timeline::interact::PendingSource;

/// Session state for source In/Out + armed asset (24 §3.1 `MonitorSession` marks).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceMarksSession {
    /// Last asset used for source peek / marks / 3-point insert.
    pub armed_asset: Option<AssetId>,
    /// Source-clock scrub position while peaking (independent of sequence playhead).
    pub source_time: Tick,
    pub mark_in: Option<Tick>,
    pub mark_out: Option<Tick>,
}

impl SourceMarksSession {
    /// Arm an asset for marks and source peek (pool click / Match Frame).
    pub fn arm(&mut self, asset: AssetId, at: Tick) {
        if self.armed_asset != Some(asset) {
            // New asset → clear previous marks (source range is asset-local).
            self.mark_in = None;
            self.mark_out = None;
        }
        self.armed_asset = Some(asset);
        self.source_time = at;
    }

    pub fn clear_marks(&mut self) {
        self.mark_in = None;
        self.mark_out = None;
    }

    /// Set mark in at `t` (source clock). Keeps `mark_in ≤ mark_out` when both set.
    pub fn set_in(&mut self, t: Tick) {
        self.mark_in = Some(t);
        if let Some(out) = self.mark_out {
            if out < t {
                self.mark_out = Some(t);
            }
        }
        self.source_time = t;
    }

    /// Set mark out at `t` (source clock). Keeps `mark_in ≤ mark_out` when both set.
    pub fn set_out(&mut self, t: Tick) {
        self.mark_out = Some(t);
        if let Some(inn) = self.mark_in {
            if inn > t {
                self.mark_in = Some(t);
            }
        }
        self.source_time = t;
    }

    /// Resolved source range for 3-point insert when marks (or one mark + default) resolve.
    ///
    /// - Both marks → `[in, out)`
    /// - Only in → `[in, in + default_duration)`
    /// - Only out → `[0, out)` when out > 0
    /// - Neither → `None` (caller falls back to whole asset / selection arming)
    pub fn resolved_range(&self, default_duration: Tick) -> Option<(Tick, Tick)> {
        match (self.mark_in, self.mark_out) {
            (Some(a), Some(b)) if a < b => Some((a, b)),
            (Some(a), Some(b)) if a == b => {
                // Degenerate: use default duration from the mark.
                let end = Tick(a.0.saturating_add(default_duration.0.max(1)));
                Some((a, end))
            }
            (Some(a), None) if default_duration.0 > 0 => {
                Some((a, Tick(a.0.saturating_add(default_duration.0))))
            }
            (None, Some(b)) if b.0 > 0 => Some((Tick::ZERO, b)),
            _ => None,
        }
    }

    /// Build a [`PendingSource`] from armed asset + marks when possible.
    pub fn pending_source(
        &self,
        asset: &MediaAsset,
        default_duration: Tick,
    ) -> Option<PendingSource> {
        if self.armed_asset != Some(asset.id) {
            return None;
        }
        let (mut src_in, mut src_out) = self.resolved_range(default_duration)?;
        if let Some((start, end)) = source_bounds(asset) {
            src_in = src_in.max(start);
            src_out = src_out.min(end);
        }
        if src_out <= src_in {
            return None;
        }
        let kind = match asset.kind {
            AssetKind::Audio => TrackKind::Audio,
            AssetKind::Video | AssetKind::Image | AssetKind::VectorDoc => TrackKind::Video,
            AssetKind::Lut3d => return None,
        };
        let name = match &asset.source {
            photonic_core::timeline::AssetSource::File { path, .. } => path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            photonic_core::timeline::AssetSource::EmbeddedVector { .. } => "Embedded vector".into(),
        };
        Some(PendingSource {
            source: ClipSource::Asset { asset: asset.id },
            src_in,
            src_out,
            name,
            kind,
        })
    }
}

/// Available source-clock bounds, including a subclip's restriction.
pub fn source_bounds(asset: &MediaAsset) -> Option<(Tick, Tick)> {
    let duration = asset.probe.as_ref()?.duration;
    let (start, end) = asset.subclip_range.unwrap_or((Tick::ZERO, duration));
    let bounds = (start.max(Tick::ZERO), end.min(duration));
    (bounds.1 > bounds.0).then_some(bounds)
}

impl SourceMarksSession {
    pub fn audition_range(&self, asset: &MediaAsset) -> Option<(Tick, Tick)> {
        if self.armed_asset != Some(asset.id) {
            return None;
        }
        let (start, end) = source_bounds(asset)?;
        let inn = self.mark_in.unwrap_or(self.source_time).clamp(start, end);
        let out = self.mark_out.unwrap_or(end).clamp(start, end);
        (out > inn).then_some((inn, out))
    }
    pub fn clamp_to_asset(&mut self, asset: &MediaAsset) {
        if self.armed_asset != Some(asset.id) {
            return;
        }
        if let Some((start, end)) = source_bounds(asset) {
            if start > end {
                return;
            }
            self.source_time = self.source_time.clamp(start, end);
            self.mark_in = self.mark_in.map(|t| t.clamp(start, end));
            self.mark_out = self.mark_out.map(|t| t.clamp(start, end));
        }
    }
}

/// Whether I/O keys should target source marks (vs sequence work range).
///
/// True only while the single monitor is peaking an asset (24 §3.2). Armed
/// asset alone does **not** sticky-capture I/O — after returning to SEQUENCE,
/// I/O edits work range again; marks remain available for Insert/Overwrite.
pub fn io_targets_source_marks(
    preview_is_asset: bool,
    sequence_playing: bool,
    _armed: Option<AssetId>,
) -> bool {
    if sequence_playing {
        return false;
    }
    preview_is_asset
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::MediaAsset;
    use std::path::PathBuf;

    #[test]
    fn source_range_respects_subclip_and_does_not_invent_audio_duration() {
        let mut asset = MediaAsset::from_file(AssetKind::Audio, "speech.wav");
        let mut session = SourceMarksSession::default();
        session.arm(asset.id, Tick::ZERO);
        assert_eq!(session.audition_range(&asset), None);
        asset.probe = Some(photonic_core::timeline::MediaProbe::basic(
            Tick(100),
            "wav",
            "pcm",
        ));
        asset.subclip_range = Some((Tick(20), Tick(80)));
        assert_eq!(session.audition_range(&asset), Some((Tick(20), Tick(80))));
        session.set_in(Tick(10));
        session.set_out(Tick(90));
        session.clamp_to_asset(&asset);
        assert_eq!(
            session.pending_source(&asset, Tick(100)).unwrap().src_out,
            Tick(80)
        );
        assert_eq!(session.mark_in, Some(Tick(20)));
    }

    #[test]
    fn set_in_out_keeps_order() {
        let mut s = SourceMarksSession::default();
        s.arm(AssetId::nil(), Tick::ZERO);
        s.set_out(Tick(5_000_000));
        s.set_in(Tick(2_000_000));
        assert_eq!(s.mark_in, Some(Tick(2_000_000)));
        assert_eq!(s.mark_out, Some(Tick(5_000_000)));
        s.set_in(Tick(6_000_000));
        assert_eq!(s.mark_in, Some(Tick(6_000_000)));
        assert_eq!(s.mark_out, Some(Tick(6_000_000)));
    }

    #[test]
    fn arm_new_asset_clears_marks() {
        let mut s = SourceMarksSession::default();
        let a = AssetId::new();
        let b = AssetId::new();
        s.arm(a, Tick::ZERO);
        s.set_in(Tick(1));
        s.set_out(Tick(2));
        s.arm(b, Tick(10));
        assert!(s.mark_in.is_none() && s.mark_out.is_none());
        assert_eq!(s.armed_asset, Some(b));
        assert_eq!(s.source_time, Tick(10));
    }

    #[test]
    fn resolved_range_both_marks() {
        let mut s = SourceMarksSession::default();
        s.set_in(Tick(1_000_000));
        s.set_out(Tick(3_000_000));
        assert_eq!(
            s.resolved_range(Tick(5_000_000)),
            Some((Tick(1_000_000), Tick(3_000_000)))
        );
    }

    #[test]
    fn pending_source_from_marks() {
        let mut s = SourceMarksSession::default();
        let asset = MediaAsset::from_file(AssetKind::Video, PathBuf::from("/media/clip.mp4"));
        s.arm(asset.id, Tick::ZERO);
        s.set_in(Tick(1_000_000));
        s.set_out(Tick(4_000_000));
        let ps = s.pending_source(&asset, Tick(10_000_000)).expect("pending");
        assert_eq!(ps.src_in, Tick(1_000_000));
        assert_eq!(ps.src_out, Tick(4_000_000));
        assert_eq!(ps.kind, TrackKind::Video);
        assert_eq!(ps.duration(), Tick(3_000_000));
    }

    #[test]
    fn io_targets_source_only_when_peaking() {
        assert!(io_targets_source_marks(true, false, None));
        // Armed alone must not steal work-range I/O under SEQUENCE.
        assert!(!io_targets_source_marks(false, false, Some(AssetId::nil())));
        assert!(!io_targets_source_marks(true, true, Some(AssetId::nil())));
        assert!(!io_targets_source_marks(false, false, None));
    }

    /// G-10 acceptance: marks under source peek → PendingSource → clip range.
    #[test]
    fn marks_to_insert_payload_preserves_source_range() {
        let mut s = SourceMarksSession::default();
        let asset = MediaAsset::from_file(AssetKind::Video, PathBuf::from("/media/clip.mp4"));
        s.arm(asset.id, Tick(2_000_000));
        // Source peek: I/O targets marks.
        assert!(io_targets_source_marks(true, false, s.armed_asset));
        s.set_in(Tick(2_000_000));
        s.set_out(Tick(5_000_000));
        let ps = s
            .pending_source(&asset, Tick(10_000_000))
            .expect("marks resolve");
        let clip = ps.to_clip(Tick(9_000_000));
        assert_eq!(clip.source_in, Tick(2_000_000));
        assert_eq!(clip.duration, Tick(3_000_000));
        assert_eq!(clip.start, Tick(9_000_000));
        // Play wins: while playing, I/O is not source-targeted.
        assert!(!io_targets_source_marks(true, true, s.armed_asset));
    }

    /// Match Frame style: both marks set (in + remainder out) must not collapse
    /// to full-asset default on re-resolve.
    #[test]
    fn match_frame_style_marks_keep_remainder_out() {
        let mut s = SourceMarksSession::default();
        let asset = MediaAsset::from_file(AssetKind::Video, PathBuf::from("/media/clip.mp4"));
        let matched = Tick(1_500_000);
        let remainder_out = Tick(4_000_000);
        s.arm(asset.id, matched);
        s.mark_in = Some(matched);
        s.mark_out = Some(remainder_out);
        s.source_time = matched;
        let ps = s
            .pending_source(&asset, Tick(99_000_000))
            .expect("match range");
        assert_eq!(ps.src_in, matched);
        assert_eq!(ps.src_out, remainder_out);
        assert_eq!(ps.duration(), Tick(2_500_000));
    }
}
