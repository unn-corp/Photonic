//! 5-band EQ (09 §6.1): low-shelf, three parametric peaking bands, high-shelf,
//! cascaded biquads. Processing order: low_shelf -> band1 -> band2 -> band3
//! -> high_shelf, each stage's output feeding the next. A band's `gain_db ==
//! 0` is a cheap no-op bypass for that stage (09 §6.1), not a separate
//! `enabled` flag.

use photonic_core::timeline::{EffectParams, PropValue};

use super::biquad::{BiquadCoeffs, BiquadState};
use super::AudioDiscontinuity;
use super::DspUnit;
use crate::audio::CHANNELS;

/// Shelf slope fixed at `Q = 1/sqrt(2)` (09 §6.1's "shelf slope fixed at
/// Q=0.707" — the registry has no `low_shelf.q`/`high_shelf.q` entry, so
/// this is a DSP-layer constant, never a user param).
const SHELF_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShelfBand {
    pub freq_hz: f64,
    pub gain_db: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakBand {
    pub freq_hz: f64,
    pub gain_db: f64,
    pub q: f64,
}

/// 09 §2's `Eq` param surface; defaults match 09 §6.1's seed table exactly
/// (mirrors `photonic_core::timeline::audio::apply_fx_defaults`'s `Eq` arm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqParams {
    pub low_shelf: ShelfBand,
    pub band1: PeakBand,
    pub band2: PeakBand,
    pub band3: PeakBand,
    pub high_shelf: ShelfBand,
}

impl Default for EqParams {
    fn default() -> Self {
        EqParams {
            low_shelf: ShelfBand {
                freq_hz: 120.0,
                gain_db: 0.0,
            },
            band1: PeakBand {
                freq_hz: 500.0,
                gain_db: 0.0,
                q: 1.0,
            },
            band2: PeakBand {
                freq_hz: 2000.0,
                gain_db: 0.0,
                q: 1.0,
            },
            band3: PeakBand {
                freq_hz: 8000.0,
                gain_db: 0.0,
                q: 1.0,
            },
            high_shelf: ShelfBand {
                freq_hz: 10000.0,
                gain_db: 0.0,
            },
        }
    }
}

