//! Raster (pixel) image editing — the Photoshop-grade subsystem.
//!
//! Pure CPU, deterministic, unit-tested: no GPU or windowing dependency. The
//! GPU is used only to *display* the resulting pixels (see `photonic-render`).
//!
//! - [`image::RasterImage`] — the 8-bit RGBA pixel buffer.
//! - [`mask::Mask`] — selections and layer masks (8-bit coverage).
//! - [`blend`] — the 16 blend modes and source-over compositing.
//! - [`adjust`] — Image > Adjustments (levels, curves, hue/sat, …).
//! - [`filter`] — Filter menu (blur, sharpen, noise, edges, …).
//! - [`brush`] — the brush family (paint, erase, clone, smudge, dodge/burn).
//! - [`geometry`] — resize, crop, rotate, flip, canvas size.

pub mod adjust;
pub mod advanced;
pub mod blend;
pub mod brush;
pub mod filter;
pub mod geometry;
pub mod image;
pub mod mask;
pub mod repair;
pub mod trace;
pub mod warp;

pub use image::{
    luma, validate_raster_dimensions, RasterImage, MAX_RASTER_DIMENSION, MAX_RASTER_PIXELS,
};
pub use mask::Mask;

/// Linearly interpolate two RGBA pixels by `t` (0..1).
#[inline]
pub fn lerp_rgba(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    let mut out = [0u8; 4];
    for c in 0..4 {
        out[c] = (a[c] as f32 * (1.0 - t) + b[c] as f32 * t)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    out
}

/// Apply a **point operation** (per-pixel color transform) to `img`, honoring an
/// optional selection `mask` — where coverage < 255 the result is blended back
/// toward the original, exactly like editing inside a Photoshop selection.
pub fn apply_point(
    img: &mut RasterImage,
    mask: Option<&Mask>,
    mut f: impl FnMut([u8; 4]) -> [u8; 4],
) {
    for y in 0..img.height {
        for x in 0..img.width {
            let old = img.pixel(x, y);
            let new = f(old);
            let out = match mask {
                Some(m) => lerp_rgba(old, new, m.coverage(x, y)),
                None => new,
            };
            img.set_pixel(x, y, out);
        }
    }
}

/// Blend a fully-computed `result` (e.g. a neighborhood filter output) back into
/// `img`, honoring an optional selection `mask`. `result` must match `img` size.
pub fn blend_result(img: &mut RasterImage, result: &RasterImage, mask: Option<&Mask>) {
    if result.width != img.width || result.height != img.height {
        return;
    }
    match mask {
        None => {
            img.pixels.copy_from_slice(&result.pixels);
        }
        Some(m) => {
            for y in 0..img.height {
                for x in 0..img.width {
                    let old = img.pixel(x, y);
                    let new = result.pixel(x, y);
                    img.set_pixel(x, y, lerp_rgba(old, new, m.coverage(x, y)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_point_respects_mask() {
        let mut img = RasterImage::filled(2, 1, [0, 0, 0, 255]);
        let mut m = Mask::empty(2, 1);
        m.set(0, 0, 255);
        apply_point(&mut img, Some(&m), |_| [255, 255, 255, 255]);
        assert_eq!(img.pixel(0, 0), [255, 255, 255, 255]);
        assert_eq!(img.pixel(1, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn lerp_midpoint() {
        assert_eq!(
            lerp_rgba([0, 0, 0, 0], [100, 100, 100, 100], 0.5),
            [50, 50, 50, 50]
        );
    }
}
