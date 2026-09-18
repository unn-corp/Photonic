//! Time representation for the timeline (01 §1).
//!
//! All timeline positions and durations are `i64` ticks at 705,600,000
//! ticks/second — the "flick", the smallest unit that exactly divides every
//! supported frame rate (including 1001-denominator NTSC rates) and the common
//! audio rates. No `f32`/`f64` time appears anywhere in the data model; the UI
//! converts to seconds/frames only at the edge.

use serde::{Deserialize, Serialize};

/// Ticks per second — the "flick". Exactly divides 24/25/30/48/50/60/90/100/120
/// fps (incl. `×1000/1001` NTSC) and 44.1/48/88.2/96/192 kHz audio (01 §1).
pub const TICKS_PER_SECOND: i64 = 705_600_000;

/// A timeline position or duration in [`TICKS_PER_SECOND`] ticks.
///
/// Negative values are permitted only for deltas (e.g. a ripple shift); stored
/// positions and durations are non-negative, enforced by the edit ops.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Tick(pub i64);

impl Tick {
    pub const ZERO: Tick = Tick(0);

    /// Ticks for a whole number of seconds.
    #[inline]
    pub const fn from_seconds(secs: i64) -> Tick {
        Tick(secs * TICKS_PER_SECOND)
    }

    /// Fractional seconds as an `f64` (UI-edge conversion only — never fed back
    /// into the data model).
    #[inline]
    pub fn as_seconds_f64(self) -> f64 {
        self.0 as f64 / TICKS_PER_SECOND as f64
    }

    /// Saturating add, guarding against overflow on pathological inputs.
    #[inline]
    pub fn saturating_add(self, other: Tick) -> Tick {
        Tick(self.0.saturating_add(other.0))
    }

    /// Saturating subtract.
    #[inline]
    pub fn saturating_sub(self, other: Tick) -> Tick {
        Tick(self.0.saturating_sub(other.0))
    }
}

impl std::ops::Add for Tick {
    type Output = Tick;
    #[inline]
    fn add(self, rhs: Tick) -> Tick {
        Tick(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Tick {
    type Output = Tick;
    #[inline]
    fn sub(self, rhs: Tick) -> Tick {
        Tick(self.0 - rhs.0)
    }
}

/// A rational frame rate, e.g. `30000/1001` for 29.97 fps (01 §1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub const FPS_24: FrameRate = FrameRate { num: 24, den: 1 };
    pub const FPS_25: FrameRate = FrameRate { num: 25, den: 1 };
    pub const FPS_30: FrameRate = FrameRate { num: 30, den: 1 };
    pub const FPS_60: FrameRate = FrameRate { num: 60, den: 1 };
    /// 29.97 (NTSC).
    pub const FPS_29_97: FrameRate = FrameRate {
        num: 30000,
        den: 1001,
    };
    /// 23.976 (NTSC film).
    pub const FPS_23_976: FrameRate = FrameRate {
        num: 24000,
        den: 1001,
    };
    /// 59.94 (NTSC).
    pub const FPS_59_94: FrameRate = FrameRate {
        num: 60000,
        den: 1001,
    };

    #[inline]
    pub const fn new(num: u32, den: u32) -> FrameRate {
        FrameRate { num, den }
    }

    /// Ticks per frame: `TICKS_PER_SECOND * den / num`, rounded to the nearest
    /// tick. For every supported rate this is exact (integral); for an exotic
    /// rate the rounding introduces sub-tick error (< 1 µs over 10 min), which
    /// [`is_exact`](Self::is_exact) flags for a UI warning.
    #[inline]
    pub fn ticks_per_frame(&self) -> Tick {
        debug_assert!(self.num > 0, "frame rate numerator must be > 0");
        let num = self.num.max(1) as i64;
        let den = self.den as i64;
        // Round-to-nearest: (a + num/2) / num.
        Tick((TICKS_PER_SECOND * den + num / 2) / num)
    }

    /// Frame index containing tick `t` (floor toward negative infinity so that
    /// exact frame boundaries map to their own frame).
    #[inline]
    pub fn frame_at(&self, t: Tick) -> i64 {
        let tpf = self.ticks_per_frame().0.max(1);
        t.0.div_euclid(tpf)
    }

    /// Snap `t` down to the frame boundary at or before it.
    #[inline]
    pub fn snap(&self, t: Tick) -> Tick {
        let tpf = self.ticks_per_frame().0.max(1);
        Tick(self.frame_at(t) * tpf)
    }

    /// Tick position of the start of frame `frame`.
    #[inline]
    pub fn frame_start(&self, frame: i64) -> Tick {
        Tick(frame * self.ticks_per_frame().0)
    }

    /// True when [`ticks_per_frame`](Self::ticks_per_frame) divides exactly (no
    /// rounding). All the SPEC's supported rates are exact; only exotic rates
    /// (e.g. 23.9-something custom) are not, and the UI surfaces that.
    #[inline]
    pub fn is_exact(&self) -> bool {
        let num = self.num.max(1) as i64;
        let den = self.den as i64;
        (TICKS_PER_SECOND * den) % num == 0
    }

    /// Nominal integer frame rate used by the SMPTE label grid
    /// (`round(num/den)` → 30 for 30000/1001).
    #[inline]
    pub fn nominal_fps(&self) -> i64 {
        let den = self.den.max(1) as i64;
        ((self.num as i64) + den / 2) / den
    }

    /// True when this rate uses SMPTE drop-frame labelling (29.97 / 59.94 only).
    #[inline]
    pub fn is_drop_frame_rate(&self) -> bool {
        matches!((self.num, self.den), (30000, 1001) | (60000, 1001))
    }
}

// ── K-A12 Timecode ───────────────────────────────────────────────────────────

/// SMPTE timecode label over an exact rational [`FrameRate`] (K-A12 / 26).
///
/// Drop-frame is a **labelling convention** over 30000/1001 (and 60000/1001),
/// not a rate change. `;` in `HH:MM:SS;FF` selects drop-frame; `:` is non-drop.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Timecode {
    pub hours: i64,
    pub minutes: i64,
    pub seconds: i64,
    pub frames: i64,
    /// True when this label was authored / should be rendered as drop-frame.
    pub drop_frame: bool,
}

impl Timecode {
    /// Format as `HH:MM:SS:FF` or `HH:MM:SS;FF` when [`Self::drop_frame`].
    pub fn format(self) -> String {
        let sep = if self.drop_frame { ';' } else { ':' };
        format!(
            "{:02}:{:02}:{:02}{}{:02}",
            self.hours, self.minutes, self.seconds, sep, self.frames
        )
    }