impl EqParams {
    /// Extract from an already-block-evaluated [`EffectParams`] bag — the
    /// seam a future wiring story reads through: it evaluates
    /// `AudioFxUnit.params: AnimProps<EffectParams>` once per block
    /// (mirroring `mixer::eval_track_audio_params`'s pattern) and hands the
    /// resulting flat bag here. Falls back to this struct's own default for
    /// any missing/non-`Float` entry (defensive, matches
    /// `mixer::eval_f64`'s orphaned-lane fallback). Path strings match
    /// `photonic_core::timeline::prop_registry`'s `AUDIOFX_EQ` table
    /// exactly.
    pub fn from_effect_params(p: &EffectParams) -> Self {
        let d = EqParams::default();
        let f = |path: &str, default: f64| match p.get(path) {
            Some(PropValue::Float(v)) => *v,
            _ => default,
        };
        EqParams {
            low_shelf: ShelfBand {
                freq_hz: f("params.low_shelf.freq_hz", d.low_shelf.freq_hz),
                gain_db: f("params.low_shelf.gain_db", d.low_shelf.gain_db),
            },
            band1: PeakBand {
                freq_hz: f("params.band1.freq_hz", d.band1.freq_hz),
                gain_db: f("params.band1.gain_db", d.band1.gain_db),
                q: f("params.band1.q", d.band1.q),
            },
            band2: PeakBand {
                freq_hz: f("params.band2.freq_hz", d.band2.freq_hz),
                gain_db: f("params.band2.gain_db", d.band2.gain_db),
                q: f("params.band2.q", d.band2.q),
            },
            band3: PeakBand {
                freq_hz: f("params.band3.freq_hz", d.band3.freq_hz),
                gain_db: f("params.band3.gain_db", d.band3.gain_db),
                q: f("params.band3.q", d.band3.q),
            },
            high_shelf: ShelfBand {
                freq_hz: f("params.high_shelf.freq_hz", d.high_shelf.freq_hz),
                gain_db: f("params.high_shelf.gain_db", d.high_shelf.gain_db),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct StereoBiquad {
    coeffs: BiquadCoeffs,
    l: BiquadState,
    r: BiquadState,
}

impl StereoBiquad {
    fn reset(&mut self) {
        self.l.reset();
        self.r.reset();
    }

    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        (
            self.l.process(&self.coeffs, l as f64) as f32,
            self.r.process(&self.coeffs, r as f64) as f32,
        )
    }
}

/// The 5-band EQ [`DspUnit`] (09 §6.1).
#[derive(Debug, Default)]
pub struct Eq {
    params: EqParams,
    low_shelf: StereoBiquad,
    band1: StereoBiquad,
    band2: StereoBiquad,
    band3: StereoBiquad,
    high_shelf: StereoBiquad,
}

impl Eq {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_params(&mut self, params: EqParams) {
        self.params = params;
    }

    pub fn params(&self) -> &EqParams {
        &self.params
    }
}

impl DspUnit for Eq {
    fn reset(&mut self, _cause: AudioDiscontinuity) {
        // Every biquad history, or the filters ring across the discontinuity.
        self.low_shelf.reset();
        self.band1.reset();
        self.band2.reset();
        self.band3.reset();
        self.high_shelf.reset();
    }

    fn process(&mut self, sample_rate: u32, block: &mut [f32], _sidechain: Option<&[f32]>) {
        let fs = sample_rate as f64;
        let p = self.params;

        // 09 §6.1: "each independently enabled by `gain_db == 0` shortcut
        // (skip processing, cheap no-op)".
        let ls_active = p.low_shelf.gain_db != 0.0;
        let b1_active = p.band1.gain_db != 0.0;
        let b2_active = p.band2.gain_db != 0.0;
        let b3_active = p.band3.gain_db != 0.0;
        let hs_active = p.high_shelf.gain_db != 0.0;

        if ls_active {
            self.low_shelf.coeffs =
                BiquadCoeffs::low_shelf(p.low_shelf.freq_hz, SHELF_Q, p.low_shelf.gain_db, fs);
        }
        if b1_active {
            self.band1.coeffs =
                BiquadCoeffs::peaking(p.band1.freq_hz, p.band1.q, p.band1.gain_db, fs);
        }
        if b2_active {
            self.band2.coeffs =
                BiquadCoeffs::peaking(p.band2.freq_hz, p.band2.q, p.band2.gain_db, fs);
        }
        if b3_active {
            self.band3.coeffs =
                BiquadCoeffs::peaking(p.band3.freq_hz, p.band3.q, p.band3.gain_db, fs);
        }
        if hs_active {
            self.high_shelf.coeffs =
                BiquadCoeffs::high_shelf(p.high_shelf.freq_hz, SHELF_Q, p.high_shelf.gain_db, fs);
        }

        let frames = block.len() / CHANNELS;
        for f in 0..frames {
            let mut l = block[f * CHANNELS];
            let mut r = block[f * CHANNELS + 1];
            if ls_active {
                (l, r) = self.low_shelf.process(l, r);
            }
            if b1_active {
                (l, r) = self.band1.process(l, r);
            }
            if b2_active {
                (l, r) = self.band2.process(l, r);
            }
            if b3_active {
                (l, r) = self.band3.process(l, r);
            }
            if hs_active {
                (l, r) = self.high_shelf.process(l, r);
            }
            block[f * CHANNELS] = l;
            block[f * CHANNELS + 1] = r;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    fn sine_block(freq: f64, amp: f64, frames: usize, fs: u32) -> Vec<f32> {
        let mut out = vec![0f32; frames * CHANNELS];
        for f in 0..frames {
            let t = f as f64 / fs as f64;
            let s = (amp * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32;
            out[f * CHANNELS] = s;
            out[f * CHANNELS + 1] = s;
        }
        out
    }

    fn measure_gain(block: &[f32], in_amp: f64, skip_frames: usize) -> f64 {
        let frames = block.len() / CHANNELS;
        let mut peak = 0f64;
        for f in skip_frames..frames {
            peak = peak.max(block[f * CHANNELS].abs() as f64);
        }
        peak / in_amp
    }

    #[test]
    fn zero_gain_bands_are_bypassed_exactly() {
        let mut eq = Eq::new();
        eq.set_params(EqParams::default());
        let input = sine_block(1000.0, 0.5, 2048, FS);
        let mut block = input.clone();
        eq.process(FS, &mut block, None);
        assert_eq!(
            input, block,
            "all-zero-gain EQ must be bit-exact passthrough"
        );
    }

    #[test]
    fn single_peaking_band_matches_analytic_magnitude() {
        let params = EqParams {
            band2: PeakBand {
                freq_hz: 2000.0,
                gain_db: 9.0,
                q: 1.5,
            },
            ..EqParams::default()
        };
        let mut eq = Eq::new();
        eq.set_params(params);

        let analytic =
            BiquadCoeffs::peaking(2000.0, 1.5, 9.0, FS as f64).magnitude_at(2000.0, FS as f64);

        let amp = 0.4;
        let mut block = sine_block(2000.0, amp, 8192, FS);
        eq.process(FS, &mut block, None);
        let measured = measure_gain(&block, amp, 4096);

        assert!(
            (measured - analytic).abs() / analytic < 0.03,
            "measured={measured} analytic={analytic}"
        );
    }

    #[test]
    fn low_shelf_matches_analytic_magnitude_at_low_freq() {
        let params = EqParams {
            low_shelf: ShelfBand {
                freq_hz: 150.0,
                gain_db: 6.0,
            },
            ..EqParams::default()
        };
        let mut eq = Eq::new();
        eq.set_params(params);

        let test_freq = 40.0;
        let analytic = BiquadCoeffs::low_shelf(150.0, super::SHELF_Q, 6.0, FS as f64)
            .magnitude_at(test_freq, FS as f64);

        let amp = 0.4;
        let mut block = sine_block(test_freq, amp, 16384, FS);
        eq.process(FS, &mut block, None);
        let measured = measure_gain(&block, amp, 8192);

        assert!(
            (measured - analytic).abs() / analytic < 0.05,
            "measured={measured} analytic={analytic}"
        );
    }

    #[test]
    fn high_shelf_matches_analytic_magnitude_at_high_freq() {
        let params = EqParams {
            high_shelf: ShelfBand {
                freq_hz: 8000.0,
                gain_db: -8.0,
            },
            ..EqParams::default()
        };
        let mut eq = Eq::new();
        eq.set_params(params);

        let test_freq = 18000.0;
        let analytic = BiquadCoeffs::high_shelf(8000.0, super::SHELF_Q, -8.0, FS as f64)
            .magnitude_at(test_freq, FS as f64);

        let amp = 0.4;
        let mut block = sine_block(test_freq, amp, 4096, FS);
        eq.process(FS, &mut block, None);
        let measured = measure_gain(&block, amp, 2048);

        assert!(
            (measured - analytic).abs() / analytic < 0.05,
            "measured={measured} analytic={analytic}"
        );
    }

    #[test]
    fn cascaded_bands_process_in_spec_order() {
        // Two bands at the same frequency compound multiplicatively — a
        // direct check that the cascade actually chains stage outputs into
        // the next stage's input (not five independent parallel sums).
        let params = EqParams {
            band1: PeakBand {
                freq_hz: 1000.0,
                gain_db: 6.0,
                q: 2.0,
            },
            band2: PeakBand {
                freq_hz: 1000.0,
                gain_db: 6.0,
                q: 2.0,
            },
            ..EqParams::default()
        };
        let mut eq = Eq::new();
        eq.set_params(params);

        let single =
            BiquadCoeffs::peaking(1000.0, 2.0, 6.0, FS as f64).magnitude_at(1000.0, FS as f64);
        let expected_cascade = single * single;

        let amp = 0.3;
        let mut block = sine_block(1000.0, amp, 8192, FS);
        eq.process(FS, &mut block, None);
        let measured = measure_gain(&block, amp, 4096);

        assert!(
            (measured - expected_cascade).abs() / expected_cascade < 0.05,
            "measured={measured} expected_cascade={expected_cascade}"
        );
    }
}
