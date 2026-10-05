// SPDX-License-Identifier: Apache-2.0
// Copyright Contributors to the ACES Project.
//! Isolated scalar ACES 2-style 100-nit Rec.709 output candidate. This module
//! is available to isolated managed graph tests; managed sequence display and
//! export remain gated pending full-pipeline qualification.

use super::{
    encode_display, encode_video_export, Aces2Chroma, Aces2GamutCompressor, Aces2Jmh,
    Aces2ToneScale,
};
use photonic_core::timeline::color::NativeOutputTransform;
use std::sync::OnceLock;

pub struct Aces2SdrOutput {
    tone: Aces2ToneScale,
    source: Aces2Jmh,
    chroma: Aces2Chroma,
    gamut: Aces2GamutCompressor,
    limit: Aces2Jmh,
}

impl Aces2SdrOutput {
    /// Packed read-only rows for the isolated WGSL output pass. Indices are
    /// mirrored by `gpu.wgsl`; the buffer is built once per pass, never per frame.
    pub(crate) fn gpu_rows(&self) -> Vec<[f32; 4]> {
        let gamut_rows = self.gamut.gpu_hue_rows();
        let mut rows = Vec::with_capacity(20 + 362 + gamut_rows.len() * 2);
        let (tone, forward_limit, ap1_to_ap0) = self.tone.gpu_params();
        rows.extend(ap1_to_ap0);
        for matrix in self.source.forward_gpu_rows() {
            rows.extend(matrix);
        }
        for matrix in self.limit.inverse_gpu_rows() {
            rows.extend(matrix);
        }
        let source_params = self.source.appearance_gpu_params();
        let limit_params = self.limit.appearance_gpu_params();
        let chroma_params = self.chroma.gpu_params();
        let gamut_params = self.gamut.gpu_params();
        rows.push(tone);
        rows.push([
            forward_limit,
            source_params[0],
            source_params[1],
            source_params[2],
        ]);
        rows.push([
            limit_params[0],
            gamut_params[0],
            1.0 / source_params[0],
            chroma_params[0],
        ]);
        rows.push([
            chroma_params[1],
            chroma_params[2],
            chroma_params[3],
            gamut_params[1],
        ]);
        rows.push([gamut_rows.len() as f32, 0.0, 0.0, 0.0]);
        debug_assert_eq!(rows.len(), 20);
        rows.extend(
            self.chroma
                .gpu_reach_table()
                .iter()
                .map(|value| [*value as f32, 0.0, 0.0, 0.0]),
        );
        debug_assert_eq!(rows.len(), 382);
        rows.extend(gamut_rows.into_iter().flatten());
        rows
    }

    pub fn shared() -> Result<&'static Self, &'static str> {
        static INSTANCE: OnceLock<Result<Aces2SdrOutput, &'static str>> = OnceLock::new();
        INSTANCE
            .get_or_init(Self::new)
            .as_ref()
            .map_err(|error| *error)
    }

