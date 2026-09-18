//! Gyro-to-video clock mapping for D-12 (22 §6.4).
//!
//! 22 §6.4 is emphatic: "never assume sensor time equals video PTS." Most
//! cameras log motion outside the video pipeline, so the two clocks differ by
//! an offset and — over a long clip — by *rate*, because they are driven by
//! different oscillators. A few hundred parts per million of rate error is
//! imperceptible at the start of a clip and a whole frame of misalignment by
//! the end of a long one, which shows up as stabilization that works early and
//! degrades late.
//!
//! The anchor count selects the model, deliberately:
//!
//! | Anchors | Model | Rationale |
//! |---|---|---|
//! | 0 | identity | the dialect declares its own alignment |
//! | 1 | offset only | one correspondence cannot observe rate |
//! | ≥2 | affine, least squares | offset *and* rate, with a drift diagnostic |

use photonic_core::timeline::{MotionSyncAnchor, TICKS_PER_SECOND};

/// `sensor_ns = scale * video_ns + offset_ns`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ClockMap {
    pub scale: f64,
    pub offset_ns: f64,
}

impl ClockMap {
    pub const IDENTITY: ClockMap = ClockMap {
        scale: 1.0,
        offset_ns: 0.0,
    };

    /// Map a source-video time to the sensor clock.
    pub fn sensor_ns(&self, video_ns: f64) -> f64 {
        self.scale * video_ns + self.offset_ns
    }

    /// Rate error in parts per million. Zero means the clocks run together.
    pub fn drift_ppm(&self) -> f64 {
        (self.scale - 1.0) * 1e6
    }
}

/// Convert a timeline tick to nanoseconds.
pub fn tick_to_ns(tick: photonic_core::timeline::Tick) -> f64 {
    tick.0 as f64 / TICKS_PER_SECOND as f64 * 1e9
}

/// Largest residual, in nanoseconds, between an anchor and the fitted map.
///
/// Reported alongside the map so the UI can say "your anchors disagree by 40 ms"
/// rather than silently fitting a line through contradictory input.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SyncFit {
    pub map: ClockMap,
    pub max_residual_ns: f64,
    pub anchors_used: usize,
}

/// Fit a clock map to `anchors`.
///
/// Least squares over all anchors rather than through the first and last: with
/// three or more, an operator-placed anchor in the middle carries real
/// information, and using only the endpoints would discard it and be maximally
/// sensitive to a mistake at either end.
pub fn fit(anchors: &[MotionSyncAnchor]) -> SyncFit {
    match anchors.len() {
        0 => SyncFit {
            map: ClockMap::IDENTITY,
            max_residual_ns: 0.0,
            anchors_used: 0,
        },
        1 => {
            let a = anchors[0];
            SyncFit {
                map: ClockMap {
                    scale: 1.0,
                    offset_ns: a.sensor_time_ns as f64 - tick_to_ns(a.video_tick),
                },
                max_residual_ns: 0.0,
                anchors_used: 1,
            }
        }
        n => {
            let xs: Vec<f64> = anchors.iter().map(|a| tick_to_ns(a.video_tick)).collect();
            let ys: Vec<f64> = anchors.iter().map(|a| a.sensor_time_ns as f64).collect();
            let nf = n as f64;
            let mean_x = xs.iter().sum::<f64>() / nf;
            let mean_y = ys.iter().sum::<f64>() / nf;
            let mut sxx = 0.0;
            let mut sxy = 0.0;
            for i in 0..n {
                let dx = xs[i] - mean_x;
                sxx += dx * dx;
                sxy += dx * (ys[i] - mean_y);
            }
            // Coincident video times cannot observe rate; fall back to offset.
            let scale = if sxx.abs() > 1e-6 { sxy / sxx } else { 1.0 };
            let map = ClockMap {
                scale,
                offset_ns: mean_y - scale * mean_x,
            };
            let max_residual_ns = (0..n)
                .map(|i| (ys[i] - map.sensor_ns(xs[i])).abs())
                .fold(0.0, f64::max);
            SyncFit {
                map,
                max_residual_ns,
                anchors_used: n,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::Tick;

    fn anchor(secs: f64, sensor_ns: i64) -> MotionSyncAnchor {
        MotionSyncAnchor {
            video_tick: Tick((secs * TICKS_PER_SECOND as f64) as i64),
            sensor_time_ns: sensor_ns,
        }
    }

    #[test]
    fn no_anchors_is_the_identity_map() {
        let f = fit(&[]);
        assert_eq!(f.map, ClockMap::IDENTITY);
        assert_eq!(f.map.sensor_ns(1_234.0), 1_234.0);
        assert_eq!(f.map.drift_ppm(), 0.0);
    }

    #[test]
    fn one_anchor_gives_offset_only() {
        // Sensor clock runs 500 ms ahead of video.
        let f = fit(&[anchor(1.0, 1_500_000_000)]);
        assert_eq!(f.map.scale, 1.0, "one correspondence cannot observe rate");
        assert!((f.map.offset_ns - 500_000_000.0).abs() < 1.0);
        assert!((f.map.sensor_ns(0.0) - 500_000_000.0).abs() < 1.0);
    }

    #[test]
    fn two_anchors_recover_offset_and_rate() {
        // 22 §6.7: "Two-anchor drift mapping aligns first/last samples within
        // one video frame." Sensor runs 1000 ppm fast with a 100 ms offset.
        let scale = 1.001;
        let offset = 100_000_000.0;
        let a0 = anchor(0.0, offset as i64);
        let a1 = anchor(10.0, (scale * 10e9 + offset) as i64);
        let f = fit(&[a0, a1]);
        assert!((f.map.scale - scale).abs() < 1e-9, "scale {}", f.map.scale);
        assert!((f.map.offset_ns - offset).abs() < 1.0);
        assert!(
            (f.map.drift_ppm() - 1000.0).abs() < 1e-3,
            "drift {} ppm",
            f.map.drift_ppm()
        );
        // Both ends land well inside one frame at 30 fps (33.3 ms).
        assert!(f.max_residual_ns < 33_000_000.0 / 2.0);
    }

    #[test]
    fn three_anchors_use_all_of_them() {
        let f = fit(&[
            anchor(0.0, 0),
            anchor(5.0, 5_000_000_000),
            anchor(10.0, 10_000_000_000),
        ]);
        assert_eq!(f.anchors_used, 3);
        assert!((f.map.scale - 1.0).abs() < 1e-12);
        assert!(f.max_residual_ns < 1.0);
    }

    #[test]
    fn contradictory_anchors_surface_as_residual() {
        // A middle anchor that disagrees by 40 ms must be reported, not
        // quietly averaged away.
        let f = fit(&[
            anchor(0.0, 0),
            anchor(5.0, 5_040_000_000),
            anchor(10.0, 10_000_000_000),
        ]);
        assert!(
            f.max_residual_ns > 20_000_000.0,
            "residual was {} ns",
            f.max_residual_ns
        );
    }

    #[test]
    fn coincident_video_times_cannot_observe_rate() {
        let f = fit(&[anchor(2.0, 1_000), anchor(2.0, 9_000)]);
        assert_eq!(f.map.scale, 1.0, "degenerate fit must not blow up");
        assert!(f.map.offset_ns.is_finite());
    }

    #[test]
    fn tick_conversion_matches_the_tick_rate() {
        assert!((tick_to_ns(Tick(TICKS_PER_SECOND)) - 1e9).abs() < 1e-6);
        assert_eq!(tick_to_ns(Tick(0)), 0.0);
    }
}
