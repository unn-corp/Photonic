//! Playback state machine + frame presentation (02 §4).
//!
//! - [`PlaybackController`] — play/pause/seek/step/loop over a [`PlaybackClock`]
//!   (audio-mastered while playing when a device is up, soft otherwise).
//! - [`FramePresenter`] — the cover-interval rule: video presents the frame
//!   whose `[pts, pts+frame)` interval covers clock time; late > 1 frame ⇒
//!   dropped (counted); early ⇒ hold.
//!
//! Both are pure state machines (no threads, no I/O) so the engine thread in
//! `session.rs` drives them and tests can simulate any clock trajectory.

use std::sync::Arc;

use photonic_core::timeline::{FrameRate, Tick};

use super::clock::PlaybackClock;
use crate::audio::MasterClock;

/// The cover-interval frame selector with late-drop accounting (02 §4).
#[derive(Debug, Default)]
pub struct FramePresenter {
    /// Frame index of the last presented frame, if any.
    last_frame: Option<i64>,
    /// Frames that were skipped over (late > 1 frame) instead of presented.
    dropped: u64,
    /// Present on the next decision even if the frame index is unchanged
    /// (seek/step/edit re-present).
    force: bool,
}

impl FramePresenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total frames dropped (late by more than one frame) since creation.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Force the next [`decide`](Self::decide) to present, even if clock time
    /// still maps to the already-presented frame (seek, step, doc edit).
    pub fn force_next(&mut self) {
        self.force = true;
    }

    /// Apply the cover-interval rule at clock time `now`: `Some(tick)` = the
    /// exact frame-start tick to evaluate and present; `None` = hold (the
    /// covering frame is already on screen, or we're early).
    ///
    /// `playing` gates drop-counting: while paused, jumping many frames is a
    /// seek, not lateness.
    pub fn decide(&mut self, now: Tick, rate: FrameRate, playing: bool) -> Option<Tick> {
        let frame = rate.frame_at(now);
        if !self.force {
            if let Some(last) = self.last_frame {
                if last == frame {
                    return None; // hold: still inside the presented interval
                }
                if playing && frame > last + 1 {
                    // Late by more than one frame: the skipped-over frames are
                    // dropped, and we jump straight to the covering frame.
                    self.dropped += (frame - last - 1) as u64;
                }
            }
        }
        self.force = false;
        self.last_frame = Some(frame);
        Some(rate.frame_start(frame))
    }
}

/// What the engine thread should do this tick.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PresentDecision {
    /// Compile + evaluate exactly this tick and publish the frame.
    Present(Tick),
    /// Nothing to do (frame already on screen / early).
    Hold,
    /// Playing past the loop end: seek to the loop start (stay playing).
    LoopWrap(Tick),
}

/// Playback state machine (02 §4): owns the clock, the loop range, and the
/// presenter. Driven by the engine thread; never blocks.
pub struct PlaybackController {
    clock: PlaybackClock,
    rate: FrameRate,
    loop_range: Option<(Tick, Tick)>,
    presenter: FramePresenter,
}

impl PlaybackController {
    pub fn new(rate: FrameRate) -> Self {
        PlaybackController {
            clock: PlaybackClock::new(),
            rate,
            loop_range: None,
            presenter: FramePresenter::new(),
        }
    }

    /// Update the frame cadence (active sequence changed). Cheap + idempotent.
    pub fn set_rate(&mut self, rate: FrameRate) {
        if self.rate != rate {
            self.rate = rate;
            self.presenter.force_next();
        }
    }

    pub fn rate(&self) -> FrameRate {
        self.rate
    }

    pub fn playhead(&self) -> Tick {
        self.clock.now()
    }

    pub fn is_playing(&self) -> bool {
        self.clock.is_playing()
    }

    pub fn dropped(&self) -> u64 {
        self.presenter.dropped()
    }

    pub fn set_loop(&mut self, range: Option<(Tick, Tick)>) {
        // Normalize inverted/empty ranges away rather than looping nowhere.
        self.loop_range = range.filter(|(s, e)| e > s);
    }

    pub fn loop_range(&self) -> Option<(Tick, Tick)> {
        self.loop_range
    }

    /// Start playing on the soft (wall-time) clock — the no-audio-device path.
    pub fn play_soft(&mut self) {
        self.clock.play_soft();
    }

    /// Start playing mastered by the audio clock (02 §4).
    pub fn play_audio(&mut self, master: Arc<MasterClock>) {
        self.clock.play_audio(master);
    }

    pub fn pause(&mut self) {
        self.clock.pause();
    }

    /// Force the next [`tick`](Self::tick) to re-present the covering frame
    /// even if it hasn't changed (doc edit, proxy-mode/sequence switch).
    pub fn request_present(&mut self) {
        self.presenter.force_next();
    }

    /// Seek (latest-wins coalescing happens upstream in the engine's command
    /// drain — see `session::coalesce_commands`). Forces a re-present.
    pub fn seek(&mut self, t: Tick) {
        self.clock.set(t);
        self.presenter.force_next();
    }

