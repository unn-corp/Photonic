//! Noise gate: the same shared envelope follower as [`super::compressor`],
//! inverted logic, no makeup/knee, plus a hold timer (09 §6.4).

use photonic_core::timeline::{EffectParams, PropValue};

use super::envelope::{EnvelopeCoeffs, EnvelopeFollower};
use super::AudioDiscontinuity;
use super::{db_to_linear, lin_to_db, DspUnit};
use crate::audio::CHANNELS;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateParams {
    pub threshold_db: f64,
    pub attack_ms: f64,
    pub hold_ms: f64,
    pub release_ms: f64,
    pub range_db: f64,
}

impl Default for GateParams {
    fn default() -> Self {
        GateParams {
            threshold_db: -40.0,
            attack_ms: 1.0,
            hold_ms: 10.0,
            release_ms: 100.0,
            range_db: 60.0,
        }
    }
}

impl GateParams {
    /// See [`super::eq::EqParams::from_effect_params`]'s doc for the seam
    /// this shares. Path strings match `prop_registry`'s `AUDIOFX_GATE`
    /// table.
    pub fn from_effect_params(p: &EffectParams) -> Self {
        let d = GateParams::default();
        let f = |path: &str, default: f64| match p.get(path) {
            Some(PropValue::Float(v)) => *v,
            _ => default,
        };
        GateParams {
            threshold_db: f("params.threshold_db", d.threshold_db),
            attack_ms: f("params.attack_ms", d.attack_ms),
            hold_ms: f("params.hold_ms", d.hold_ms),
            release_ms: f("params.release_ms", d.release_ms),
            range_db: f("params.range_db", d.range_db),
        }
    }
}

/// Gate (09 §6.4). **Documented implementation decision** (same rationale as
/// `Compressor`): detector input is `max(|L|,|R|)`, stereo-linked. The
/// resulting `gr_db` is applied directly from the shared envelope
/// follower's own attack/release-smoothed `env_db` (09 §6.4's pseudocode is
/// a direct threshold compare on `env_db`, with no separate gr-smoothing
/// stage named beyond that) — attack_ms governs how fast the gate reopens
/// (envelope rising past threshold), release_ms how fast `env_db` decays
/// once the signal drops, and `hold_ms` extends the open state after the
/// last above-threshold sample regardless of how fast `env_db` itself
/// decays.
#[derive(Debug, Default)]
pub struct Gate {
    params: GateParams,
    envelope: EnvelopeFollower,
    hold_remaining_samples: u64,
}

impl Gate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_params(&mut self, params: GateParams) {
        self.params = params;
    }

    pub fn params(&self) -> &GateParams {
        &self.params
    }
}

impl DspUnit for Gate {
    fn reset(&mut self, _cause: AudioDiscontinuity) {
        self.envelope.reset();
        // Hold must clear too: a gate mid-hold across a cut would keep the
        // incoming clip open for the remainder of the previous clip's hold.
        self.hold_remaining_samples = 0;
    }

