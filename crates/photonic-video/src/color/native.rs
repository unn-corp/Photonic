//! Scalar reference for colorimetric conversion into AP1 primaries.
//!
//! Matrices are derived from published chromaticities, not copied from a
//! third-party implementation. Bradford adapts D65 input white to ACES white.
//! Display-referred sRGB converted into linear AP1 remains display-referred.
//! This is not an ACES camera input, inverse rendering, or output transform.
//! Primaries: https://docs.acescentral.com/encodings/acescg/ . sRGB transfer:
//! https://www.w3.org/TR/css-color-4/#predefined-sRGB .
//! Source video coefficients: https://www.itu.int/rec/R-REC-BT.601 ,
//! https://www.itu.int/rec/R-REC-BT.709 and
//! https://www.itu.int/rec/R-REC-BT.2020 .

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix3(pub [[f64; 3]; 3]);

impl Matrix3 {
    pub fn transform(self, v: [f64; 3]) -> [f64; 3] {
        self.0
            .map(|row| row[0].mul_add(v[0], row[1].mul_add(v[1], row[2] * v[2])))
    }

    pub fn product(self, rhs: Self) -> Self {
        Self(std::array::from_fn(|r| {
            std::array::from_fn(|c| {
                self.0[r][0].mul_add(
                    rhs.0[0][c],
                    self.0[r][1].mul_add(rhs.0[1][c], self.0[r][2] * rhs.0[2][c]),
                )
            })
        }))
    }

    pub fn inverse(self) -> Result<Self, &'static str> {
        let m = self.0;
        let cof = [
            [
                m[1][1] * m[2][2] - m[1][2] * m[2][1],
                m[0][2] * m[2][1] - m[0][1] * m[2][2],
                m[0][1] * m[1][2] - m[0][2] * m[1][1],
            ],
            [
                m[1][2] * m[2][0] - m[1][0] * m[2][2],
                m[0][0] * m[2][2] - m[0][2] * m[2][0],
                m[0][2] * m[1][0] - m[0][0] * m[1][2],
            ],
            [
                m[1][0] * m[2][1] - m[1][1] * m[2][0],
                m[0][1] * m[2][0] - m[0][0] * m[2][1],
                m[0][0] * m[1][1] - m[0][1] * m[1][0],
            ],
        ];
        let det = m[0][0].mul_add(cof[0][0], m[0][1].mul_add(cof[1][0], m[0][2] * cof[2][0]));
        if !det.is_finite() || det.abs() < 1e-15 {
            return Err("color matrix is singular or non-finite");
        }
        Ok(Self(cof.map(|row| row.map(|value| value / det))))
    }
}

#[derive(Clone, Copy)]
struct Chromaticity {
    x: f64,
    y: f64,
}

impl Chromaticity {
    fn xyz(self) -> [f64; 3] {
        [self.x / self.y, 1.0, (1.0 - self.x - self.y) / self.y]
    }
}

#[derive(Clone, Copy)]
struct Primaries {
    r: Chromaticity,
    g: Chromaticity,
    b: Chromaticity,
    white: Chromaticity,
}

impl Primaries {
    fn to_xyz(self) -> Matrix3 {
        let [r, g, b] = [self.r.xyz(), self.g.xyz(), self.b.xyz()];
        let basis = Matrix3([[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]]);
        let scale = basis
            .inverse()
            .expect("valid standard primaries")
            .transform(self.white.xyz());
        Matrix3(std::array::from_fn(|row| {
            std::array::from_fn(|col| basis.0[row][col] * scale[col])
        }))
    }
}

/// Internal matrix constructor used by the ACES 2 appearance model. Entries
/// are red, green, blue and white xy chromaticities in that order.
pub(crate) fn rgb_to_xyz_from_xy(xy: [[f64; 2]; 4]) -> Matrix3 {
    let [r, g, b, white] = xy.map(|pair| Chromaticity {
        x: pair[0],
        y: pair[1],
    });
    Primaries { r, g, b, white }.to_xyz()
}

const SRGB: Primaries = Primaries {
    r: Chromaticity { x: 0.64, y: 0.33 },
    g: Chromaticity { x: 0.30, y: 0.60 },
    b: Chromaticity { x: 0.15, y: 0.06 },
    white: Chromaticity {
        x: 0.3127,
        y: 0.3290,
    },
};
const BT2020: Primaries = Primaries {
    r: Chromaticity { x: 0.708, y: 0.292 },
    g: Chromaticity { x: 0.170, y: 0.797 },
    b: Chromaticity { x: 0.131, y: 0.046 },
    white: Chromaticity {
        x: 0.3127,
        y: 0.3290,
    },
};
const AP1: Primaries = Primaries {
    r: Chromaticity { x: 0.713, y: 0.293 },
    g: Chromaticity { x: 0.165, y: 0.830 },
    b: Chromaticity { x: 0.128, y: 0.044 },
    white: Chromaticity {
        x: 0.32168,
        y: 0.33767,
    },
};

