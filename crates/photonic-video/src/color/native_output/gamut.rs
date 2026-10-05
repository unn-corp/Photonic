// SPDX-License-Identifier: Apache-2.0
// Copyright Contributors to the ACES Project.
//! Isolated Rec.709/D65 display-cube cusp search for a future ACES 2 gamut
//! compressor. It is not a complete gamut mapping and is not used for output.
//! Reference: <https://github.com/aces-aswf/aces-core/blob/069b0bc3e1f6c62820f19fdae2fecec3f4fc0f80/lib/Lib.Academy.OutputTransform.ctl>.

use super::{Aces2Jmh, Jmh};

/// A hue-matched point on a saturated edge of the 100-nit Rec.709 cube.
#[derive(Clone, Copy, Debug)]
pub struct DisplayCusp {
    pub jmh: Jmh,
    pub rgb: [f64; 3],
}

pub struct Aces2GamutCusp {
    limit: Aces2Jmh,
    corners: [(f64, [f64; 3]); 6],
}

/// Scalar ACES 2-style gamut compressor for a 100-nit Rec.709 limit. It uses
/// focus lines, a smoothed limiting cusp, a fitted upper/lower hull and a
/// modeled reach shell. Its compact corner-aware hue table is Photonic's own
/// implementation; display/export integration is still gated.
pub struct Aces2GamutCompressor {
    reach: Aces2Jmh,
    limit: Aces2Jmh,
    max_j: f64,
    mid_j: f64,
    hue_table: Vec<HueGamutParams>,
}

#[derive(Clone, Copy)]
struct HueGamutParams {
    hue_degrees: f64,
    cusp_j: f64,
    cusp_m: f64,
    gamma_top_inv: f64,
    reach_max_m: f64,
}

impl Aces2GamutCompressor {
    pub(super) fn gpu_params(&self) -> [f32; 2] {
        [self.max_j as f32, self.mid_j as f32]
    }

    pub(super) fn gpu_hue_rows(&self) -> Vec<[[f32; 4]; 2]> {
        self.hue_table
            .iter()
            .map(|entry| {
                [
                    [
                        entry.hue_degrees as f32,
                        entry.cusp_j as f32,
                        entry.cusp_m as f32,
                        entry.gamma_top_inv as f32,
                    ],
                    [entry.reach_max_m as f32, 0.0, 0.0, 0.0],
                ]
            })
            .collect()
    }

