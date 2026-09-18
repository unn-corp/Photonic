//! The brush family — paint, erase, clone, smudge, dodge/burn, sponge, and fills.
//!
//! A stroke is a polyline of points; round dabs are stamped along it at
//! `spacing`-controlled intervals. Within one stroke, coverage **accumulates by
//! flow** but is **capped by opacity** (Photoshop semantics): overlapping dabs
//! in a single stroke don't darken past the opacity ceiling. The color is then
//! composited through that accumulated coverage once.

use super::image::{luma, RasterImage};
use super::mask::Mask;

/// A round brush tip.
#[derive(Debug, Clone)]
pub struct Brush {
    /// Radius in pixels.
    pub radius: f32,
    /// Edge hardness, 0 (soft) .. 1 (hard). Below `hardness*radius` coverage is
    /// full; it falls off to 0 at `radius`.
    pub hardness: f32,
    /// Per-dab build-up, 0..1.
    pub flow: f32,
    /// Stroke opacity ceiling, 0..1.
    pub opacity: f32,
    /// Dab spacing as a fraction of diameter (0.1 = 10% of 2*radius).
    pub spacing: f32,
    /// Paint color (RGBA, straight alpha).
    pub color: [u8; 4],
}

/// Clamp a radius to a safe, finite range. Non-finite radii (NaN / ±inf) become
/// a usable default; everything else is clamped so downstream `ceil() as i64`
/// stamp-bounds math can never overflow.
#[inline]
fn sanitize_radius(r: f32) -> f32 {
    if r.is_finite() {
        r.clamp(0.5, 4096.0)
    } else {
        1.0
    }
}

/// Clamp a 0..1 parameter, substituting `default` for non-finite values.
#[inline]
fn clamp01(x: f32, default: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        default
    }
}

impl Brush {
    pub fn new(radius: f32, color: [u8; 4]) -> Self {
        Self {
            radius: sanitize_radius(radius),
            hardness: 0.8,
            flow: 1.0,
            opacity: 1.0,
            spacing: 0.1,
            color,
        }
    }

    /// A hard-edged pencil.
    pub fn pencil(radius: f32, color: [u8; 4]) -> Self {
        Self {
            hardness: 1.0,
            ..Self::new(radius, color)
        }
    }

    /// A copy with every parameter forced into a safe, finite range. All
    /// stamping/stroke code runs through this first so public fields mutated to
    /// NaN / inf / absurd values can never panic the rasterizer.
    fn sanitized(&self) -> Brush {
        Brush {
            radius: sanitize_radius(self.radius),
            hardness: clamp01(self.hardness, 0.8),
            flow: clamp01(self.flow, 1.0),
            opacity: clamp01(self.opacity, 1.0),
            spacing: if self.spacing.is_finite() {
                self.spacing.clamp(0.001, 100.0)
            } else {
                0.1
            },
            color: self.color,
        }
    }

    #[inline]
    fn dab_coverage(&self, dist: f32) -> f32 {
        if dist >= self.radius {
            return 0.0;
        }
        let inner = self.hardness * self.radius;
        if dist <= inner || self.radius - inner <= 1e-3 {
            1.0
        } else {
            // smooth falloff from inner..radius
            let t = (dist - inner) / (self.radius - inner);
            (1.0 - t).clamp(0.0, 1.0)
        }
    }
}

/// Drop any points with a non-finite (NaN / ±inf) coordinate, preserving order.
/// Stroke math (segment length, dab interpolation, stamp bounds) assumes finite
/// inputs; filtering here lets every public entry point accept arbitrary input.
fn clean_points(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    points
        .iter()
        .copied()
        .filter(|(x, y)| x.is_finite() && y.is_finite())
        .collect()
}

/// Inclusive integer bounds of a dab centered at `(cx, cy)` with integer radius
/// `r`, clamped into `[0, width-1] × [0, height-1]`. Uses saturating arithmetic
/// so absurd-but-finite centers (e.g. 1e30) produce empty ranges instead of
/// overflowing. Returns `(x0, x1, y0, y1)`; an empty range means "off image".
#[inline]
fn dab_bounds(cx: f32, cy: f32, r: i64, width: u32, height: u32) -> (i64, i64, i64, i64) {
    let icx = cx as i64; // saturating cast (NaN→0, ±inf→i64::MIN/MAX)
    let icy = cy as i64;
    let x0 = icx.saturating_sub(r).max(0);
    let x1 = icx.saturating_add(r).min(width as i64 - 1);
    let y0 = icy.saturating_sub(r).max(0);
    let y1 = icy.saturating_add(r).min(height as i64 - 1);
    (x0, x1, y0, y1)
}