    /// Step (CAP-004): pause, snap the clock to exactly ±`frames` frame starts
    /// from the current frame, and force evaluation of exactly that tick.
    pub fn step(&mut self, frames: i32) {
        self.clock.pause();
        let cur = self.rate.frame_at(self.clock.now());
        let target = (cur + frames as i64).max(0);
        self.clock.set(self.rate.frame_start(target));
        self.presenter.force_next();
    }

    /// One engine-loop decision at the current clock time.
    pub fn tick(&mut self) -> PresentDecision {
        let now = self.clock.now();
        if self.clock.is_playing() {
            if let Some((start, end)) = self.loop_range {
                if now >= end {
                    return PresentDecision::LoopWrap(start);
                }
            }
        }
        match self
            .presenter
            .decide(now, self.rate, self.clock.is_playing())
        {
            Some(t) => PresentDecision::Present(t),
            None => PresentDecision::Hold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: FrameRate = FrameRate::FPS_30;

    fn tpf() -> i64 {
        RATE.ticks_per_frame().0
    }

    /// The story's soft-clock playback test: simulate 2s of clock samples (4
    /// samples per frame, with sub-frame jitter) and assert presentation is
    /// monotonic, frame selection follows the cover-interval rule, and nothing
    /// is dropped when the clock never skips.
    #[test]
    fn simulated_2s_playback_presents_every_frame_monotonically() {
        let mut p = FramePresenter::new();
        let mut presented: Vec<Tick> = Vec::new();
        let samples_per_frame = 4;
        let total_frames = 60; // 2s @ 30fps
        for s in 0..(total_frames * samples_per_frame) {
            let jitter = (s % 3) as i64 * 7; // sub-frame, deterministic
            let now = Tick(s as i64 * tpf() / samples_per_frame as i64 + jitter);
            if let Some(t) = p.decide(now, RATE, true) {
                presented.push(t);
            }
        }
        assert_eq!(presented.len(), total_frames, "one present per frame");
        for (i, t) in presented.iter().enumerate() {
            assert_eq!(
                *t,
                Tick(i as i64 * tpf()),
                "frame {i} presented at its exact frame-start tick"
            );
        }
        assert!(
            presented.windows(2).all(|w| w[0] < w[1]),
            "presentation is strictly monotonic"
        );
        assert_eq!(p.dropped(), 0, "no drops when the clock never skips");
    }

    #[test]
    fn late_by_more_than_one_frame_counts_drops() {
        let mut p = FramePresenter::new();
        assert_eq!(p.decide(Tick(0), RATE, true), Some(Tick(0)));
        // Clock jumps from frame 0 to frame 5: frames 1-4 were never shown.
        let t5 = Tick(5 * tpf());
        assert_eq!(p.decide(t5, RATE, true), Some(t5));
        assert_eq!(p.dropped(), 4);
        // While paused the same jump is a seek, not lateness.
        let mut q = FramePresenter::new();
        q.decide(Tick(0), RATE, false);
        q.decide(Tick(9 * tpf()), RATE, false);
        assert_eq!(q.dropped(), 0);
    }

    #[test]
    fn hold_within_the_covering_interval_and_force_re_presents() {
        let mut p = FramePresenter::new();
        assert_eq!(p.decide(Tick(0), RATE, true), Some(Tick(0)));
        // Mid-frame sample: still covered by frame 0 => hold.
        assert_eq!(p.decide(Tick(tpf() / 2), RATE, true), None);
        p.force_next();
        assert_eq!(
            p.decide(Tick(tpf() / 2), RATE, true),
            Some(Tick(0)),
            "force re-presents the covering frame at its exact tick"
        );
    }

    #[test]
    fn step_is_exact_tick_and_pauses() {
        let mut c = PlaybackController::new(RATE);
        c.seek(Tick(10 * tpf() + 123)); // mid-frame 10
        assert_eq!(c.tick(), PresentDecision::Present(Tick(10 * tpf())));
        c.play_soft();
        c.step(1);
        assert!(!c.is_playing(), "step pauses");
        assert_eq!(c.playhead(), Tick(11 * tpf()), "clock snapped exactly");
        assert_eq!(c.tick(), PresentDecision::Present(Tick(11 * tpf())));
        c.step(-2);
        assert_eq!(c.tick(), PresentDecision::Present(Tick(9 * tpf())));
    }

    #[test]
    fn loop_wrap_fires_at_loop_end_while_playing() {
        let mut c = PlaybackController::new(RATE);
        c.set_loop(Some((Tick(0), Tick(2 * tpf()))));
        c.seek(Tick(3 * tpf()));
        // Paused: no wrap, just present.
        assert_eq!(c.tick(), PresentDecision::Present(Tick(3 * tpf())));
        c.play_soft();
        assert_eq!(c.tick(), PresentDecision::LoopWrap(Tick(0)));
        // Inverted/empty ranges are ignored.
        c.set_loop(Some((Tick(5), Tick(5))));
        assert_eq!(c.loop_range(), None);
    }
}
