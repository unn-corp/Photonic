// SPDX-License-Identifier: Apache-2.0
// Copyright Contributors to the ACES Project.
//! Isolated scalar ACES 2 tone scale. This is one component of the Academy's
//! output rendering transform, not a complete display or export transform.
//! Equations: <https://github.com/aces-aswf/aces-core/blob/069b0bc3e1f6c62820f19fdae2fecec3f4fc0f80/lib/Lib.Academy.Tonescale.ctl>.
//! Published numerical targets: <https://docs.acescentral.com/system-components/output-transforms/technical-details/tone-mapping/>.

mod aces2_sdr;
mod chroma;
mod gamut;
pub mod gpu;
mod jmh;
mod sdr;
pub use aces2_sdr::Aces2SdrOutput;
pub use chroma::Aces2Chroma;
pub use gamut::{Aces2GamutCompressor, Aces2GamutCusp, DisplayCusp};
pub use jmh::{Aces2Jmh, Jmh};
pub use sdr::PhotonicSdrOutput;

use crate::color::native::{rgb_to_xyz_from_xy, Matrix3};
use photonic_core::timeline::color::NativeOutputTransform;

/// Final encoding stage for the currently declared native SDR display/export
/// target. Input channels are linear Rec.709/D65 luminance relative to 100 nits.
/// The rendering transform must have supplied those channels first; this
/// function alone cannot turn scene-referred AP1 into a display image.
pub fn encode_display(
    selection: NativeOutputTransform,
    relative_luminance_rgb: [f64; 3],
) -> Result<[f64; 3], &'static str> {
    if relative_luminance_rgb
        .iter()
        .any(|channel| !channel.is_finite())
    {
        return Err("native display input must be finite");
    }
    match selection {
        NativeOutputTransform::SrgbSdr => Ok(relative_luminance_rgb
            .map(|channel| crate::color::native::encode_srgb(channel.clamp(0.0, 1.0)))),
        NativeOutputTransform::Bt709VideoSdr => {
            Err("BT.709 video export transform cannot be used for display")
        }
    }
}

/// Encode the already-rendered 100-nit Rec.709/D65 linear output as a BT.709
/// video signal. Matrix, range and bit-depth packing follow this stage in the
/// encoder. Scene-linear AP1 values must first pass the output rendering
/// transform; this function is not itself a tone mapper.
pub fn encode_video_export(
    selection: NativeOutputTransform,
    relative_luminance_rgb: [f64; 3],
) -> Result<[f64; 3], &'static str> {
    if relative_luminance_rgb
        .iter()
        .any(|channel| !channel.is_finite())
    {
        return Err("native video export input must be finite");
    }
    match selection {
        NativeOutputTransform::Bt709VideoSdr => Ok(relative_luminance_rgb.map(|channel| {
            let value = channel.clamp(0.0, 1.0);
            if value <= 0.018 {
                4.5 * value
            } else {
                1.099 * value.powf(0.45) - 0.099
            }
        })),
        NativeOutputTransform::SrgbSdr => {
            Err("sRGB display transform cannot be used as a BT.709 video export transform")
        }
    }
}

/// A narrow neutral-only reference for the 100-nit sRGB output preset. It
/// traverses the source appearance, tone scale, limiting appearance and
/// display-encoding stages. Chroma/gamut compression is omitted because
/// colorfulness is zero; colored pixels must use the full qualified path.
pub fn neutral_sdr_reference(ap1: [f64; 3]) -> Result<[f64; 3], &'static str> {
    if ap1.iter().any(|channel| !channel.is_finite())
        || (ap1[0] - ap1[1]).abs() > 1e-12
        || (ap1[1] - ap1[2]).abs() > 1e-12
    {
        return Err("neutral SDR reference requires finite equal AP1 channels");
    }
    let tone = Aces2ToneScale::new(100.0)?;
    let source = Aces2Jmh::ap0_reference()?;
    let limit = Aces2Jmh::rec709_d65_reference()?;
    let jmh = source.rgb_to_jmh(tone.clamp_ap1_to_ap0(ap1)?)?;
    let linear = source.lightness_to_luminance(jmh.lightness)? / 100.0;
    let output_nits = tone.map_lightness(linear)?;
    let mapped_j = source.luminance_to_lightness(output_nits)?;
    let relative_output = limit.jmh_to_rgb(Jmh {
        lightness: mapped_j,
        colorfulness: 0.0,
        hue_degrees: 0.0,
    })?;
    encode_display(NativeOutputTransform::SrgbSdr, relative_output)
}

/// Precomputed tone-scale parameters for a target display peak. Values are
/// f64 here so this module can serve as a CPU reference for a later WGSL pass.
#[derive(Clone, Copy, Debug)]
pub struct Aces2ToneScale {
    peak_nits: f64,
    contrast: f64,
    toe: f64,
    shoulder_scale: f64,
    shoulder_offset: f64,
    forward_limit: f64,
    ap1_to_ap0: Matrix3,
}