const BRADFORD: Matrix3 = Matrix3([
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
]);

fn adapt_white(source: Chromaticity, target: Chromaticity) -> Matrix3 {
    let src = BRADFORD.transform(source.xyz());
    let dst = BRADFORD.transform(target.xyz());
    let scale = Matrix3([
        [dst[0] / src[0], 0.0, 0.0],
        [0.0, dst[1] / src[1], 0.0],
        [0.0, 0.0, dst[2] / src[2]],
    ]);
    BRADFORD
        .inverse()
        .expect("Bradford matrix invertible")
        .product(scale)
        .product(BRADFORD)
}

/// Linear-light sRGB/D65 to AP1/ACES-white primaries. This changes gamut and
/// white point, not the signal's scene/display-referred meaning.
pub fn linear_srgb_to_ap1() -> Matrix3 {
    AP1.to_xyz()
        .inverse()
        .expect("AP1 matrix invertible")
        .product(adapt_white(SRGB.white, AP1.white))
        .product(SRGB.to_xyz())
}

/// Inverse colorimetric transform for numerical qualification.
pub fn ap1_to_linear_srgb() -> Matrix3 {
    linear_srgb_to_ap1()
        .inverse()
        .expect("color matrix invertible")
}

/// Linear BT.2020/D65 to AP1/ACES-white primaries. Transfer decoding must
/// precede this conversion; scene/display interpretation remains unchanged.
pub fn linear_bt2020_to_ap1() -> Matrix3 {
    AP1.to_xyz()
        .inverse()
        .expect("AP1 matrix invertible")
        .product(adapt_white(BT2020.white, AP1.white))
        .product(BT2020.to_xyz())
}