    pub fn new() -> Result<Self, &'static str> {
        Ok(Self {
            tone: Aces2ToneScale::new(100.0)?,
            source: Aces2Jmh::ap0_reference()?,
            chroma: Aces2Chroma::new(100.0)?,
            gamut: Aces2GamutCompressor::rec709_sdr()?,
            limit: Aces2Jmh::rec709_d65_reference()?,
        })
    }

    /// Straight scene-linear AP1 to straight sRGB code values. The caller
    /// handles coverage; nonlinear color mapping must precede repremultiplication.
    pub fn map_straight(&self, ap1: [f64; 3]) -> Result<[f64; 3], &'static str> {
        encode_display(
            NativeOutputTransform::SrgbSdr,
            self.map_straight_linear(ap1)?,
        )
    }

    /// Scene-to-output rendering through the 100-nit Rec.709 limiting gamut,
    /// before selecting the display or technical video transfer function.
    pub fn map_straight_linear(&self, ap1: [f64; 3]) -> Result<[f64; 3], &'static str> {
        let appearance = self.source.rgb_to_jmh(self.tone.clamp_ap1_to_ap0(ap1)?)?;
        let shaped = self.chroma.map(appearance)?;
        let mapped = self.gamut.map(shaped)?;
        self.limit.jmh_to_rgb(mapped)
    }

    /// Isolated scene-to-BT.709 export reference. This does not by itself
    /// qualify encoder range, matrix, codec metadata, or reimport behavior.
    pub fn map_straight_video(&self, ap1: [f64; 3]) -> Result<[f64; 3], &'static str> {
        encode_video_export(
            NativeOutputTransform::Bt709VideoSdr,
            self.map_straight_linear(ap1)?,
        )
    }

    pub fn map_premultiplied(&self, rgba: [f64; 4]) -> Result<[f64; 4], &'static str> {
        self.map_premultiplied_with(rgba, Self::map_straight)
    }

    /// Preserve coverage for technical BT.709 video output. RGB is
    /// premultiplied in the encoded domain; Y′CbCr packing must unpremultiply.
    pub fn map_premultiplied_video(&self, rgba: [f64; 4]) -> Result<[f64; 4], &'static str> {
        self.map_premultiplied_with(rgba, Self::map_straight_video)
    }

    fn map_premultiplied_with(
        &self,
        rgba: [f64; 4],
        transfer: fn(&Self, [f64; 3]) -> Result<[f64; 3], &'static str>,
    ) -> Result<[f64; 4], &'static str> {
        if rgba.iter().any(|value| !value.is_finite()) || !(0.0..=1.0).contains(&rgba[3]) {
            return Err("ACES 2 SDR output requires finite premultiplied RGB and alpha in 0..=1");
        }
        let alpha = rgba[3];
        if alpha == 0.0 {
            if rgba[..3].iter().any(|value| *value != 0.0) {
                return Err("transparent ACES 2 SDR pixel has nonzero premultiplied RGB");
            }
            return Ok([0.0; 4]);
        }
        let straight = [rgba[0] / alpha, rgba[1] / alpha, rgba[2] / alpha];
        let encoded = transfer(self, straight)?;
        Ok([
            encoded[0] * alpha,
            encoded[1] * alpha,
            encoded[2] * alpha,
            alpha,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_output_preserves_coverage_and_matches_neutral_reference() {
        let output = Aces2SdrOutput::shared().unwrap();
        assert!(std::ptr::eq(output, Aces2SdrOutput::shared().unwrap()));
        for gray in [0.0, 0.18, 1.0, 2.0] {
            let expected = super::super::neutral_sdr_reference([gray; 3]).unwrap();
            let got = output.map_straight([gray; 3]).unwrap();
            for channel in 0..3 {
                assert!((got[channel] - expected[channel]).abs() < 0.001);
            }
        }
        let straight = [1.0, 0.02, 0.0];
        let alpha = 0.25;
        let got = output
            .map_premultiplied([
                straight[0] * alpha,
                straight[1] * alpha,
                straight[2] * alpha,
                alpha,
            ])
            .unwrap();
        let expected = output.map_straight(straight).unwrap();
        for channel in 0..3 {
            assert!((got[channel] / alpha - expected[channel]).abs() < 1e-12);
        }
        assert_eq!(got[3], alpha);
        assert_eq!(output.map_premultiplied([0.0; 4]).unwrap(), [0.0; 4]);
        assert!(output.map_premultiplied([0.1, 0.0, 0.0, 0.0]).is_err());
    }

    #[test]
    fn technical_video_encoding_branches_after_shared_sdr_rendering() {
        let output = Aces2SdrOutput::shared().unwrap();
        for ap1 in [[0.18; 3], [1.0, 0.05, 0.02], [0.02, 0.3, 0.8]] {
            let linear = output.map_straight_linear(ap1).unwrap();
            let display = output.map_straight(ap1).unwrap();
            let video = output.map_straight_video(ap1).unwrap();
            let display_reference = encode_display(NativeOutputTransform::SrgbSdr, linear).unwrap();
            let video_reference =
                encode_video_export(NativeOutputTransform::Bt709VideoSdr, linear).unwrap();
            assert_eq!(display, display_reference);
            assert_eq!(video, video_reference);
            assert!(linear.iter().all(|value| value.is_finite()));
            assert!(video.iter().all(|value| (0.0..=1.0).contains(value)));
        }
        let gray = [0.18; 3];
        assert!(
            (output.map_straight(gray).unwrap()[0] - output.map_straight_video(gray).unwrap()[0])
                .abs()
                > 0.01
        );
        let alpha = 0.375;
        let ap1 = [0.9, 0.1, 0.03];
        let video = output
            .map_premultiplied_video([ap1[0] * alpha, ap1[1] * alpha, ap1[2] * alpha, alpha])
            .unwrap();
        let straight = output.map_straight_video(ap1).unwrap();
        assert_eq!(video[3], alpha);
        for channel in 0..3 {
            assert!((video[channel] / alpha - straight[channel]).abs() < 1e-12);
        }
        assert_eq!(output.map_premultiplied_video([0.0; 4]).unwrap(), [0.0; 4]);
        assert!(output
            .map_premultiplied_video([0.1, 0.0, 0.0, 0.0])
            .is_err());
    }

    fn assert_reference_fixture(fixture: &str, expected_count: usize, tolerance: f64) {
        let output = Aces2SdrOutput::shared().unwrap();
        let mut count = 0;
        let mut worst = (0.0, [0.0; 3], [0.0; 3], [0.0; 3], 0);
        for line in fixture.lines().filter(|line| !line.starts_with('#')) {
            let values: Vec<f64> = line
                .split_whitespace()
                .map(|value| value.parse().expect("finite reference scalar"))
                .collect();
            assert_eq!(values.len(), 6);
            let input = [values[0], values[1], values[2]];
            let reference = [values[3], values[4], values[5]];
            let got = output.map_straight(input).unwrap();
            for channel in 0..3 {
                let error = (got[channel] - reference[channel]).abs();
                if error > worst.0 {
                    worst = (error, input, got, reference, channel);
                }
            }
            count += 1;
        }
        assert_eq!(count, expected_count);
        assert!(
            worst.0 < tolerance,
            "worst error {} for source {:?} channel {}: output {:?}, reference {:?}",
            worst.0,
            worst.1,
            worst.4,
            worst.2,
            worst.3
        );
    }

    #[test]
    fn scalar_output_matches_independent_aces_two_sdr_grid() {
        // Offline float-EXR vectors from OpenColorIO-Config-ACES v4.0.0's
        // ACES 2 SDR 100-nit Rec.709 view; see fixtures and THIRD_PARTY.md.
        assert_reference_fixture(
            include_str!("../../../../../tests/fixtures/aces2_sdr_100nit_4cube.tsv"),
            64,
            0.002,
        );
    }

    #[test]
    fn scalar_output_matches_independent_corners_highlights_and_negatives() {
        assert_reference_fixture(
            include_str!("../../../../../tests/fixtures/aces2_sdr_100nit_edges.tsv"),
            32,
            0.002,
        );
    }
}
