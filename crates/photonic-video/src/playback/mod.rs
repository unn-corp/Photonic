//! Playback controller, clock, prefetch (02 §4).
//!
//! - [`clock`] — the position clock: audio-mastered while playing (02 §4
//!   "master clock = audio"), settable soft clock while paused/scrubbing.
//! - [`controller`] — play/pause/seek/step/loop state machine + the
//!   cover-interval frame presenter with late-drop counting.
//! - [`prefetch`] — v1 ring pumping for active sources (cut-ahead is a
//!   documented seam).
//! - [`policy`] — documented prefill / drop-recovery knobs (32 §4 / E-5).
//! - [`pcm`] — [`FfmpegPcmSource`](pcm::FfmpegPcmSource) and
//!   [`TimeWarpPcmSource`](pcm::TimeWarpPcmSource), the mixer's
//!   [`PcmSource`](crate::audio::PcmSource) over an ffmpeg `-f f32le` sidecar
//!   pipe (documented seam: moves into `decode/` when the parallel PCM pipe
//!   story lands there).
//!
//! The engine thread that drives all of this lives in [`crate::session`].

pub mod clock;
pub mod controller;
pub mod pcm;
pub mod policy;
pub mod prefetch;

pub use clock::PlaybackClock;
pub use controller::{FramePresenter, PlaybackController, PresentDecision};
pub use pcm::{FfmpegPcmSource, TimeWarpPcmSource};
pub use policy::PlaybackPolicy;
