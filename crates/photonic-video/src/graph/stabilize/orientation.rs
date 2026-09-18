//! Orientation estimation for D-12 (22 §6.4 steps 2, 4, 5, 6).
//!
//! Turns a stream of angular-velocity samples into two orientation curves: what
//! the camera **actually did**, and what we **wish it had done**. The
//! difference between them is the correction the warp applies.
//!
//! All of this is Photonic-authored per 23 §9.3. The normative sources are
//! standard rigid-body attitude references — Kuipers, *Quaternions and Rotation
//! Sequences* (Princeton, 1999) for the kinematics, and Shoemake, "Animating
//! Rotation with Quaternion Curves," *SIGGRAPH '85* for spherical interpolation.
//! No `GPL-3.0` source was consulted.

use glam::{DQuat, DVec3};
use photonic_core::timeline::MotionSample;

/// Standard gravity, m/s² (SI exact).
pub const G0: f64 = 9.806_65;

/// Longest smoothing time constant, seconds, at `smoothness == 1.0`.
///
/// Two seconds is about the point where a handheld/airborne shot reads as
/// "locked off" rather than "smoothed"; beyond it the crop demand grows fast
/// for very little additional perceived stability.
const MAX_SMOOTHING_TAU_S: f64 = 2.0;

// ── bias estimation (22 §6.4 step 2) ────────────────────────────────────────

/// Result of gyro bias estimation.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BiasEstimate {
    pub bias_rad_s: [f64; 3],
    /// False when no sufficiently still span existed, in which case
    /// `bias_rad_s` is zero. Reported rather than hidden: an unremoved bias
    /// shows up as slow drift, and the user deserves to know why.
    pub estimated: bool,
}

impl BiasEstimate {
    pub const ZERO: BiasEstimate = BiasEstimate {
        bias_rad_s: [0.0; 3],
        estimated: false,
    };
}

