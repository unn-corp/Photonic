//! cpal audio device host (09 §5 threading recap; 02 §1). Owns the real-time
//! output callback (no locks, no allocation) and the sample-accurate master
//! clock (02 §4: "master clock = audio").
//!
//! Device open is **lazy** — deferred to the first [`AudioEngine::start`] call
//! (10 §7's headless recommendation: probing hardware at construction time
//! would make every headless/CI/MCP session pay for a device that may not
//! exist). Failure is always a structured [`AudioEngineError`], never a panic.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

use super::ring::RingConsumer;
use super::CHANNELS;

/// Structured device/stream failure (10 §7: "init failure → structured error,
/// never panic").
#[derive(Debug, thiserror::Error)]
pub enum AudioEngineError {
    #[error("no output audio device available")]
    NoOutputDevice,
    #[error("unsupported output sample format: {0:?}")]
    UnsupportedSampleFormat(SampleFormat),
    #[error("engine already has an active output stream")]
    AlreadyStarted,
    #[error("no prepared output stream")]
    NotPrepared,
    #[error(transparent)]
    Cpal(#[from] cpal::Error),
}

/// The sample-accurate master clock (02 §4): total frames the cpal callback
/// has written to the device since [`AudioEngine::start`]. The playback
/// controller reads this while playing; paused/scrubbing uses a settable
/// software clock it owns itself (out of this crate's `audio/` scope).
#[derive(Debug, Default)]
pub struct MasterClock {
    frames_written: AtomicU64,
    /// 0 until a stream has started.
    sample_rate: AtomicU32,
}

impl MasterClock {
    pub fn frames(&self) -> u64 {
        self.frames_written.load(Ordering::Acquire)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Acquire)
    }

    pub fn seconds(&self) -> f64 {
        let sr = self.sample_rate();
        if sr == 0 {
            0.0
        } else {
            self.frames() as f64 / sr as f64
        }
    }

    fn advance(&self, frames: u64) {
        self.frames_written.fetch_add(frames, Ordering::AcqRel);
    }

    fn set_sample_rate(&self, sr: u32) {
        self.sample_rate.store(sr, Ordering::Release);
    }
}

/// Largest per-callback frame count this module will process without
/// allocating (real-time-safety: the scratch buffer sized to this is
/// allocated once, before `stream.play()`, and reused for the stream's
/// lifetime). Device callbacks in practice request far fewer frames; if one
/// ever asks for more, the callback loops over `SCRATCH_FRAMES`-sized
/// sub-chunks rather than growing the buffer.
const SCRATCH_FRAMES: usize = 4096;

/// The cpal host plus an optional active output stream.
pub struct AudioEngine {
    stream: Option<cpal::Stream>,
    clock: Arc<MasterClock>,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    pub fn new() -> Self {
        AudioEngine {
            stream: None,
            clock: Arc::new(MasterClock::default()),
        }
    }

    /// A handle to the master clock, readable from any thread.
    pub fn clock(&self) -> Arc<MasterClock> {
        self.clock.clone()
    }

    /// Open the default output device (lazy: first call only) and start
    /// draining `consumer` in the real-time callback. Prefers the device's
    /// default config, which on most platforms negotiates to 48 kHz stereo
    /// f32 (09 §4's sample-rate policy); non-f32 sample formats are converted
    /// in the callback, and non-stereo channel counts are handled by
    /// duplicating/truncating our stereo mix (v1 output bus is stereo only,
    /// 09 §4). Returns the negotiated sample rate.
    pub fn start(&mut self, consumer: RingConsumer) -> Result<u32, AudioEngineError> {
        let sample_rate = self.prepare(consumer)?;
        if let Err(error) = self.play_prepared() {
            self.stop();
            return Err(error);
        }
        Ok(sample_rate)
    }

    /// Build a paused stream so a source audition can prefill PCM before its
    /// master clock starts. Existing program playback uses `start` above.
    pub fn prepare(&mut self, consumer: RingConsumer) -> Result<u32, AudioEngineError> {
        if self.stream.is_some() {
            return Err(AudioEngineError::AlreadyStarted);
        }
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioEngineError::NoOutputDevice)?;
        let supported = device.default_output_config()?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let sample_rate = config.sample_rate;
        let channels = config.channels as usize;