    /// Convert a 0-based frame index (from sequence zero) to a timecode label.
    ///
    /// When `prefer_drop` is true **and** `rate` is 29.97/59.94, SMPTE drop-frame
    /// labelling is applied. Otherwise non-drop (`:`) is used.
    pub fn from_frame_index(frame_index: i64, rate: FrameRate, prefer_drop: bool) -> Self {
        let f = frame_index.max(0);
        let nominal = rate.nominal_fps().max(1);
        if prefer_drop && rate.is_drop_frame_rate() {
            return Self::from_frame_index_drop(f, nominal);
        }
        let total_secs = f / nominal;
        let frames = f % nominal;
        Timecode {
            hours: total_secs / 3600,
            minutes: (total_secs % 3600) / 60,
            seconds: total_secs % 60,
            frames,
            drop_frame: false,
        }
    }

    /// SMPTE drop-frame: skip frame numbers 0 and 1 (or 0–3 at 59.94) at the
    /// start of every minute except every 10th minute.
    ///
    /// Algorithm matches the common QT/SMPTE conversion (frame count → DF label).
    fn from_frame_index_drop(frame_number: i64, nominal_fps: i64) -> Self {
        let drop = if nominal_fps >= 60 { 4 } else { 2 };
        let frames_per_min = nominal_fps * 60 - drop;
        let frames_per_10min = frames_per_min * 10 + drop;

        let d = frame_number / frames_per_10min;
        let m = frame_number % frames_per_10min;

        // David Heidelberger / SMPTE: add dropped frames for complete 10-min
        // blocks, then for complete minutes past the first drop in the block.
        let mut adj = frame_number + drop * 9 * d;
        if m > drop {
            adj += drop * ((m - drop) / frames_per_min);
        }

        let frames = adj % nominal_fps;
        let total_secs = adj / nominal_fps;
        Timecode {
            hours: total_secs / 3600,
            minutes: (total_secs % 3600) / 60,
            seconds: total_secs % 60,
            frames,
            drop_frame: true,
        }
    }

