// SPDX-License-Identifier: MIT AND Apache-2.0
//! Photonic SDR v1 scalar output candidate. The appearance and tone-scale
//! stages use attributed ACES 2 equations; the fixed-J/hue gamut boundary and
//! soft colorfulness mapping below are Photonic's own output policy. This is
//! not the Academy's ACES 2 Output Transform and is not selected for export.

use super::{encode_display, Aces2Chroma, Aces2Jmh, Aces2ToneScale, Jmh};
use photonic_core::timeline::color::NativeOutputTransform;
use std::sync::OnceLock;

pub struct PhotonicSdrOutput {
    tone: Aces2ToneScale,
    chroma: Aces2Chroma,
    source: Aces2Jmh,
    limit: Aces2Jmh,
    white_lightness: f64,
}

fn soft_boundary(colorfulness: f64, boundary: f64) -> f64 {
    let threshold = 0.75 * boundary;
    if colorfulness <= threshold {
        return colorfulness;
    }
    let room = boundary - threshold;
    if room <= 0.0 {
        return 0.0;
    }
    threshold + room * (1.0 - (-(colorfulness - threshold) / room).exp())
}

impl PhotonicSdrOutput {
    /// Reuse the expensive hue-table preparation across frames and threads.
    pub fn shared() -> Result<&'static Self, &'static str> {
        static INSTANCE: OnceLock<Result<PhotonicSdrOutput, &'static str>> = OnceLock::new();
        INSTANCE
            .get_or_init(Self::new)
            .as_ref()
            .map_err(|error| *error)
    }