/// Ordered list of dab centers along `points`, spaced by the brush's step.
/// `points` must already be finite (see [`clean_points`]); the per-segment dab
/// count is capped at `max_dabs` so a huge finite segment can't spin forever.
fn dab_centers(points: &[(f32, f32)], brush: &Brush, max_dabs: i64) -> Vec<(f32, f32)> {
    let mut centers = Vec::new();
    if points.is_empty() {
        return centers;
    }
    if points.len() == 1 {
        centers.push(points[0]);
        return centers;
    }
    let step = (brush.spacing * 2.0 * brush.radius).max(1.0);
    for w in points.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        let seg = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        // saturating cast handles overflow; clamp bounds runaway dab counts.
        let n = ((seg / step).floor() as i64).clamp(0, max_dabs);
        if n == 0 {
            centers.push((x0, y0));
            continue;
        }
        for k in 0..=n {
            let t = k as f32 / n as f32;
            centers.push((x0 + (x1 - x0) * t, y0 + (y1 - y0) * t));
        }
    }
    centers
}

/// A sane upper bound on the number of dabs per segment, scaled to image size so
/// a long legitimate stroke is fine but absurd coordinates can't hang the loop.
#[inline]
fn max_dabs_for(width: u32, height: u32) -> i64 {
    4 * (width as i64 + height as i64) + 4
}

/// Accumulate stroke coverage (0..1 per pixel) for `brush` along `points`.
fn stroke_coverage(width: u32, height: u32, points: &[(f32, f32)], brush: &Brush) -> Vec<f32> {
    let mut cov = vec![0.0f32; (width as usize) * (height as usize)];
    let centers = dab_centers(points, brush, max_dabs_for(width, height));
    let r = brush.radius.ceil() as i64;
    for (cx, cy) in centers {
        let (x0, x1, y0, y1) = dab_bounds(cx, cy, r, width, height);
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                let dx = xx as f32 + 0.5 - cx;
                let dy = yy as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt();
                let c = brush.dab_coverage(d) * brush.flow;
                if c > 0.0 {
                    let i = (yy as usize) * (width as usize) + xx as usize;
                    // build-up: combine like alpha over (1 - (1-a)(1-c))
                    cov[i] = 1.0 - (1.0 - cov[i]) * (1.0 - c);
                }
            }
        }
    }
    cov
}