    fn process(&mut self, sample_rate: u32, block: &mut [f32], _sidechain: Option<&[f32]>) {
        let fs = sample_rate as f64;
        let coeffs = EnvelopeCoeffs::new(self.params.attack_ms, self.params.release_ms, fs);
        let hold_samples = (self.params.hold_ms.max(0.0) * 0.001 * fs).round() as u64;
        let frames = block.len() / CHANNELS;

        for f in 0..frames {
            let l = block[f * CHANNELS] as f64;
            let r = block[f * CHANNELS + 1] as f64;
            let x = l.abs().max(r.abs());
            let env = self.envelope.step(x, &coeffs);
            let env_db = lin_to_db(env.max(1e-10));

            let above = env_db >= self.params.threshold_db;
            if above {
                self.hold_remaining_samples = hold_samples;
            } else if self.hold_remaining_samples > 0 {
                self.hold_remaining_samples -= 1;
            }

            // 09 §6.4's exact formula: open (above threshold, or still
            // within the post-above-threshold hold window) -> 0dB; else the
            // fixed `-range_db` floor.
            let gr_db = if above || self.hold_remaining_samples > 0 {
                0.0
            } else {
                -self.params.range_db
            };
            let gain = db_to_linear(gr_db);
            block[f * CHANNELS] = (l * gain) as f32;
            block[f * CHANNELS + 1] = (r * gain) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    fn tone(amp: f64, frames: usize, freq: f64, fs: u32) -> Vec<f32> {
        let mut out = vec![0f32; frames * CHANNELS];
        for f in 0..frames {
            let t = f as f64 / fs as f64;
            let s = (amp * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32;
            out[f * CHANNELS] = s;
            out[f * CHANNELS + 1] = s;
        }
        out
    }

    #[test]
    fn above_threshold_signal_passes_at_unity() {
        let mut gate = Gate::new();
        gate.set_params(GateParams {
            threshold_db: -30.0,
            attack_ms: 0.5,
            hold_ms: 5.0,
            release_ms: 20.0,
            range_db: 60.0,
        });
        let amp = 0.5;
        let mut block = tone(amp, FS as usize / 4, 500.0, FS);
        let input = block.clone();
        gate.process(FS, &mut block, None);
        let tail_start = block.len() - CHANNELS * 4000;
        for (i, o) in input[tail_start..].iter().zip(&block[tail_start..]) {
            assert!(
                (i - o).abs() < 1e-4,
                "above-threshold tone should pass near-unchanged"
            );
        }
    }

    #[test]
    fn below_threshold_signal_is_attenuated_by_range_db() {
        let mut gate = Gate::new();
        let range_db = 60.0;
        gate.set_params(GateParams {
            threshold_db: -10.0,
            attack_ms: 0.5,
            hold_ms: 1.0,
            release_ms: 5.0,
            range_db,
        });
        let quiet = 0.001; // ~ -60dBFS peak, well below -10dB threshold
        let mut block = tone(quiet, FS as usize / 2, 500.0, FS);
        gate.process(FS, &mut block, None);
        let tail = &block[block.len() - CHANNELS * 4000..];
        let peak = tail.iter().fold(0f32, |m, &s| m.max(s.abs())) as f64;
        let expected_peak = quiet * 10f64.powf(-range_db / 20.0);
        assert!(
            (peak - expected_peak).abs() / expected_peak.max(1e-9) < 0.2,
            "peak={peak} expected~{expected_peak}"
        );
    }

    /// Gate hold (09 §11's test hook): drive a tone above threshold, then
    /// drop to a quiet probe tone (below threshold on its own, but loud
    /// enough to make "is the gate open" audible/measurable); the gate must
    /// stay open through the hold window and only close well after it
    /// elapses.
    #[test]
    fn hold_keeps_gate_open_after_signal_drops() {
        let hold_ms = 20.0;
        let above_frames = (FS as f64 * 0.05) as usize; // 50ms above threshold
        let silence_frames = (FS as f64 * 0.1) as usize; // 100ms probe tail

        let mut probe = tone(0.5, above_frames, 500.0, FS);
        let probe_amp = 0.01; // below threshold on its own
        probe.extend(tone(probe_amp, silence_frames, 500.0, FS));

        let mut gate = Gate::new();
        gate.set_params(GateParams {
            threshold_db: -20.0,
            attack_ms: 0.2,
            hold_ms,
            release_ms: 0.2, // fast, so env_db tracks the probe quickly once hold elapses
            range_db: 60.0,
        });
        gate.process(FS, &mut probe, None);

        let hold_samples = (hold_ms * 0.001 * FS as f64).round() as usize;

        // Comfortably inside the hold window: still open (probe passes near
        // unity).
        let inside_start = above_frames + hold_samples / 4;
        let inside = &probe[inside_start * CHANNELS..(inside_start + 20) * CHANNELS];
        let inside_peak = inside.iter().fold(0f32, |m, &s| m.max(s.abs())) as f64;
        assert!(
            inside_peak > probe_amp * 0.9,
            "gate should still be open inside hold window, peak={inside_peak}"
        );

        // Comfortably after the hold window plus a fast release settle:
        // closed (probe attenuated toward -range_db).
        let after_start = above_frames + hold_samples + (FS as f64 * 0.02) as usize;
        let after = &probe[after_start * CHANNELS..(after_start + 20) * CHANNELS];
        let after_peak = after.iter().fold(0f32, |m, &s| m.max(s.abs())) as f64;
        assert!(
            after_peak < probe_amp * 0.5,
            "gate should have closed after hold window, peak={after_peak}"
        );
    }
}