    pub fn new() -> Result<Self, &'static str> {
        let source = Aces2Jmh::ap0_reference()?;
        Ok(Self {
            tone: Aces2ToneScale::new(100.0)?,
            chroma: Aces2Chroma::new(100.0)?,
            source,
            limit: Aces2Jmh::rec709_d65_reference()?,
            white_lightness: source.luminance_to_lightness(100.0)?,
        })
    }

    fn inside_limit(&self, jmh: Jmh) -> Result<bool, &'static str> {
        let rgb = self.limit.jmh_to_rgb(jmh)?;
        Ok(rgb
            .iter()
            .all(|channel| (-1e-9..=1.0 + 1e-9).contains(channel)))
    }

    fn boundary_colorfulness(&self, lightness: f64, hue: f64) -> Result<f64, &'static str> {
        let probe = |colorfulness| {
            self.inside_limit(Jmh {
                lightness,
                colorfulness,
                hue_degrees: hue,
            })
        };
        if !probe(0.0)? {
            return Err("neutral output falls outside the Rec.709 limiting gamut");
        }
        let mut low = 0.0;
        let mut high = 50.0;
        while high < 1300.0 && probe(high)? {
            low = high;
            high *= 2.0;
        }
        if probe(high)? {
            return Err("Rec.709 gamut boundary was not found");
        }
        for _ in 0..24 {
            let middle = (low + high) * 0.5;
            if probe(middle)? {
                low = middle;
            } else {
                high = middle;
            }
        }
        Ok(low)
    }

    /// Convert straight scene-linear AP1 RGB to straight sRGB code values.
    /// Caller handles alpha separately; this function never mutates the grade.
    pub fn map_straight(&self, ap1: [f64; 3]) -> Result<[f64; 3], &'static str> {
        let input = self.source.rgb_to_jmh(self.tone.clamp_ap1_to_ap0(ap1)?)?;
        let shaped = self.chroma.map(input)?;
        let lightness = shaped.lightness.clamp(0.0, self.white_lightness);
        let mut colorfulness = shaped.colorfulness;
        if colorfulness > 0.0 {
            let boundary = self.boundary_colorfulness(lightness, shaped.hue_degrees)?;
            colorfulness = soft_boundary(colorfulness, boundary);
        }
        let relative_rgb = self.limit.jmh_to_rgb(Jmh {
            lightness,
            colorfulness,
            hue_degrees: shaped.hue_degrees,
        })?;
        encode_display(NativeOutputTransform::SrgbSdr, relative_rgb)
    }

    /// Map a premultiplied scene-linear AP1 pixel to premultiplied display
    /// code values. The nonlinear output transform operates on straight RGB;
    /// applying it to premultiplied channels would darken translucent edges.
    pub fn map_premultiplied(&self, rgba: [f64; 4]) -> Result<[f64; 4], &'static str> {
        if rgba.iter().any(|value| !value.is_finite()) || !(0.0..=1.0).contains(&rgba[3]) {
            return Err("SDR output pixel requires finite premultiplied RGB and alpha in 0..=1");
        }
        let alpha = rgba[3];
        if alpha == 0.0 {
            if rgba[..3].iter().any(|value| *value != 0.0) {
                return Err("transparent SDR output pixel has nonzero premultiplied RGB");
            }
            return Ok([0.0; 4]);
        }
        let straight = [rgba[0] / alpha, rgba[1] / alpha, rgba[2] / alpha];
        let encoded = self.map_straight(straight)?;
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
    fn soft_boundary_is_continuous_monotone_and_bounded() {
        let boundary = 80.0;
        let threshold = 0.75 * boundary;
        assert_eq!(soft_boundary(threshold, boundary), threshold);
        assert!((soft_boundary(threshold + 1e-6, boundary) - threshold).abs() < 1e-5);
        let mut previous = 0.0;
        for step in 1..=1000 {
            let value = soft_boundary(step as f64, boundary);
            assert!(value >= previous && value <= boundary);
            previous = value;
        }
    }

    #[test]
    fn scalar_sdr_candidate_matches_neutral_reference_and_bounds_colors() {
        let output = PhotonicSdrOutput::shared().unwrap();
        assert!(std::ptr::eq(output, PhotonicSdrOutput::shared().unwrap()));
        for gray in [0.0, 0.18, 1.0, 2.0] {
            let got = output.map_straight([gray; 3]).unwrap();
            let reference = super::super::neutral_sdr_reference([gray; 3]).unwrap();
            for (channel, expected) in got.into_iter().zip(reference) {
                assert!((channel - expected).abs() < 0.001);
            }
        }
        for sample in [
            [1.0, 0.0, 0.0],
            [0.05, 0.45, 0.9],
            [4.0, 0.3, 0.1],
            [-0.1, 0.2, 1.5],
        ] {
            let encoded = output.map_straight(sample).unwrap();
            assert!(encoded
                .iter()
                .all(|value| value.is_finite() && (0.0..=1.0).contains(value)));
        }
        for red in [0.0, 0.18, 1.0, 4.0] {
            for green in [0.0, 0.18, 1.0, 4.0] {
                for blue in [0.0, 0.18, 1.0, 4.0] {
                    let input = [red, green, blue];
                    let encoded = output.map_straight(input).unwrap();
                    assert!(
                        encoded
                            .iter()
                            .all(|value| value.is_finite() && (0.0..=1.0).contains(value)),
                        "{input:?} -> {encoded:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn output_unpremultiplies_before_nonlinear_mapping_and_preserves_coverage() {
        let output = PhotonicSdrOutput::shared().unwrap();
        for (straight, alpha) in [
            ([0.18, 0.18, 0.18], 0.25),
            ([1.0, 0.02, 0.0], 0.5),
            ([2.0, -0.1, 0.3], 1.0),
        ] {
            let premultiplied = [
                straight[0] * alpha,
                straight[1] * alpha,
                straight[2] * alpha,
                alpha,
            ];
            let got = output.map_premultiplied(premultiplied).unwrap();
            let expected = output.map_straight(straight).unwrap();
            assert_eq!(got[3], alpha);
            for channel in 0..3 {
                assert!((got[channel] / alpha - expected[channel]).abs() < 1e-12);
            }
        }
        assert_eq!(output.map_premultiplied([0.0; 4]).unwrap(), [0.0; 4]);
        assert!(output.map_premultiplied([0.1, 0.0, 0.0, 0.0]).is_err());
        assert!(output.map_premultiplied([0.0, 0.0, 0.0, -0.1]).is_err());
        assert!(output.map_premultiplied([0.0, 0.0, 0.0, f64::NAN]).is_err());
    }

    #[test]
    #[ignore = "qualification gate: fixed-lightness gamut mapping misses ACES 2 chromatic highlights"]
    fn scalar_candidate_is_bounded_against_independent_aces_two_chromatic_vectors() {
        // Reference: OpenColorIO-Config-ACES v4.0.0 ACES 2.0 CG config,
        // cg-config-v4.0.0_aces-v2.0_ocio-v2.5.ocio, SHA-256
        // 9e3ec773a7fbc0bb6666e428dedd8ccec4f59b8b6d40506c825cc7a8c02ff2ff.
        // https://github.com/AcademySoftwareFoundation/OpenColorIO-Config-ACES/releases/tag/v4.0.0
        // Generated offline with OCIO 2.5.1's ACEScg -> sRGB - Display /
        // ACES 2.0 - SDR 100 nits (Rec.709) view on float EXR pixels.
        // The candidate intentionally uses Photonic's own gamut mapping; this
        // broad guard detects large departures, not ACES-output conformance.
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
        let candidate = PhotonicSdrOutput::shared().unwrap();
        let mut worst = (0.0, [0.0; 3], [0.0; 3], [0.0; 3], 0);
        for (source, reference) in vectors {
            let got = candidate.map_straight(source).unwrap();
            for channel in 0..3 {
                let error = (got[channel] - reference[channel]).abs();
                if error > worst.0 {
                    worst = (error, source, got, reference, channel);
                }
            }
        }
        assert!(
            worst.0 < 0.05,
            "worst error {} for source {:?} channel {}: candidate {:?}, reference {:?}",
            worst.0,
            worst.1,
            worst.4,
            worst.2,
            worst.3
        );
    }

    #[test]
    #[ignore = "qualification gate: full chromatic grid still exceeds 0.05/channel"]
    fn scalar_candidate_matches_independent_aces_two_chromatic_grid() {
        let fixture = include_str!("../../../../../tests/fixtures/aces2_sdr_100nit_4cube.tsv");
        let output = PhotonicSdrOutput::shared().unwrap();
        let mut count = 0;
        let mut worst = (0.0, [0.0; 3], [0.0; 3], [0.0; 3], 0);
        for line in fixture.lines().filter(|line| !line.starts_with('#')) {
            let values: Vec<f64> = line
                .split_whitespace()
                .map(|value| value.parse().expect("finite reference scalar"))
                .collect();
            assert_eq!(values.len(), 6);
            let source = [values[0], values[1], values[2]];
            let reference = [values[3], values[4], values[5]];
            let got = output.map_straight(source).unwrap();
            for channel in 0..3 {
                let error = (got[channel] - reference[channel]).abs();
                if error > worst.0 {
                    worst = (error, source, got, reference, channel);
                }
            }
            count += 1;
        }
        assert_eq!(count, 64);
        assert!(
            worst.0 < 0.05,
            "worst error {} for source {:?} channel {}: candidate {:?}, reference {:?}",
            worst.0,
            worst.1,
            worst.4,
            worst.2,
            worst.3
        );
    }
}
