//! Explicit Planckian-white chromatic adaptation, separate from artistic WB.
//!
//! Kelvin→xy approximation: Kang et al. (2002), JKPS41(6),865–871.
//! Reference and numerical example:
//! https://colour.readthedocs.io/en/latest/generated/colour.temperature.CCT_to_xy_Kang2002.html
//! Coefficients: https://colour.readthedocs.io/en/latest/_modules/colour/temperature/kang2002.html
//! This independently implemented polynomial/matrix calculation is not a camera
//! IDT, spectral reconstruction, illuminant estimator or daylight-white model.

use glam::{DMat3, DVec3};

pub const MIN_KELVIN: f32 = 1667.0;
pub const MAX_KELVIN: f32 = 25000.0;

pub fn planckian_xy(kelvin: f64) -> Result<[f64; 2], &'static str> {
    if !kelvin.is_finite() || !(f64::from(MIN_KELVIN)..=f64::from(MAX_KELVIN)).contains(&kelvin) {
        return Err("Kelvin white balance requires finite temperatures within 1667..=25000 K");
    }
    let reciprocal = 1.0 / kelvin;
    let x = if kelvin <= 4000.0 {
        ((-266123900.0 * reciprocal - 234358.9) * reciprocal + 877.6956) * reciprocal + 0.179910
    } else {
        ((-3025846900.0 * reciprocal + 2107037.9) * reciprocal + 222.6347) * reciprocal + 0.240390
    };
    let y = if kelvin <= 2222.0 {
        ((-1.1063814 * x - 1.34811020) * x + 2.18555832) * x - 0.20219683
    } else if kelvin <= 4000.0 {
        ((-0.9549476 * x - 1.37418593) * x + 2.09137015) * x - 0.16748867
    } else {
        ((3.0817580 * x - 5.8733867) * x + 3.75112997) * x - 0.37001483
    };
    Ok([x, y])
}

fn xyz([x, y]: [f64; 2]) -> DVec3 {
    DVec3::new(x / y, 1.0, (1.0 - x - y) / y)
}
fn from_rows(rows: [[f64; 3]; 3]) -> DMat3 {
    DMat3::from_cols_array_2d(&rows).transpose()
}
fn rgb_to_xyz(native_ap1: bool) -> DMat3 {
    let (primaries, white) = if native_ap1 {
        (
            [[0.713, 0.293], [0.165, 0.830], [0.128, 0.044]],
            [0.32168, 0.33767],
        )
    } else {
        ([[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]], [0.3127, 0.3290])
    };
    let basis = DMat3::from_cols(xyz(primaries[0]), xyz(primaries[1]), xyz(primaries[2]));
    basis * DMat3::from_diagonal(basis.inverse() * xyz(white))
}

/// Row-major linear RGB matrix. Source and target are explicit approximate
/// Planckian whites; 6500 K is neither exactly D65 nor the ACES white point.
/// Native uses AP1/ACES-white primaries; Legacy SDR uses linear Rec.709/D65.
pub fn cct_matrix(
    source_kelvin: f32,
    target_kelvin: f32,
    native_ap1: bool,
) -> Result<[[f32; 3]; 3], &'static str> {
    let source = xyz(planckian_xy(f64::from(source_kelvin))?);
    let target = xyz(planckian_xy(f64::from(target_kelvin))?);
    if source_kelvin == target_kelvin {
        return Ok([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    }
    let bradford = from_rows([
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ]);
    let scale = (bradford * target) / (bradford * source);
    let rgb = rgb_to_xyz(native_ap1);
    let matrix = rgb.inverse() * bradford.inverse() * DMat3::from_diagonal(scale) * bradford * rgb;
    let rows = matrix
        .transpose()
        .to_cols_array_2d()
        .map(|row| row.map(|v| v as f32));
    validate_matrix(&rows)?;
    Ok(rows)
}

pub fn validate_matrix(rows: &[[f32; 3]; 3]) -> Result<(), &'static str> {
    if rows
        .iter()
        .flatten()
        .any(|v| !v.is_finite() || v.abs() > 64.0)
    {
        Err("linear color matrix requires finite coefficients within +/-64")
    } else {
        Ok(())
    }
}
pub fn is_identity(rows: &[[f32; 3]; 3]) -> bool {
    *rows == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}
pub fn apply_matrix(rows: &[[f32; 3]; 3], rgb: [f32; 3]) -> [f32; 3] {
    rows.map(|r| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kelvin_whitepoints_match_published_example_and_independent_polynomial_reference() {
        for (temperature, expected) in [
            (1667.0, [0.5646383046146511, 0.40288714347586385]),
            (2222.0, [0.5031875330377638, 0.415250933113849]),
            (4000.0, [0.3805282828125, 0.3767335309611145]),
            (6504.38938305, [0.3134259983610848, 0.32359596920055655]),
            (25000.0, [0.2524729944384, 0.25225479124365346]),
        ] {
            let actual = planckian_xy(temperature).unwrap();
            for c in 0..2 {
                assert!((actual[c] - expected[c]).abs() < 1e-12);
            }
        }
        for bad in [f64::NAN, f64::INFINITY, 0.0, 1666.99, 25000.01] {
            assert!(planckian_xy(bad).is_err());
        }
    }
    #[test]
    fn kelvin_bradford_matrix_matches_independent_reference_and_maps_white() {
        let rows = cct_matrix(3200.0, 6500.0, true).unwrap();
        let expected = [
            [
                0.8362725575657225,
                -0.1193823905420604,
                0.031029158813366927,
            ],
            [
                -0.007711296922916835,
                1.097383492096454,
                -0.01607029242391716,
            ],
            [
                0.0020721608081474903,
                -0.05301311226008177,
                2.6040455383691157,
            ],
        ];
        for (a, b) in rows.iter().flatten().zip(expected.iter().flatten()) {
            assert!((f64::from(*a) - b).abs() < 2e-7);
        }
        let mapped = apply_matrix(&rows, [1.3100387868, 0.9190466922, 0.4443549900]);
        for (a, b) in mapped
            .into_iter()
            .zip([0.9996194572, 0.9913036558, 1.1111137148])
        {
            assert!((a - b).abs() < 2e-6);
        }
        assert_ne!(rows, cct_matrix(3200.0, 6500.0, false).unwrap());
    }
    #[test]
    fn kelvin_identity_inverse_and_extreme_ranges_are_bounded() {
        for native in [false, true] {
            for source in [1667.0, 2222.0, 3200.0, 4000.0, 6500.0, 12000.0, 25000.0] {
                assert!(is_identity(&cct_matrix(source, source, native).unwrap()));
                for target in [1667.0, 3200.0, 6500.0, 25000.0] {
                    let a = cct_matrix(source, target, native).unwrap();
                    let b = cct_matrix(target, source, native).unwrap();
                    for rgb in [[-0.03, 0.18, 3.0], [1.0; 3]] {
                        let recovered = apply_matrix(&b, apply_matrix(&a, rgb));
                        for c in 0..3 {
                            assert!((recovered[c] - rgb[c]).abs() < 0.00002);
                        }
                    }
                }
            }
        }
        assert!(cct_matrix(1666.0, 1666.0, true).is_err());
        assert!(validate_matrix(&[[f32::NAN; 3]; 3]).is_err());
    }
}
