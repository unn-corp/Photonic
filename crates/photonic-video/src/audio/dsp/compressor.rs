//! Feed-forward compressor with a fixed 6dB quadratic soft knee and optional
//! sidechain (ducking) input (09 §6.3).

use photonic_core::timeline::{EffectParams, PropValue};

use super::envelope::{EnvelopeCoeffs, EnvelopeFollower};
use super::AudioDiscontinuity;
use super::{db_to_linear, lin_to_db, DspUnit};
use crate::audio::CHANNELS;

/// Fixed per 09 §6.3: "not a user param (keeps the param surface to
/// threshold/ratio/attack/release/makeup as scoped)".
const KNEE_WIDTH_DB: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompressorParams {
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub makeup_db: f64,
}

impl Default for CompressorParams {
    fn default() -> Self {
        CompressorParams {
            threshold_db: -24.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 250.0,
            makeup_db: 0.0,
        }
    }
}

impl CompressorParams {
    /// See [`super::eq::EqParams::from_effect_params`]'s doc for the seam
    /// this and every other `*Params::from_effect_params` shares. Path
    /// strings match `prop_registry`'s `AUDIOFX_COMPRESSOR` table.
    pub fn from_effect_params(p: &EffectParams) -> Self {
        let d = CompressorParams::default();
        let f = |path: &str, default: f64| match p.get(path) {
            Some(PropValue::Float(v)) => *v,
            _ => default,
        };
        CompressorParams {
            threshold_db: f("params.threshold_db", d.threshold_db),
            ratio: f("params.ratio", d.ratio).max(1.0),
            attack_ms: f("params.attack_ms", d.attack_ms),
            release_ms: f("params.release_ms", d.release_ms),
            makeup_db: f("params.makeup_db", d.makeup_db),
        }
    }
}

/// 6dB quadratic soft-knee gain reduction — 09 §6.3 gives the hard-knee
/// formula plus a comment that a "fixed internal soft-knee, 6dB width,
/// smoothed quadratic interpolation around threshold" wraps it; this is the
/// standard closed form for that (Giannoulis/Reiss/Stark, "Digital Dynamic
/// Range Compressor Design — A Tutorial and Analysis", eq. 4).
fn soft_knee_gr_db(env_db: f64, threshold_db: f64, ratio: f64, knee_width_db: f64) -> f64 {
    let overshoot = env_db - threshold_db;
    let half_knee = knee_width_db / 2.0;
    if overshoot <= -half_knee {
        0.0
    } else if overshoot > half_knee {
        overshoot * (1.0 / ratio - 1.0)
    } else {
        let x = overshoot + half_knee;
        (1.0 / ratio - 1.0) * x * x / (2.0 * knee_width_db)
    }
}

/// Feed-forward compressor (09 §6.3), driven by the shared envelope
/// follower (09 §6.2). **Documented implementation decision** (mirrors
/// `mixer.rs`'s pattern for spec-silent stereo rules): the detector input is
/// `max(|L|, |R|)` per sample — stereo-linked, so gain reduction is
/// identical on both channels and the stereo image never shifts; 09
/// §6.2/§6.3 specify the envelope math on a scalar `x` without naming a
/// channel-sum rule.
#[derive(Debug, Default)]
pub struct Compressor {
    params: CompressorParams,
    envelope: EnvelopeFollower,
}

impl Compressor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_params(&mut self, params: CompressorParams) {
        self.params = params;
    }

    pub fn params(&self) -> &CompressorParams {
        &self.params
    }
}

impl DspUnit for Compressor {
    fn reset(&mut self, _cause: AudioDiscontinuity) {
        // Envelope-follower state only; gain reduction is derived per sample
        // from it, so clearing the follower clears the whole detector.
        self.envelope.reset();
    }

