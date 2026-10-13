//! Read-only presentation of scene-linear AP1 graph image outputs.
use super::Aces2SdrOutput;

#[derive(Debug)]
pub struct ImageInspection {
    pub rgba: Vec<u8>,
    pub scene_min: [f32; 3],
    pub scene_max: [f32; 3],
    pub alpha_min: f32,
    pub alpha_max: f32,
}

/// Keep scene statistics separate from the SDR display preview. Scene extrema
/// are measured on straight color at covered pixels, without clipping extended
/// range. Preview bytes use the same ACES output transform as the native viewer.
pub fn inspect_scene_image(
    pixels: &[[f32; 4]],
    width: u32,
    height: u32,
) -> Result<ImageInspection, &'static str> {
    let count = (width as usize)
        .checked_mul(height as usize)
        .filter(|&n| n > 0)
        .ok_or("invalid image dimensions")?;
    if count != pixels.len() || count.checked_mul(4).is_none() {
        return Err("incomplete image readback");
    }
    let transform = Aces2SdrOutput::shared()?;
    let mut out = ImageInspection {
        rgba: Vec::with_capacity(count * 4),
        scene_min: [f32::INFINITY; 3],
        scene_max: [f32::NEG_INFINITY; 3],
        alpha_min: 1.0,
        alpha_max: 0.0,
    };
    let mut covered = false;
    for &pixel in pixels {
        if pixel.iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&pixel[3]) {
            return Err("non-finite image values or invalid coverage");
        }
        let alpha = pixel[3];
        out.alpha_min = out.alpha_min.min(alpha);
        out.alpha_max = out.alpha_max.max(alpha);
        if alpha == 0.0 {
            out.rgba.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        covered = true;
        for c in 0..3 {
            let value = pixel[c] / alpha;
            if !value.is_finite() {
                return Err("non-finite straight scene color");
            }
            out.scene_min[c] = out.scene_min[c].min(value);
            out.scene_max[c] = out.scene_max[c].max(value);
        }
        let mapped = transform.map_premultiplied(pixel.map(f64::from))?;
        for value in &mapped[..3] {
            out.rgba
                .push((value / alpha as f64 * 255.0).clamp(0.0, 255.0).round() as u8);
        }
        out.rgba.push((alpha * 255.0).round() as u8);
    }
    if !covered {
        out.scene_min = [0.0; 3];
        out.scene_max = [0.0; 3];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_preview_preserves_alpha_and_extended_scene_statistics() {
        let src = [[-0.1, 1.0, 0.05, 0.25], [0.0; 4]];
        let output = inspect_scene_image(&src, 2, 1).unwrap();
        assert_eq!(output.scene_min, [-0.4, 4.0, 0.2]);
        assert_eq!(output.scene_max, output.scene_min);
        assert_eq!(output.alpha_min, 0.0);
        assert_eq!(output.alpha_max, 0.25);
        assert_eq!(output.rgba[3], 64);
        assert_eq!(&output.rgba[4..], [0, 0, 0, 0]);
        let expected = Aces2SdrOutput::shared()
            .unwrap()
            .map_straight([-0.4, 4.0, 0.2])
            .unwrap();
        for c in 0..3 {
            assert!(output.rgba[c].abs_diff((expected[c] * 255.0).round() as u8) <= 1);
        }
    }
    #[test]
    fn transparent_scene_statistics_are_defined() {
        let output = inspect_scene_image(&[[0.0; 4]], 1, 1).unwrap();
        assert_eq!(output.scene_min, [0.0; 3]);
        assert_eq!(output.scene_max, [0.0; 3]);
        assert_eq!(output.rgba, [0, 0, 0, 0]);
    }
    #[test]
    fn invalid_readbacks_fail_instead_of_clamping_scene_data() {
        assert!(inspect_scene_image(&[], 0, 1).is_err());
        assert!(inspect_scene_image(&[], 1, 1).is_err());
        for pixel in [
            [f32::NAN, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, -0.1],
            [0.0, 0.0, 0.0, 1.1],
        ] {
            assert!(inspect_scene_image(&[pixel], 1, 1).is_err());
        }
    }
}
