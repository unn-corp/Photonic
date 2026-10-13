//! Advanced, Photoshop-grade filters — pure CPU, deterministic, unit-tested.
//!
//! These are the higher-end entries of the Filter menu that go beyond the
//! primitive blurs and sharpens in [`crate::raster::filter`]:
//!
//! - [`surface_blur`] — edge-preserving bilateral smoothing ("Surface Blur").
//! - [`lens_blur`] — disc/bokeh defocus ("Lens Blur").
//! - [`smart_sharpen`] — unsharp masking with edge gating + threshold.
//! - [`reduce_noise`] — edge-preserving bilateral denoise.
//! - [`clarity`] — midtone local-contrast enhancement.
//! - [`vignette`] — darken/lighten toward the corners.
//! - [`chromatic_aberration`] — radial per-channel RGB split.
//!
//! Every neighborhood filter follows the house rule: take an immutable snapshot
//! (`clone`), compute a *full* result image from that snapshot, then hand it to
//! [`blend_result`] so an optional selection [`Mask`] is honored exactly like
//! editing inside a Photoshop selection. Most filters preserve center alpha;
//! Lens Blur averages premultiplied color and coverage over its aperture. The math is
//! `f32` internally and clamped back to `0..=255`. Border addressing uses
//! clamp-to-edge ([`RasterImage::sample_clamped`] / `sample_bilinear`).

use crate::raster::{
    blend_result,
    filter::{gaussian_blur, san_amount, san_radius, MAX_RADIUS},
    image::{luma, RasterImage},
    mask::Mask,
};

// ── shared helpers ───────────────────────────────────────────────────────────

/// Clamp and round an `f32` into a `u8` channel value.
#[inline]
fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Core bilateral filter used by both [`surface_blur`] and [`reduce_noise`].
///
/// `radius` is the spatial window in pixels, `sigma_s` the spatial Gaussian
/// sigma, and `sigma_r` the *range* sigma in 0..255 color units (larger ⇒ more
/// aggressive smoothing across color differences). Alpha is copied from the
/// center pixel. Returns a fresh, fully-computed result image.
fn bilateral(src: &RasterImage, radius: i64, sigma_s: f32, sigma_r: f32) -> RasterImage {
    let inv_2ss = 1.0 / (2.0 * sigma_s * sigma_s);
    let inv_2sr = 1.0 / (2.0 * sigma_r * sigma_r);
    let mut result = RasterImage::new(src.width, src.height);
    for y in 0..src.height as i64 {
        for x in 0..src.width as i64 {
            let center = src.sample_clamped(x, y);
            let ca = center[3] as f32;
            let caf = ca / 255.0;
            // Work in premultiplied alpha so transparent neighbors (black RGB)
            // can't bleed dark fringes into opaque pixels.
            let cr = center[0] as f32 * caf;
            let cg = center[1] as f32 * caf;
            let cb = center[2] as f32 * caf;
            let mut acc = [0.0f32; 3];
            // `asum` is the alpha-weighted weight sum (Σ w·a/255). Dividing the
            // premultiplied accumulators by it un-premultiplies by the *blurred*
            // alpha, so fully-transparent neighbors drop out entirely instead of
            // inflating the denominator and darkening the opaque result.
            let mut asum = 0.0f32;
            for wy in -radius..=radius {
                for wx in -radius..=radius {
                    let s = src.sample_clamped(x + wx, y + wy);
                    let saf = s[3] as f32 / 255.0;
                    let sr = s[0] as f32 * saf;
                    let sg = s[1] as f32 * saf;
                    let sb = s[2] as f32 * saf;
                    let dr = sr - cr;
                    let dg = sg - cg;
                    let db = sb - cb;
                    let color_dist_sq = dr * dr + dg * dg + db * db;
                    let spatial = (-((wx * wx + wy * wy) as f32) * inv_2ss).exp();
                    let range = (-color_dist_sq * inv_2sr).exp();
                    let w = spatial * range;
                    acc[0] += sr * w;
                    acc[1] += sg * w;
                    acc[2] += sb * w;
                    asum += saf * w;
                }
            }
            // Alpha is taken from the center pixel; un-premultiply RGB by `asum`.
            let out = if ca <= 0.0 {
                [0, 0, 0, 0]
            } else if asum > 0.0 {
                [
                    to_u8(acc[0] / asum),
                    to_u8(acc[1] / asum),
                    to_u8(acc[2] / asum),
                    center[3],
                ]
            } else {
                center
            };
            result.set_pixel(x as u32, y as u32, out);
        }
    }
    result
}

