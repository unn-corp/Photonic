//! Geometric image operations — resize, crop, canvas size, rotate, flip.
//!
//! Resampling for `resize` uses the `image` crate's high-quality filters
//! (Lanczos3 by default); the remaining ops are exact pixel moves.

use super::image::RasterImage;

/// Resampling quality for [`resize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resample {
    Nearest,
    Bilinear,
    /// Lanczos3 — best quality for downscaling (Photoshop "Bicubic Sharper"-like).
    Lanczos3,
}

impl Resample {
    fn to_filter(self) -> image::imageops::FilterType {
        match self {
            Resample::Nearest => image::imageops::FilterType::Nearest,
            Resample::Bilinear => image::imageops::FilterType::Triangle,
            Resample::Lanczos3 => image::imageops::FilterType::Lanczos3,
        }
    }
}

fn to_image_buffer(img: &RasterImage) -> image::RgbaImage {
    image::ImageBuffer::from_raw(img.width, img.height, img.pixels.clone())
        .unwrap_or_else(|| image::ImageBuffer::new(img.width, img.height))
}

fn from_image_buffer(buf: image::RgbaImage) -> RasterImage {
    let (width, height) = buf.dimensions();
    RasterImage {
        width: width.max(1),
        height: height.max(1),
        pixels: buf.into_raw(),
    }
}

/// Upper bound on any resampled/allocated dimension, so adversarial sizes can't
/// overflow allocation math.
const DIM_MAX: u32 = 16384;

/// Sanitize a possibly non-finite scalar: NaN / ±inf collapse to `default`.
#[inline]
fn san(x: f32, default: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        default
    }
}

/// Build a premultiplied-alpha copy of `img` (R,G,B *= A/255).
///
/// Resampling (resize / rotate) must run in premultiplied space, otherwise the
/// RGB of transparent pixels (commonly black) bleeds into opaque neighbors at
/// edges, producing "dark fringing". We premultiply, resample, then
/// [`unpremultiply_in_place`] the result.
fn premultiplied(img: &RasterImage) -> RasterImage {
    let mut out = img.clone();
    for px in out.pixels.chunks_exact_mut(4) {
        let a = px[3] as u32;
        px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
        px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
        px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
    }
    out
}