/// Estimate the gyro's zero-rate offset from the stillest spans in the clip.
///
/// MEMS gyros read non-zero at rest, and that offset integrates into unbounded
/// heading drift. The estimator splits the series into short windows, ranks
/// them by how still they are (variance of angular speed), and averages the
/// stillest quartile. Ranking rather than thresholding means it adapts to
/// footage that is never truly still — a drone in flight always vibrates — and
/// still finds its calmest moments.
///
/// A window whose *mean* rate is large is excluded regardless of variance: a
/// smooth constant pan has low variance but is emphatically not at rest, and
/// averaging it in would subtract real motion.
pub fn estimate_bias(samples: &[MotionSample]) -> BiasEstimate {
    const WINDOW_S: f64 = 0.25;
    // Above this mean angular speed a window is moving, not resting.
    const MAX_REST_RATE: f64 = 0.15; // rad/s ≈ 8.6 °/s

    if samples.len() < 8 {
        return BiasEstimate::ZERO;
    }
    let window_ns = (WINDOW_S * 1e9) as i64;

    struct Window {
        mean: [f64; 3],
        var: f64,
        speed: f64,
    }
    let mut windows: Vec<Window> = Vec::new();
    let mut start = 0usize;
    while start < samples.len() {
        let t0 = samples[start].sensor_time_ns;
        let mut end = start;
        while end < samples.len() && samples[end].sensor_time_ns - t0 < window_ns {
            end += 1;
        }
        if end - start >= 4 {
            let n = (end - start) as f64;
            let mut mean = [0.0; 3];
            for s in &samples[start..end] {
                for i in 0..3 {
                    mean[i] += s.gyro_rad_s[i];
                }
            }
            for m in &mut mean {
                *m /= n;
            }
            let speed = (mean[0] * mean[0] + mean[1] * mean[1] + mean[2] * mean[2]).sqrt();
            let var = samples[start..end]
                .iter()
                .map(|s| {
                    (0..3)
                        .map(|i| {
                            let d = s.gyro_rad_s[i] - mean[i];
                            d * d
                        })
                        .sum::<f64>()
                })
                .sum::<f64>()
                / n;
            windows.push(Window { mean, var, speed });
        }
        if end == start {
            break;
        }
        start = end;
    }

    let mut eligible: Vec<&Window> = windows.iter().filter(|w| w.speed < MAX_REST_RATE).collect();
    if eligible.is_empty() {
        return BiasEstimate::ZERO;
    }
    eligible.sort_by(|a, b| {
        a.var
            .partial_cmp(&b.var)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let take = (eligible.len() / 4).max(1);
    let mut bias = [0.0; 3];
    for w in &eligible[..take] {
        for i in 0..3 {
            bias[i] += w.mean[i];
        }
    }
    for b in &mut bias {
        *b /= take as f64;
    }
    BiasEstimate {
        bias_rad_s: bias,
        estimated: true,
    }
}

// ── integration (22 §6.4 step 4) ────────────────────────────────────────────

/// An orientation curve: `(sensor_time_ns, orientation)` in ascending time.
#[derive(Clone, Debug, PartialEq)]
pub struct OrientationCurve {
    pub times_ns: Vec<i64>,
    pub q: Vec<DQuat>,
}

impl OrientationCurve {
    pub fn len(&self) -> usize {
        self.q.len()
    }

    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }

    /// Orientation at `t_ns`, interpolated with SLERP between bracketing
    /// samples and clamped at the ends.
    ///
    /// Binary search rather than a running cursor: callers sample at frame
    /// times, which are sparse relative to the gyro rate, and a stateless
    /// lookup keeps this usable from a parallel evaluator.
    pub fn sample(&self, t_ns: i64) -> DQuat {
        if self.q.is_empty() {
            return DQuat::IDENTITY;
        }
        match self.times_ns.binary_search(&t_ns) {
            Ok(i) => self.q[i],
            Err(0) => self.q[0],
            Err(i) if i >= self.q.len() => self.q[self.q.len() - 1],
            Err(i) => {
                let (t0, t1) = (self.times_ns[i - 1], self.times_ns[i]);
                let span = (t1 - t0) as f64;
                let f = if span > 0.0 {
                    ((t_ns - t0) as f64 / span).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                self.q[i - 1].slerp(self.q[i], f)
            }
        }
    }
}

/// Integrate angular velocity into an orientation curve.
///
/// Uses the midpoint rule — each step rotates by the *average* of the angular
/// velocity at both ends of the interval, rather than either endpoint alone.
/// For a constant rotation rate this is exact; for a varying one it is
/// second-order accurate where the rectangle rule is first-order. That matters
/// because a clip is tens of thousands of steps long and first-order error
/// accumulates into visible horizon drift.
///
/// Integration happens at the **native sample rate**, not at frame rate. 22
/// §6.4 lists resampling before integration; doing it in this order produces
/// the same per-frame result the contract calls for while avoiding the
/// integration error that decimating a several-kHz signal to 30 Hz *before*
/// integrating would bake in. Sampling the resulting curve at frame times (see
/// [`OrientationCurve::sample`]) is the resampling step.
///
/// When a dialect supplies pre-integrated orientation it is used directly:
/// the camera's own fusion had access to data we do not (magnetometer,
/// per-sample timing) and second-guessing it would only add error.
pub fn integrate(samples: &[MotionSample], bias: &BiasEstimate) -> OrientationCurve {
    let mut times_ns = Vec::with_capacity(samples.len());
    let mut q = Vec::with_capacity(samples.len());
    if samples.is_empty() {
        return OrientationCurve { times_ns, q };
    }

    // A dialect that carries orientation on every sample has already done this.
    if samples.iter().all(|s| s.orientation.is_some()) {
        for s in samples {
            let o = s.orientation.expect("checked above");
            times_ns.push(s.sensor_time_ns);
            q.push(DQuat::from_xyzw(o[0], o[1], o[2], o[3]).normalize());
        }
        return OrientationCurve { times_ns, q };
    }

    let b = DVec3::from_array(bias.bias_rad_s);
    let mut cur = DQuat::IDENTITY;
    times_ns.push(samples[0].sensor_time_ns);
    q.push(cur);

    for pair in samples.windows(2) {
        let (a, c) = (&pair[0], &pair[1]);
        let dt = (c.sensor_time_ns - a.sensor_time_ns) as f64 * 1e-9;
        if dt <= 0.0 {
            // Duplicate timestamps carry no rotation; keep the sample so the
            // curve stays index-aligned with the input.
            times_ns.push(c.sensor_time_ns);
            q.push(cur);
            continue;
        }
        let w0 = DVec3::from_array(a.gyro_rad_s) - b;
        let w1 = DVec3::from_array(c.gyro_rad_s) - b;
        let w_mid = (w0 + w1) * 0.5;
        cur = (cur * exp_map(w_mid * dt)).normalize();
        times_ns.push(c.sensor_time_ns);
        q.push(cur);
    }
    OrientationCurve { times_ns, q }
}

/// Exponential map: a rotation vector (axis × angle) to a unit quaternion.
///
/// Falls back to the small-angle series below the point where `sin(θ/2)/θ`
/// loses precision to cancellation, which is the overwhelmingly common case at
/// kHz sample rates where each step is a fraction of a milliradian.
fn exp_map(r: DVec3) -> DQuat {
    let angle = r.length();
    if angle < 1e-9 {
        // sin(θ/2)/θ → 1/2 as θ → 0.
        let h = r * 0.5;
        return DQuat::from_xyzw(h.x, h.y, h.z, 1.0).normalize();
    }
    let half = angle * 0.5;
    let s = half.sin() / angle;
    DQuat::from_xyzw(r.x * s, r.y * s, r.z * s, half.cos())
}

// ── smoothing (22 §6.4 step 5) ──────────────────────────────────────────────

/// Low-pass an orientation sequence, deterministically and without phase lag.
///
/// A one-pole filter run forward introduces lag — the smoothed camera trails
/// the real one, which reads as the shot "swimming" behind the action. Running
/// the same filter backward introduces exactly the opposite lag, so blending
/// the two passes cancels it. This is the standard zero-phase (forward-backward)
/// filtering construction, done in quaternion space with SLERP standing in for
/// the linear average.
///
/// `smoothness` maps to a time constant: `0.0` disables the filter entirely
/// (output is the input), `1.0` gives [`MAX_SMOOTHING_TAU_S`].
pub fn smooth(q: &[DQuat], dt_s: f64, smoothness: f64) -> Vec<DQuat> {
    let s = smoothness.clamp(0.0, 1.0);
    if q.is_empty() || s <= 0.0 || dt_s <= 0.0 {
        return q.to_vec();
    }
    let tau = MAX_SMOOTHING_TAU_S * s;
    // Fraction of the way from the filter state toward each new input sample.
    let beta = 1.0 - (-dt_s / tau).exp();

    let mut fwd = Vec::with_capacity(q.len());
    let mut acc = q[0];
    for &x in q {
        acc = acc.slerp(x, beta).normalize();
        fwd.push(acc);
    }

    let mut bwd = vec![DQuat::IDENTITY; q.len()];
    acc = q[q.len() - 1];
    for i in (0..q.len()).rev() {
        acc = acc.slerp(q[i], beta).normalize();
        bwd[i] = acc;
    }

    fwd.iter()
        .zip(bwd.iter())
        .map(|(f, b)| f.slerp(*b, 0.5).normalize())
        .collect()
}

// ── horizon lock (22 §6.4 step 6) ───────────────────────────────────────────

/// How much to trust an accelerometer reading as a gravity measurement.
///
/// An accelerometer measures specific force: gravity *plus* whatever the
/// airframe is doing. When the magnitude is near `G0` the reading is
/// predominantly gravity and the derived "down" is trustworthy; during a hard
/// acceleration it is not, and leaning on it would tilt the horizon toward the
/// manoeuvre. Confidence falls off linearly and reaches zero at ±30 %.
pub fn gravity_confidence(accel_mps2: [f64; 3]) -> f64 {
    let mag = DVec3::from_array(accel_mps2).length();
    if !mag.is_finite() || mag <= 0.0 {
        return 0.0;
    }
    let deviation = (mag - G0).abs() / G0;
    (1.0 - deviation / 0.30).clamp(0.0, 1.0)
}

/// Roll `q` so the camera's down axis lines up with measured gravity.
///
/// `strength` is the user's horizon-lock setting and `confidence` the
/// measurement's trustworthiness; the applied correction is scaled by both, so
/// a confident reading under a light setting nudges, and an untrustworthy
/// reading under a heavy setting still does nothing.
///
/// Only roll is corrected. Levelling pitch too would fight the operator's
/// framing — pointing the camera down is a deliberate choice in a way that
/// banking sideways usually is not.
pub fn apply_horizon_lock(q: DQuat, accel_mps2: [f64; 3], strength: f64, confidence: f64) -> DQuat {
    let gain = strength.clamp(0.0, 1.0) * confidence.clamp(0.0, 1.0);
    if gain <= 0.0 {
        return q;
    }
    let a = DVec3::from_array(accel_mps2);
    if a.length_squared() < 1e-12 {
        return q;
    }
    // A resting accelerometer reads +g *upward* (specific force opposes
    // gravity), so "down" in the camera frame is the negated reading.
    let down = -a.normalize();
    // Camera frame is +X right, +Y down, +Z forward, so a level camera sees
    // down as +Y and the roll error is the angle between them in the XY plane.
    let roll_err = down.x.atan2(down.y);
    if !roll_err.is_finite() {
        return q;
    }
    // Rotate about the optical axis to *cancel* the measured share of the
    // error — hence the negation. Getting this sign wrong doubles the roll
    // instead of removing it, which is why the test pins the exact residual
    // rather than merely asserting it got smaller.
    (q * DQuat::from_rotation_z(-roll_err * gain)).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(rate_rad_s: [f64; 3], hz: f64, secs: f64) -> Vec<MotionSample> {
        let n = (hz * secs) as usize;
        (0..=n)
            .map(|i| MotionSample {
                sensor_time_ns: (i as f64 / hz * 1e9) as i64,
                gyro_rad_s: rate_rad_s,
                accel_mps2: None,
                orientation: None,
            })
            .collect()
    }

    // ── integration ─────────────────────────────────────────────────────

    #[test]
    fn constant_rate_integrates_to_the_known_angle() {
        // 22 §6.7: "Synthetic constant-rate rotations integrate to known
        // quaternions." 90°/s about Y for 1 s must be exactly a 90° yaw.
        let rate = std::f64::consts::FRAC_PI_2;
        let s = series([0.0, rate, 0.0], 1000.0, 1.0);
        let curve = integrate(&s, &BiasEstimate::ZERO);
        let end = curve.q[curve.len() - 1];
        let expect = DQuat::from_rotation_y(rate);
        let dot = end.dot(expect).abs();
        assert!(
            dot > 1.0 - 1e-9,
            "integrated {end:?} vs expected {expect:?} (dot {dot})"
        );
    }

    #[test]
    fn integration_is_exact_for_constant_rate_at_any_sample_rate() {
        // The midpoint rule is exact when the rate is constant, so halving the
        // sample rate must not change the answer. This is the property that
        // makes decimation safe *only* for constant motion — and the reason
        // integration runs at native rate for everything else.
        let rate = 1.0_f64;
        let a = integrate(&series([rate, 0.0, 0.0], 2000.0, 1.0), &BiasEstimate::ZERO);
        let b = integrate(&series([rate, 0.0, 0.0], 100.0, 1.0), &BiasEstimate::ZERO);
        let dot = a.q[a.len() - 1].dot(b.q[b.len() - 1]).abs();
        assert!(dot > 1.0 - 1e-12, "dot {dot}");
    }

    #[test]
    fn zero_rate_integrates_to_identity() {
        let curve = integrate(&series([0.0; 3], 500.0, 2.0), &BiasEstimate::ZERO);
        for q in &curve.q {
            assert!(q.dot(DQuat::IDENTITY).abs() > 1.0 - 1e-12);
        }
    }

    #[test]
    fn precomputed_orientation_is_used_verbatim() {
        let expect = DQuat::from_rotation_x(0.7);
        let s: Vec<MotionSample> = (0..4)
            .map(|i| MotionSample {
                sensor_time_ns: i * 1_000_000,
                // Deliberately inconsistent with `orientation`, to prove the
                // gyro path is not silently preferred.
                gyro_rad_s: [9.0, 9.0, 9.0],
                accel_mps2: None,
                orientation: Some([expect.x, expect.y, expect.z, expect.w]),
            })
            .collect();
        let curve = integrate(&s, &BiasEstimate::ZERO);
        assert!(curve.q.iter().all(|q| q.dot(expect).abs() > 1.0 - 1e-12));
    }

    #[test]
    fn curve_sampling_interpolates_and_clamps() {
        let curve = integrate(
            &series([0.0, std::f64::consts::FRAC_PI_2, 0.0], 100.0, 1.0),
            &BiasEstimate::ZERO,
        );
        // Halfway through a constant 90°/s yaw is 45°.
        let mid = curve.sample(500_000_000);
        let expect = DQuat::from_rotation_y(std::f64::consts::FRAC_PI_4);
        assert!(mid.dot(expect).abs() > 1.0 - 1e-6);
        // Outside the range clamps rather than extrapolating.
        assert!(curve.sample(-1_000).dot(curve.q[0]).abs() > 1.0 - 1e-12);
        assert!(
            curve
                .sample(999_000_000_000)
                .dot(curve.q[curve.len() - 1])
                .abs()
                > 1.0 - 1e-12
        );
    }

    // ── bias ────────────────────────────────────────────────────────────

    #[test]
    fn bias_is_recovered_from_a_still_clip() {
        let bias = [0.01, -0.02, 0.005];
        let s = series(bias, 500.0, 3.0);
        let est = estimate_bias(&s);
        assert!(est.estimated);
        for i in 0..3 {
            assert!(
                (est.bias_rad_s[i] - bias[i]).abs() < 1e-9,
                "axis {i}: {} vs {}",
                est.bias_rad_s[i],
                bias[i]
            );
        }
    }

    #[test]
    fn removing_estimated_bias_cancels_the_drift() {
        // The whole point: an uncorrected offset integrates into drift.
        let bias = [0.0, 0.03, 0.0];
        let s = series(bias, 500.0, 5.0);
        let drifted = integrate(&s, &BiasEstimate::ZERO);
        let corrected = integrate(&s, &estimate_bias(&s));
        let drift_angle = drifted.q[drifted.len() - 1]
            .dot(DQuat::IDENTITY)
            .abs()
            .acos()
            * 2.0;
        let residual = corrected.q[corrected.len() - 1]
            .dot(DQuat::IDENTITY)
            .abs()
            .acos()
            * 2.0;
        assert!(drift_angle > 0.1, "uncorrected drift should be obvious");
        assert!(residual < 1e-6, "corrected residual was {residual}");
    }

    #[test]
    fn a_steady_pan_is_not_mistaken_for_bias() {
        // A constant fast pan has near-zero variance but is real motion.
        // Subtracting it as "bias" would erase the pan from the footage.
        let s = series([0.0, 1.5, 0.0], 500.0, 3.0);
        assert!(
            !estimate_bias(&s).estimated,
            "a 1.5 rad/s pan must not be absorbed as bias"
        );
    }

    #[test]
    fn bias_estimation_declines_on_too_few_samples() {
        assert_eq!(estimate_bias(&[]), BiasEstimate::ZERO);
        assert_eq!(
            estimate_bias(&series([0.01; 3], 100.0, 0.01)),
            BiasEstimate::ZERO
        );
    }

    // ── smoothing ───────────────────────────────────────────────────────

    #[test]
    fn zero_smoothness_is_a_no_op() {
        let q: Vec<DQuat> = (0..50)
            .map(|i| DQuat::from_rotation_y(i as f64 * 0.01))
            .collect();
        assert_eq!(smooth(&q, 1.0 / 60.0, 0.0), q);
    }

    #[test]
    fn smoothing_attenuates_shake_around_a_static_pose() {
        // 22 §6.7: "Static camera with injected gyro noise becomes stable
        // without false motion."
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let noisy: Vec<DQuat> = (0..600)
            .map(|_| {
                DQuat::from_euler(
                    glam::EulerRot::XYZ,
                    rng() * 0.05,
                    rng() * 0.05,
                    rng() * 0.05,
                )
            })
            .collect();
        let out = smooth(&noisy, 1.0 / 60.0, 0.8);

        let spread = |v: &[DQuat]| -> f64 {
            v.windows(2)
                .map(|w| w[0].dot(w[1]).abs().clamp(-1.0, 1.0).acos() * 2.0)
                .sum::<f64>()
                / (v.len() - 1) as f64
        };
        let (before, after) = (spread(&noisy), spread(&out));
        assert!(
            after < before * 0.25,
            "frame-to-frame motion {before} -> {after}"
        );
        // "Without false motion": the smoothed result must still sit at the
        // pose the noise was centred on, not wander off.
        let mean_angle = out
            .iter()
            .map(|q| q.dot(DQuat::IDENTITY).abs().clamp(-1.0, 1.0).acos() * 2.0)
            .sum::<f64>()
            / out.len() as f64;
        assert!(
            mean_angle < 0.05,
            "smoothed pose drifted by {mean_angle} rad"
        );
    }

    #[test]
    fn smoothing_has_no_phase_lag_on_a_ramp() {
        // A causal-only filter would trail a steady ramp by a fixed offset.
        // The forward-backward construction must not.
        let q: Vec<DQuat> = (0..400)
            .map(|i| DQuat::from_rotation_y(i as f64 * 0.002))
            .collect();
        let out = smooth(&q, 1.0 / 60.0, 0.6);
        let mid = q.len() / 2;
        let err = out[mid].dot(q[mid]).abs().clamp(-1.0, 1.0).acos() * 2.0;
        assert!(err < 1e-3, "mid-ramp lag was {err} rad");
    }

    // ── horizon lock ────────────────────────────────────────────────────

    #[test]
    fn gravity_confidence_peaks_at_rest_and_vanishes_under_load() {
        assert!((gravity_confidence([0.0, G0, 0.0]) - 1.0).abs() < 1e-12);
        assert_eq!(gravity_confidence([0.0, G0 * 2.0, 0.0]), 0.0);
        assert_eq!(gravity_confidence([0.0, 0.0, 0.0]), 0.0);
        let partial = gravity_confidence([0.0, G0 * 1.15, 0.0]);
        assert!(partial > 0.4 && partial < 0.6, "got {partial}");
    }

    #[test]
    fn horizon_lock_levels_a_rolled_camera() {
        // 22 §6.7: "Horizon-lock fixture converges while respecting strength."
        let roll = 0.3_f64;
        // Camera rolled by `roll`; gravity therefore appears rotated in frame.
        let down_in_cam = DQuat::from_rotation_z(-roll) * DVec3::Y;
        let accel = (-down_in_cam * G0).to_array();
        let q = DQuat::from_rotation_z(roll);

        let full = apply_horizon_lock(q, accel, 1.0, 1.0);
        let residual = (full.to_euler(glam::EulerRot::XYZ).2).abs();
        assert!(residual < 1e-9, "full lock left {residual} rad of roll");
    }

    #[test]
    fn horizon_lock_respects_partial_strength() {
        let roll = 0.4_f64;
        let down_in_cam = DQuat::from_rotation_z(-roll) * DVec3::Y;
        let accel = (-down_in_cam * G0).to_array();
        let q = DQuat::from_rotation_z(roll);
        let half = apply_horizon_lock(q, accel, 0.5, 1.0);
        let residual = half.to_euler(glam::EulerRot::XYZ).2;
        assert!(
            (residual - roll * 0.5).abs() < 1e-9,
            "half strength should leave half the roll, left {residual}"
        );
    }

    #[test]
    fn horizon_lock_ignores_untrustworthy_gravity() {
        // Under 2 g the accelerometer is measuring the manoeuvre, not gravity.
        let q = DQuat::from_rotation_z(0.3);
        let out = apply_horizon_lock(
            q,
            [0.0, G0 * 2.0, 0.0],
            1.0,
            gravity_confidence([0.0, G0 * 2.0, 0.0]),
        );
        assert!(out.dot(q).abs() > 1.0 - 1e-12, "must be a no-op");
    }

    #[test]
    fn horizon_lock_is_a_no_op_at_zero_strength() {
        let q = DQuat::from_rotation_z(0.3);
        assert_eq!(apply_horizon_lock(q, [0.0, G0, 0.0], 0.0, 1.0), q);
    }
}