/// Signed sRGB transfer extension for floating source values. Its positive
/// branch follows IEC sRGB; the odd extension preserves negative values.
pub fn decode_srgb(encoded: f64) -> f64 {
    let magnitude = encoded.abs();
    if magnitude <= 0.04045 {
        encoded / 12.92
    } else {
        encoded.signum() * ((magnitude + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse BT.709 source OETF, extended symmetrically outside its specified
/// 0–1 interval so decoding does not discard source excursions. Production
/// camera looks can differ from this reference curve and require their own IDT.
pub fn decode_bt709_source(encoded: f64) -> f64 {
    let magnitude = encoded.abs();
    if magnitude <= 0.081 {
        encoded / 4.5
    } else {
        encoded.signum() * ((magnitude + 0.099) / 1.099).powf(1.0 / 0.45)
    }
}

/// Inverse BT.2020 source OETF using the continuous coefficients specified by
/// BT.2020-2. Like BT.709, this does not identify a camera-specific look.
pub fn decode_bt2020_source(encoded: f64) -> f64 {
    const ALPHA: f64 = 1.099_296_826_809_44;
    const BETA: f64 = 0.018_053_968_510_807;
    let magnitude = encoded.abs();
    if magnitude <= 4.5 * BETA {
        encoded / 4.5
    } else {
        encoded.signum() * ((magnitude + ALPHA - 1.0) / ALPHA).powf(1.0 / 0.45)
    }
}

/// Inverse signed extension of the sRGB transfer curve for display-referred
/// linear values. This is not an ACES output rendering transform.
pub fn encode_srgb(linear: f64) -> f64 {
    let magnitude = linear.abs();
    if magnitude <= 0.003_130_8 {
        linear * 12.92
    } else {
        linear.signum() * (1.055 * magnitude.powf(1.0 / 2.4) - 0.055)
    }
}

pub fn encoded_srgb_to_linear_ap1_display(rgb: [f64; 3]) -> [f64; 3] {
    linear_srgb_to_ap1().transform(rgb.map(decode_srgb))
}

/// Reverses the display-referred gamut conversion above. An actual ACEScg
/// scene image needs a qualified output rendering transform before this step.
pub fn linear_ap1_display_to_encoded_srgb(rgb: [f64; 3]) -> [f64; 3] {
    ap1_to_linear_srgb().transform(rgb).map(encode_srgb)
}

/// ACEScct and ACEScg share AP1 primaries and the ACES white point. This
/// transfer-only conversion is valid for a source explicitly tagged ACEScct.
pub fn acescct_to_acescg(rgb: [f64; 3]) -> [f64; 3] {
    rgb.map(super::acescct::decode)
}

pub fn acescg_to_acescct(rgb: [f64; 3]) -> [f64; 3] {
    rgb.map(super::acescct::encode)
}

/// Exposure is a scene-linear operation; alpha is kept separate by callers.
pub fn expose_acescg(rgb: [f64; 3], stops: f64) -> [f64; 3] {
    let scale = stops.exp2();
    rgb.map(|channel| channel * scale)
}

/// Non-constant-luminance source matrix. The returned RGB remains nonlinear;
/// a separately selected source transfer must be decoded afterward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YcbcrMatrix {
    Bt601,
    Bt709,
    Bt2020NonConstant,
}

/// Decode integer Y′CbCr source codes without clipping nominal-range
/// excursions. Chroma reconstruction and transfer interpretation are separate.
/// BT.709/BT.2020 coefficients and studio code ranges follow ITU-R BT.709-6
/// and BT.2020-2. BT.601 coefficients follow ITU-R BT.601-7.
pub fn decode_ycbcr_codes(
    codes: [u16; 3],
    bit_depth: u8,
    limited_range: bool,
    matrix: YcbcrMatrix,
) -> Result<[f64; 3], &'static str> {
    if !(8..=16).contains(&bit_depth) {
        return Err("Y′CbCr bit depth must be between 8 and 16");
    }
    let max = (1_u32 << bit_depth) - 1;
    if codes.iter().any(|&code| u32::from(code) > max) {
        return Err("Y′CbCr code exceeds its bit depth");
    }
    let scale = (1_u32 << (bit_depth - 8)) as f64;
    let (y, cb, cr) = if limited_range {
        (
            (f64::from(codes[0]) - 16.0 * scale) / (219.0 * scale),
            (f64::from(codes[1]) - 128.0 * scale) / (224.0 * scale),
            (f64::from(codes[2]) - 128.0 * scale) / (224.0 * scale),
        )
    } else {
        (
            f64::from(codes[0]) / f64::from(max),
            (f64::from(codes[1]) - f64::from(1_u32 << (bit_depth - 1))) / f64::from(max),
            (f64::from(codes[2]) - f64::from(1_u32 << (bit_depth - 1))) / f64::from(max),
        )
    };
    let (kr, kb) = match matrix {
        YcbcrMatrix::Bt601 => (0.299, 0.114),
        YcbcrMatrix::Bt709 => (0.2126, 0.0722),
        YcbcrMatrix::Bt2020NonConstant => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = (y - kr * r - kb * b) / kg;
    Ok([r, g, b])
}

/// Explicit source standard for the native scalar reference. BT.601 is not
/// included here because its matrix tag alone does not identify its primaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeVideoInput {
    Bt2100HlgScene {
        reference_white_nits: u32,
        display_peak_nits: u32,
    },
    Bt2100PqDisplay {
        reference_white_nits: u32,
    },
    Bt709,
    Bt2020NonConstant,
}

/// Compose code-range, Y′CbCr, source-transfer, and gamut steps into a
/// scene-linear AP1 reference sample. This assumes source encoding follows the
/// named broadcast OETF; camera-specific looks and ACES IDTs need separate
/// qualification. Alpha/chroma siting are handled outside this scalar path.
pub fn decode_video_sample_to_ap1(
    codes: [u16; 3],
    bit_depth: u8,
    limited_range: bool,
    input: NativeVideoInput,
) -> Result<[f64; 3], &'static str> {
    if let NativeVideoInput::Bt2100PqDisplay {
        reference_white_nits,
    } = input
    {
        if !(1..=10000).contains(&reference_white_nits) {
            return Err("invalid PQ reference white");
        }
    }
    if let NativeVideoInput::Bt2100HlgScene {
        reference_white_nits,
        display_peak_nits,
    } = input
    {
        hlg_scene_normalization(reference_white_nits, display_peak_nits)?;
    }
    let matrix = match input {
        NativeVideoInput::Bt709 => YcbcrMatrix::Bt709,
        NativeVideoInput::Bt2020NonConstant
        | NativeVideoInput::Bt2100PqDisplay { .. }
        | NativeVideoInput::Bt2100HlgScene { .. } => YcbcrMatrix::Bt2020NonConstant,
    };
    let nonlinear = decode_ycbcr_codes(codes, bit_depth, limited_range, matrix)?;
    let (linear, gamut) = match input {
        NativeVideoInput::Bt2100HlgScene {
            reference_white_nits,
            display_peak_nits,
        } => {
            let scale = hlg_scene_normalization(reference_white_nits, display_peak_nits)?;
            (
                nonlinear.map(|v| decode_hlg_scene(v) * scale),
                linear_bt2020_to_ap1(),
            )
        }
        NativeVideoInput::Bt2100PqDisplay {
            reference_white_nits,
        } => (
            nonlinear.map(|v| decode_pq_nits(v) / f64::from(reference_white_nits)),
            linear_bt2020_to_ap1(),
        ),
        NativeVideoInput::Bt709 => (nonlinear.map(decode_bt709_source), linear_srgb_to_ap1()),
        NativeVideoInput::Bt2020NonConstant => {
            (nonlinear.map(decode_bt2020_source), linear_bt2020_to_ap1())
        }
    };
    Ok(gamut.transform(linear))
}

/// Absolute PQ EOTF (ITU-R BT.2100-3). Excursions clip to the defined code domain.
/// https://www.itu.int/dms_pubrec/itu-r/rec/bt/R-REC-BT.2100-3-202502-I!!PDF-E.pdf
pub fn decode_pq_nits(code: f64) -> f64 {
    let n = code.clamp(0.0, 1.0).powf(32.0 / 2523.0);
    10000.0
        * ((n - 3424.0 / 4096.0).max(0.0) / (2413.0 / 128.0 - 2392.0 / 128.0 * n))
            .powf(16384.0 / 2610.0)
}

/// HLG inverse OETF scene light. The signed odd extension preserves production excursions.
pub fn decode_hlg_scene(code: f64) -> f64 {
    let a: f64 = 0.17883277;
    let b = 1.0 - 4.0 * a;
    let c = 0.5 - a * (4.0 * a).ln();
    let v = code.abs();
    let scene = if v <= 0.5 {
        v * v / 3.0
    } else {
        (((v - c) / a).exp() + b) / 12.0
    };
    code.signum() * scene
}
/// Neutral white anchor at zero reference black. Gamma sets the exposure scale,
/// not a per-channel power or display OOTF; HLG remains scene-referred in ACEScg.
/// ITU-R BT.2100-3 Table 5 and Note 5f.
pub fn hlg_scene_normalization(white: u32, peak: u32) -> Result<f64, &'static str> {
    if !(400..=2000).contains(&peak) || !(1..=peak).contains(&white) {
        return Err("invalid HLG white/peak normalization");
    }
    let gamma = 1.2 + 0.42 * (f64::from(peak) / 1000.0).log10();
    Ok((f64::from(white) / f64::from(peak)).powf(-1.0 / gamma))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primaries_and_white_are_consistent() {
        let matrix = linear_srgb_to_ap1();
        let white = matrix.transform([1.0; 3]);
        assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-12));
        let rgb = [0.2, -0.04, 12.0];
        let back = ap1_to_linear_srgb().transform(matrix.transform(rgb));
        for i in 0..3 {
            assert!((back[i] - rgb[i]).abs() < 1e-11);
        }
        let matrix = linear_bt2020_to_ap1();
        let white = matrix.transform([1.0; 3]);
        assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-12));
        let back = matrix.inverse().unwrap().transform(matrix.transform(rgb));
        for i in 0..3 {
            assert!((back[i] - rgb[i]).abs() < 1e-11);
        }
    }

    #[test]
    fn srgb_decode_preserves_extended_range() {
        assert_eq!(decode_srgb(0.0), 0.0);
        assert_eq!(decode_srgb(1.0), 1.0);
        assert!((decode_srgb(0.04045) - 0.04045 / 12.92).abs() < 1e-12);
        assert_eq!(decode_srgb(-0.5), -decode_srgb(0.5));
        assert!(decode_srgb(2.0) > 1.0);
        let white = encoded_srgb_to_linear_ap1_display([1.0; 3]);
        assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-12));
        for encoded in [-0.5, -0.04045, 0.0, 0.04045, 0.5, 1.0, 2.0] {
            // The published rounded branch thresholds have a tiny mismatch.
            assert!((encode_srgb(decode_srgb(encoded)) - encoded).abs() < 4e-8);
        }
        let encoded = [2.0, -0.25, 0.7];
        let recovered =
            linear_ap1_display_to_encoded_srgb(encoded_srgb_to_linear_ap1_display(encoded));
        for i in 0..3 {
            assert!((recovered[i] - encoded[i]).abs() < 1e-11);
        }
    }

    #[test]
    fn acescct_scene_transfer_and_exposure_preserve_extended_values() {
        let scene = [-0.1, 0.18, 8.0];
        let log = acescg_to_acescct(scene);
        let recovered = acescct_to_acescg(log);
        for i in 0..3 {
            assert!((recovered[i] - scene[i]).abs() < 1e-10);
        }
        assert_eq!(expose_acescg(scene, 1.0), [-0.2, 0.36, 16.0]);
    }

    #[test]
    fn ycbcr_code_ranges_and_matrices() {
        for (depth, black, neutral, white) in [
            (8, 16, 128, 235),
            (10, 64, 512, 940),
            (16, 4096, 32768, 60160),
        ] {
            for matrix in [
                YcbcrMatrix::Bt601,
                YcbcrMatrix::Bt709,
                YcbcrMatrix::Bt2020NonConstant,
            ] {
                let rgb =
                    decode_ycbcr_codes([black, neutral, neutral], depth, true, matrix).unwrap();
                assert!(rgb.iter().all(|value| value.abs() < 1e-12));
                let rgb =
                    decode_ycbcr_codes([white, neutral, neutral], depth, true, matrix).unwrap();
                assert!(rgb.iter().all(|value| (value - 1.0).abs() < 1e-12));
            }
        }
        let below_black = decode_ycbcr_codes([0, 128, 128], 8, true, YcbcrMatrix::Bt709).unwrap();
        assert!(below_black.iter().all(|value| *value < 0.0));
        let above_white = decode_ycbcr_codes([255, 128, 128], 8, true, YcbcrMatrix::Bt709).unwrap();
        assert!(above_white.iter().all(|value| *value > 1.0));
        let rgb = decode_ycbcr_codes([128, 128, 128], 8, false, YcbcrMatrix::Bt709).unwrap();
        assert!(rgb
            .iter()
            .all(|value| (value - 128.0 / 255.0).abs() < 1e-12));
        // Rounded BT.709 legal-range codes for a saturated red patch.
        let red = decode_ycbcr_codes([63, 102, 240], 8, true, YcbcrMatrix::Bt709).unwrap();
        assert!((red[0] - 1.0).abs() < 0.01);
        assert!(red[1].abs() < 0.01);
        assert!(red[2].abs() < 0.01);
        let wrong_matrix = decode_ycbcr_codes([63, 102, 240], 8, true, YcbcrMatrix::Bt601).unwrap();
        assert!((wrong_matrix[1] - red[1]).abs() > 0.05);
        assert!(decode_ycbcr_codes([256, 128, 128], 8, false, YcbcrMatrix::Bt709).is_err());
        assert!(decode_ycbcr_codes([0, 0, 0], 7, false, YcbcrMatrix::Bt709).is_err());
    }

    #[test]
    fn bt709_source_transfer_preserves_excursions() {
        assert_eq!(decode_bt709_source(0.0), 0.0);
        assert_eq!(decode_bt709_source(1.0), 1.0);
        assert!((decode_bt709_source(0.081) - 0.018).abs() < 1e-6);
        assert_eq!(decode_bt709_source(-0.5), -decode_bt709_source(0.5));
        assert!(decode_bt709_source(1.2) > 1.0);
        assert_eq!(decode_bt2020_source(0.0), 0.0);
        assert!((decode_bt2020_source(1.0) - 1.0).abs() < 1e-12);
        assert_eq!(decode_bt2020_source(-0.5), -decode_bt2020_source(0.5));
        assert!(decode_bt2020_source(1.2) > 1.0);
    }

    #[test]
    fn explicit_video_input_reference_composes_source_steps() {
        for input in [NativeVideoInput::Bt709, NativeVideoInput::Bt2020NonConstant] {
            let black = decode_video_sample_to_ap1([16, 128, 128], 8, true, input).unwrap();
            assert!(black.iter().all(|value| value.abs() < 1e-12));
            let white = decode_video_sample_to_ap1([235, 128, 128], 8, true, input).unwrap();
            assert!(white.iter().all(|value| (value - 1.0).abs() < 1e-12));
            let highlight = decode_video_sample_to_ap1([255, 128, 128], 8, true, input).unwrap();
            assert!(highlight.iter().all(|value| *value > 1.0));
            let shadow = decode_video_sample_to_ap1([0, 128, 128], 8, true, input).unwrap();
            assert!(shadow.iter().all(|value| *value < 0.0));
        }
        assert!(
            decode_video_sample_to_ap1([1024, 512, 512], 10, true, NativeVideoInput::Bt709)
                .is_err()
        );
    }
}
