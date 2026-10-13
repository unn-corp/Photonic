//! Paired native scene and SDR-display measurements from one rendered frame.
//! Scene stops are relative AP1 luminance to 0.18, not nits or video IRE.
use serde::Serialize;

pub const MAX_PAIRED_PIXELS: usize = 16_777_216;
pub const STOP_BINS: usize = 128;
pub const STOP_MIN: f64 = -12.0;
pub const STOP_MAX: f64 = 12.0;
// AP1 primaries and ACES white: https://docs.acescentral.com/encodings/acescg/
// The independent chromaticity derivation below guards this Y row.
const AP1_Y: [f64; 3] = [0.27222871678091454, 0.6740817658111484, 0.05368951740793705];

#[derive(Debug, Serialize)]
pub struct RgbMeasurement {
    pub min: Option<[f64; 3]>,
    pub max: Option<[f64; 3]>,
    pub mean: Option<[f64; 3]>,
    pub below_zero: [u64; 3],
    pub above_one: [u64; 3],
}
impl RgbMeasurement {
    fn new() -> Self {
        Self {
            min: None,
            max: None,
            mean: None,
            below_zero: [0; 3],
            above_one: [0; 3],
        }
    }
    fn add(&mut self, rgb: [f64; 3]) {
        let lo = self.min.get_or_insert(rgb);
        let hi = self.max.get_or_insert(rgb);
        let sum = self.mean.get_or_insert([0.0; 3]);
        for c in 0..3 {
            lo[c] = lo[c].min(rgb[c]);
            hi[c] = hi[c].max(rgb[c]);
            sum[c] += rgb[c];
            self.below_zero[c] += u64::from(rgb[c] < 0.0);
            self.above_one[c] += u64::from(rgb[c] > 1.0);
        }
    }
    fn finish(&mut self, count: u64) {
        if let Some(sum) = self.mean.as_mut() {
            for v in sum {
                *v /= count as f64;
            }
        }
    }
}
#[derive(Debug, Serialize)]
pub struct SceneStops {
    pub reference_gray: f64,
    pub minimum_stop: f64,
    pub maximum_stop: f64,
    pub counts: Vec<u64>,
    pub nonpositive: u64,
    pub below_range: u64,
    pub above_range: u64,
}
#[derive(Debug, Serialize)]
pub struct FrameMeasurements {
    pub covered_pixels: u64,
    pub transparent_pixels: u64,
    pub alpha_min: f32,
    pub alpha_max: f32,
    pub scene_rgb: RgbMeasurement,
    pub display_rgb: RgbMeasurement,
    pub scene_luminance_stops: SceneStops,
    #[serde(skip)]
    pub display_rgba: Vec<u8>,
}