    pub fn rec709_sdr() -> Result<Self, &'static str> {
        let source = Aces2Jmh::ap0_reference()?;
        let cusp_search = Aces2GamutCusp::rec709_sdr()?;
        let mut result = Self {
            reach: Aces2Jmh::ap1_reference()?,
            limit: Aces2Jmh::rec709_d65_reference()?,
            max_j: source.luminance_to_lightness(100.0)?,
            mid_j: source.luminance_to_lightness(10.013)?,
            hue_table: Vec::with_capacity(367),
        };
        // Include limiting cube corners exactly; a uniform table interpolates
        // across the cusp's derivative change and shifts saturated yellows.
        let mut hues: Vec<f64> = (0..360).map(f64::from).collect();
        hues.extend(cusp_search.corners.iter().map(|(hue, _)| *hue));
        hues.sort_by(f64::total_cmp);
        hues.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        for hue in hues {
            let mut cusp = cusp_search.at_hue(hue)?.jmh;
            cusp.colorfulness *= 1.0 + 0.27 * 0.12;
            let focus_j = result.focus_j(cusp.lightness);
            let threshold_j = cusp.lightness * 0.7 + result.max_j * 0.3;
            let gamma_top_inv = result.upper_hull_gamma_inv(cusp, focus_j, threshold_j)?;
            let reach_max_m = result.boundary(result.max_j, 0.0, hue, true)?;
            result.hue_table.push(HueGamutParams {
                hue_degrees: hue,
                cusp_j: cusp.lightness,
                cusp_m: cusp.colorfulness,
                gamma_top_inv,
                reach_max_m,
            });
        }
        result.hue_table.push(HueGamutParams {
            hue_degrees: 360.0,
            ..result.hue_table[0]
        });
        Ok(result)
    }

    fn focus_j(&self, cusp_j: f64) -> f64 {
        let blend = (1.3 - cusp_j / self.max_j).min(1.0);
        cusp_j * (1.0 - blend) + self.mid_j * blend
    }

    fn hue_params(&self, hue_degrees: f64) -> HueGamutParams {
        let hue = hue_degrees.rem_euclid(360.0);
        let index = self
            .hue_table
            .partition_point(|entry| entry.hue_degrees <= hue)
            .saturating_sub(1);
        let a = self.hue_table[index];
        let b = self.hue_table[index + 1];
        let fraction = (hue - a.hue_degrees) / (b.hue_degrees - a.hue_degrees);
        let mix = |left: f64, right: f64| left * (1.0 - fraction) + right * fraction;
        HueGamutParams {
            hue_degrees: hue,
            cusp_j: mix(a.cusp_j, b.cusp_j),
            cusp_m: mix(a.cusp_m, b.cusp_m),
            gamma_top_inv: mix(a.gamma_top_inv, b.gamma_top_inv),
            reach_max_m: mix(a.reach_max_m, b.reach_max_m),
        }
    }

    fn axis_intersection(&self, j: f64, m: f64, focus: f64, gain: f64) -> f64 {
        if m == 0.0 {
            return j;
        }
        let scaled = m / gain;
        let a = scaled / focus;
        if j < focus {
            let b = 1.0 - scaled;
            let c = -j;
            -2.0 * c / (b + (b * b - 4.0 * a * c).sqrt())
        } else {
            let b = -(1.0 + scaled + self.max_j * a);
            let c = self.max_j * scaled + j;
            -2.0 * c / (b - (b * b - 4.0 * a * c).sqrt())
        }
    }

    fn focus_gain(&self, j: f64, threshold: f64) -> f64 {
        let mut gain = self.max_j * 1.35;
        if j > threshold {
            let adjustment = ((self.max_j - threshold) / (self.max_j - j).max(0.0001)).log10();
            gain *= adjustment * adjustment + 1.0;
        }
        gain
    }

    fn slope(&self, axis_j: f64, focus_j: f64, gain: f64) -> f64 {
        let direction = if axis_j < focus_j {
            axis_j
        } else {
            self.max_j - axis_j
        };
        direction * (axis_j - focus_j) / (focus_j * gain)
    }

    fn estimate_boundary(
        axis_j: f64,
        slope: f64,
        inv_gamma: f64,
        max_j: f64,
        max_m: f64,
        reference_j: f64,
    ) -> f64 {
        let shifted = reference_j * (axis_j / reference_j).powf(inv_gamma);
        shifted * max_m / (max_j - slope * max_m)
    }

    fn fitted_boundary(
        &self,
        cusp: Jmh,
        axis_j: f64,
        slope: f64,
        axis_cusp: f64,
        top_gamma_inv: f64,
    ) -> f64 {
        let lower = Self::estimate_boundary(
            axis_j,
            slope,
            1.0 / 1.14,
            cusp.lightness,
            cusp.colorfulness,
            axis_cusp,
        );
        let upper = Self::estimate_boundary(
            self.max_j - axis_j,
            -slope,
            top_gamma_inv,
            self.max_j - cusp.lightness,
            cusp.colorfulness,
            self.max_j - axis_cusp,
        );
        let smoothing = 0.12 * cusp.colorfulness;
        let overlap = (smoothing - (lower - upper).abs()).max(0.0) / smoothing;
        lower.min(upper) - overlap.powi(3) * smoothing / 6.0
    }

    fn upper_hull_gamma_inv(
        &self,
        cusp: Jmh,
        focus_j: f64,
        threshold_j: f64,
    ) -> Result<f64, &'static str> {
        let fits = |gamma: f64| -> Result<bool, &'static str> {
            for position in [0.01, 0.1, 0.5, 0.8, 0.99] {
                let j = cusp.lightness * (1.0 - position) + self.max_j * position;
                let gain = self.focus_gain(j, threshold_j);
                let axis_j = self.axis_intersection(j, cusp.colorfulness, focus_j, gain);
                let slope = self.slope(axis_j, focus_j, gain);
                let axis_cusp =
                    self.axis_intersection(cusp.lightness, cusp.colorfulness, focus_j, gain);
                let m = self.fitted_boundary(cusp, axis_j, slope, axis_cusp, 1.0 / gamma);
                let rgb = self.limit.jmh_to_rgb(Jmh {
                    lightness: axis_j + slope * m,
                    colorfulness: m,
                    hue_degrees: cusp.hue_degrees,
                })?;
                if !rgb.iter().any(|channel| *channel > 1.0) {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        let mut low = 0.0;
        let mut high = 0.4;
        while high < 5.0 && !fits(high)? {
            low = high;
            high += 0.4;
        }
        if !fits(high)? {
            return Err("upper gamut hull gamma could not be fitted");
        }
        while high - low > 1e-5 {
            let middle = (low + high) * 0.5;
            if fits(middle)? {
                high = middle;
            } else {
                low = middle;
            }
        }
        Ok(1.0 / high)
    }

    fn boundary(
        &self,
        axis_j: f64,
        slope: f64,
        hue: f64,
        reach: bool,
    ) -> Result<f64, &'static str> {
        let inside = |m: f64| -> bool {
            let j = axis_j + m * slope;
            if !(0.0..=self.max_j).contains(&j) {
                return false;
            }
            let model = if reach { &self.reach } else { &self.limit };
            model
                .jmh_to_rgb(Jmh {
                    lightness: j,
                    colorfulness: m,
                    hue_degrees: hue,
                })
                .is_ok_and(|rgb| {
                    rgb.iter()
                        .all(|value| *value >= -1e-8 && (reach || *value <= 1.0 + 1e-8))
                })
        };
        if !inside(0.0) {
            return Err("neutral focus line is outside the gamut");
        }
        let mut low = 0.0;
        let mut high = 50.0;
        while high < 1600.0 && inside(high) {
            low = high;
            high *= 2.0;
        }
        if inside(high) {
            return Err("gamut boundary was not found");
        }
        for _ in 0..25 {
            let middle = (low + high) * 0.5;
            if inside(middle) {
                low = middle;
            } else {
                high = middle;
            }
        }
        Ok(low)
    }

    pub fn map(&self, input: Jmh) -> Result<Jmh, &'static str> {
        if !input.lightness.is_finite()
            || !input.colorfulness.is_finite()
            || !input.hue_degrees.is_finite()
            || input.colorfulness < 0.0
        {
            return Err("focus-line gamut input is invalid");
        }
        if input.lightness <= 0.0 {
            return Ok(Jmh {
                lightness: 0.0,
                colorfulness: 0.0,
                hue_degrees: input.hue_degrees,
            });
        }
        if input.colorfulness == 0.0 {
            return Ok(input);
        }
        if input.lightness > self.max_j {
            return Ok(Jmh {
                lightness: input.lightness,
                colorfulness: 0.0,
                hue_degrees: input.hue_degrees,
            });
        }
        let params = self.hue_params(input.hue_degrees);
        let cusp = Jmh {
            lightness: params.cusp_j,
            colorfulness: params.cusp_m,
            hue_degrees: input.hue_degrees,
        };
        let focus_j = self.focus_j(cusp.lightness);
        let threshold_j = cusp.lightness * 0.7 + self.max_j * 0.3;
        let gain = self.focus_gain(input.lightness, threshold_j);
        let axis_j = self.axis_intersection(input.lightness, input.colorfulness, focus_j, gain);
        if !axis_j.is_finite() || !(0.0..=self.max_j).contains(&axis_j) {
            return Err("focus-line axis intersection is invalid");
        }
        let slope = self.slope(axis_j, focus_j, gain);
        let axis_cusp = self.axis_intersection(cusp.lightness, cusp.colorfulness, focus_j, gain);
        let limit_m = self.fitted_boundary(cusp, axis_j, slope, axis_cusp, params.gamma_top_inv);
        // ACES 2 models the reach shell from its J=max reach table instead
        // of intersecting the actual AP1 boundary along every focus line.
        let reach_max_m = params.reach_max_m;
        let model_gamma = 0.59 * (1.48 + (20.0_f64 / 100.0).sqrt());
        let reach_m = Self::estimate_boundary(
            axis_j,
            slope,
            1.0 / model_gamma,
            self.max_j,
            reach_max_m,
            self.max_j,
        );
        if !reach_m.is_finite() || reach_m <= limit_m {
            return Err("focus-line reach does not exceed the limiting boundary");
        }
        let proportion = (limit_m / reach_m).max(0.75);
        let threshold_m = proportion * limit_m;
        let remapped_m = if input.colorfulness <= threshold_m || proportion >= 1.0 {
            input.colorfulness
        } else {
            let gamut_room = limit_m - threshold_m;
            let reach_room = reach_m - threshold_m;
            let scale = reach_room / (reach_room / gamut_room - 1.0);
            let n = (input.colorfulness - threshold_m) / scale;
            threshold_m + scale * n / (1.0 + n)
        };
        let safe_m = remapped_m.min(limit_m);
        let result = Jmh {
            lightness: axis_j + safe_m * slope,
            colorfulness: safe_m,
            hue_degrees: input.hue_degrees,
        };
        if !result.lightness.is_finite() || !result.colorfulness.is_finite() {
            return Err("focus-line gamut mapping produced a nonfinite result");
        }
        Ok(result)
    }
}

