// SPDX-License-Identifier: Apache-2.0
// Copyright Contributors to the ACES Project.
//! Isolated ACES 2 JMh appearance coordinates for AP1 working RGB. This is a
//! scalar reference, not yet a complete output rendering transform.
//! Model equations: <https://github.com/aces-aswf/aces-core/blob/069b0bc3e1f6c62820f19fdae2fecec3f4fc0f80/lib/Lib.Academy.OutputTransform.ctl>.

use crate::color::native::{rgb_to_xyz_from_xy, Matrix3};

const REFERENCE_LUMINANCE: f64 = 100.0;
const CONE_OFFSET: f64 = 0.2713 * REFERENCE_LUMINANCE;
const CONE_SCALE: f64 = 4.0 * REFERENCE_LUMINANCE;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Jmh {
    pub lightness: f64,
    pub colorfulness: f64,
    pub hue_degrees: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Aces2Jmh {
    rgb_to_cone: Matrix3,
    cone_to_rgb: Matrix3,
    cone_to_aab: Matrix3,
    aab_to_cone: Matrix3,
    adaptation_scale: f64,
    model_power: f64,
    white_response: f64,
}

fn compress_cone(value: f64) -> f64 {
    let magnitude = value.abs().powf(0.42);
    value.signum() * magnitude / (CONE_OFFSET + magnitude)
}

fn expand_cone(value: f64) -> f64 {
    let magnitude = value.abs().min(0.99);
    let response = CONE_OFFSET * magnitude / (1.0 - magnitude);
    value.signum() * response.powf(1.0 / 0.42)
}

impl Aces2Jmh {
    pub(super) fn forward_gpu_rows(&self) -> [[[f32; 4]; 3]; 2] {
        [
            self.rgb_to_cone
                .0
                .map(|row| [row[0] as f32, row[1] as f32, row[2] as f32, 0.0]),
            self.cone_to_aab
                .0
                .map(|row| [row[0] as f32, row[1] as f32, row[2] as f32, 0.0]),
        ]
    }

    pub(super) fn inverse_gpu_rows(&self) -> [[[f32; 4]; 3]; 2] {
        [
            self.aab_to_cone
                .0
                .map(|row| [row[0] as f32, row[1] as f32, row[2] as f32, 0.0]),
            self.cone_to_rgb
                .0
                .map(|row| [row[0] as f32, row[1] as f32, row[2] as f32, 0.0]),
        ]
    }

    pub(super) fn appearance_gpu_params(&self) -> [f32; 3] {
        [
            self.model_power as f32,
            self.white_response as f32,
            self.adaptation_scale as f32,
        ]
    }

    pub(crate) fn model_power(&self) -> f64 {
        self.model_power
    }

    pub fn ap1_reference() -> Result<Self, &'static str> {
        Self::from_primaries([
            [0.713, 0.293],
            [0.165, 0.830],
            [0.128, 0.044],
            [0.32168, 0.33767],
        ])
    }

    /// The Academy output transform evaluates appearance in AP0 after its
    /// input clamp stage. AP1 is the managed working space, not this stage's
    /// appearance-model input.
    pub fn ap0_reference() -> Result<Self, &'static str> {
        Self::from_primaries([
            [0.73470, 0.26530],
            [0.00000, 1.00000],
            [0.00010, -0.07700],
            [0.32168, 0.33767],
        ])
    }

    /// Limiting/display primaries for the pinned 100-nit sRGB SDR preset.
    pub fn rec709_d65_reference() -> Result<Self, &'static str> {
        Self::from_primaries([
            [0.6400, 0.3300],
            [0.3000, 0.6000],
            [0.1500, 0.0600],
            [0.3127, 0.3290],
        ])
    }

    fn from_primaries(xy: [[f64; 2]; 4]) -> Result<Self, &'static str> {
        let source_to_xyz = rgb_to_xyz_from_xy(xy);
        let cam_to_xyz = rgb_to_xyz_from_xy([
            [0.8336, 0.1735],
            [2.3854, -1.4659],
            [0.087, -0.125],
            [0.333, 0.333],
        ]);
        let xyz_to_cam = cam_to_xyz.inverse()?;
        let xyz_white = source_to_xyz.transform([REFERENCE_LUMINANCE; 3]);
        let cone_white = xyz_to_cam.transform(xyz_white);
        let adaptation: f64 = 100.0;
        let k = 1.0 / (5.0 * adaptation + 1.0);
        let k4 = k.powi(4);
        let fl = 0.2 * k4 * 5.0 * adaptation + 0.1 * (1.0 - k4).powi(2) * (5.0 * adaptation).cbrt();
        let adaptation_scale = fl / REFERENCE_LUMINANCE;
        let model_power = 0.59 * (1.48 + (20.0 / REFERENCE_LUMINANCE).sqrt());
        let white_scale = std::array::from_fn::<_, 3, _>(|index| {
            adaptation_scale * xyz_white[1] / cone_white[index]
        });
        let diagonal = Matrix3([
            [white_scale[0], 0.0, 0.0],
            [0.0, white_scale[1], 0.0],
            [0.0, 0.0, white_scale[2]],
        ]);
        let rgb_to_cone = diagonal.product(xyz_to_cam).product(source_to_xyz);
        let rgb_to_cone = Matrix3(rgb_to_cone.0.map(|row| row.map(|value| value * 100.0)));
        let cone_to_rgb = rgb_to_cone.inverse()?;

        // The Academy's base cone-response matrix is expressed for row
        // vectors. This transposed layout uses Matrix3's column-vector API.
        let base = Matrix3([
            [2.0, 1.0, 1.0 / 20.0],
            [1.0, -12.0 / 11.0, 1.0 / 11.0],
            [1.0 / 9.0, 1.0 / 9.0, -2.0 / 9.0],
        ]);
        let scaled = Matrix3(base.0.map(|row| row.map(|value| value * CONE_SCALE)));
        let adapted_white = std::array::from_fn::<_, 3, _>(|index| {
            compress_cone(white_scale[index] * cone_white[index])
        });
        let achromatic_white = scaled.transform(adapted_white)[0];
        let colorfulness_scale = 43.0 * 0.9;
        let cone_to_aab = Matrix3([
            scaled.0[0].map(|value| value / achromatic_white),
            scaled.0[1].map(|value| value * colorfulness_scale),
            scaled.0[2].map(|value| value * colorfulness_scale),
        ]);
        let aab_to_cone = cone_to_aab.inverse()?;
        let white_response = compress_cone(fl);
        if !adaptation_scale.is_finite()
            || !model_power.is_finite()
            || !white_response.is_finite()
            || achromatic_white <= 0.0
        {
            return Err("invalid ACES 2 appearance model parameters");
        }
        Ok(Self {
            rgb_to_cone,
            cone_to_rgb,
            cone_to_aab,
            aab_to_cone,
            adaptation_scale,
            model_power,
            white_response,
        })
    }

    pub fn rgb_to_jmh(&self, rgb: [f64; 3]) -> Result<Jmh, &'static str> {
        if rgb.iter().any(|value| !value.is_finite()) {
            return Err("ACES 2 appearance input must be finite");
        }
        let cones = self.rgb_to_cone.transform(rgb).map(compress_cone);
        let [achromatic, a, b] = self.cone_to_aab.transform(cones);
        if [achromatic, a, b].iter().any(|value| !value.is_finite()) {
            return Err("ACES 2 appearance conversion overflowed");
        }
        if achromatic <= 0.0 {
            return Ok(Jmh {
                lightness: 0.0,
                colorfulness: 0.0,
                hue_degrees: 0.0,
            });
        }
        let result = Jmh {
            lightness: 100.0 * achromatic.powf(self.model_power),
            colorfulness: a.hypot(b),
            hue_degrees: b.atan2(a).to_degrees().rem_euclid(360.0),
        };
        if !result.lightness.is_finite() || !result.colorfulness.is_finite() {
            return Err("ACES 2 appearance conversion overflowed");
        }
        Ok(result)
    }

    pub fn jmh_to_rgb(&self, jmh: Jmh) -> Result<[f64; 3], &'static str> {
        if !jmh.lightness.is_finite()
            || !jmh.colorfulness.is_finite()
            || !jmh.hue_degrees.is_finite()
            || jmh.lightness < 0.0
            || jmh.colorfulness < 0.0
        {
            return Err("ACES 2 appearance coordinates are invalid");
        }
        let radians = jmh.hue_degrees.to_radians();
        let aab = [
            (jmh.lightness / 100.0).powf(1.0 / self.model_power),
            jmh.colorfulness * radians.cos(),
            jmh.colorfulness * radians.sin(),
        ];
        let cones = self.aab_to_cone.transform(aab).map(expand_cone);
        let result = self.cone_to_rgb.transform(cones);
        if result.iter().any(|value| !value.is_finite()) {
            return Err("ACES 2 appearance inverse overflowed");
        }
        Ok(result)
    }

    pub fn lightness_to_luminance(&self, lightness: f64) -> Result<f64, &'static str> {
        if !lightness.is_finite() {
            return Err("ACES 2 lightness must be finite");
        }
        let achromatic = (lightness.abs() / 100.0).powf(1.0 / self.model_power);
        let response = expand_cone(self.white_response * achromatic);
        let luminance = response / self.adaptation_scale;
        if !luminance.is_finite() {
            return Err("ACES 2 lightness conversion overflowed");
        }
        Ok(luminance)
    }

    pub fn luminance_to_lightness(&self, luminance: f64) -> Result<f64, &'static str> {
        if !luminance.is_finite() {
            return Err("ACES 2 luminance must be finite");
        }
        let response = compress_cone(luminance.abs() * self.adaptation_scale);
        let lightness =
            luminance.signum() * 100.0 * (response / self.white_response).powf(self.model_power);
        if !lightness.is_finite() {
            return Err("ACES 2 luminance conversion overflowed");
        }
        Ok(lightness)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_lightness_round_trips_to_scene_luminance() {
        let model = Aces2Jmh::ap1_reference().unwrap();
        for gray in [0.001, 0.18, 1.0, 16.0] {
            let jmh = model.rgb_to_jmh([gray; 3]).unwrap();
            assert!(jmh.colorfulness < 0.001, "neutral M = {}", jmh.colorfulness);
            let luminance = model.lightness_to_luminance(jmh.lightness).unwrap();
            assert!((luminance - gray * 100.0).abs() < 1e-5);
            let restored = model.jmh_to_rgb(jmh).unwrap();
            for channel in restored {
                assert!((channel - gray).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn saturated_colors_round_trip_in_appearance_coordinates() {
        for model in [
            Aces2Jmh::ap1_reference().unwrap(),
            Aces2Jmh::ap0_reference().unwrap(),
        ] {
            for rgb in [[0.7, 0.12, 0.04], [0.05, 0.45, 0.9], [1.5, 0.3, 0.1]] {
                let jmh = model.rgb_to_jmh(rgb).unwrap();
                let restored = model.jmh_to_rgb(jmh).unwrap();
                for (source, got) in rgb.into_iter().zip(restored) {
                    assert!((source - got).abs() < 1e-5, "{source} vs {got}");
                }
            }
        }
    }

    #[test]
    fn appearance_rejects_nonfinite_coordinates_and_handles_large_finite_input() {
        let model = Aces2Jmh::ap0_reference().unwrap();
        assert!(model.rgb_to_jmh([f64::NAN, 0.0, 0.0]).is_err());
        let large = model.rgb_to_jmh([f64::MAX; 3]).unwrap();
        assert!(large.lightness.is_finite());
        assert!(model
            .jmh_to_rgb(Jmh {
                lightness: f64::INFINITY,
                colorfulness: 0.0,
                hue_degrees: 0.0,
            })
            .is_err());
    }
}