/// Invert [`premultiplied`] in place: `R = Rp * 255 / A`. `A == 0` → `[0,0,0,0]`.
fn unpremultiply_in_place(img: &mut RasterImage) {
    for px in img.pixels.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else {
            px[0] = ((px[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[1] = ((px[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[2] = ((px[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
}

/// Resample the image to a new pixel size (Image > Image Size).
pub fn resize(img: &RasterImage, new_w: u32, new_h: u32, filter: Resample) -> RasterImage {
    let new_w = new_w.clamp(1, DIM_MAX);
    let new_h = new_h.clamp(1, DIM_MAX);
    // Resample in premultiplied alpha so transparent pixels don't dark-fringe edges.
    let buf = to_image_buffer(&premultiplied(img));
    let resized = image::imageops::resize(&buf, new_w, new_h, filter.to_filter());
    let mut out = from_image_buffer(resized);
    unpremultiply_in_place(&mut out);
    out
}

/// Scale by a uniform factor.
pub fn scale(img: &RasterImage, factor: f32, filter: Resample) -> RasterImage {
    let f = san(factor, 1.0).max(0.0001);
    resize(
        img,
        (img.width as f32 * f).round() as u32,
        (img.height as f32 * f).round() as u32,
        filter,
    )
}

/// Crop to a rectangle (clamped to image bounds). Returns a new image.
pub fn crop(img: &RasterImage, x: i64, y: i64, w: u32, h: u32) -> RasterImage {
    let x0 = x.clamp(0, img.width as i64);
    let y0 = y.clamp(0, img.height as i64);
    let x1 = x.saturating_add(i64::from(w)).clamp(0, img.width as i64);
    let y1 = y.saturating_add(i64::from(h)).clamp(0, img.height as i64);
    let cw = (x1 - x0).max(1) as u32;
    let ch = (y1 - y0).max(1) as u32;
    let mut out = RasterImage::new(cw, ch);
    for yy in 0..ch {
        for xx in 0..cw {
            out.set_pixel(xx, yy, img.pixel(x0 as u32 + xx, y0 as u32 + yy));
        }
    }
    out
}

/// Resize the canvas without resampling content (Image > Canvas Size). Content is
/// placed at `(offset_x, offset_y)`; new area is transparent.
pub fn resize_canvas(
    img: &RasterImage,
    new_w: u32,
    new_h: u32,
    offset_x: i64,
    offset_y: i64,
) -> RasterImage {
    let mut out = RasterImage::new(new_w.clamp(1, DIM_MAX), new_h.clamp(1, DIM_MAX));
    for y in 0..img.height {
        for x in 0..img.width {
            let nx = offset_x.saturating_add(i64::from(x));
            let ny = offset_y.saturating_add(i64::from(y));
            if (0..i64::from(out.width)).contains(&nx) && (0..i64::from(out.height)).contains(&ny) {
                out.set_pixel(nx as u32, ny as u32, img.pixel(x, y));
            }
        }
    }
    out
}

/// Rotate 90° clockwise.
pub fn rotate90(img: &RasterImage) -> RasterImage {
    let mut out = RasterImage::new(img.height, img.width);
    for y in 0..img.height {
        for x in 0..img.width {
            out.set_pixel(img.height - 1 - y, x, img.pixel(x, y));
        }
    }
    out
}

/// Rotate 180°.
pub fn rotate180(img: &RasterImage) -> RasterImage {
    let mut out = RasterImage::new(img.width, img.height);
    for y in 0..img.height {
        for x in 0..img.width {
            out.set_pixel(img.width - 1 - x, img.height - 1 - y, img.pixel(x, y));
        }
    }
    out
}

/// Rotate 270° clockwise (90° counter-clockwise).
pub fn rotate270(img: &RasterImage) -> RasterImage {
    let mut out = RasterImage::new(img.height, img.width);
    for y in 0..img.height {
        for x in 0..img.width {
            out.set_pixel(y, img.width - 1 - x, img.pixel(x, y));
        }
    }
    out
}

/// Flip horizontally (mirror left↔right).
pub fn flip_h(img: &RasterImage) -> RasterImage {
    let mut out = RasterImage::new(img.width, img.height);
    for y in 0..img.height {
        for x in 0..img.width {
            out.set_pixel(img.width - 1 - x, y, img.pixel(x, y));
        }
    }
    out
}

/// Flip vertically (mirror top↔bottom).
pub fn flip_v(img: &RasterImage) -> RasterImage {
    let mut out = RasterImage::new(img.width, img.height);
    for y in 0..img.height {
        for x in 0..img.width {
            out.set_pixel(x, img.height - 1 - y, img.pixel(x, y));
        }
    }
    out
}

/// Rotate by an arbitrary angle (degrees, clockwise) about the image center,
/// expanding the canvas to fit. Empty area is transparent; bilinear sampling.
pub fn rotate_arbitrary(img: &RasterImage, angle_deg: f32) -> RasterImage {
    let angle_deg = san(angle_deg, 0.0);
    let theta = angle_deg.to_radians();
    let (sin, cos) = theta.sin_cos();
    let w = img.width as f32;
    let h = img.height as f32;
    // new bounding box, clamped so an absurd source can't overflow allocation
    let new_w = (w * cos.abs() + h * sin.abs())
        .ceil()
        .clamp(1.0, DIM_MAX as f32);
    let new_h = (w * sin.abs() + h * cos.abs())
        .ceil()
        .clamp(1.0, DIM_MAX as f32);
    let mut out = RasterImage::new(new_w as u32, new_h as u32);
    // Sample in premultiplied alpha so transparent borders don't darken edges.
    let src = premultiplied(img);
    let cx = w / 2.0;
    let cy = h / 2.0;
    let ncx = new_w / 2.0;
    let ncy = new_h / 2.0;
    for oy in 0..out.height {
        for ox in 0..out.width {
            // inverse-map output pixel back into source space
            let dx = ox as f32 + 0.5 - ncx;
            let dy = oy as f32 + 0.5 - ncy;
            let sx = cos * dx + sin * dy + cx;
            let sy = -sin * dx + cos * dy + cy;
            if sx >= 0.0 && sy >= 0.0 && sx < w && sy < h {
                out.set_pixel(ox, oy, src.sample_bilinear(sx - 0.5, sy - 0.5));
            }
        }
    }
    unpremultiply_in_place(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: u32, h: u32) -> RasterImage {
        let mut img = RasterImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(x, y, [(x * 10) as u8, (y * 10) as u8, 0, 255]);
            }
        }
        img
    }

    #[test]
    fn resize_changes_dims() {
        let img = ramp(4, 4);
        let r = resize(&img, 8, 2, Resample::Bilinear);
        assert_eq!((r.width, r.height), (8, 2));
    }

    #[test]
    fn crop_region() {
        let img = ramp(5, 5);
        let c = crop(&img, 1, 1, 2, 3);
        assert_eq!((c.width, c.height), (2, 3));
        assert_eq!(c.pixel(0, 0), img.pixel(1, 1));
    }

    #[test]
    fn image_and_mask_crops_clamp_far_positive_coordinates() {
        let img = ramp(2, 2);
        let mask = crate::raster::Mask::full(2, 2);
        for (x, y, expected) in [(i64::MAX, 0, (1, 2)), (0, i64::MAX, (2, 1))] {
            let cropped_image = crop(&img, x, y, 2, 2);
            let cropped_mask = mask.crop(x, y, 2, 2);
            assert_eq!((cropped_image.width, cropped_image.height), expected);
            assert_eq!((cropped_mask.width, cropped_mask.height), expected);
            assert!(cropped_image.pixels.iter().all(|&channel| channel == 0));
            assert!(cropped_mask.is_empty_selection());
        }
    }

    #[test]
    fn canvas_grows_with_transparent_border() {
        let img = RasterImage::filled(2, 2, [9, 9, 9, 255]);
        let c = resize_canvas(&img, 4, 4, 1, 1);
        assert_eq!((c.width, c.height), (4, 4));
        assert_eq!(c.pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(c.pixel(1, 1), [9, 9, 9, 255]);
    }

    #[test]
    fn canvas_offsets_cannot_wrap_content_back_into_view() {
        let img = ramp(2, 2);
        for (x, y) in [
            (1_i64 << 32, 0),
            (0, 1_i64 << 32),
            (i64::MAX, 0),
            (0, i64::MAX),
        ] {
            let resized = resize_canvas(&img, 2, 2, x, y);
            assert_eq!((resized.width, resized.height), (2, 2));
            assert!(
                resized.pixels.iter().all(|&channel| channel == 0),
                "content at ({x}, {y}) is outside the canvas"
            );
        }
    }

    #[test]
    fn rotate90_swaps_dims_and_corner() {
        let img = ramp(3, 2);
        let r = rotate90(&img);
        assert_eq!((r.width, r.height), (2, 3));
        // top-left of source goes to top-right of rotated
        assert_eq!(r.pixel(r.width - 1, 0), img.pixel(0, 0));
    }

    #[test]
    fn rotate90_four_times_is_identity() {
        let img = ramp(3, 4);
        let r = rotate90(&rotate90(&rotate90(&rotate90(&img))));
        assert_eq!(r, img);
    }

    #[test]
    fn flips_are_involutions() {
        let img = ramp(4, 3);
        assert_eq!(flip_h(&flip_h(&img)), img);
        assert_eq!(flip_v(&flip_v(&img)), img);
    }

    #[test]
    fn rotate_arbitrary_zero_is_identity_size() {
        let img = ramp(5, 5);
        let r = rotate_arbitrary(&img, 0.0);
        assert_eq!((r.width, r.height), (5, 5));
    }

    // ── Identity content preserved (±2) ──────────────────────────────────────

    #[test]
    fn rotate_arbitrary_zero_preserves_content() {
        let img = ramp(6, 6);
        let r = rotate_arbitrary(&img, 0.0);
        for y in 0..6 {
            for x in 0..6 {
                let a = img.pixel(x, y);
                let b = r.pixel(x, y);
                assert!(
                    (0..4).all(|c| (a[c] as i32 - b[c] as i32).abs() <= 2),
                    "rotate 0° changed pixel at {},{}: {:?} vs {:?}",
                    x,
                    y,
                    a,
                    b
                );
            }
        }
    }

    #[test]
    fn resize_same_size_nearest_is_identity() {
        let img = ramp(5, 4);
        let r = resize(&img, 5, 4, Resample::Nearest);
        assert_eq!(r, img);
    }

    // ── BUG 1: non-finite / huge inputs never panic, dims stay bounded ────────

    #[test]
    fn geometry_panic_free_on_non_finite_and_huge() {
        let img = ramp(8, 6);
        // rotate_arbitrary handles every non-finite / huge angle without panic and
        // keeps its expanded canvas within bounds.
        for &v in &[
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            1.0e30f32,
            -1.0e30f32,
        ] {
            let r = rotate_arbitrary(&img, v);
            assert!(r.width >= 1 && r.width <= DIM_MAX);
            assert!(r.height >= 1 && r.height <= DIM_MAX);
        }
        // scale: non-finite factors collapse to a safe default, never panic.
        for &v in &[f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 2.0f32] {
            let s = scale(&img, v, Resample::Bilinear);
            assert!(s.width >= 1 && s.width <= DIM_MAX && s.height >= 1 && s.height <= DIM_MAX);
        }
        // Absurd dimensions are clamped to DIM_MAX, not over-allocated. Keep the
        // other axis tiny so the test stays cheap while still exercising the clamp.
        let wide = resize(&img, u32::MAX, 2, Resample::Nearest);
        assert_eq!(wide.width, DIM_MAX);
        let tall = resize(&img, 2, u32::MAX, Resample::Nearest);
        assert_eq!(tall.height, DIM_MAX);
        let canvas = resize_canvas(&img, u32::MAX, 2, 0, 0);
        assert_eq!(canvas.width, DIM_MAX);
    }

    // ── BUG 2: premultiplied resampling — no dark fringing ───────────────────

    /// Left half opaque white, right half fully transparent.
    fn half_white(w: u32, h: u32) -> RasterImage {
        let mut img = RasterImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                if x < w / 2 {
                    img.set_pixel(x, y, [255, 255, 255, 255]);
                } else {
                    img.set_pixel(x, y, [0, 0, 0, 0]);
                }
            }
        }
        img
    }

    fn assert_no_dark_fringe(img: &RasterImage) {
        for px in img.pixels.chunks_exact(4) {
            if px[3] > 0 {
                assert!(
                    px[0] >= 250 && px[1] >= 250 && px[2] >= 250,
                    "dark fringing: opaque pixel {:?} not white",
                    [px[0], px[1], px[2], px[3]]
                );
            }
        }
    }

    #[test]
    fn resize_premultiplied_no_dark_fringe() {
        // Downscale across the opaque/transparent boundary with a blending filter.
        let img = half_white(32, 4);
        for f in [Resample::Bilinear, Resample::Lanczos3] {
            let r = resize(&img, 17, 4, f);
            assert_no_dark_fringe(&r);
        }
    }

    #[test]
    fn rotate_premultiplied_no_dark_fringe() {
        let img = half_white(24, 24);
        let r = rotate_arbitrary(&img, 30.0);
        assert_no_dark_fringe(&r);
    }
}