/// Both arrays must come from the same snapshot, tick and logical canvas. This
/// validates readback size/finite values and unchanged coverage, but provenance
/// is enforced by the engine caller. Covered pixels have equal weight regardless
/// of alpha; RGB is unpremultiplied in f64 without clipping measured values.
pub fn measure_pair(
    scene: &[[f32; 4]],
    display: &[[f32; 4]],
    width: u32,
    height: u32,
) -> Result<FrameMeasurements, &'static str> {
    let count = (width as usize)
        .checked_mul(height as usize)
        .filter(|n| *n > 0)
        .ok_or("invalid measurement dimensions")?;
    if count > MAX_PAIRED_PIXELS {
        return Err("paired measurement exceeds the 16777216-pixel bound");
    }
    if scene.len() != count || display.len() != count || count.checked_mul(4).is_none() {
        return Err("incomplete paired readback");
    }
    let mut out = FrameMeasurements {
        covered_pixels: 0,
        transparent_pixels: 0,
        alpha_min: 1.0,
        alpha_max: 0.0,
        scene_rgb: RgbMeasurement::new(),
        display_rgb: RgbMeasurement::new(),
        scene_luminance_stops: SceneStops {
            reference_gray: 0.18,
            minimum_stop: STOP_MIN,
            maximum_stop: STOP_MAX,
            counts: vec![0; STOP_BINS],
            nonpositive: 0,
            below_range: 0,
            above_range: 0,
        },
        display_rgba: Vec::with_capacity(count * 4),
    };
    for (a, b) in scene.iter().zip(display) {
        if a.iter().chain(b).any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&a[3]) || a[3] != b[3]
        {
            return Err("invalid paired pixels or changed coverage");
        }
        let alpha = a[3];
        out.alpha_min = out.alpha_min.min(alpha);
        out.alpha_max = out.alpha_max.max(alpha);
        if alpha == 0.0 {
            out.transparent_pixels += 1;
            out.display_rgba.extend_from_slice(&[0; 4]);
            continue;
        }
        out.covered_pixels += 1;
        let scene_rgb = std::array::from_fn(|c| f64::from(a[c]) / f64::from(alpha));
        let display_rgb = std::array::from_fn(|c| f64::from(b[c]) / f64::from(alpha));
        out.scene_rgb.add(scene_rgb);
        out.display_rgb.add(display_rgb);
        let y = scene_rgb.iter().zip(AP1_Y).map(|(v, c)| v * c).sum::<f64>();
        let histogram = &mut out.scene_luminance_stops;
        if y <= 0.0 {
            histogram.nonpositive += 1;
        } else {
            let stop = (y / 0.18).log2();
            if stop < STOP_MIN {
                histogram.below_range += 1;
            } else if stop >= STOP_MAX {
                histogram.above_range += 1;
            } else {
                let bin =
                    ((stop - STOP_MIN) / (STOP_MAX - STOP_MIN) * STOP_BINS as f64).floor() as usize;
                histogram.counts[bin.min(STOP_BINS - 1)] += 1;
            }
        }
        out.display_rgba
            .extend(display_rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8));
        out.display_rgba.push((alpha * 255.0).round() as u8);
    }
    out.scene_rgb.finish(out.covered_pixels);
    out.display_rgb.finish(out.covered_pixels);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ap1_luminance_weights_match_independent_chromaticity_derivation() {
        let xyz = |x: f64, y: f64| glam::DVec3::new(x / y, 1.0, (1.0 - x - y) / y);
        let primaries =
            glam::DMat3::from_cols(xyz(0.713, 0.293), xyz(0.165, 0.830), xyz(0.128, 0.044));
        let scale = primaries.inverse() * xyz(0.32168, 0.33767);
        for (actual, expected) in AP1_Y.into_iter().zip(scale.to_array()) {
            assert!((actual - expected).abs() < 1e-12);
        }
        assert!((AP1_Y.iter().sum::<f64>() - 1.0).abs() < 1e-14);
    }

    #[test]
    fn signed_highlight_and_partial_alpha_are_measured_without_clipping() {
        let m = measure_pair(
            &[[-0.25, 2.0, 0.5, 0.5], [1.0, 0.0, 0.0, 0.0]],
            &[[0.1, 0.4, 0.2, 0.5], [0.0; 4]],
            2,
            1,
        )
        .unwrap();
        assert_eq!(m.covered_pixels, 1);
        assert_eq!(m.transparent_pixels, 1);
        assert_eq!(m.scene_rgb.min, Some([-0.5, 4.0, 1.0]));
        assert_eq!(m.scene_rgb.mean, m.scene_rgb.min);
        assert_eq!(m.scene_rgb.below_zero, [1, 0, 0]);
        assert_eq!(m.scene_rgb.above_one, [0, 1, 0]);
        assert_eq!(m.display_rgba, [51, 204, 102, 128, 0, 0, 0, 0]);
        assert_eq!(m.scene_luminance_stops.counts.iter().sum::<u64>(), 1);
    }
    #[test]
    fn covered_pixels_have_equal_weight_and_oversized_readbacks_are_rejected() {
        let scene = [[0.2, 0.2, 0.2, 0.1], [0.0, 0.0, 0.0, 1.0]];
        let display = [[0.05, 0.05, 0.05, 0.1], [0.0, 0.0, 0.0, 1.0]];
        let m = measure_pair(&scene, &display, 2, 1).unwrap();
        assert_eq!(m.scene_rgb.mean, Some([1.0; 3]));
        assert_eq!(m.display_rgb.mean, Some([0.25; 3]));
        assert_eq!(
            measure_pair(&[], &[], 4097, 4096).unwrap_err(),
            "paired measurement exceeds the 16777216-pixel bound"
        );
    }

    #[test]
    fn transparent_measurements_are_absent_rather_than_a_false_black_signal() {
        let m = measure_pair(&[[0.0; 4]], &[[0.0; 4]], 1, 1).unwrap();
        assert_eq!(m.scene_rgb.mean, None);
        assert_eq!(m.display_rgb.min, None);
        assert_eq!(m.covered_pixels, 0);
        assert_eq!(m.scene_luminance_stops.nonpositive, 0);
    }
    #[test]
    fn relative_stop_bins_and_outliers_account_for_every_covered_pixel() {
        let values = [
            -1.0,
            0.0,
            0.18 * 2f32.powi(-13),
            0.18 * 2f32.powi(-6),
            0.18,
            0.18 * 2f32.powi(6),
            0.18 * 2f32.powi(13),
        ];
        let scene: Vec<_> = values.into_iter().map(|v| [v, v, v, 1.0]).collect();
        let m = measure_pair(&scene, &vec![[0.5, 0.5, 0.5, 1.0]; scene.len()], 7, 1).unwrap();
        let h = m.scene_luminance_stops;
        assert_eq!(h.nonpositive, 2);
        assert_eq!(h.below_range, 1);
        assert_eq!(h.above_range, 1);
        assert_eq!(h.counts[32], 1);
        assert_eq!(h.counts[64], 1);
        assert_eq!(h.counts[96], 1);
        assert_eq!(
            h.counts.iter().sum::<u64>() + h.nonpositive + h.below_range + h.above_range,
            7
        );
    }
    #[test]
    fn malformed_or_mismatched_pairs_are_rejected() {
        assert!(measure_pair(&[], &[], 0, 1).is_err());
        assert!(measure_pair(&[], &[], 1, 1).is_err());
        for p in [
            [f32::NAN, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, -0.1],
            [0.0, 0.0, 0.0, 1.1],
        ] {
            assert!(measure_pair(&[p], &[p], 1, 1).is_err());
        }
        assert!(measure_pair(&[[0.0, 0.0, 0.0, 0.5]], &[[0.0, 0.0, 0.0, 1.0]], 1, 1).is_err());
    }
}
