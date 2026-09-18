//! Playback clocks (02 §4).
//!
//! **Master clock = audio while playing**: position is derived from the
//! sample-accurate [`MasterClock`] (frames the cpal callback has written).
//! Paused / scrubbing / audio-device-less playback uses a settable soft clock
//! (wall-time anchored while soft-playing, frozen while paused).
//!
//! The clock only answers "what time is it"; frame selection (cover-interval
//! rule, drop counting) lives in [`super::controller`].

use std::sync::Arc;
use std::time::Instant;

use photonic_core::timeline::{Tick, TICKS_PER_SECOND};

use crate::audio::MasterClock;

/// How the clock currently advances.
enum ClockMode {
    /// Frozen: `now() == origin` (paused / scrub).
    Paused,
    /// Soft clock: wall time since `anchor`, 1:1 rate (no audio device — the
    /// audio engine opens lazily, 02 §1/§4).
    Soft { anchor: Instant },
    /// Audio master: frames the device callback consumed since `anchor_frames`
    /// (02 §4 "position = audio samples consumed by cpal callback").
    Audio {
        master: Arc<MasterClock>,
        anchor_frames: u64,
    },
}

/// The playback position clock: audio-mastered while playing (when a device is
/// up), soft otherwise, settable while paused/scrubbing.
pub struct PlaybackClock {
    /// Position at the anchor instant/frame-count.
    origin: Tick,
    mode: ClockMode,
}

impl Default for PlaybackClock {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackClock {
    /// Paused at tick 0.
    pub fn new() -> Self {
        PlaybackClock {
            origin: Tick::ZERO,
            mode: ClockMode::Paused,
        }
    }

    /// The current position.
    pub fn now(&self) -> Tick {
        match &self.mode {
            ClockMode::Paused => self.origin,
            ClockMode::Soft { anchor } => {
                let elapsed = anchor.elapsed();
                // 1 µs granularity is far below a tick's resolution needs.
                let ticks =
                    (elapsed.as_micros() as i64).saturating_mul(TICKS_PER_SECOND / 1_000_000);
                self.origin.saturating_add(Tick(ticks))
            }
            ClockMode::Audio {
                master,
                anchor_frames,
            } => {
                let sr = master.sample_rate();
                if sr == 0 {
                    return self.origin;
                }
                let frames = master.frames().saturating_sub(*anchor_frames) as i128;
                let ticks = frames * TICKS_PER_SECOND as i128 / sr as i128;
                self.origin.saturating_add(Tick(ticks as i64))
            }
        }
    }

    /// Set the position (seek / scrub / step). Re-anchors in any mode, so a
    /// seek during playback keeps playing from the new position.
    pub fn set(&mut self, t: Tick) {
        self.origin = t;
        match &mut self.mode {
            ClockMode::Paused => {}
            ClockMode::Soft { anchor } => *anchor = Instant::now(),
            ClockMode::Audio {
                master,
                anchor_frames,
            } => *anchor_frames = master.frames(),
        }
    }

    /// Start advancing on wall time from the current position.
    pub fn play_soft(&mut self) {
        self.origin = self.now();
        self.mode = ClockMode::Soft {
            anchor: Instant::now(),
        };
    }

    /// Start advancing on the audio master clock from the current position
    /// (anchor = frames already consumed, so only *new* samples move time).
    pub fn play_audio(&mut self, master: Arc<MasterClock>) {
        self.origin = self.now();
        let anchor_frames = master.frames();
        self.mode = ClockMode::Audio {
            master,
            anchor_frames,
        };
    }

    /// Freeze at the current position.
    pub fn pause(&mut self) {
        self.origin = self.now();
        self.mode = ClockMode::Paused;
    }

    pub fn is_playing(&self) -> bool {
        !matches!(self.mode, ClockMode::Paused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_clock_holds_and_sets() {
        let mut c = PlaybackClock::new();
        assert_eq!(c.now(), Tick::ZERO);
        assert!(!c.is_playing());
        c.set(Tick::from_seconds(3));
        assert_eq!(c.now(), Tick::from_seconds(3));
    }

    #[test]
    fn soft_clock_advances_monotonically() {
        let mut c = PlaybackClock::new();
        c.set(Tick::from_seconds(1));
        c.play_soft();
        assert!(c.is_playing());
        let a = c.now();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let b = c.now();
        assert!(b > a, "soft clock advances ({a:?} -> {b:?})");
        assert!(b >= Tick::from_seconds(1));
        c.pause();
        let frozen = c.now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(c.now(), frozen, "paused clock is frozen");
    }

    #[test]
    fn audio_clock_tracks_master_frames() {
        // A MasterClock that never advances (no device) pins the position.
        let master = Arc::new(MasterClock::default());
        let mut c = PlaybackClock::new();
        c.set(Tick::from_seconds(2));
        c.play_audio(master);
        assert!(c.is_playing());
        // sample_rate() == 0 (never started) => position holds at origin.
        assert_eq!(c.now(), Tick::from_seconds(2));
    }
}
