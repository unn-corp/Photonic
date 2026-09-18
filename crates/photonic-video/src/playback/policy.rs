//! Explicit playback drop / prefill policy (32 §4 / 26 E-5).
//!
//! Stating these knobs in one place closes the "undocumented policy" complaint
//! from 27 and gives soak tests a single source of truth. Values match the
//! 32 §4 table; ring-depth byte budgeting lives in 32 §5 / 25 and is not
//! duplicated here.

/// Documented playback policy (32 §4). Pure constants — no runtime mutation.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PlaybackPolicy {
    /// Decoded frames held before playback starts (Draft quality).
    pub prefill_frames_draft: u32,
    /// Decoded frames held before playback starts (Full quality).
    pub prefill_frames_full: u32,
    /// After this many consecutive late drops, force-render the next frame
    /// rather than continuing to drop indefinitely.
    pub max_consecutive_drops: u32,
    /// Forward ring depth in frames (superseded by the §5 byte budget when
    /// that lands; kept here as the named default).
    pub ring_fwd_frames: u32,
    /// Backward ring depth in frames.
    pub ring_back_frames: u32,
}

impl PlaybackPolicy {
    /// Spec defaults from 32 §4.
    pub const DEFAULT: Self = Self {
        prefill_frames_draft: 6,
        prefill_frames_full: 12,
        max_consecutive_drops: 5,
        ring_fwd_frames: 24,
        ring_back_frames: 6,
    };

    /// Prefill depth for a given interactive quality.
    pub fn prefill_for(self, full_quality: bool) -> u32 {
        if full_quality {
            self.prefill_frames_full
        } else {
            self.prefill_frames_draft
        }
    }
}

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Underrun behaviour: report `buffering`, hold the last presented frame —
/// **never** present black (32 §4).
pub const UNDERRUN_HOLDS_LAST_FRAME: bool = true;

/// Recovery after `max_consecutive_drops`: force-render the next covering
/// frame, then re-evaluate the drop budget (32 §4).
pub const RECOVERY_FORCE_RENDERS: bool = true;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec_table() {
        let p = PlaybackPolicy::DEFAULT;
        assert_eq!(p.prefill_frames_draft, 6);
        assert_eq!(p.prefill_frames_full, 12);
        assert_eq!(p.max_consecutive_drops, 5);
        assert_eq!(p.ring_fwd_frames, 24);
        assert_eq!(p.ring_back_frames, 6);
        assert_eq!(p.prefill_for(false), 6);
        assert_eq!(p.prefill_for(true), 12);
        const {
            assert!(UNDERRUN_HOLDS_LAST_FRAME);
            assert!(RECOVERY_FORCE_RENDERS);
        }
    }
}
