//! Audio mixer engine (02 §1 module map; 09-audio-mixer.md, normative).
//!
//! Owns everything downstream of the data model in `photonic_core::timeline`
//! (`TrackAudio`/`ClipAudio`/`MasterBus`/`AudioFxUnit`, 09 §2): the cpal device
//! host and master clock ([`engine`]), the lock-free worker→callback ring
//! ([`ring`]), the mixer graph itself ([`mixer`]), and waveform peak-pyramid
//! generation ([`waveform`]).
//!
//! **P3 scope (this module):** CORE signal path per 09 §1 — gain/pan/mute/solo,
//! fades, channel mapping, meters, the realtime block/ring/smoothing
//! architecture (09 §5), and the offline export mix path (09 §7, same code,
//! no device clock). `AudioFxUnit`/`fx_chain` DSP (EQ/Compressor/Gate/Limiter,
//! 09 §6) and the loudness-normalization pass (09 §6.6) are **out of scope**
//! here — they land in P8 per 09 §1's phasing (schema exists now, empty
//! `Vec<AudioFxUnit>`, always a pass-through until then; see [`mixer`] doc).
//!
//! **Crate-seam boundary:** this module owns everything from decoded PCM
//! onward. It does not know how to decode media, nor which clip covers a given
//! timeline tick at any moment — those are `media`/`decode`'s and the P3
//! playback controller's jobs respectively. The seam is [`mixer::PcmSource`]
//! (the decode side implements it over the ffmpeg sidecar pipe; tests here use
//! synthetic sources) and [`mixer::TrackVoice`]/[`mixer::ClipVoice`] (the
//! playback controller resolves "what's active at tick t" once per block and
//! hands the mixer a voice list — see [`mixer::Mixer::render_block`]).

pub mod engine;
pub mod mixer;
pub mod ring;
pub mod spectrum;
pub mod waveform;

/// Mixer block size in frames (09 §5): ~10.7 ms @ 48 kHz. Params/coefficients
/// resolve once per block, not per-sample.
pub const BLOCK_FRAMES: usize = 512;

/// Ring depth in blocks (09 §5): ~32 ms @ 48 kHz interactive-latency budget —
/// deliberately shallow (decouples mixer-worker jitter from the cpal deadline;
/// this is not a prefetch cache, that's the decode ring, 02 §3).
pub const RING_BLOCKS: usize = 3;

/// v1 output bus channel count — stereo only (09 §4: "Surround output is a
/// SPEC.md non-goal").
pub const CHANNELS: usize = 2;

pub use engine::{AudioEngine, AudioEngineError, MasterClock};
pub use mixer::{ClipVoice, DeclickConfig, Mixer, PcmSource, StereoMeter, TrackVoice};
pub use ring::{audio_ring, RingConsumer, RingProducer, XrunCounters};
pub use waveform::{build_pyramid, PeakBucket, WaveformLevel, WaveformPyramid};

/// Synthetic [`mixer::PcmSource`] implementations shared by this module's unit
/// tests (mixer signal-flow tests, waveform pyramid tests) — exercises the
/// mixer/waveform math (09 §11 test hooks) without a real decode dependency.
#[cfg(test)]
pub(crate) mod test_support {
    use super::mixer::PcmSource;

    /// Silence: every sample is 0.0.
    pub(crate) struct SilenceSource {
        pub channels: u16,
        pub sample_rate: u32,
    }
    impl PcmSource for SilenceSource {
        fn channels(&self) -> u16 {
            self.channels
        }
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
        fn read(&mut self, out: &mut [f32], frames: usize) -> usize {
            out[..frames * self.channels as usize].fill(0.0);
            frames
        }
    }

    /// A constant DC value on every channel — useful for exact-gain assertions.
    pub(crate) struct StepSource {
        pub channels: u16,
        pub sample_rate: u32,
        pub value: f32,
    }
    impl PcmSource for StepSource {
        fn channels(&self) -> u16 {
            self.channels
        }
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
        fn read(&mut self, out: &mut [f32], frames: usize) -> usize {
            out[..frames * self.channels as usize].fill(self.value);
            frames
        }
    }

    /// A stationary sine tone, phase-continuous across `read` calls.
    pub(crate) struct SineSource {
        pub channels: u16,
        pub sample_rate: u32,
        pub freq_hz: f32,
        pub amplitude: f32,
        pub phase: f32,
    }
    impl PcmSource for SineSource {
        fn channels(&self) -> u16 {
            self.channels
        }
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
        fn read(&mut self, out: &mut [f32], frames: usize) -> usize {
            let step = std::f32::consts::TAU * self.freq_hz / self.sample_rate as f32;
            for f in 0..frames {
                let s = self.phase.sin() * self.amplitude;
                self.phase += step;
                for c in 0..self.channels as usize {
                    out[f * self.channels as usize + c] = s;
                }
            }
            frames
        }
    }

    /// A finite linear ramp `0..len` (one value per frame, duplicated across
    /// channels), then end-of-stream — used for waveform-pyramid value
    /// assertions where an exact expected min/max/rms per bucket is easy to
    /// hand-compute.
    pub(crate) struct RampSource {
        pub channels: u16,
        pub sample_rate: u32,
        pub len: usize,
        pos: usize,
    }
    impl RampSource {
        pub(crate) fn new(channels: u16, sample_rate: u32, len: usize) -> Self {
            RampSource {
                channels,
                sample_rate,
                len,
                pos: 0,
            }
        }
    }
    impl PcmSource for RampSource {
        fn channels(&self) -> u16 {
            self.channels
        }
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
        fn read(&mut self, out: &mut [f32], frames: usize) -> usize {
            let mut written = 0;
            for f in 0..frames {
                if self.pos >= self.len {
                    break;
                }
                let v = self.pos as f32;
                for c in 0..self.channels as usize {
                    out[f * self.channels as usize + c] = v;
                }
                self.pos += 1;
                written += 1;
            }
            written
        }
    }
}
pub mod dsp;