        self.clock.set_sample_rate(sample_rate);

        let stream = match sample_format {
            SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, consumer, self.clock.clone(), channels)?
            }
            SampleFormat::I16 => {
                build_stream::<i16>(&device, &config, consumer, self.clock.clone(), channels)?
            }
            SampleFormat::U16 => {
                build_stream::<u16>(&device, &config, consumer, self.clock.clone(), channels)?
            }
            other => return Err(AudioEngineError::UnsupportedSampleFormat(other)),
        };
        self.stream = Some(stream);
        Ok(sample_rate)
    }

    pub fn play_prepared(&self) -> Result<(), AudioEngineError> {
        self.stream
            .as_ref()
            .ok_or(AudioEngineError::NotPrepared)?
            .play()?;
        Ok(())
    }

    /// Stop and drop the active stream, if any. Idempotent.
    pub fn stop(&mut self) {
        self.stream = None;
    }

    pub fn is_started(&self) -> bool {
        self.stream.is_some()
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: RingConsumer,
    clock: Arc<MasterClock>,
    channels: usize,
) -> Result<cpal::Stream, AudioEngineError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = channels.max(1);
    let mut scratch = vec![0f32; SCRATCH_FRAMES * CHANNELS];

    let stream = device.build_output_stream(
        *config,
        move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
            let frames_total = data.len() / channels;
            let mut done = 0usize;
            while done < frames_total {
                let chunk_frames = (frames_total - done).min(SCRATCH_FRAMES);
                let stereo = &mut scratch[..chunk_frames * CHANNELS];
                consumer.fill(stereo);
                for f in 0..chunk_frames {
                    let l = stereo[f * CHANNELS];
                    let r = stereo[f * CHANNELS + 1];
                    let out_base = (done + f) * channels;
                    if channels == 1 {
                        data[out_base] = T::from_sample((l + r) * 0.5);
                    } else {
                        data[out_base] = T::from_sample(l);
                        data[out_base + 1] = T::from_sample(r);
                        for c in 2..channels {
                            data[out_base + c] = T::from_sample(0.0f32);
                        }
                    }
                }
                done += chunk_frames;
            }
            clock.advance(frames_total as u64);
        },
        |err| tracing::error!(error = %err, "photonic-video audio stream error"),
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::ring::audio_ring;

    /// 09 §11 / this story's DoD: cpal device tests skip-with-message when no
    /// audio device is present (CI headless), never fail the suite.
    #[test]
    fn opens_default_device_or_skips() {
        let host = cpal::default_host();
        if host.default_output_device().is_none() {
            eprintln!("SKIP opens_default_device_or_skips: no output audio device available");
            return;
        }

        let mut engine = AudioEngine::new();
        let (_producer, consumer, _xruns) = audio_ring();
        match engine.start(consumer) {
            Ok(sr) => {
                assert!(sr > 0);
                assert!(engine.is_started());
                let clock = engine.clock();
                assert_eq!(clock.sample_rate(), sr);
                engine.stop();
                assert!(!engine.is_started());
            }
            Err(e) => {
                // A device was enumerated but couldn't actually be opened
                // (e.g. a CI runner's dummy/null ALSA device) — still not a
                // suite failure, per the DoD's headless-skip rule.
                eprintln!("SKIP opens_default_device_or_skips: device open failed: {e}");
            }
        }
    }

    #[test]
    fn double_start_errors_without_panicking() {
        let host = cpal::default_host();
        if host.default_output_device().is_none() {
            eprintln!(
                "SKIP double_start_errors_without_panicking: no output audio device available"
            );
            return;
        }
        let mut engine = AudioEngine::new();
        let (_p1, c1, _x1) = audio_ring();
        if engine.start(c1).is_err() {
            eprintln!("SKIP double_start_errors_without_panicking: device open failed");
            return;
        }
        let (_p2, c2, _x2) = audio_ring();
        assert!(matches!(
            engine.start(c2),
            Err(AudioEngineError::AlreadyStarted)
        ));
        engine.stop();
    }

    #[test]
    fn master_clock_defaults_to_zero() {
        let clock = MasterClock::default();
        assert_eq!(clock.frames(), 0);
        assert_eq!(clock.sample_rate(), 0);
        assert_eq!(clock.seconds(), 0.0);
    }
}