// ── Surface Blur (bilateral) ─────────────────────────────────────────────────

/// Edge-preserving bilateral smoothing ("Surface Blur"): smooths flat regions
/// while preserving edges. `radius` is the window in pixels; `threshold`
/// (`0..1`) is the range sensitivity — small values keep more edges, large
/// values behave closer to a plain blur. No-op when `radius == 0` or
/// `threshold <= 0`.
pub fn surface_blur(img: &mut RasterImage, radius: u32, threshold: f32, sel: Option<&Mask>) {
    let radius = radius.min(MAX_RADIUS);
    let threshold = san_amount(threshold);
    if radius == 0 || threshold <= 0.0 {
        return;
    }
    let src = img.clone();
    let sigma_s = (radius as f32).max(1.0);
    let sigma_r = (threshold * 255.0).max(1.0);
    let result = bilateral(&src, radius as i64, sigma_s, sigma_r);
    blend_result(img, &result, sel);
}

// ── Lens Blur (disc / bokeh) ─────────────────────────────────────────────────

/// Disc ("Lens Blur") defocus — averages every neighbor whose distance from the
/// center is within `radius`, giving the round-aperture bokeh look rather than a
/// Gaussian falloff. Averages in premultiplied alpha (so transparent neighbors
/// don't darken edges); alpha is averaged across the disc. No-op when
/// `radius <= 0`.
pub fn lens_blur(img: &mut RasterImage, radius: f32, sel: Option<&Mask>) {
    let radius = san_radius(radius);
    if radius <= 0.0 {
        return;
    }
    let src = img.clone();
    let r = radius.ceil() as i64;
    let r2 = radius * radius;
    let mut result = RasterImage::new(src.width, src.height);
    for y in 0..src.height as i64 {
        for x in 0..src.width as i64 {
            let center = src.sample_clamped(x, y);
            // Premultiplied disc average so transparent neighbors don't darken.
            let mut acc = [0.0f32; 4]; // premultiplied R,G,B + accumulated alpha
            let mut count = 0.0f32;
            for wy in -r..=r {
                for wx in -r..=r {
                    if (wx * wx + wy * wy) as f32 <= r2 {
                        let s = src.sample_clamped(x + wx, y + wy);
                        let af = s[3] as f32 / 255.0;
                        acc[0] += s[0] as f32 * af;
                        acc[1] += s[1] as f32 * af;
                        acc[2] += s[2] as f32 * af;
                        acc[3] += s[3] as f32;
                        count += 1.0;
                    }
                }
            }
            let out = if count > 0.0 {
                let ap = acc[3] / count;
                if ap <= 0.0 {
                    [0, 0, 0, 0]
                } else {
                    let afp = ap / 255.0;
                    [
                        to_u8((acc[0] / count) / afp),
                        to_u8((acc[1] / count) / afp),
                        to_u8((acc[2] / count) / afp),
                        to_u8(ap),
                    ]
                }
            } else {
                center
            };
            result.set_pixel(x as u32, y as u32, out);
        }
    }
    blend_result(img, &result, sel);
}

// ── Smart Sharpen ────────────────────────────────────────────────────────────

