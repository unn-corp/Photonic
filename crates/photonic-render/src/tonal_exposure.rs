//! Relative-luminance-selected linked RGB exposure. A creative primary, not
//! a display tone mapper or a spatial shadows/highlights restoration filter.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ResolvedTonalExposure {
    pub stops: [f32; 3],
    pub boundaries: [f32; 2],
    pub softness: f32,
    pub native_ap1: bool,
}
impl ResolvedTonalExposure {
    pub fn validate(&self) -> Result<(), &'static str> {
        photonic_core::timeline::validate_tonal_exposure(self.stops, self.boundaries, self.softness)
    }
    pub fn luma_weights(&self) -> [f32; 3] {
        if self.native_ap1 {
            [0.27222872, 0.67408174, 0.053689517]
        } else {
            [0.2126, 0.7152, 0.0722]
        }
    }
    pub fn weights(&self, luma: f32) -> [f32; 3] {
        if luma <= 0.0 {
            return [1.0, 0.0, 0.0];
        }
        let relative = (luma / 0.18).log2();
        let gate = |center: f32| {
            let t = ((relative - center + self.softness) / (2.0 * self.softness)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let low = gate(self.boundaries[0]);
        let high = gate(self.boundaries[1]);
        [1.0 - low, low - high, high]
    }
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        if self.stops == [0.0; 3] {
            return rgb;
        }
        let coefficients = self.luma_weights();
        let weights = self.weights(rgb.iter().zip(coefficients).map(|(v, c)| v * c).sum());
        let gain = weights
            .iter()
            .zip(self.stops)
            .map(|(w, s)| w * s)
            .sum::<f32>()
            .exp2();
        rgb.map(|v| v * gain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tonal_partition_and_linked_exposure_have_known_reference_values() {
        let op = ResolvedTonalExposure {
            stops: [1.0, 0.0, -1.0],
            boundaries: [-2.0, 2.0],
            softness: 1.0,
            native_ap1: true,
        };
        for (stops, expected) in [
            (-4.0, [1.0, 0.0, 0.0]),
            (-2.0, [0.5, 0.5, 0.0]),
            (0.0, [0.0, 1.0, 0.0]),
            (2.0, [0.0, 0.5, 0.5]),
            (4.0, [0.0, 0.0, 1.0]),
        ] {
            let y = 0.18 * 2.0f32.powf(stops);
            let w = op.weights(y);
            for c in 0..3 {
                assert!((w[c] - expected[c]).abs() < 1e-6);
            }
        }
        assert_eq!(op.weights(-1.0), [1.0, 0.0, 0.0]);
        assert_eq!(op.weights(0.0), [1.0, 0.0, 0.0]);
        for width in [0.01, 1.0, 6.0] {
            let op = ResolvedTonalExposure {
                softness: width,
                ..op
            };
            for i in -240..=240 {
                let w = op.weights(0.18 * 2.0f32.powf(i as f32 / 10.0));
                assert!(w.iter().all(|v| *v >= 0.0 && *v <= 1.0));
                assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
            }
        }
        for native in [false, true] {
            let op = ResolvedTonalExposure {
                stops: [1.0; 3],
                native_ap1: native,
                ..op
            };
            for rgb in [
                [-0.1, 0.2, 3.0],
                [-0.2; 3],
                [0.0; 3],
                [0.18; 3],
                [16.0, 4.0, 1.0],
            ] {
                assert_eq!(op.apply(rgb), rgb.map(|v| v * 2.0));
                assert_eq!(
                    ResolvedTonalExposure {
                        stops: [0.0; 3],
                        ..op
                    }
                    .apply(rgb),
                    rgb
                );
            }
        }
    }
    #[test]
    fn tonal_invalid_values_and_crossed_boundaries_are_rejected() {
        let valid = ResolvedTonalExposure {
            stops: [0.0; 3],
            boundaries: [-2.0, 2.0],
            softness: 1.0,
            native_ap1: true,
        };
        assert!(valid.validate().is_ok());
        for op in [
            ResolvedTonalExposure {
                stops: [9.0, 0.0, 0.0],
                ..valid
            },
            ResolvedTonalExposure {
                stops: [f32::NAN, 0.0, 0.0],
                ..valid
            },
            ResolvedTonalExposure {
                boundaries: [2.0, -2.0],
                ..valid
            },
            ResolvedTonalExposure {
                boundaries: [0.0, 0.0],
                ..valid
            },
            ResolvedTonalExposure {
                boundaries: [-13.0, 2.0],
                ..valid
            },
            ResolvedTonalExposure {
                softness: 0.0,
                ..valid
            },
            ResolvedTonalExposure {
                softness: f32::INFINITY,
                ..valid
            },
        ] {
            assert!(op.validate().is_err());
        }
    }
}