#[inline]
fn over(dst: [u8; 4], src_rgb: [u8; 3], a: f32) -> [u8; 4] {
    let a = a.clamp(0.0, 1.0);
    let da = dst[3] as f32 / 255.0;
    let oa = a + da * (1.0 - a);
    let mut out = [0u8; 4];
    if oa > 0.0 {
        for c in 0..3 {
            let co = (src_rgb[c] as f32 / 255.0 * a + dst[c] as f32 / 255.0 * da * (1.0 - a)) / oa;
            out[c] = (co * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
    out
}

/// Paint a brush stroke of `brush.color` along `points`.
pub fn stroke(img: &mut RasterImage, points: &[(f32, f32)], brush: &Brush, sel: Option<&Mask>) {
    let brush = &brush.sanitized();
    let points = &clean_points(points);
    let cov = stroke_coverage(img.width, img.height, points, brush);
    let rgb = [brush.color[0], brush.color[1], brush.color[2]];
    let src_a = brush.color[3] as f32 / 255.0;
    for y in 0..img.height {
        for x in 0..img.width {
            let mut a =
                cov[(y as usize) * (img.width as usize) + x as usize] * brush.opacity * src_a;
            if a <= 0.0 {
                continue;
            }
            if let Some(m) = sel {
                a *= m.coverage(x, y);
            }
            let out = over(img.pixel(x, y), rgb, a);
            img.set_pixel(x, y, out);
        }
    }
}

/// Erase along a stroke — reduces destination alpha by the brush coverage.
pub fn erase(img: &mut RasterImage, points: &[(f32, f32)], brush: &Brush, sel: Option<&Mask>) {
    let brush = &brush.sanitized();
    let points = &clean_points(points);
    let cov = stroke_coverage(img.width, img.height, points, brush);
    for y in 0..img.height {
        for x in 0..img.width {
            let mut a = cov[(y as usize) * (img.width as usize) + x as usize] * brush.opacity;
            if a <= 0.0 {
                continue;
            }
            if let Some(m) = sel {
                a *= m.coverage(x, y);
            }
            let mut px = img.pixel(x, y);
            px[3] = (px[3] as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
            img.set_pixel(x, y, px);
        }
    }
}

/// Clone-stamp: paint along `points`, sampling source pixels offset by
/// `(src_dx, src_dy)` from each painted pixel.
pub fn clone_stamp(
    img: &mut RasterImage,
    points: &[(f32, f32)],
    brush: &Brush,
    src_dx: i64,
    src_dy: i64,
    sel: Option<&Mask>,
) {
    let brush = &brush.sanitized();
    let points = &clean_points(points);
    let cov = stroke_coverage(img.width, img.height, points, brush);
    let snapshot = img.clone();
    for y in 0..img.height {
        for x in 0..img.width {
            let mut a = cov[(y as usize) * (img.width as usize) + x as usize] * brush.opacity;
            if a <= 0.0 {
                continue;
            }
            if let Some(m) = sel {
                a *= m.coverage(x, y);
            }
            let s = snapshot.sample_clamped(x as i64 - src_dx, y as i64 - src_dy);
            let sa = s[3] as f32 / 255.0;
            let out = over(img.pixel(x, y), [s[0], s[1], s[2]], a * sa);
            img.set_pixel(x, y, out);
        }
    }
}

/// Dodge (lighten) under the brush by `amount` (0..1).
pub fn dodge(
    img: &mut RasterImage,
    points: &[(f32, f32)],
    brush: &Brush,
    amount: f32,
    sel: Option<&Mask>,
) {
    tone(img, points, brush, amount, true, sel);
}

/// Burn (darken) under the brush by `amount` (0..1).
pub fn burn(
    img: &mut RasterImage,
    points: &[(f32, f32)],
    brush: &Brush,
    amount: f32,
    sel: Option<&Mask>,
) {
    tone(img, points, brush, amount, false, sel);
}

fn tone(
    img: &mut RasterImage,
    points: &[(f32, f32)],
    brush: &Brush,
    amount: f32,
    lighten: bool,
    sel: Option<&Mask>,
) {
    let brush = &brush.sanitized();
    let points = &clean_points(points);
    let cov = stroke_coverage(img.width, img.height, points, brush);
    let amount = if amount.is_finite() {
        amount.clamp(0.0, 1.0)
    } else {
        0.0
    };
    for y in 0..img.height {
        for x in 0..img.width {
            let mut a = cov[(y as usize) * (img.width as usize) + x as usize] * amount;
            if a <= 0.0 {
                continue;
            }
            if let Some(m) = sel {
                a *= m.coverage(x, y);
            }
            let mut px = img.pixel(x, y);
            for c in 0..3 {
                let v = px[c] as f32 / 255.0;
                let nv = if lighten {
                    v + (1.0 - v) * a
                } else {
                    v * (1.0 - a)
                };
                px[c] = (nv * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            img.set_pixel(x, y, px);
        }
    }
}

/// Sponge: increase (`saturate=true`) or decrease saturation under the brush.
pub fn sponge(
    img: &mut RasterImage,
    points: &[(f32, f32)],
    brush: &Brush,
    saturate: bool,
    amount: f32,
    sel: Option<&Mask>,
) {
    let brush = &brush.sanitized();
    let points = &clean_points(points);
    let cov = stroke_coverage(img.width, img.height, points, brush);
    let amount = if amount.is_finite() {
        amount.clamp(0.0, 1.0)
    } else {
        0.0
    };
    for y in 0..img.height {
        for x in 0..img.width {
            let mut a = cov[(y as usize) * (img.width as usize) + x as usize] * amount;
            if a <= 0.0 {
                continue;
            }
            if let Some(m) = sel {
                a *= m.coverage(x, y);
            }
            let mut px = img.pixel(x, y);
            let l = luma([
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ]);
            for c in 0..3 {
                let v = px[c] as f32 / 255.0;
                let nv = if saturate {
                    v + (v - l) * a
                } else {
                    v + (l - v) * a
                };
                px[c] = (nv * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            img.set_pixel(x, y, px);
        }
    }
}

/// Coverage-weighted average RGBA (as 0..255 floats) of the pixels under a dab
/// centered at `(cx, cy)`. This is the color the brush "picks up". Falls back to
/// a bilinear sample at the center if the dab covers no in-bounds pixels.
fn sample_under_brush(img: &RasterImage, cx: f32, cy: f32, brush: &Brush) -> [f32; 4] {
    let r = brush.radius.ceil() as i64;
    let (x0, x1, y0, y1) = dab_bounds(cx, cy, r, img.width, img.height);
    let mut acc = [0.0f32; 4];
    let mut wsum = 0.0f32;
    for yy in y0..=y1 {
        for xx in x0..=x1 {
            let dx = xx as f32 + 0.5 - cx;
            let dy = yy as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let c = brush.dab_coverage(d);
            if c > 0.0 {
                let p = img.pixel(xx as u32, yy as u32);
                for k in 0..4 {
                    acc[k] += p[k] as f32 * c;
                }
                wsum += c;
            }
        }
    }
    if wsum > 0.0 {
        for k in 0..4 {
            acc[k] /= wsum;
        }
        acc
    } else {
        let p = img.sample_bilinear(cx, cy);
        [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]
    }
}

/// Smudge: drag color along the stroke, like a finger through wet paint.
///
/// Walks the dab centers in order carrying a "picked-up" color. At each dab the
/// carried color is blended into the pixels under the brush by
/// `strength * coverage` (respecting `brush.opacity`, falloff, and the optional
/// selection), then the carried color is refreshed toward the freshly sampled
/// color at the new location (lerp by `strength`). This drags color in the
/// stroke direction. Fully deterministic.
pub fn smudge(
    img: &mut RasterImage,
    points: &[(f32, f32)],
    brush: &Brush,
    strength: f32,
    sel: Option<&Mask>,
) {
    let brush = &brush.sanitized();
    let points = &clean_points(points);
    let strength = if strength.is_finite() {
        strength.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if strength <= 0.0 {
        return;
    }
    let centers = dab_centers(points, brush, max_dabs_for(img.width, img.height));
    if centers.len() < 2 {
        return; // need at least one move to drag color
    }

    let r = brush.radius.ceil() as i64;
    // Pick up the color under the brush at the stroke's start.
    let mut carried = sample_under_brush(img, centers[0].0, centers[0].1, brush);

    for &(cx, cy) in &centers[1..] {
        // Fresh color at the new location, sampled before we modify it.
        let picked = sample_under_brush(img, cx, cy, brush);

        // Blend the carried color into the pixels under the brush.
        let (x0, x1, y0, y1) = dab_bounds(cx, cy, r, img.width, img.height);
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                let dx = xx as f32 + 0.5 - cx;
                let dy = yy as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt();
                let cov = brush.dab_coverage(d);
                if cov <= 0.0 {
                    continue;
                }
                let mut f = strength * cov * brush.opacity;
                if let Some(m) = sel {
                    f *= m.coverage(xx as u32, yy as u32);
                }
                if f <= 0.0 {
                    continue;
                }
                let px = img.pixel(xx as u32, yy as u32);
                let mut out = [0u8; 4];
                for k in 0..4 {
                    let v = px[k] as f32 * (1.0 - f) + carried[k] * f;
                    out[k] = v.round().clamp(0.0, 255.0) as u8;
                }
                img.set_pixel(xx as u32, yy as u32, out);
            }
        }

        // Refresh the carried color toward the new location's color.
        for k in 0..4 {
            carried[k] = carried[k] * (1.0 - strength) + picked[k] * strength;
        }
    }
}

/// Paint-bucket flood fill from a seed by color tolerance (0..1).
pub fn bucket_fill(
    img: &mut RasterImage,
    seed_x: u32,
    seed_y: u32,
    color: [u8; 4],
    tolerance: f32,
) {
    let tolerance = if tolerance.is_finite() {
        tolerance.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let region = Mask::magic_wand(img, seed_x, seed_y, tolerance);
    let a = color[3] as f32 / 255.0;
    for y in 0..img.height {
        for x in 0..img.width {
            let c = region.coverage(x, y);
            if c <= 0.0 {
                continue;
            }
            let out = over(img.pixel(x, y), [color[0], color[1], color[2]], a * c);
            img.set_pixel(x, y, out);
        }
    }
}

/// Linear gradient fill from `(x0,y0)`→`(x1,y1)` between two RGBA colors,
/// confined to an optional selection.
pub fn gradient_fill(
    img: &mut RasterImage,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    c0: [u8; 4],
    c1: [u8; 4],
    sel: Option<&Mask>,
) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len2 = (dx * dx + dy * dy).max(1e-6);
    for y in 0..img.height {
        for x in 0..img.width {
            let px = x as f32 + 0.5 - x0;
            let py = y as f32 + 0.5 - y0;
            let t = ((px * dx + py * dy) / len2).clamp(0.0, 1.0);
            let col = super::lerp_rgba(c0, c1, t);
            let mut a = col[3] as f32 / 255.0;
            if let Some(m) = sel {
                a *= m.coverage(x, y);
            }
            let out = over(img.pixel(x, y), [col[0], col[1], col[2]], a);
            img.set_pixel(x, y, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_paints_center() {
        let mut img = RasterImage::new(20, 20);
        let b = Brush::new(4.0, [255, 0, 0, 255]);
        stroke(&mut img, &[(10.0, 10.0)], &b, None);
        assert_eq!(img.pixel(10, 10), [255, 0, 0, 255]);
        assert_eq!(img.pixel(0, 0), [0, 0, 0, 0]); // far away untouched
    }

    #[test]
    fn erase_removes_alpha() {
        let mut img = RasterImage::filled(10, 10, [0, 0, 0, 255]);
        let b = Brush::pencil(3.0, [0, 0, 0, 255]);
        erase(&mut img, &[(5.0, 5.0)], &b, None);
        assert!(img.pixel(5, 5)[3] < 255);
    }

    #[test]
    fn pencil_is_hard_edged() {
        let mut img = RasterImage::new(20, 20);
        let b = Brush::pencil(5.0, [255, 255, 255, 255]);
        stroke(&mut img, &[(10.0, 10.0)], &b, None);
        // a pixel just inside the radius is fully opaque
        assert_eq!(img.pixel(13, 10)[3], 255);
    }

    #[test]
    fn bucket_fills_contiguous() {
        let mut img = RasterImage::filled(6, 6, [255, 255, 255, 255]);
        bucket_fill(&mut img, 0, 0, [0, 0, 255, 255], 0.1);
        assert_eq!(img.pixel(3, 3), [0, 0, 255, 255]);
    }

    #[test]
    fn gradient_endpoints() {
        let mut img = RasterImage::new(10, 1);
        gradient_fill(
            &mut img,
            0.0,
            0.0,
            10.0,
            0.0,
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            None,
        );
        assert!(img.pixel(0, 0)[0] < 30);
        assert!(img.pixel(9, 0)[0] > 220);
    }

    #[test]
    fn dodge_lightens_burn_darkens() {
        let mut img = RasterImage::filled(10, 10, [100, 100, 100, 255]);
        let b = Brush::new(3.0, [0, 0, 0, 255]);
        dodge(&mut img, &[(5.0, 5.0)], &b, 0.5, None);
        assert!(img.pixel(5, 5)[0] > 100);
        let mut img2 = RasterImage::filled(10, 10, [100, 100, 100, 255]);
        burn(&mut img2, &[(5.0, 5.0)], &b, 0.5, None);
        assert!(img2.pixel(5, 5)[0] < 100);
    }

    #[test]
    fn smudge_drags_color() {
        // Left half red, right half blue.
        let mut img = RasterImage::new(20, 10);
        for y in 0..10 {
            for x in 0..20 {
                let c = if x < 10 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                img.set_pixel(x, y, c);
            }
        }
        let b = Brush::new(3.0, [0, 0, 0, 255]);
        // Drag from the red region into the blue region.
        smudge(&mut img, &[(5.0, 5.0), (15.0, 5.0)], &b, 0.8, None);
        // Just past the boundary the dragged red is now visible on top of blue.
        let p = img.pixel(11, 5);
        assert!(p[0] > 60, "expected dragged red, got {:?}", p);
        // ...but it's an intermediate, not pure red — blue is still present.
        assert!(
            p[2] > 20 && p[0] < 255,
            "expected intermediate color, got {:?}",
            p
        );
    }

    #[test]
    fn smudge_handles_bad_input() {
        let mut img = RasterImage::filled(10, 10, [255, 0, 0, 255]);
        let b = Brush::new(3.0, [0, 0, 0, 255]);
        // NaN/inf points, empty points, and out-of-range strength must not panic.
        smudge(
            &mut img,
            &[(f32::NAN, 5.0), (5.0, 5.0), (f32::INFINITY, 1.0)],
            &b,
            0.5,
            None,
        );
        smudge(&mut img, &[], &b, 0.5, None);
        smudge(&mut img, &[(2.0, 2.0), (8.0, 8.0)], &b, f32::NAN, None);
    }

    #[test]
    fn brush_new_sanitizes_radius() {
        for r in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -5.0, 0.0] {
            let b = Brush::new(r, [255, 0, 0, 255]);
            assert!(b.radius.is_finite(), "radius not finite for input {r}");
            assert!(
                b.radius >= 0.5 && b.radius <= 4096.0,
                "radius {} out of range for {r}",
                b.radius
            );
        }
        let p = Brush::pencil(f32::NAN, [0, 0, 0, 255]);
        assert!(p.radius.is_finite());
        // A sanitized brush is still usable for painting.
        let mut img = RasterImage::new(10, 10);
        let b = Brush::new(f32::NAN, [255, 0, 0, 255]);
        stroke(&mut img, &[(5.0, 5.0)], &b, None);
        assert!(img.pixel(5, 5)[3] > 0);
    }

    #[test]
    fn stroke_survives_nan_and_huge_input() {
        let mut img = RasterImage::new(10, 10);
        let b = Brush::new(3.0, [255, 0, 0, 255]);
        // NaN / inf / huge-but-finite points are filtered or bounded — no panic, no hang.
        stroke(
            &mut img,
            &[
                (f32::NAN, 5.0),
                (5.0, 5.0),
                (1e30, -1e30),
                (f32::INFINITY, f32::NAN),
            ],
            &b,
            None,
        );
        assert!(img.pixel(5, 5)[3] > 0); // the one finite point still painted

        // A brush whose public fields were corrupted must not panic the rasterizer.
        let mut bad = Brush::new(3.0, [0, 255, 0, 255]);
        bad.radius = f32::NAN;
        bad.hardness = f32::INFINITY;
        bad.flow = f32::NAN;
        bad.opacity = -1.0;
        bad.spacing = f32::NAN;
        let mut img2 = RasterImage::new(10, 10);
        stroke(&mut img2, &[(2.0, 2.0), (7.0, 7.0)], &bad, None);
        erase(&mut img2, &[(f32::NAN, 1.0)], &bad, None);
        clone_stamp(&mut img2, &[(3.0, 3.0)], &bad, 1, 1, None);
        dodge(&mut img2, &[(4.0, 4.0)], &bad, f32::NAN, None);
        bucket_fill(&mut img2, 0, 0, [1, 2, 3, 255], f32::NAN);
        gradient_fill(
            &mut img2,
            f32::NAN,
            0.0,
            f32::INFINITY,
            1.0,
            [0; 4],
            [255; 4],
            None,
        );
    }

    #[test]
    fn selection_confines_paint() {
        let mut img = RasterImage::new(20, 20);
        let b = Brush::new(8.0, [255, 0, 0, 255]);
        let sel = Mask::rect(20, 20, 10, 0, 10, 20); // right half only
        stroke(&mut img, &[(10.0, 10.0)], &b, Some(&sel));
        assert_eq!(img.pixel(5, 10)[3], 0); // left half blocked
        assert!(img.pixel(13, 10)[3] > 0); // right half painted
    }
}
