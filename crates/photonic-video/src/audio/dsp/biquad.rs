//! RBJ ("Audio EQ Cookbook") biquad coefficient design + Direct Form I state
//! (09 §6.1). Shared by [`super::eq::Eq`] (parametric/shelf bands) and
//! [`super::loudness`]'s K-weighting pre-filter (same math, fixed
//! standard-mandated frequencies/Q/gain instead of user params).

use std::f64::consts::PI;

/// Normalized (`a0 == 1`) biquad coefficients:
/// `y[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2] - a1*y[n-1] - a2*y[n-2]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Default for BiquadCoeffs {
    /// Pass-through (identity) — lets a stereo/cascade stage default to a
    /// cheap no-op without wrapping every stage in `Option`.
    fn default() -> Self {
        BiquadCoeffs {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

impl BiquadCoeffs {
    /// RBJ peaking EQ (09 §6.1, exact formula given by the spec).
    pub fn peaking(freq_hz: f64, q: f64, gain_db: f64, fs: f64) -> Self {
        let w0 = 2.0 * PI * freq_hz / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q.max(1e-6));
        let a = 10f64.powf(gain_db / 40.0);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    /// RBJ low shelf. `q` is the cookbook's shelving "Q" parametrization of
    /// `alpha` (`alpha = sin(w0)/(2Q)`, the same form `peaking` uses) rather
    /// than the shelf-slope `S` parametrization — at `Q = 1/sqrt(2)` this is
    /// exactly the cookbook's documented `S == 1` "as steep as possible,
    /// monotonic" point (09 §6.1's "shelf slope fixed at Q=0.707" default),
    /// and the same closed form covers §6.6's K-weighting shelf, whose
    /// standard-mandated Q (`0.7071752369554196`) is not quite `1/sqrt(2)`.
    pub fn low_shelf(freq_hz: f64, q: f64, gain_db: f64, fs: f64) -> Self {
        let w0 = 2.0 * PI * freq_hz / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q.max(1e-6));
        let a = 10f64.powf(gain_db / 40.0);
        let sqrt_a = a.sqrt();

        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    /// RBJ high shelf — see [`Self::low_shelf`]'s doc for the `q` parametrization.
    pub fn high_shelf(freq_hz: f64, q: f64, gain_db: f64, fs: f64) -> Self {
        let w0 = 2.0 * PI * freq_hz / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q.max(1e-6));
        let a = 10f64.powf(gain_db / 40.0);
        let sqrt_a = a.sqrt();

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    /// RBJ high-pass — not an `AudioFxKind::Eq` band (09 §2's registry has no
    /// HPF band); used by [`super::loudness`]'s K-weighting "RLB" stage,
    /// same closed-form biquad family (09 §6.6: "same biquad machinery as
    /// §6.1").
    pub fn high_pass(freq_hz: f64, q: f64, fs: f64) -> Self {
        let w0 = 2.0 * PI * freq_hz / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q.max(1e-6));

        let b0 = (1.0 + cos_w0) / 2.0;
        let b1 = -(1.0 + cos_w0);
        let b2 = (1.0 + cos_w0) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        BiquadCoeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Closed-form magnitude response at `freq_hz`, evaluated directly on the
    /// unit circle (`z = e^{jw}`) rather than by running samples through
    /// [`BiquadState`] — the analytic reference the sine-sweep frequency-
    /// response tests (09 §11) check a measured sweep against.
    pub fn magnitude_at(&self, freq_hz: f64, fs: f64) -> f64 {
        let w = 2.0 * PI * freq_hz / fs;
        let (sw, cw) = w.sin_cos();
        let (s2w, c2w) = (2.0 * w).sin_cos();
        // H(e^jw) = (b0 + b1*e^-jw + b2*e^-2jw) / (1 + a1*e^-jw + a2*e^-2jw)
        let num_re = self.b0 + self.b1 * cw + self.b2 * c2w;
        let num_im = -self.b1 * sw - self.b2 * s2w;
        let den_re = 1.0 + self.a1 * cw + self.a2 * c2w;
        let den_im = -self.a1 * sw - self.a2 * s2w;
        ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).sqrt()
    }
}

/// Direct-Form-I biquad state (one instance per filtered channel — stereo
/// processing needs two, sharing one [`BiquadCoeffs`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl BiquadState {
    /// Clear the two-sample input/output histories. Without this a filter
    /// rings across a seek, playing back energy from before the jump.
    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    pub fn process(&mut self, c: &BiquadCoeffs, x: f64) -> f64 {
        let y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    #[test]
    fn peaking_at_zero_gain_is_identity() {
        let c = BiquadCoeffs::peaking(1000.0, 1.0, 0.0, FS);
        for freq in [50.0, 500.0, 1000.0, 5000.0, 18000.0] {
            assert!(
                (c.magnitude_at(freq, FS) - 1.0).abs() < 1e-9,
                "peaking@0dB should be transparent at {freq}Hz"
            );
        }
    }

    #[test]
    fn shelves_at_zero_gain_are_identity() {
        let ls = BiquadCoeffs::low_shelf(120.0, std::f64::consts::FRAC_1_SQRT_2, 0.0, FS);
        let hs = BiquadCoeffs::high_shelf(10000.0, std::f64::consts::FRAC_1_SQRT_2, 0.0, FS);
        for freq in [50.0, 1000.0, 15000.0] {
            assert!((ls.magnitude_at(freq, FS) - 1.0).abs() < 1e-9);
            assert!((hs.magnitude_at(freq, FS) - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn peaking_boost_matches_gain_at_center_freq() {
        for gain_db in [-12.0, 6.0, 12.0] {
            let c = BiquadCoeffs::peaking(1000.0, 1.0, gain_db, FS);
            let expected = 10f64.powf(gain_db / 20.0);
            let got = c.magnitude_at(1000.0, FS);
            assert!(
                (got - expected).abs() / expected < 0.01,
                "gain={gain_db} expected~{expected} got={got}"
            );
        }
    }

    #[test]
    fn low_shelf_boosts_low_freq_and_is_flat_far_above() {
        let c = BiquadCoeffs::low_shelf(200.0, std::f64::consts::FRAC_1_SQRT_2, 12.0, FS);
        let low = c.magnitude_at(20.0, FS);
        let high = c.magnitude_at(15000.0, FS);
        assert!((low - 10f64.powf(12.0 / 20.0)).abs() / 10f64.powf(12.0 / 20.0) < 0.05);
        assert!(
            (high - 1.0).abs() < 0.02,
            "high freq should be ~flat, got {high}"
        );
    }

    #[test]
    fn high_shelf_boosts_high_freq_and_is_flat_far_below() {
        let c = BiquadCoeffs::high_shelf(8000.0, std::f64::consts::FRAC_1_SQRT_2, 12.0, FS);
        let low = c.magnitude_at(50.0, FS);
        let high = c.magnitude_at(18000.0, FS);
        assert!(
            (low - 1.0).abs() < 0.02,
            "low freq should be ~flat, got {low}"
        );
        assert!((high - 10f64.powf(12.0 / 20.0)).abs() / 10f64.powf(12.0 / 20.0) < 0.05);
    }

    #[test]
    fn high_pass_attenuates_below_cutoff_and_passes_above() {
        let c = BiquadCoeffs::high_pass(1000.0, std::f64::consts::FRAC_1_SQRT_2, FS);
        assert!(
            c.magnitude_at(50.0, FS) < 0.1,
            "well below cutoff should be heavily attenuated"
        );
        assert!((c.magnitude_at(1000.0, FS) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.02);
        assert!(
            (c.magnitude_at(16000.0, FS) - 1.0).abs() < 0.05,
            "well above cutoff should pass"
        );
    }

    /// Sine-sweep vs. analytic magnitude (09 §11's test hook): actually run
    /// samples through [`BiquadState`], measure the steady-state gain, and
    /// check it against [`BiquadCoeffs::magnitude_at`].
    #[test]
    fn sine_sweep_matches_analytic_magnitude() {
        let c = BiquadCoeffs::peaking(1000.0, 2.0, 9.0, FS);
        for freq in [200.0, 1000.0, 4000.0] {
            let mut state = BiquadState::default();
            let n = 4096;
            let mut peak_in = 0f64;
            let mut peak_out = 0f64;
            for i in 0..n {
                let t = i as f64 / FS;
                let x = (2.0 * PI * freq * t).sin();
                let y = state.process(&c, x);
                // Only measure the settled tail (skip the filter's transient).
                if i > n / 2 {
                    peak_in = peak_in.max(x.abs());
                    peak_out = peak_out.max(y.abs());
                }
            }
            let measured = peak_out / peak_in;
            let analytic = c.magnitude_at(freq, FS);
            assert!(
                (measured - analytic).abs() / analytic < 0.03,
                "freq={freq} measured={measured} analytic={analytic}"
            );
        }
    }
}