impl Aces2ToneScale {
    pub(crate) fn gpu_params(&self) -> ([f32; 4], f32, [[f32; 4]; 3]) {
        (
            [
                self.shoulder_scale as f32,
                self.shoulder_offset as f32,
                self.contrast as f32,
                self.toe as f32,
            ],
            self.forward_limit as f32,
            self.ap1_to_ap0
                .0
                .map(|row| [row[0] as f32, row[1] as f32, row[2] as f32, 0.0]),
        )
    }

    /// ACES 2 preset targets span 48-nit cinema through 10,000-nit HDR.
    pub fn new(peak_nits: f64) -> Result<Self, &'static str> {
        if !peak_nits.is_finite() || !(48.0..=10_000.0).contains(&peak_nits) {
            return Err("ACES 2 peak luminance must be finite and within 48..=10000 nits");
        }
        let normalized_peak = peak_nits / 100.0;
        let contrast = 1.15;
        let toe = 0.04;
        let roof = 128.0 + 768.0 * (normalized_peak.ln() / 100.0_f64.ln());
        let m1 = 0.5 * (normalized_peak + (normalized_peak * (normalized_peak + 4.0 * toe)).sqrt());
        let roof_ratio = roof / m1;
        let u = (roof_ratio / (roof_ratio + 1.0)).powf(contrast);
        let m = m1 / u;
        let gray_target = 0.10013 * (1.0 + normalized_peak.log2() * 0.14);
        let gray_intermediate =
            0.5 * (gray_target + (gray_target * (gray_target + 4.0 * toe)).sqrt());
        let ratio = (gray_intermediate / m).powf(1.0 / contrast);
        let gray_roof = -m1 * ratio / (ratio - 1.0);
        let weight = 0.18 / gray_roof;
        let shoulder_offset = weight * m1;
        let u2 = (roof_ratio / (roof_ratio + weight)).powf(contrast);
        let shoulder_scale = m1 / u2;
        let forward_limit = 8.0 * roof;
        let ap1_xyz = rgb_to_xyz_from_xy([
            [0.713, 0.293],
            [0.165, 0.830],
            [0.128, 0.044],
            [0.32168, 0.33767],
        ]);
        let ap0_xyz = rgb_to_xyz_from_xy([
            [0.73470, 0.26530],
            [0.00000, 1.00000],
            [0.00010, -0.07700],
            [0.32168, 0.33767],
        ]);
        let ap1_to_ap0 = ap0_xyz.inverse()?.product(ap1_xyz);
        if !shoulder_offset.is_finite() || shoulder_offset <= 0.0 || !shoulder_scale.is_finite() {
            return Err("ACES 2 tone scale parameters are invalid");
        }
        Ok(Self {
            peak_nits,
            contrast,
            toe,
            shoulder_scale,
            shoulder_offset,
            forward_limit,
            ap1_to_ap0,
        })
    }

    pub fn peak_nits(&self) -> f64 {
        self.peak_nits
    }

    /// Output-stage input clamp: AP1 working RGB is bounded before conversion
    /// into AP0 appearance-model coordinates. This is a named rendering step,
    /// not a clamp in the creative grade or scene-linear compositor.
    pub fn clamp_ap1_to_ap0(&self, ap1: [f64; 3]) -> Result<[f64; 3], &'static str> {
        if ap1.iter().any(|value| !value.is_finite()) {
            return Err("ACES 2 output input must be finite");
        }
        Ok(self
            .ap1_to_ap0
            .transform(ap1.map(|value| value.clamp(0.0, self.forward_limit))))
    }

    /// Map one scene-referred lightness value to output luminance in nits.
    /// The Academy curve's toe maps negative values to black at this stage;
    /// negative RGB channels must be handled by the full JMh/gamut pipeline.
    pub fn map_lightness(&self, scene_lightness: f64) -> Result<f64, &'static str> {
        if !scene_lightness.is_finite() {
            return Err("ACES 2 tone scale input must be finite");
        }
        let lightness = scene_lightness.max(0.0);
        let michaelis = self.shoulder_scale
            * (lightness / (lightness + self.shoulder_offset)).powf(self.contrast);
        let output = michaelis * michaelis / (michaelis + self.toe) * 100.0;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_aces_two_tone_scale_vectors() {
        // Academy tone-mapping documentation, table dated 2025-09-10.
        let vectors = [
            (100.0, [0.0, 10.000, 45.757, 63.988]),
            (500.0, [0.0, 13.193, 89.098, 158.949]),
            (1_000.0, [0.0, 14.512, 106.564, 205.783]),
            (2_000.0, [0.0, 15.747, 121.664, 248.779]),
            (4_000.0, [0.0, 16.824, 133.883, 284.433]),
        ];
        for (peak, expected) in vectors {
            let tone = Aces2ToneScale::new(peak).unwrap();
            for (input, expected_nits) in [0.0, 0.18, 1.0, 2.0].into_iter().zip(expected) {
                let got = tone.map_lightness(input).unwrap();
                assert!(
                    (got - expected_nits).abs() < 0.05,
                    "peak {peak}, lightness {input}: {got} vs {expected_nits} nits"
                );
            }
        }
    }

    #[test]
    fn tone_scale_is_monotone_and_rejects_invalid_parameters() {
        let tone = Aces2ToneScale::new(100.0).unwrap();
        assert_eq!(tone.peak_nits(), 100.0);
        assert_eq!(tone.map_lightness(-0.5).unwrap(), 0.0);
        let mut previous = 0.0;
        for step in 1..=4096 {
            let output = tone.map_lightness(step as f64 / 32.0).unwrap();
            assert!(output > previous);
            previous = output;
        }
        assert!(tone.map_lightness(f64::NAN).is_err());
        assert!(Aces2ToneScale::new(0.0).is_err());
        assert!(Aces2ToneScale::new(f64::INFINITY).is_err());
    }

    #[test]
    fn neutral_ap1_output_input_reaches_published_sdr_gray() {
        let tone = Aces2ToneScale::new(100.0).unwrap();
        let appearance = Aces2Jmh::ap0_reference().unwrap();
        let ap0 = tone.clamp_ap1_to_ap0([0.18; 3]).unwrap();
        let jmh = appearance.rgb_to_jmh(ap0).unwrap();
        let scene_lightness = appearance.lightness_to_luminance(jmh.lightness).unwrap() / 100.0;
        assert!((scene_lightness - 0.18).abs() < 1e-8);
        assert!((tone.map_lightness(scene_lightness).unwrap() - 10.0).abs() < 0.05);
        let below_black = tone.clamp_ap1_to_ap0([-1.0; 3]).unwrap();
        assert!(below_black.iter().all(|channel| channel.abs() < 1e-10));
        let above_white = tone.clamp_ap1_to_ap0([16.0; 3]).unwrap();
        assert!(above_white
            .iter()
            .all(|channel| (*channel - 16.0).abs() < 1e-8));
        assert!(tone.clamp_ap1_to_ap0([f64::NAN; 3]).is_err());
    }

    #[test]
    fn sdr_display_encoding_is_separate_and_bounded() {
        let encoded =
            encode_display(NativeOutputTransform::SrgbSdr, [0.003_130_8, 0.18, 1.0]).unwrap();
        assert!((encoded[0] - 0.04045).abs() < 0.00001);
        assert!((encoded[1] - 0.461_356_129_500_441_64).abs() < 1e-12);
        assert!((encoded[2] - 1.0).abs() < 1e-12);
        let bounds = encode_display(NativeOutputTransform::SrgbSdr, [-0.1, 2.0, 0.0]).unwrap();
        assert_eq!(bounds[0], 0.0);
        assert!((bounds[1] - 1.0).abs() < 1e-12);
        assert_eq!(bounds[2], 0.0);
        assert!(encode_display(NativeOutputTransform::SrgbSdr, [f64::NAN; 3]).is_err());
    }

    #[test]
    fn bt709_export_encoding_is_distinct_from_srgb_display() {
        let input = [0.0, 0.18, 1.0];
        let video = encode_video_export(NativeOutputTransform::Bt709VideoSdr, input).unwrap();
        let display = encode_display(NativeOutputTransform::SrgbSdr, input).unwrap();
        assert_eq!(video[0], 0.0);
        assert_eq!(video[2], 1.0);
        assert!((video[1] - (1.099 * 0.18_f64.powf(0.45) - 0.099)).abs() < 1e-12);
        assert!((video[1] - display[1]).abs() > 0.01);
        assert!(encode_video_export(NativeOutputTransform::SrgbSdr, input).is_err());
        assert!(encode_display(NativeOutputTransform::Bt709VideoSdr, input).is_err());
        assert!(encode_video_export(NativeOutputTransform::Bt709VideoSdr, [f64::NAN; 3]).is_err());
    }

    #[test]
    fn neutral_sdr_chain_follows_published_tone_scale() {
        let tone = Aces2ToneScale::new(100.0).unwrap();
        for gray in [0.0, 0.18, 1.0, 2.0] {
            let got = neutral_sdr_reference([gray; 3]).unwrap();
            let expected_relative = tone.map_lightness(gray).unwrap() / 100.0;
            let expected = crate::color::native::encode_srgb(expected_relative);
            for channel in got {
                assert!(
                    (channel - expected).abs() < 0.001,
                    "gray {gray}: {channel} vs {expected}"
                );
            }
        }
        assert!(neutral_sdr_reference([0.5, 0.5, 0.4]).is_err());
    }
}