/// Smart Sharpen — unsharp masking with an edge-gating term and a threshold.
///
/// Computes `orig + amount · edge · (orig − gaussian(orig))` per RGB channel,
/// where the per-channel difference must exceed `threshold` to be applied and
/// `edge` is a smooth `0..1` ramp of the local difference magnitude (so faint
/// noise is left alone while real edges get the full effect). Alpha preserved.
/// No-op when `amount <= 0` or `radius <= 0`.
pub fn smart_sharpen(
    img: &mut RasterImage,
    amount: f32,
    radius: f32,
    threshold: u8,
    sel: Option<&Mask>,
) {
    let amount = san_amount(amount);
    let radius = san_radius(radius);
    if amount <= 0.0 || radius <= 0.0 {
        return;
    }
    let src = img.clone();
    let mut blurred = img.clone();
    gaussian_blur(&mut blurred, radius, None);
    let thr = threshold as f32;
    let mut result = src.clone();
    let n = src.len();
    for i in 0..n {
        let base = i * 4;
        for c in 0..3 {
            let orig = src.pixels[base + c] as f32;
            let diff = orig - blurred.pixels[base + c] as f32;
            let mag = diff.abs();
            if mag > thr {
                // Edge gate: ramp from threshold up to threshold + 32 so flat /
                // low-contrast areas are sharpened less than strong edges.
                let edge = ((mag - thr) / 32.0).clamp(0.0, 1.0);
                result.pixels[base + c] = to_u8(orig + amount * edge * diff);
            }
        }
        // alpha (base + 3) preserved by the clone
    }
    blend_result(img, &result, sel);
}

// ── Reduce Noise ─────────────────────────────────────────────────────────────

/// Reduce Noise — edge-preserving bilateral denoise. `strength` (`0..1`) scales
/// both the smoothing window influence and the range sigma, so higher strength
/// averages away more noise while still respecting edges. No-op when
/// `strength <= 0`.
pub fn reduce_noise(img: &mut RasterImage, strength: f32, sel: Option<&Mask>) {
    let strength = san_amount(strength);
    if strength <= 0.0 {
        return;
    }
    let s = strength.clamp(0.0, 1.0);
    let src = img.clone();
    let radius: i64 = 2;
    let sigma_s = 1.5 + s * 1.5;
    // Range sigma grows with strength so noise (small color jitter) is averaged
    // while genuine edges (large jumps) survive.
    let sigma_r = 10.0 + s * 70.0;
    let result = bilateral(&src, radius, sigma_s, sigma_r);
    blend_result(img, &result, sel);
}

// ── Clarity (midtone local contrast) ─────────────────────────────────────────

/// Clarity — midtone local-contrast enhancement via a large-radius unsharp.
///
/// Adds `amount · midtone · (orig − bigBlur(orig))` per channel, where
/// `midtone` is a bell weight peaking at mid-luma so highlights and shadows are
/// left comparatively untouched (the classic "clarity" feel). `amount` ranges
/// `-1..1` (negative softens). Alpha preserved. No-op when `amount == 0`.
pub fn clarity(img: &mut RasterImage, amount: f32, sel: Option<&Mask>) {
    let amount = san_amount(amount);
    if amount == 0.0 {
        return;
    }
    let src = img.clone();
    // Large radius relative to the image gives the broad local-contrast halo.
    let radius = ((src.width.min(src.height) as f32) / 8.0).clamp(3.0, 60.0);
    let mut blurred = img.clone();
    gaussian_blur(&mut blurred, radius, None);
    let mut result = src.clone();
    let n = src.len();
    for i in 0..n {
        let base = i * 4;
        let l = luma([
            src.pixels[base] as f32 / 255.0,
            src.pixels[base + 1] as f32 / 255.0,
            src.pixels[base + 2] as f32 / 255.0,
        ]);
        // Bell centered on mid-luma: 1 at 0.5, 0 at the extremes.
        let midtone = (1.0 - (l - 0.5).abs() * 2.0).clamp(0.0, 1.0);
        for c in 0..3 {
            let orig = src.pixels[base + c] as f32;
            let diff = orig - blurred.pixels[base + c] as f32;
            result.pixels[base + c] = to_u8(orig + amount * midtone * diff);
        }
        // alpha preserved by the clone
    }
    blend_result(img, &result, sel);
}

// ── Vignette ─────────────────────────────────────────────────────────────────