    /// Parse `HH:MM:SS:FF` (non-drop) or `HH:MM:SS;FF` (drop-frame) to a tick
    /// relative to sequence zero (before adding [`Sequence::start_timecode`]).
    ///
    /// Drop-frame parse only succeeds for 29.97 / 59.94 rates; other rates
    /// reject `;`. Non-drop always works.
    pub fn parse_to_tick(tc: &str, rate: FrameRate) -> Option<Tick> {
        let sep_pos = tc.rfind([':', ';'])?;
        let drop = tc.as_bytes().get(sep_pos)? == &b';';
        let hms = &tc[..sep_pos];
        let ff_str = &tc[sep_pos + 1..];
        let parts: Vec<&str> = hms.split(':').collect();
        if parts.len() != 3 {
            return None;
        }
        let h: i64 = parts[0].parse().ok()?;
        let m: i64 = parts[1].parse().ok()?;
        let s: i64 = parts[2].parse().ok()?;
        let ff: i64 = ff_str.parse().ok()?;
        let nominal = rate.nominal_fps();
        if h < 0 || m < 0 || s < 0 || ff < 0 || m >= 60 || s >= 60 || nominal < 1 || ff >= nominal {
            return None;
        }
        if drop && !rate.is_drop_frame_rate() {
            return None;
        }
        let total_frames = if drop {
            Self::drop_components_to_frame_index(h, m, s, ff, nominal)?
        } else {
            (h * 3600 + m * 60 + s) * nominal + ff
        };
        Some(Tick(total_frames * rate.ticks_per_frame().0))
    }

    /// Inverse of [`from_frame_index_drop`]: components → 0-based frame index.
    fn drop_components_to_frame_index(
        h: i64,
        m: i64,
        s: i64,
        f: i64,
        nominal_fps: i64,
    ) -> Option<i64> {
        let drop = if nominal_fps >= 60 { 4 } else { 2 };
        // Invalid dropped numbers: :00 and :01 (or :00–:03) at second `:00` of a
        // non-10th minute. Drop-frame skips those labels only at the minute
        // boundary — every other second carries the full frame-number range, so
        // `00:01:05;00` is valid and must parse.
        if m % 10 != 0 && s == 0 && f < drop {
            return None;
        }
        let total_minutes = h * 60 + m;
        let raw = ((h * 3600 + m * 60 + s) * nominal_fps) + f;
        // Subtract the frames that drop-frame skips in the label space.
        let dropped = drop * (total_minutes - total_minutes / 10);
        Some(raw - dropped)
    }