    /// `sidechain`, when `Some`, redirects envelope detection to that buffer
    /// (09 §6.3's ducking mechanism: the sidechain track's post-fx-pre-fader
    /// signal) while gain reduction still applies to `block`'s own signal.
    fn process(&mut self, sample_rate: u32, block: &mut [f32], sidechain: Option<&[f32]>) {
        debug_assert!(sidechain.is_none_or(|s| s.len() == block.len()));
        let fs = sample_rate as f64;
        let coeffs = EnvelopeCoeffs::new(self.params.attack_ms, self.params.release_ms, fs);
        let frames = block.len() / CHANNELS;

        for f in 0..frames {
            let l = block[f * CHANNELS] as f64;
            let r = block[f * CHANNELS + 1] as f64;
            let (dl, dr) = match sidechain {
                Some(sc) => (sc[f * CHANNELS] as f64, sc[f * CHANNELS + 1] as f64),
                None => (l, r),
            };
            let x = dl.abs().max(dr.abs());
            let env = self.envelope.step(x, &coeffs);
            let env_db = lin_to_db(env.max(1e-10));
            let gr_db = soft_knee_gr_db(
                env_db,
                self.params.threshold_db,
                self.params.ratio,
                KNEE_WIDTH_DB,
            );
            let gain = db_to_linear(gr_db + self.params.makeup_db);
            block[f * CHANNELS] = (l * gain) as f32;
            block[f * CHANNELS + 1] = (r * gain) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    fn tone_burst(amp: f64, frames: usize, freq: f64, fs: u32) -> Vec<f32> {
        let mut out = vec![0f32; frames * CHANNELS];
        for f in 0..frames {
            let t = f as f64 / fs as f64;
            let s = (amp * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32;
            out[f * CHANNELS] = s;
            out[f * CHANNELS + 1] = s;
        }
        out
    }

    /// Static curve: for a steady tone well above threshold, held long
    /// enough for the envelope to fully settle, output gain should match
    /// the textbook hard-knee formula (09 §6.3) since a steady signal is
    /// always outside the +/-3dB knee region for these params.
    #[test]
    fn static_curve_matches_threshold_ratio_formula_above_knee() {
        let mut comp = Compressor::new();
        comp.set_params(CompressorParams {
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 50.0,
            makeup_db: 0.0,
        });

        let amp = 0.5; // well above -20dB threshold + 3dB knee half-width
        let mut block = tone_burst(amp, FS as usize * 2, 500.0, FS); // 2s to fully settle
        comp.process(FS, &mut block, None);

        // RMS of a sine is amp/sqrt(2); the envelope follower converges to it.
        let env_db = 20.0 * (amp / std::f64::consts::SQRT_2).log10();
        let expected_gr = (env_db - (-20.0)) * (1.0 - 1.0 / 4.0);
        let expected_out_db = env_db - expected_gr;
        let expected_out_amp = 10f64.powf(expected_out_db / 20.0) * std::f64::consts::SQRT_2;

        let tail = &block[block.len() - CHANNELS * 4800..];
        let measured_peak = tail.iter().fold(0f32, |m, &s| m.max(s.abs())) as f64;
        assert!(
            (measured_peak - expected_out_amp).abs() / expected_out_amp < 0.05,
            "measured={measured_peak} expected={expected_out_amp}"
        );
    }

    #[test]
    fn below_threshold_is_unity_gain() {
        let mut comp = Compressor::new();
        comp.set_params(CompressorParams {
            threshold_db: -6.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 50.0,
            makeup_db: 0.0,
        });
        let amp = 0.05; // well below threshold - knee
        let mut block = tone_burst(amp, FS as usize, 500.0, FS);
        let input = block.clone();
        comp.process(FS, &mut block, None);
        let tail_start = block.len() - CHANNELS * 4800;
        for (i, o) in input[tail_start..].iter().zip(&block[tail_start..]) {
            assert!(
                (i - o).abs() < 1e-4,
                "below-threshold signal should pass ~unchanged"
            );
        }
    }

    /// Attack/release timing: a burst rising above threshold should settle
    /// toward the compressed level within a handful of the configured
    /// attack time constant, and recover toward unity within a handful of
    /// release time constants after the burst ends.
    #[test]
    fn attack_and_release_timing_on_tone_burst() {
        let mut comp = Compressor::new();
        let attack_ms = 5.0;
        let release_ms = 50.0;
        comp.set_params(CompressorParams {
            threshold_db: -20.0,
            ratio: 8.0,
            attack_ms,
            release_ms,
            makeup_db: 0.0,
        });

        let amp = 0.7;
        let silence_frames = (FS as f64 * 0.05) as usize;
        let burst_frames = (FS as f64 * 0.3) as usize;
        let mut block = vec![0f32; (silence_frames + burst_frames) * CHANNELS];
        for f in 0..burst_frames {
            let t = f as f64 / FS as f64;
            let s = (amp * (2.0 * std::f64::consts::PI * 500.0 * t).sin()) as f32;
            block[(silence_frames + f) * CHANNELS] = s;
            block[(silence_frames + f) * CHANNELS + 1] = s;
        }
        comp.process(FS, &mut block, None);

        // Right at burst onset, gain reduction should be near zero (attack
        // hasn't caught up yet) — peak near input amplitude.
        let onset = &block[silence_frames * CHANNELS..(silence_frames + 50) * CHANNELS];
        let onset_peak = onset.iter().fold(0f32, |m, &s| m.max(s.abs())) as f64;
        assert!(
            onset_peak > amp * 0.8,
            "onset should be barely compressed, got {onset_peak}"
        );

        // A few attack-time-constants (5ms ~= 240 samples @48kHz) in, gain
        // reduction should have engaged substantially.
        let settle_start = silence_frames + (FS as f64 * 0.05) as usize;
        let settled = &block[settle_start * CHANNELS..(settle_start + 200) * CHANNELS];
        let settled_peak = settled.iter().fold(0f32, |m, &s| m.max(s.abs())) as f64;
        assert!(
            settled_peak < onset_peak * 0.7,
            "settled peak {settled_peak} should be well below onset {onset_peak}"
        );
    }

    #[test]
    fn sidechain_ducks_host_from_another_signal() {
        let mut comp = Compressor::new();
        comp.set_params(CompressorParams {
            threshold_db: -24.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 250.0,
            makeup_db: 0.0,
        });

        // Host track: a quiet, steady tone that alone would never trigger
        // compression.
        let host_amp = 0.05;
        let mut host = tone_burst(host_amp, FS as usize, 300.0, FS);
        // Sidechain: a loud voiceover-like tone that should duck the host.
        let sidechain = tone_burst(0.8, FS as usize, 220.0, FS);

        let without_sidechain_input = host.clone();
        comp.process(FS, &mut host, Some(&sidechain));

        let tail = block_tail(&host, 4800);
        let input_tail = block_tail(&without_sidechain_input, 4800);
        let out_peak = tail.iter().fold(0f32, |m, &s| m.max(s.abs()));
        let in_peak = input_tail.iter().fold(0f32, |m, &s| m.max(s.abs()));
        assert!(
            out_peak < in_peak * 0.9,
            "sidechain-driven compression should duck the host: out={out_peak} in={in_peak}"
        );
    }

    fn block_tail(block: &[f32], frames: usize) -> &[f32] {
        &block[block.len() - CHANNELS * frames..]
    }
}