/// Vignette — multiplies RGB by a corner-weighted factor. `amount` (`-1..1`)
/// darkens (negative) or lightens (positive) toward the corners; the center is
/// unchanged. `feather` (`0..1`) controls how gradually the effect ramps in
/// (0 = abrupt at the very corner, 1 = a smooth falloff across the whole frame).
/// Alpha preserved. No-op when `amount == 0`.
pub fn vignette(img: &mut RasterImage, amount: f32, feather: f32, sel: Option<&Mask>) {
    let amount = san_amount(amount);
    let feather = san_amount(feather);
    if amount == 0.0 {
        return;
    }
    let src = img.clone();
    let w = src.width as f32;
    let h = src.height as f32;
    let cx = (w - 1.0) / 2.0;
    let cy = (h - 1.0) / 2.0;
    // Distance from center to a corner — used to normalize so corners hit 1.0.
    let max_dist = (cx * cx + cy * cy).sqrt().max(1e-4);
    let inner = (1.0 - feather).clamp(0.0, 1.0);
    let span = (1.0 - inner).max(1e-4);
    let mut result = src.clone();
    for y in 0..src.height {
        for x in 0..src.width {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d = (dx * dx + dy * dy).sqrt() / max_dist; // 0 center .. 1 corner
            let t = ((d - inner) / span).clamp(0.0, 1.0);
            let vig = t * t * (3.0 - 2.0 * t); // smoothstep
            let factor = (1.0 + amount * vig).max(0.0);
            let p = src.pixel(x, y);
            result.set_pixel(
                x,
                y,
                [
                    to_u8(p[0] as f32 * factor),
                    to_u8(p[1] as f32 * factor),
                    to_u8(p[2] as f32 * factor),
                    p[3],
                ],
            );
        }
    }
    blend_result(img, &result, sel);
}

// ── Chromatic Aberration ─────────────────────────────────────────────────────

