//! Brick-wall lookahead limiter (09 §6.5) — the `MasterBus` default unit.

use std::collections::VecDeque;

use photonic_core::timeline::{EffectParams, PropValue};

use super::AudioDiscontinuity;
use super::{db_to_linear, one_pole_coeff, DspUnit};
use crate::audio::CHANNELS;

/// Fixed, not user-exposed (09 §6.5: "a lookahead buffer this size is
/// inaudible as added latency in context and removes a knob that has one
/// correct-ish answer").
const LOOKAHEAD_MS: f64 = 5.0;
/// "Fast attack (~1ms, effectively as fast as the lookahead allows)" — fixed,
/// not in the `AudioFxKind::Limiter` param table (09 §2 only lists
/// `ceiling_db`/`release_ms`).
const ATTACK_MS: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimiterParams {
    pub ceiling_db: f64,
    pub release_ms: f64,
}

impl Default for LimiterParams {
    fn default() -> Self {
        LimiterParams {
            ceiling_db: -1.0,
            release_ms: 50.0,
        }
    }
}

impl LimiterParams {
    /// See [`super::eq::EqParams::from_effect_params`]'s doc for the seam
    /// this shares. Path strings match `prop_registry`'s `AUDIOFX_LIMITER`
    /// table.
    pub fn from_effect_params(p: &EffectParams) -> Self {
        let d = LimiterParams::default();
        let f = |path: &str, default: f64| match p.get(path) {
            Some(PropValue::Float(v)) => *v,
            _ => default,
        };
        LimiterParams {
            ceiling_db: f("params.ceiling_db", d.ceiling_db),
            release_ms: f("params.release_ms", d.release_ms),
        }
    }
}

/// Sample-peak lookahead limiter (09 §6.5). Silence-pads the delay line at
/// construction, so the first `lookahead_samples` of output are always
/// exactly delayed silence — this fixed delay is what gives the "lookahead
/// latency is exactly 5ms of samples" invariant (09 §11), independent of
/// the gain-smoothing behavior below.
///
/// Gain reduction is a smoothed one-pole ramp toward `ceiling / window_peak`
/// (fast attack, configurable release), which — being an exponential ramp —
/// never *exactly* reaches its target in finite time. A well-designed
/// lookahead window makes the residual negligible in practice, but a true
/// brick-wall limiter must never exceed its ceiling at all: the final
/// output sample is hard-clamped to `[-ceiling, ceiling]` as a safety net on
/// top of the smoothed gain, same as any real brick-wall limiter's final
/// safety clipper stage.
#[derive(Debug)]
pub struct Limiter {
    params: LimiterParams,
    sample_rate: u32,
    lookahead_samples: usize,
    delay_l: VecDeque<f32>,
    delay_r: VecDeque<f32>,
    /// Sliding-window maximum of the detector signal over the lookahead
    /// window: a monotonic-decreasing deque of `(sample_index, abs_value)`,
    /// giving O(1)-amortized "worst upcoming peak" per sample.
    window: VecDeque<(u64, f32)>,
    sample_index: u64,
    smoothed_gain: f64,
}

impl Default for Limiter {
    fn default() -> Self {
        Limiter {
            params: LimiterParams::default(),
            sample_rate: 0,
            lookahead_samples: 0,
            delay_l: VecDeque::new(),
            delay_r: VecDeque::new(),
            window: VecDeque::new(),
            sample_index: 0,
            smoothed_gain: 1.0,
        }
    }
}

impl Limiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_params(&mut self, params: LimiterParams) {
        self.params = params;
    }

    pub fn params(&self) -> &LimiterParams {
        &self.params
    }

    pub fn lookahead_samples(&self) -> usize {
        self.lookahead_samples
    }

    /// (Re)initialize delay/window state for `sample_rate`, on first use or
    /// a sample-rate change.
    fn ensure_ready(&mut self, sample_rate: u32) {
        if self.sample_rate == sample_rate && self.lookahead_samples > 0 {
            return;
        }
        self.sample_rate = sample_rate;
        self.lookahead_samples =
            (LOOKAHEAD_MS * 0.001 * sample_rate as f64).round().max(1.0) as usize;
        self.delay_l = std::iter::repeat_n(0.0, self.lookahead_samples).collect();
        self.delay_r = std::iter::repeat_n(0.0, self.lookahead_samples).collect();
        self.window.clear();
        self.sample_index = 0;
        self.smoothed_gain = 1.0;
    }
}

impl DspUnit for Limiter {
    fn reset(&mut self, _cause: AudioDiscontinuity) {
        // Force `ensure_ready` to rebuild on the next block, which re-primes
        // the delay lines with silence. That preserves the documented
        // invariant that the first `lookahead_samples` of output after a
        // (re)start are exactly delayed silence — clearing the deques without
        // re-priming would shorten the delay and desync this path against
        // every other one feeding the master bus.
        self.sample_rate = 0;
        self.lookahead_samples = 0;
        self.delay_l.clear();
        self.delay_r.clear();
        self.window.clear();
        self.sample_index = 0;
        self.smoothed_gain = 1.0;
    }

    /// The fixed lookahead delay. Reported so the mixer can compensate other
    /// paths against it (31 §3.1) and so export flushes it (see `tail_samples`).
    fn latency_samples(&self) -> u32 {
        self.lookahead_samples as u32
    }

