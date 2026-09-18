//! Shared envelope follower (09 §6.2), used by [`super::compressor::Compressor`]
//! and [`super::gate::Gate`]. Two cascaded one-pole stages: a short (5ms)
//! RMS detector, then asymmetric attack/release ballistics on top of that
//! RMS signal — 09 §6.2 gives the ballistics formula and separately notes
//! "detection signal: RMS of a short (5ms) window by default", which this
//! module composes as detector -> ballistics rather than either alone.

use super::one_pole_coeff;

/// Detection window (09 §6.2).
const RMS_WINDOW_MS: f64 = 5.0;

/// Precomputed per-block coefficients (09 §5: "coefficients... resolved once
/// per block, not per-sample") — construct once per block from that block's
/// resolved `attack_ms`/`release_ms`, then call [`EnvelopeFollower::step`]
/// once per sample with the same instance.
#[derive(Clone, Copy, Debug)]
pub struct EnvelopeCoeffs {
    rms: f64,
    attack: f64,
    release: f64,
}

impl EnvelopeCoeffs {
    pub fn new(attack_ms: f64, release_ms: f64, fs: f64) -> Self {
        EnvelopeCoeffs {
            rms: one_pole_coeff(RMS_WINDOW_MS, fs),
            attack: one_pole_coeff(attack_ms.max(0.001), fs),
            release: one_pole_coeff(release_ms.max(0.001), fs),
        }
    }
}

/// Per-instance running state (mean-square + ballistics envelope). One
/// instance per detector (mono, since [`super::compressor::Compressor`] and
/// [`super::gate::Gate`] both stereo-link their detection to a single scalar
/// — see those modules' doc comments).
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvelopeFollower {
    mean_sq: f64,
    env: f64,
}

impl EnvelopeFollower {
    pub fn new() -> Self {
        Self::default()
    }

    /// Discard the running mean-square and envelope, so the next `step` starts
    /// from silence rather than from the level before a seek or a cut.
    pub fn reset(&mut self) {
        self.mean_sq = 0.0;
        self.env = 0.0;
    }

    /// One sample step. `x` is a full-scale linear sample. Returns the
    /// current envelope value (linear, always `>= 0`).
    pub fn step(&mut self, x: f64, coeffs: &EnvelopeCoeffs) -> f64 {
        self.mean_sq += (x * x - self.mean_sq) * coeffs.rms;
        let rms = self.mean_sq.max(0.0).sqrt();

        let coeff = if rms > self.env {
            coeffs.attack
        } else {
            coeffs.release
        };
        self.env += (rms - self.env) * coeff;
        self.env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    #[test]
    fn tracks_a_step_toward_target_with_asymmetric_speed() {
        let coeffs = EnvelopeCoeffs::new(1.0, 100.0, FS);
        let mut env = EnvelopeFollower::new();
        // Step from silence to full scale: attack (1ms) should get most of
        // the way there in a few hundred samples.
        for _ in 0..500 {
            env.step(1.0, &coeffs);
        }
        assert!(
            env.env > 0.9,
            "attack should have mostly converged, got {}",
            env.env
        );

        // Drop back to silence: release (100ms) should still be well above
        // zero after only 100 samples (much less than one release time
        // constant at 48kHz).
        let mut env2 = env;
        for _ in 0..100 {
            env2.step(0.0, &coeffs);
        }
        assert!(env2.env > 0.5, "release should be slow, got {}", env2.env);
    }

    #[test]
    fn steady_sine_rms_converges_to_analytic_rms() {
        // A full-scale sine's RMS is amplitude/sqrt(2); the 5ms RMS window
        // plus fast attack/release should converge close to that constant.
        let coeffs = EnvelopeCoeffs::new(0.5, 0.5, FS);
        let mut env = EnvelopeFollower::new();
        let freq = 1000.0;
        let amp = 0.8;
        let n = 20_000;
        let mut last = 0.0;
        for i in 0..n {
            let t = i as f64 / FS;
            let x = amp * (2.0 * std::f64::consts::PI * freq * t).sin();
            last = env.step(x, &coeffs);
        }
        let expected = amp / std::f64::consts::SQRT_2;
        assert!(
            (last - expected).abs() / expected < 0.05,
            "expected~{expected} got={last}"
        );
    }
}