/// Chromatic aberration — radial per-channel RGB split. The red channel is
/// sampled shifted *outward* from the image center and the blue channel
/// *inward*, with the shift scaling from 0 at the center to `amount` pixels at
/// the corners (green stays put). Mimics lens fringing. Alpha preserved. No-op
/// when `amount == 0`.
pub fn chromatic_aberration(img: &mut RasterImage, amount: f32, sel: Option<&Mask>) {
    // Sanitize and bound the pixel shift so sampling coordinates stay finite.
    let amount = san_amount(amount).clamp(-(4.0 * MAX_RADIUS as f32), 4.0 * MAX_RADIUS as f32);
    if amount == 0.0 {
        return;
    }
    let src = img.clone();
    let w = src.width as f32;
    let h = src.height as f32;
    let cx = (w - 1.0) / 2.0;
    let cy = (h - 1.0) / 2.0;
    let max_dist = (cx * cx + cy * cy).sqrt().max(1e-4);
    let mut result = RasterImage::new(src.width, src.height);
    for y in 0..src.height {
        for x in 0..src.width {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let center = src.pixel(x, y);
            let (ux, uy) = if dist > 1e-4 {
                (dx / dist, dy / dist)
            } else {
                (0.0, 0.0)
            };
            let off = amount * (dist / max_dist);
            let red = src.sample_bilinear(x as f32 + ux * off, y as f32 + uy * off);
            let blue = src.sample_bilinear(x as f32 - ux * off, y as f32 - uy * off);
            result.set_pixel(x, y, [red[0], center[1], blue[2], center[3]]);
        }
    }
    blend_result(img, &result, sel);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32, c: [u8; 4]) -> RasterImage {
        RasterImage::filled(w, h, c)
    }

    /// Hard vertical edge: left half black, right half white.
    fn edge_image(w: u32, h: u32, split: u32) -> RasterImage {
        let mut img = RasterImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if x < split { 0 } else { 255 };
                img.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        img
    }

    // ── surface_blur ──────────────────────────────────────────────────────────

    #[test]
    fn surface_blur_flat_identity() {
        let orig = flat(8, 8, [60, 120, 200, 255]);
        let mut img = orig.clone();
        surface_blur(&mut img, 3, 0.2, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn surface_blur_zero_radius_noop() {
        let orig = flat(5, 5, [10, 20, 30, 255]);
        let mut img = orig.clone();
        surface_blur(&mut img, 0, 0.5, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn surface_blur_preserves_edge_more_than_gaussian() {
        let orig = edge_image(9, 5, 5); // white starts at x=5; x=4 is black-side edge
        let (tx, ty) = (4u32, 2u32);

        let mut surf = orig.clone();
        surface_blur(&mut surf, 3, 0.05, None);

        let mut gauss = orig.clone();
        gaussian_blur(&mut gauss, 3.0, None);

        let orig_v = orig.pixel(tx, ty)[0] as i32; // 0
        let surf_dev = (surf.pixel(tx, ty)[0] as i32 - orig_v).abs();
        let gauss_dev = (gauss.pixel(tx, ty)[0] as i32 - orig_v).abs();

        // Surface blur should bleed across the hard edge far less than Gaussian.
        assert!(
            surf_dev < gauss_dev,
            "surface dev {surf_dev} should be < gaussian dev {gauss_dev}"
        );
    }

    #[test]
    fn surface_blur_preserves_alpha() {
        let mut img = flat(6, 6, [100, 100, 100, 123]);
        img.set_pixel(2, 2, [200, 50, 10, 77]);
        surface_blur(&mut img, 2, 0.3, None);
        for y in 0..6 {
            for x in 0..6 {
                // alpha untouched everywhere (only 77 at one pixel, else 123)
                let a = img.pixel(x, y)[3];
                assert!(a == 123 || a == 77);
            }
        }
    }

    // ── lens_blur ──────────────────────────────────────────────────────────────

    #[test]
    fn lens_blur_zero_radius_identity() {
        let orig = flat(7, 7, [33, 66, 99, 255]);
        let mut img = orig.clone();
        lens_blur(&mut img, 0.0, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn lens_blur_flat_identity() {
        let orig = flat(7, 7, [33, 66, 99, 255]);
        let mut img = orig.clone();
        lens_blur(&mut img, 3.0, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn lens_blur_spreads_a_dot() {
        let mut img = flat(11, 11, [0, 0, 0, 255]);
        img.set_pixel(5, 5, [255, 255, 255, 255]);
        lens_blur(&mut img, 3.0, None);
        // A neighbor that was black should now have picked up some light.
        assert!(img.pixel(5, 6)[0] > 0);
        // The center should have spread out (no longer fully saturated).
        assert!(img.pixel(5, 5)[0] < 255);
    }

    // ── smart_sharpen ───────────────────────────────────────────────────────────

    #[test]
    fn smart_sharpen_flat_unchanged() {
        let orig = flat(8, 8, [128, 128, 128, 255]);
        let mut img = orig.clone();
        smart_sharpen(&mut img, 2.0, 1.5, 0, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn smart_sharpen_zero_amount_noop() {
        let orig = edge_image(8, 8, 4);
        let mut img = orig.clone();
        smart_sharpen(&mut img, 0.0, 2.0, 0, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn smart_sharpen_increases_edge_contrast() {
        // Midtone edge (80 vs 180) so sharpening overshoots without clamping at
        // the 0/255 rails the way a pure black/white step would.
        let w = 9;
        let h = 5;
        let mut orig = RasterImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if x < 5 { 80 } else { 180 };
                orig.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let mut img = orig.clone();
        smart_sharpen(&mut img, 3.0, 2.0, 0, None);
        let mut changed = false;
        for x in 0..w {
            if img.pixel(x, 2) != orig.pixel(x, 2) {
                changed = true;
            }
        }
        assert!(changed, "smart_sharpen should alter pixels near an edge");
    }

    #[test]
    fn smart_sharpen_preserves_alpha() {
        let mut img = edge_image(9, 5, 5);
        for y in 0..5 {
            for x in 0..9 {
                let mut p = img.pixel(x, y);
                p[3] = 200;
                img.set_pixel(x, y, p);
            }
        }
        smart_sharpen(&mut img, 3.0, 2.0, 0, None);
        for y in 0..5 {
            for x in 0..9 {
                assert_eq!(img.pixel(x, y)[3], 200);
            }
        }
    }

    // ── reduce_noise ──────────────────────────────────────────────────────────

    fn variance_red(img: &RasterImage, x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
        let mut vals = Vec::new();
        for y in y0..y1 {
            for x in x0..x1 {
                vals.push(img.pixel(x, y)[0] as f32);
            }
        }
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32
    }

    #[test]
    fn reduce_noise_zero_strength_noop() {
        let orig = flat(6, 6, [40, 80, 120, 255]);
        let mut img = orig.clone();
        reduce_noise(&mut img, 0.0, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn reduce_noise_flat_identity() {
        let orig = flat(6, 6, [40, 80, 120, 255]);
        let mut img = orig.clone();
        reduce_noise(&mut img, 1.0, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn reduce_noise_lowers_variance_of_noisy_region() {
        // Constant 128 region with deterministic +/-30 checkerboard noise.
        let w = 10;
        let h = 10;
        let mut img = RasterImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let delta: i32 = if (x + y) % 2 == 0 { 30 } else { -30 };
                let v = (128 + delta).clamp(0, 255) as u8;
                img.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let before = variance_red(&img, 2, 2, 8, 8);
        reduce_noise(&mut img, 1.0, None);
        let after = variance_red(&img, 2, 2, 8, 8);
        assert!(
            after < before,
            "noise variance should drop: before {before}, after {after}"
        );
    }

    // ── clarity ────────────────────────────────────────────────────────────────

    #[test]
    fn clarity_zero_amount_noop() {
        let orig = edge_image(16, 16, 8);
        let mut img = orig.clone();
        clarity(&mut img, 0.0, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn clarity_flat_identity() {
        let orig = flat(16, 16, [128, 128, 128, 255]);
        let mut img = orig.clone();
        clarity(&mut img, 0.5, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn clarity_changes_midtone_edges() {
        // Midtone gradient edge (96 vs 160) so the midtone bell is active.
        let w = 16;
        let h = 8;
        let mut img = RasterImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 96 } else { 160 };
                img.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let orig = img.clone();
        clarity(&mut img, 0.8, None);
        assert_ne!(img, orig, "clarity should boost midtone local contrast");
    }

    // ── vignette ──────────────────────────────────────────────────────────────

    #[test]
    fn vignette_zero_amount_noop() {
        let orig = flat(9, 9, [100, 100, 100, 255]);
        let mut img = orig.clone();
        vignette(&mut img, 0.0, 0.5, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn vignette_darkens_corners_not_center() {
        let mut img = flat(11, 11, [100, 100, 100, 255]);
        vignette(&mut img, -0.6, 0.6, None);
        let center = img.pixel(5, 5)[0];
        let corner = img.pixel(0, 0)[0];
        assert_eq!(center, 100, "center should be unchanged");
        assert!(
            corner < center,
            "corner {corner} should be darker than center {center}"
        );
    }

    #[test]
    fn vignette_lightens_corners_when_positive() {
        let mut img = flat(11, 11, [100, 100, 100, 255]);
        vignette(&mut img, 0.5, 0.7, None);
        let center = img.pixel(5, 5)[0];
        let corner = img.pixel(0, 0)[0];
        assert_eq!(center, 100);
        assert!(
            corner > center,
            "corner {corner} should be brighter than center {center}"
        );
    }

    #[test]
    fn vignette_preserves_alpha() {
        let mut img = flat(9, 9, [100, 100, 100, 222]);
        vignette(&mut img, -0.8, 0.5, None);
        for y in 0..9 {
            for x in 0..9 {
                assert_eq!(img.pixel(x, y)[3], 222);
            }
        }
    }

    // ── chromatic_aberration ───────────────────────────────────────────────────

    #[test]
    fn chromatic_aberration_zero_amount_noop() {
        let orig = flat(11, 11, [50, 100, 150, 255]);
        let mut img = orig.clone();
        chromatic_aberration(&mut img, 0.0, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn chromatic_aberration_flat_identity() {
        // A flat image samples the same color everywhere, so no visible shift.
        let orig = flat(11, 11, [50, 100, 150, 255]);
        let mut img = orig.clone();
        chromatic_aberration(&mut img, 3.0, None);
        assert_eq!(img, orig);
    }

    #[test]
    fn chromatic_aberration_shifts_channels_at_edges() {
        // Horizontal RGB gradient so channels vary across x.
        let w = 11;
        let h = 11;
        let mut img = RasterImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let r = (x as i32 * 20).clamp(0, 255) as u8;
                let b = (255 - x as i32 * 20).clamp(0, 255) as u8;
                img.set_pixel(x, y, [r, 128, b, 255]);
            }
        }
        let orig = img.clone();
        chromatic_aberration(&mut img, 3.0, None);

        // Center pixel has zero radial distance -> unchanged.
        assert_eq!(img.pixel(5, 5), orig.pixel(5, 5));

        // An off-center pixel should have a shifted red or blue channel.
        let edge = img.pixel(9, 5);
        let edge_o = orig.pixel(9, 5);
        assert!(
            edge[0] != edge_o[0] || edge[2] != edge_o[2],
            "expected channel shift at edge: got {edge:?} vs {edge_o:?}"
        );
        // Green is never shifted.
        assert_eq!(edge[1], edge_o[1]);
    }

    // ── selection mask honored ──────────────────────────────────────────────────

    #[test]
    fn respects_selection_mask() {
        let orig = flat(8, 8, [0, 0, 0, 255]);
        let mut img = orig.clone();
        img.set_pixel(4, 4, [255, 255, 255, 255]);
        // Select only the left column region; right side must stay identical.
        let mut m = Mask::empty(8, 8);
        for y in 0..8 {
            for x in 0..4 {
                m.set(x, y, 255);
            }
        }
        let before = img.clone();
        lens_blur(&mut img, 3.0, Some(&m));
        // Right half (outside selection) unchanged.
        for y in 0..8 {
            for x in 4..8 {
                assert_eq!(img.pixel(x, y), before.pixel(x, y));
            }
        }
    }

    // ── BUG 1: no advanced filter panics on NaN / inf / huge inputs ─────────────

    #[test]
    fn no_panic_on_hostile_inputs() {
        let bad_f32 = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30, 0.0];
        let bad_u32 = [u32::MAX, u32::MAX - 1, 1_000_000, 0];

        for &f in &bad_f32 {
            let mut img = edge_image(4, 4, 2);
            lens_blur(&mut img, f, None);
            let mut img = edge_image(4, 4, 2);
            smart_sharpen(&mut img, f, f, 0, None);
            let mut img = edge_image(4, 4, 2);
            reduce_noise(&mut img, f, None);
            let mut img = edge_image(4, 4, 2);
            clarity(&mut img, f, None);
            let mut img = edge_image(4, 4, 2);
            vignette(&mut img, f, f, None);
            let mut img = edge_image(4, 4, 2);
            chromatic_aberration(&mut img, f, None);
            let mut img = edge_image(4, 4, 2);
            surface_blur(&mut img, 3, f, None);
        }

        for &n in &bad_u32 {
            let mut img = edge_image(4, 4, 2);
            surface_blur(&mut img, n, 0.3, None);
        }
    }

    // ── BUG 2: premultiplied disc blur does not darken opaque pixels ─────────────

    /// Opaque white next to fully-transparent stays ~white after a lens blur.
    #[test]
    fn lens_blur_premultiplied_no_dark_fringe() {
        let (w, h) = (16u32, 8u32);
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
        lens_blur(&mut img, 3.0, None);
        let p = img.pixel(w / 2 - 1, h / 2);
        assert!(
            p[0] > 240 && p[1] > 240 && p[2] > 240,
            "lens blur darkened opaque side: {p:?}"
        );
    }

    /// Surface (bilateral) blur preserves opaque RGB beside transparency too.
    #[test]
    fn surface_blur_premultiplied_no_dark_fringe() {
        let (w, h) = (16u32, 8u32);
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
        surface_blur(&mut img, 3, 0.9, None);
        let p = img.pixel(w / 2 - 1, h / 2);
        assert!(
            p[0] > 240 && p[1] > 240 && p[2] > 240,
            "surface blur darkened opaque side: {p:?}"
        );
    }
}
