//! Read-only presentation of raw, unassociated matte weights.
//!
//! A matte is not RGB imagery: do not apply a display transfer or divide its
//! weight by source alpha. Statistics retain the float weights; only the
//! opaque grayscale preview is quantized.

#[derive(Debug, Clone, PartialEq)]
pub struct MatteInspection {
    pub rgba: Vec<u8>,
    pub minimum: f32,
    pub maximum: f32,
    pub mean: f64,
}

pub fn inspect_weights(
    pixels: &[[f32; 4]],
    width: u32,
    height: u32,
) -> Result<MatteInspection, &'static str> {
    let count = (width as usize)
        .checked_mul(height as usize)
        .filter(|&count| count > 0)
        .ok_or("invalid matte dimensions")?;
    if pixels.len() != count || count.checked_mul(4).is_none() {
        return Err("incomplete matte readback");
    }
    let mut result = MatteInspection {
        rgba: Vec::with_capacity(count * 4),
        minimum: 1.0,
        maximum: 0.0,
        mean: 0.0,
    };
    for pixel in pixels {
        let weight = pixel[0];
        if !weight.is_finite() || !(0.0..=1.0).contains(&weight) {
            return Err("matte weight is non-finite or outside [0,1]");
        }
        result.minimum = result.minimum.min(weight);
        result.maximum = result.maximum.max(weight);
        result.mean += f64::from(weight);
        let code = (weight * 255.0).round() as u8;
        result.rgba.extend_from_slice(&[code, code, code, 255]);
    }
    result.mean /= count as f64;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_are_not_unpremultiplied_or_display_encoded() {
        let result = inspect_weights(&[[0.0; 4], [0.25, 0.25, 0.25, 0.5], [1.0; 4]], 3, 1).unwrap();
        assert_eq!(
            result.rgba,
            [0, 0, 0, 255, 64, 64, 64, 255, 255, 255, 255, 255]
        );
        assert_eq!(result.minimum, 0.0);
        assert_eq!(result.maximum, 1.0);
        assert_eq!(result.mean, 1.25 / 3.0);
    }

    #[test]
    fn statistics_preserve_sub_byte_precision() {
        let result = inspect_weights(&[[0.0001, 0.0, 0.0, 1.0]], 1, 1).unwrap();
        assert_eq!(result.rgba, [0, 0, 0, 255]);
        assert_eq!(result.mean, f64::from(0.0001_f32));
    }

    #[test]
    fn malformed_readbacks_fail_closed() {
        assert!(inspect_weights(&[], 0, 1).is_err());
        assert!(inspect_weights(&[], 1, 1).is_err());
        for weight in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            assert!(inspect_weights(&[[weight, 0.0, 0.0, 1.0]], 1, 1).is_err());
        }
    }
}