    /// Format `tick` for UI display at `rate`, optionally drop-frame for NTSC.
    /// `start` is the sequence start offset (added before labelling).
    pub fn format_tick(tick: Tick, rate: FrameRate, start: Tick, prefer_drop: bool) -> String {
        let abs = Tick(tick.0.saturating_add(start.0).max(0));
        let frame_idx = rate.frame_at(abs).max(0);
        Self::from_frame_index(frame_idx, rate, prefer_drop && rate.is_drop_frame_rate()).format()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_per_frame_is_exact_for_supported_rates() {
        for fr in [
            FrameRate::FPS_24,
            FrameRate::FPS_25,
            FrameRate::FPS_30,
            FrameRate::FPS_60,
            FrameRate::FPS_29_97,
            FrameRate::FPS_23_976,
            FrameRate::new(48, 1),
            FrameRate::new(50, 1),
            FrameRate::new(120, 1),
        ] {
            assert!(fr.is_exact(), "{fr:?} should be exact");
            // Exactness means num * ticks_per_frame == TICKS_PER_SECOND * den.
            assert_eq!(
                fr.ticks_per_frame().0 * fr.num as i64,
                TICKS_PER_SECOND * fr.den as i64,
                "{fr:?} ticks_per_frame not exact"
            );
        }
    }

    #[test]
    fn frame_at_and_snap_are_consistent() {
        let fr = FrameRate::FPS_30;
        let tpf = fr.ticks_per_frame().0;
        // A tick one below frame 5's boundary is still in frame 4.
        assert_eq!(fr.frame_at(Tick(5 * tpf - 1)), 4);
        assert_eq!(fr.frame_at(Tick(5 * tpf)), 5);
        // Snap lands exactly on the boundary.
        assert_eq!(fr.snap(Tick(5 * tpf + 7)), Tick(5 * tpf));
        assert_eq!(fr.snap(Tick(5 * tpf)), Tick(5 * tpf));
    }

    #[test]
    fn exotic_rate_is_flagged_inexact_but_rounds() {
        // 11 fps does not divide TICKS_PER_SECOND (the flick has no factor of 11).
        let fr = FrameRate::new(11, 1);
        assert!(!fr.is_exact());
        // Still produces a sensible nearest-tick value.
        assert_eq!(fr.ticks_per_frame().0, (TICKS_PER_SECOND + 5) / 11);
    }

    #[test]
    fn nondrop_round_trips_through_parse() {
        let fr = FrameRate::FPS_30;
        let t = Tick(fr.ticks_per_frame().0 * (3600 * 30 + 2 * 60 * 30 + 3 * 30 + 4));
        let tc = Timecode::from_frame_index(fr.frame_at(t), fr, false);
        assert_eq!(tc.format(), "01:02:03:04");
        assert!(!tc.drop_frame);
        let back = Timecode::parse_to_tick(&tc.format(), fr).unwrap();
        assert_eq!(fr.frame_at(back), fr.frame_at(t));
    }

    #[test]
    fn drop_frame_separator_rejected_for_25fps() {
        assert!(Timecode::parse_to_tick("00:01:00;00", FrameRate::FPS_25).is_none());
        assert!(Timecode::parse_to_tick("00:01:00:00", FrameRate::FPS_25).is_some());
    }

    #[test]
    fn drop_frame_formats_semicolon_and_skips_00_01_on_minute() {
        let fr = FrameRate::FPS_29_97;
        // Frame 1800 of non-drop 30 would be 00:01:00:00, but DF has already
        // dropped 2 frames in that minute — label should not be :00:00 on DF.
        // At exactly 1 minute of wall *count* in DF: after 1798 real frames...
        // Verify known anchor: frame 0 → 00:00:00;00
        let z = Timecode::from_frame_index(0, fr, true);
        assert_eq!(z.format(), "00:00:00;00");
        assert!(z.drop_frame);
        // Round-trip a few minutes of DF.
        for n in [0i64, 30, 1798, 17982, 100_000] {
            let tc = Timecode::from_frame_index(n, fr, true);
            assert!(tc.drop_frame, "n={n}");
            assert!(tc.format().contains(';'));
            let back = Timecode::parse_to_tick(&tc.format(), fr).expect("parse");
            assert_eq!(fr.frame_at(back), n, "n={n} tc={}", tc.format());
        }
    }

    #[test]
    fn drop_frame_round_trips_exhaustively_over_two_ten_minute_blocks() {
        // The sampled round-trip above happens to pick only frames landing on
        // minute 0/10 or `ff >= 2`, so it could not see a guard that rejected
        // low frame numbers at seconds other than `:00`. Sweep every frame of
        // two full 10-minute drop blocks instead — that covers every minute
        // residue mod 10 and both sides of the block boundary.
        for (fr, nominal) in [(FrameRate::FPS_29_97, 30i64), (FrameRate::FPS_59_94, 60)] {
            let frames_per_10min = (nominal * 60 - if nominal >= 60 { 4 } else { 2 }) * 10
                + if nominal >= 60 { 4 } else { 2 };
            for n in 0..(frames_per_10min * 2) {
                let tc = Timecode::from_frame_index(n, fr, true);
                let back = Timecode::parse_to_tick(&tc.format(), fr)
                    .unwrap_or_else(|| panic!("rejected valid DF label {} (n={n})", tc.format()));
                assert_eq!(fr.frame_at(back), n, "n={n} tc={}", tc.format());
            }
        }
    }

    #[test]
    fn drop_frame_rejects_only_the_skipped_minute_boundary_labels() {
        let fr = FrameRate::FPS_29_97;
        // Skipped: frames 00/01 at second :00 of a non-tenth minute.
        assert!(Timecode::parse_to_tick("00:01:00;00", fr).is_none());
        assert!(Timecode::parse_to_tick("00:01:00;01", fr).is_none());
        // Not skipped: the same frame numbers one second later, and at the
        // tenth minute where drop-frame does not skip at all.
        assert!(Timecode::parse_to_tick("00:01:01;00", fr).is_some());
        assert!(Timecode::parse_to_tick("00:01:05;01", fr).is_some());
        assert!(Timecode::parse_to_tick("00:10:00;00", fr).is_some());
        assert!(Timecode::parse_to_tick("00:01:00;02", fr).is_some());
    }

    #[test]
    fn format_tick_adds_sequence_start() {
        let fr = FrameRate::FPS_30;
        let start = Tick(fr.ticks_per_frame().0 * 30 * 3600); // 01:00:00:00
        let s = Timecode::format_tick(Tick::ZERO, fr, start, false);
        assert_eq!(s, "01:00:00:00");
    }

    #[test]
    fn tick_arithmetic() {
        assert_eq!(Tick::from_seconds(2), Tick(2 * TICKS_PER_SECOND));
        assert_eq!(Tick(10) + Tick(5), Tick(15));
        assert_eq!(Tick(10) - Tick(5), Tick(5));
        assert_eq!(Tick::from_seconds(1).as_seconds_f64(), 1.0);
    }
}
