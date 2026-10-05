// SPDX-License-Identifier: Apache-2.0
// Copyright Contributors to the ACES Project.
//! Isolated ACES 2 in-gamut chroma shaping. Output gamut compression is a
//! separate stage and is not implemented here.
//! Equations: <https://github.com/aces-aswf/aces-core/blob/069b0bc3e1f6c62820f19fdae2fecec3f4fc0f80/lib/Lib.Academy.OutputTransform.ctl>.

use super::{Aces2Jmh, Aces2ToneScale, Jmh};

const HUE_SAMPLES: usize = 360;

pub struct Aces2Chroma {
    source: Aces2Jmh,
    tone: Aces2ToneScale,
    reach: [f64; HUE_SAMPLES + 2],
    limit_lightness: f64,
    inverse_power: f64,
    saturation: f64,
    saturation_threshold: f64,
    compression: f64,
    norm_scale: f64,
}

fn toe(x: f64, limit: f64, first: f64, second: f64) -> f64 {
    if x > limit {
        return x;
    }
    let second = second.max(0.001);
    let first = first.hypot(second);
    let ratio = (limit + first) / (limit + second);
    let minus_b = ratio * x - first;
    let minus_c = second * ratio * x;
    0.5 * (minus_b + (minus_b * minus_b + 4.0 * minus_c).sqrt())
}

fn chroma_norm(hue_degrees: f64, scale: f64) -> f64 {
    let (sin_h, cos_h) = hue_degrees.to_radians().sin_cos();
    let cos2 = cos_h * cos_h - sin_h * sin_h;
    let sin2 = 2.0 * cos_h * sin_h;
    let cos3 = 4.0 * cos_h.powi(3) - 3.0 * cos_h;
    let sin3 = 3.0 * sin_h - 4.0 * sin_h.powi(3);
    (11.34072 * cos_h + 16.46899 * cos2 + 7.88380 * cos3 + 14.66441 * sin_h - 6.37224 * sin2
        + 9.19364 * sin3
        + 77.12896)
        * scale
}

impl Aces2Chroma {
    pub(super) fn gpu_params(&self) -> [f32; 4] {
        [
            self.norm_scale as f32,
            self.saturation as f32,
            self.saturation_threshold as f32,
            self.compression as f32,
        ]
    }

    pub(super) fn gpu_reach_table(&self) -> &[f64; HUE_SAMPLES + 2] {
        &self.reach
    }

    pub fn new(peak_nits: f64) -> Result<Self, &'static str> {
        let source = Aces2Jmh::ap0_reference()?;
        let reach_model = Aces2Jmh::ap1_reference()?;
        let tone = Aces2ToneScale::new(peak_nits)?;
        let limit_lightness = source.luminance_to_lightness(peak_nits)?;
        let log_peak = (peak_nits / 100.0).log10();
        let mut reach = [0.0; HUE_SAMPLES + 2];
        for hue in 0..HUE_SAMPLES {
            let mut low = 0.0;
            let mut high = 50.0;
            while high < 1300.0
                && reach_model
                    .jmh_to_rgb(Jmh {
                        lightness: limit_lightness,
                        colorfulness: high,
                        hue_degrees: hue as f64,
                    })?
                    .iter()
                    .all(|channel| *channel >= 0.0)
            {
                low = high;
                high += 50.0;
            }
            while high - low > 0.01 {
                let middle = (high + low) * 0.5;
                let rgb = reach_model.jmh_to_rgb(Jmh {
                    lightness: limit_lightness,
                    colorfulness: middle,
                    hue_degrees: hue as f64,
                })?;
                if rgb.iter().any(|channel| *channel < 0.0) {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            reach[hue + 1] = high;
        }
        reach[0] = reach[HUE_SAMPLES];
        reach[HUE_SAMPLES + 1] = reach[1];
        let inverse_power = 1.0 / source.model_power();
        Ok(Self {
            source,
            tone,
            reach,
            limit_lightness,
            inverse_power,
            saturation: (1.3 - 1.3 * 0.69 * log_peak).max(0.2),
            saturation_threshold: 0.5 / peak_nits,
            compression: 2.4 + 2.4 * 3.3 * log_peak,
            norm_scale: (0.03379 * peak_nits).powf(0.30596) - 0.45135,
        })
    }

    fn reach_at(&self, hue_degrees: f64) -> f64 {
        let wrapped = hue_degrees.rem_euclid(360.0);
        let index = wrapped.floor() as usize;
        let fraction = wrapped - index as f64;
        self.reach[index + 1] * (1.0 - fraction) + self.reach[index + 2] * fraction
    }

    /// Maps J and M together while preserving hue. This is not yet the output
    /// gamut compressor, limiting-gamut conversion, or display encoding.
    pub fn map(&self, input: Jmh) -> Result<Jmh, &'static str> {
        if !input.lightness.is_finite()
            || !input.colorfulness.is_finite()
            || !input.hue_degrees.is_finite()
            || input.lightness < 0.0
            || input.colorfulness < 0.0
        {
            return Err("ACES 2 chroma input is invalid");
        }
        let scene = self.source.lightness_to_luminance(input.lightness)? / 100.0;
        let mapped_nits = self.tone.map_lightness(scene)?;
        let mapped_j = self.source.luminance_to_lightness(mapped_nits)?;
        if input.colorfulness == 0.0 {
            return Ok(Jmh {
                lightness: mapped_j,
                colorfulness: 0.0,
                hue_degrees: input.hue_degrees,
            });
        }
        if input.lightness == 0.0 || mapped_j == 0.0 {
            return Err("nonzero ACES 2 colorfulness at black is unsupported");
        }
        let n_j = mapped_j / self.limit_lightness;
        let shadow = (1.0 - n_j).max(0.0);
        let norm = chroma_norm(input.hue_degrees, self.norm_scale);
        if norm <= 0.0 || !norm.is_finite() {
            return Err("ACES 2 chroma normalization is invalid");
        }
        let limit = n_j.powf(self.inverse_power) * self.reach_at(input.hue_degrees) / norm;
        let mut m =
            input.colorfulness * (mapped_j / input.lightness).powf(self.inverse_power) / norm;
        m = limit
            - toe(
                limit - m,
                limit - 0.001,
                shadow * self.saturation,
                (n_j * n_j + self.saturation_threshold).sqrt(),
            );
        m = toe(m, limit, n_j * self.compression, shadow) * norm;
        if !m.is_finite() || m < 0.0 {
            return Err("ACES 2 chroma compression produced an invalid value");
        }
        Ok(Jmh {
            lightness: mapped_j,
            colorfulness: m,
            hue_degrees: input.hue_degrees,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chroma_stage_keeps_neutral_and_hue_and_shapes_saturation() {
        let stage = Aces2Chroma::new(100.0).unwrap();
        let neutral = stage
            .map(Jmh {
                lightness: 50.0,
                colorfulness: 0.0,
                hue_degrees: 270.0,
            })
            .unwrap();
        assert_eq!(neutral.colorfulness, 0.0);
        assert_eq!(neutral.hue_degrees, 270.0);
        let source = Aces2Jmh::ap0_reference().unwrap();
        let saturated = source.rgb_to_jmh([0.7, 0.12, 0.04]).unwrap();
        let shaped = stage.map(saturated).unwrap();
        assert_eq!(shaped.hue_degrees, saturated.hue_degrees);
        assert!(shaped.colorfulness.is_finite());
        assert!(shaped.colorfulness > 0.0);
        assert_ne!(shaped.colorfulness, saturated.colorfulness);
    }
}