    /// Output persists for exactly the delay-line length after silence in.
    fn tail_samples(&self) -> u32 {
        self.lookahead_samples as u32
    }

    fn process(&mut self, sample_rate: u32, block: &mut [f32], _sidechain: Option<&[f32]>) {
        self.ensure_ready(sample_rate);
        let fs = sample_rate as f64;
        let ceiling_lin = db_to_linear(self.params.ceiling_db);
        let attack_coeff = one_pole_coeff(ATTACK_MS, fs);
        let release_coeff = one_pole_coeff(self.params.release_ms.max(0.001), fs);
        let lookahead = self.lookahead_samples as u64;

        let frames = block.len() / CHANNELS;
        for f in 0..frames {
            let l = block[f * CHANNELS];
            let r = block[f * CHANNELS + 1];
            let detect = l.abs().max(r.abs());
            let t = self.sample_index;

            while self.window.back().is_some_and(|&(_, v)| v <= detect) {
                self.window.pop_back();
            }
            self.window.push_back((t, detect));
            while self
                .window
                .front()
                .is_some_and(|&(idx, _)| idx + lookahead <= t)
            {
                self.window.pop_front();
            }
            let window_peak = self.window.front().map_or(0.0, |&(_, v)| v) as f64;

            let required_gain = if window_peak > ceiling_lin {
                ceiling_lin / window_peak
            } else {
                1.0
            };
            let coeff = if required_gain < self.smoothed_gain {
                attack_coeff
            } else {
                release_coeff
            };
            self.smoothed_gain += (required_gain - self.smoothed_gain) * coeff;

            self.delay_l.push_back(l);
            self.delay_r.push_back(r);
            let dl = self.delay_l.pop_front().unwrap_or(0.0) as f64;
            let dr = self.delay_r.pop_front().unwrap_or(0.0) as f64;

            block[f * CHANNELS] = (dl * self.smoothed_gain).clamp(-ceiling_lin, ceiling_lin) as f32;
            block[f * CHANNELS + 1] =
                (dr * self.smoothed_gain).clamp(-ceiling_lin, ceiling_lin) as f32;

            self.sample_index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::loudness;

    const FS: u32 = 48_000;

    #[test]
    fn lookahead_latency_is_exactly_5ms_of_samples() {
        let mut limiter = Limiter::new();
        limiter.set_params(LimiterParams {
            ceiling_db: -1.0,
            release_ms: 50.0,
        });

        // A single below-ceiling sample so the limiter never engages gain
        // reduction (smoothed_gain stays exactly 1.0) — isolates pure delay
        // latency from any gain-smoothing behavior.
        let marker = 0.3f32;
        let mut block = vec![0f32; 2000 * CHANNELS];
        block[0] = marker;
        block[1] = marker;
        limiter.process(FS, &mut block, None);

        let expected_latency = (0.005 * FS as f64).round() as usize;
        assert_eq!(limiter.lookahead_samples(), expected_latency);
        assert_eq!(block[expected_latency * CHANNELS], marker);
        assert_eq!(block[expected_latency * CHANNELS + 1], marker);
        // Every other sample must be exactly silent.
        for f in 0..2000 {
            if f != expected_latency {
                assert_eq!(block[f * CHANNELS], 0.0, "unexpected nonzero at frame {f}");
            }
        }
    }

    #[test]
    fn full_scale_impulse_never_exceeds_ceiling_true_peak() {
        let mut limiter = Limiter::new();
        let ceiling_db = -1.0;
        limiter.set_params(LimiterParams {
            ceiling_db,
            release_ms: 50.0,
        });

        let mut block = vec![0f32; FS as usize * CHANNELS];
        // A handful of full-scale impulses scattered through the buffer.
        for &pos in &[100usize, 5000, 15000, 30000] {
            block[pos * CHANNELS] = 1.0;
            block[pos * CHANNELS + 1] = -1.0;
        }
        limiter.process(FS, &mut block, None);

        let sample_peak = block.iter().fold(0f32, |m, &s| m.max(s.abs()));
        let true_peak_db = loudness::true_peak_dbtp(&block, FS);
        let ceiling_lin = db_to_linear(ceiling_db) as f32;

        assert!(
            sample_peak <= ceiling_lin + 1e-6,
            "sample peak {sample_peak} exceeds ceiling {ceiling_lin}"
        );
        assert!(
            true_peak_db <= ceiling_db as f32 + 0.2,
            "true peak {true_peak_db}dBTP exceeds ceiling {ceiling_db}dBTP by more than tolerance"
        );
    }

    #[test]
    fn signal_below_ceiling_is_untouched() {
        let mut limiter = Limiter::new();
        limiter.set_params(LimiterParams {
            ceiling_db: -1.0,
            release_ms: 50.0,
        });
        let amp = 0.3f32;
        let mut block = vec![amp; 4000 * CHANNELS];
        limiter.process(FS, &mut block, None);
        let lookahead = limiter.lookahead_samples();
        // After the initial lookahead-latency ramp-in, output should equal
        // input exactly (never above ceiling to begin with).
        for f in (lookahead + 10)..4000 {
            assert!((block[f * CHANNELS] - amp).abs() < 1e-5);
        }
    }
}