impl Aces2GamutCusp {
    pub fn rec709_sdr() -> Result<Self, &'static str> {
        let limit = Aces2Jmh::rec709_d65_reference()?;
        let cube = [
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
        ];
        let mut corners = [(0.0, [0.0; 3]); 6];
        for (slot, rgb) in corners.iter_mut().zip(cube) {
            *slot = (limit.rgb_to_jmh(rgb)?.hue_degrees, rgb);
        }
        corners.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in corners.windows(2) {
            if pair[1].0 - pair[0].0 < 1e-6 {
                return Err("limiting gamut has duplicate cusp hues");
            }
        }
        Ok(Self { limit, corners })
    }

    /// Find the display-cube edge point at a hue. The returned RGB is on a
    /// cube face; the JMh value can seed a later hue/lightness gamut boundary.
    pub fn at_hue(&self, hue_degrees: f64) -> Result<DisplayCusp, &'static str> {
        if !hue_degrees.is_finite() {
            return Err("gamut cusp hue must be finite");
        }
        let hue = hue_degrees.rem_euclid(360.0);
        let upper = self.corners.partition_point(|(angle, _)| *angle <= hue);
        let lower = if upper == 0 { 5 } else { upper - 1 };
        let upper = upper % 6;
        let lower_hue = self.corners[lower].0;
        let upper_hue = if upper == 0 {
            self.corners[upper].0 + 360.0
        } else {
            self.corners[upper].0
        };
        let target = if hue < lower_hue { hue + 360.0 } else { hue };
        let start = self.corners[lower].1;
        let end = self.corners[upper].1;
        let sample = |t: f64| -> Result<DisplayCusp, &'static str> {
            let rgb = std::array::from_fn(|channel| start[channel] * (1.0 - t) + end[channel] * t);
            Ok(DisplayCusp {
                jmh: self.limit.rgb_to_jmh(rgb)?,
                rgb,
            })
        };
        let mut low = 0.0;
        let mut high = 1.0;
        for _ in 0..25 {
            let middle = (low + high) * 0.5;
            let cusp = sample(middle)?;
            let mut angle = cusp.jmh.hue_degrees;
            if angle < lower_hue {
                angle += 360.0;
            }
            if angle < target {
                low = middle;
            } else {
                high = middle;
            }
        }
        let result = sample((low + high) * 0.5)?;
        let mut result_hue = result.jmh.hue_degrees;
        if result_hue < lower_hue {
            result_hue += 360.0;
        }
        if !(lower_hue..=upper_hue).contains(&result_hue) || (result_hue - target).abs() > 0.001 {
            return Err("limiting gamut cusp search did not converge");
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::native_output::{encode_display, Aces2Chroma, Aces2ToneScale};
    use photonic_core::timeline::color::NativeOutputTransform;

    #[test]
    fn rec709_cusp_search_tracks_hue_and_cube_edges() {
        let gamut = Aces2GamutCusp::rec709_sdr().unwrap();
        for step in 0..360 {
            let hue = step as f64;
            let cusp = gamut.at_hue(hue).unwrap();
            let difference = (cusp.jmh.hue_degrees - hue + 540.0).rem_euclid(360.0) - 180.0;
            assert!(difference.abs() < 0.001, "hue {hue}: {cusp:?}");
            assert!(cusp.rgb.iter().all(|value| (0.0..=1.0).contains(value)));
            assert!(cusp.rgb.contains(&0.0));
            assert!(cusp.rgb.contains(&1.0));
            assert!(cusp.jmh.colorfulness > 0.0);
        }
        assert!(gamut.at_hue(f64::NAN).is_err());
        let seam_a = gamut.at_hue(359.999).unwrap();
        let seam_b = gamut.at_hue(-0.001).unwrap();
        for channel in 0..3 {
            assert!((seam_a.rgb[channel] - seam_b.rgb[channel]).abs() < 1e-8);
        }
    }

    #[test]
    fn focus_line_candidate_remains_finite_on_chromatic_grid() {
        let gamut = Aces2GamutCompressor::rec709_sdr().unwrap();
        for lightness in [5.0, 30.0, 60.0, 90.0] {
            for colorfulness in [0.0, 10.0, 50.0, 100.0] {
                for hue in (0..360).step_by(30) {
                    let input = Jmh {
                        lightness,
                        colorfulness,
                        hue_degrees: hue as f64,
                    };
                    let mapped = gamut.map(input).unwrap();
                    let rgb = gamut.limit.jmh_to_rgb(mapped).unwrap();
                    // The Academy's fitted hull can leave small excursions
                    // before the final display-code clamp. The independent
                    // output vectors below qualify the displayed result.
                    assert!(
                        rgb.iter()
                            .all(|channel| channel.is_finite() && (-0.5..=1.5).contains(channel)),
                        "{input:?} -> {mapped:?} -> {rgb:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn corner_aware_hue_table_wraps_continuously_and_rejects_nonfinite_input() {
        let gamut = Aces2GamutCompressor::rec709_sdr().unwrap();
        let baseline = gamut
            .map(Jmh {
                lightness: 45.0,
                colorfulness: 40.0,
                hue_degrees: 359.999,
            })
            .unwrap();
        let wrapped = gamut
            .map(Jmh {
                lightness: 45.0,
                colorfulness: 40.0,
                hue_degrees: -0.001,
            })
            .unwrap();
        assert!((baseline.lightness - wrapped.lightness).abs() < 1e-10);
        assert!((baseline.colorfulness - wrapped.colorfulness).abs() < 1e-10);
        assert!(gamut
            .map(Jmh {
                lightness: 45.0,
                colorfulness: 40.0,
                hue_degrees: f64::NAN,
            })
            .is_err());
        let over_white = gamut
            .map(Jmh {
                lightness: gamut.max_j + 1.0,
                colorfulness: 20.0,
                hue_degrees: 30.0,
            })
            .unwrap();
        assert_eq!(over_white.lightness, gamut.max_j + 1.0);
        assert_eq!(over_white.colorfulness, 0.0);
    }

    #[test]
    fn focus_line_candidate_against_independent_aces_two_grid() {
        let fixture = include_str!("../../../../../tests/fixtures/aces2_sdr_100nit_4cube.tsv");
        let tone = Aces2ToneScale::new(100.0).unwrap();
        let source = Aces2Jmh::ap0_reference().unwrap();
        let chroma = Aces2Chroma::new(100.0).unwrap();
        let gamut = Aces2GamutCompressor::rec709_sdr().unwrap();
        let mut worst = (0.0, [0.0; 3], [0.0; 3], [0.0; 3], 0);
        for line in fixture.lines().filter(|line| !line.starts_with('#')) {
            let values: Vec<f64> = line
                .split_whitespace()
                .map(|value| value.parse().expect("finite reference scalar"))
                .collect();
            assert_eq!(values.len(), 6);
            let input = [values[0], values[1], values[2]];
            let reference = [values[3], values[4], values[5]];
            let appearance = source
                .rgb_to_jmh(tone.clamp_ap1_to_ap0(input).unwrap())
                .unwrap();
            let shaped = chroma.map(appearance).unwrap();
            let mapped = gamut.map(shaped).unwrap();
            let rgb = gamut.limit.jmh_to_rgb(mapped).unwrap();
            let got = encode_display(NativeOutputTransform::SrgbSdr, rgb).unwrap();
            for channel in 0..3 {
                let error = (got[channel] - reference[channel]).abs();
                if error > worst.0 {
                    worst = (error, input, got, reference, channel);
                }
            }
        }
        assert!(
            worst.0 < 0.002,
            "worst error {} for source {:?} channel {}: candidate {:?}, reference {:?}",
            worst.0,
            worst.1,
            worst.4,
            worst.2,
            worst.3
        );
    }

    #[test]
    fn focus_line_candidate_matches_independent_highlight_and_negative_vectors() {
        let tone = Aces2ToneScale::new(100.0).unwrap();
        let source = Aces2Jmh::ap0_reference().unwrap();
        let chroma = Aces2Chroma::new(100.0).unwrap();
        let gamut = Aces2GamutCompressor::rec709_sdr().unwrap();
        let vectors = [
            ([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            ([0.18, 0.18, 0.18], [0.349188, 0.349188, 0.349188]),
            ([1.0, 1.0, 1.0], [0.706683, 0.706683, 0.706683]),
            ([1.0, 0.0, 0.0], [0.877155, 0.0, 0.040251]),
            ([0.0, 1.0, 0.0], [0.0, 0.714440, 0.274862]),
            ([0.0, 0.0, 1.0], [0.0, 0.210545, 0.646732]),
            ([0.0, 1.0, 1.0], [0.0, 0.720480, 0.698956]),
            ([1.0, 0.0, 1.0], [0.878617, 0.0, 0.743507]),
            ([1.0, 1.0, 0.0], [0.738466, 0.733361, 0.0]),
            ([0.5, 0.25, 0.15], [0.655992, 0.399624, 0.315902]),
            ([4.0, 0.3, 0.1], [1.000007, 0.498901, 0.432804]),
            ([-0.1, 0.2, 1.5], [0.0, 0.440422, 0.772242]),
        ];
        let mut worst = (0.0, [0.0; 3], [0.0; 3], [0.0; 3], 0);
        for (input, reference) in vectors {
            let appearance = source
                .rgb_to_jmh(tone.clamp_ap1_to_ap0(input).unwrap())
                .unwrap();
            let shaped = chroma.map(appearance).unwrap();
            let mapped = gamut.map(shaped).unwrap();
            let rgb = gamut.limit.jmh_to_rgb(mapped).unwrap();
            let got = encode_display(NativeOutputTransform::SrgbSdr, rgb).unwrap();
            for channel in 0..3 {
                let error = (got[channel] - reference[channel]).abs();
                if error > worst.0 {
                    worst = (error, input, got, reference, channel);
                }
            }
        }
        assert!(
            worst.0 < 0.002,
            "worst error {} for source {:?} channel {}: candidate {:?}, reference {:?}",
            worst.0,
            worst.1,
            worst.4,
            worst.2,
            worst.3
        );
    }
}
