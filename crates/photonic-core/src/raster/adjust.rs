//! Image > Adjustments — Photoshop-grade tonal and color adjustments.
//!
//! Pure CPU, deterministic, unit-tested point operations. Every public function
//! mutates a [`RasterImage`] in place and takes an optional selection
//! [`Mask`] as its **last** parameter, routing through
//! [`crate::raster::apply_point`] so masking (feathered selections, partial
//! coverage) is handled uniformly: where the mask coverage is `< 1` the result
//! is blended back toward the original pixel.
//!
//! Color math is done in `f32` (0..1 sRGB) per pixel; the public buffer stays
//! compact 8-bit RGBA. **Alpha is always preserved** — these are color/tone
//! transforms, never compositing operations.
//!
//! Adjustments here mirror the entries under Photoshop's *Image > Adjustments*
//! menu: Brightness/Contrast, Levels, Curves, Exposure, Hue/Saturation, Color
//! Balance, Vibrance, Desaturate, Black & White, Invert, Posterize, Threshold,
//! Photo Filter, Channel Mixer, Gradient Map, Selective Color,
//! Shadows/Highlights, Gamma, Auto Contrast and Auto Levels.

use crate::raster::{
    apply_point, blend_result,
    image::{luma, RasterImage},
    mask::Mask,
};

// ── small helpers ────────────────────────────────────────────────────────────

/// Clamp a scalar into the 0..=1 range.
#[inline]
fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// Smooth Hermite interpolation between `edge0` and `edge1` (the GLSL
/// `smoothstep`). Returns 0 below `edge0`, 1 above `edge1`, and an S-curve
/// in between. Used to give Shadows/Highlights a soft tonal-width falloff.
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if (edge1 - edge0).abs() < 1e-9 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ── parameter sanitization (panic / NaN safety) ────────────────────────────────
//
// Every public adjustment guards its inputs so that non-finite (NaN / ±inf)
// parameters degrade to an identity no-op instead of producing a solid-black
// (or NaN-poisoned) image or panicking. Finite scalars are additionally clamped
// to each adjustment's valid range.

/// True only when every component of an RGB triple is finite.
#[inline]
fn finite3(a: [f32; 3]) -> bool {
    a[0].is_finite() && a[1].is_finite() && a[2].is_finite()
}

/// Clamp a finite scalar into `[lo, hi]`. Returns `None` for non-finite input so
/// callers can early-return as a no-op.
#[inline]
fn san(v: f32, lo: f32, hi: f32) -> Option<f32> {
    if v.is_finite() {
        Some(v.clamp(lo, hi))
    } else {
        None
    }
}

/// Linearize a single sRGB channel (0..1) into linear light.
#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    let c = clamp01(c);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Encode a single linear-light channel back to sRGB (0..1).
#[inline]
fn linear_to_srgb(c: f32) -> f32 {
    let c = clamp01(c);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Decompose an 8-bit RGBA pixel into 0..1 RGB plus the raw alpha byte.
#[inline]
fn to_f(px: [u8; 4]) -> ([f32; 3], u8) {
    (
        [
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
        ],
        px[3],
    )
}

/// Recompose an 8-bit RGBA pixel from 0..1 RGB (clamped) and the original alpha.
#[inline]
fn from_f(rgb: [f32; 3], a: u8) -> [u8; 4] {
    [
        (clamp01(rgb[0]) * 255.0).round() as u8,
        (clamp01(rgb[1]) * 255.0).round() as u8,
        (clamp01(rgb[2]) * 255.0).round() as u8,
        a,
    ]
}

/// Convert sRGB (0..1) to HSL: hue in degrees 0..360, saturation and lightness 0..1.
fn rgb_to_hsl(rgb: [f32; 3]) -> [f32; 3] {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    let s = if d.abs() < 1e-9 {
        0.0
    } else {
        d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-9)
    };
    let h = if d.abs() < 1e-9 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    [h.rem_euclid(360.0), clamp01(s), clamp01(l)]
}

/// Convert HSL (hue degrees, sat 0..1, light 0..1) back to sRGB (0..1).
fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let h = hsl[0].rem_euclid(360.0);
    let s = clamp01(hsl[1]);
    let l = clamp01(hsl[2]);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - ((hp.rem_euclid(2.0)) - 1.0).abs());
    let (r1, g1, b1) = match hp.floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r1 + m, g1 + m, b1 + m]
}

/// Run a per-channel RGB transform as a masked point operation, preserving alpha.
#[inline]
fn map_point(img: &mut RasterImage, sel: Option<&Mask>, mut f: impl FnMut([f32; 3]) -> [f32; 3]) {
    apply_point(img, sel, move |px| {
        let (rgb, a) = to_f(px);
        from_f(f(rgb), a)
    });
}

// ── adjustments ──────────────────────────────────────────────────────────────

/// Brightness/Contrast. `brightness` and `contrast` are both -1..1. Brightness
/// is an additive offset; contrast scales each channel around the 0.5 midpoint.
pub fn brightness_contrast(
    img: &mut RasterImage,
    brightness: f32,
    contrast: f32,
    sel: Option<&Mask>,
) {
    let (brightness, contrast) = match (san(brightness, -1.0, 1.0), san(contrast, -1.0, 1.0)) {
        (Some(b), Some(c)) => (b, c),
        _ => return, // non-finite input → no-op
    };
    let c = contrast.clamp(-1.0, 1.0);
    // Slope: 1.0 at c=0, → steep as c→1, → 0 (flat gray) at c=-1.
    let slope = if c >= 0.0 {
        1.0 / (1.0 - c.min(0.999))
    } else {
        1.0 + c
    };
    map_point(img, sel, move |rgb| {
        let mut out = [0.0; 3];
        for i in 0..3 {
            let v = clamp01(rgb[i] + brightness);
            out[i] = clamp01((v - 0.5) * slope + 0.5);
        }
        out
    });
}

/// Shared levels math. Managed video keeps the normalized out-of-range
/// residual as linear tails around the bounded gamma correction. Raster retains
/// its original input/output clamps; no byte quantization enters native video.
pub fn levels_rgb(
    rgb: [f32; 3],
    black_white: [f32; 2],
    gamma: f32,
    output: [f32; 2],
    extended: bool,
) -> [f32; 3] {
    let ib = clamp01(black_white[0]);
    let iw = clamp01(black_white[1]);
    let ob = clamp01(output[0]);
    let ow = clamp01(output[1]);
    let span = (iw - ib).max(1e-4);
    let inv_gamma = 1.0 / gamma.clamp(0.01, 9.99);
    rgb.map(|c| {
        let v = (c - ib) / span;
        let b = clamp01(v);
        let corrected = b.powf(inv_gamma);
        let mapped = if extended {
            corrected + (v - b)
        } else {
            corrected
        };
        let result = ob + mapped * (ow - ob);
        if extended {
            result
        } else {
            clamp01(result)
        }
    })
}

/// Levels. Maps input range `[in_black, in_white]` (0..1) through a midtone
/// `gamma` (~0.1..9.99) onto output range `[out_black, out_white]` (0..1).
pub fn levels(
    img: &mut RasterImage,
    in_black: f32,
    in_white: f32,
    gamma: f32,
    out_black: f32,
    out_white: f32,
    sel: Option<&Mask>,
) {
    if !(in_black.is_finite()
        && in_white.is_finite()
        && gamma.is_finite()
        && out_black.is_finite()
        && out_white.is_finite())
    {
        return; // non-finite input → no-op
    }
    map_point(img, sel, move |rgb| {
        levels_rgb(
            rgb,
            [in_black, in_white],
            gamma,
            [out_black, out_white],
            false,
        )
    });
}

/// Prepared floating Hermite curve shared by raster and managed video.
#[derive(Clone, Debug)]
pub struct TransferCurve {
    coefficients: Vec<[f32; 3]>,
}
impl TransferCurve {
    pub fn new(points: &[(f32, f32)]) -> Self {
        // Sanitize: finite-only, clamp to 0..1, sort by x, drop coincident x's.
        let mut pts: Vec<(f32, f32)> = points
            .iter()
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .map(|(x, y)| (clamp01(*x), clamp01(*y)))
            .collect();
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);

        let n = pts.len();
        if n < 2 {
            return Self {
                coefficients: Vec::new(),
            };
        }

        // Secant slopes between consecutive knots.
        let mut d = vec![0.0f32; n - 1];
        for i in 0..n - 1 {
            d[i] = (pts[i + 1].1 - pts[i].1) / (pts[i + 1].0 - pts[i].0);
        }

        // Initial Hermite tangents.
        let mut m = vec![0.0f32; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            if d[i - 1] * d[i] <= 0.0 {
                // Local extremum (or flat) — force a flat tangent to preserve monotonicity.
                m[i] = 0.0;
            } else {
                m[i] = (d[i - 1] + d[i]) / 2.0;
            }
        }

        // Fritsch–Carlson monotonicity limiting on each interval.
        for i in 0..n - 1 {
            if d[i].abs() < 1e-12 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
                continue;
            }
            let alpha = m[i] / d[i];
            let beta = m[i + 1] / d[i];
            let s = alpha * alpha + beta * beta;
            if s > 9.0 {
                let tau = 3.0 / s.sqrt();
                m[i] = tau * alpha * d[i];
                m[i + 1] = tau * beta * d[i];
            }
        }

        Self {
            coefficients: pts
                .into_iter()
                .zip(m)
                .map(|((x, y), slope)| [x, y, slope])
                .collect(),
        }
    }
    pub fn coefficients(&self) -> &[[f32; 3]] {
        &self.coefficients
    }
    pub fn is_identity(&self) -> bool {
        self.coefficients.iter().all(|k| k[0] == k[1])
    }
    pub fn sample(&self, x: f32, extend: bool) -> f32 {
        let pts = &self.coefficients;
        let n = pts.len();
        if n < 2 {
            return x;
        }
        if x <= pts[0][0] {
            return pts[0][1]
                + if extend {
                    (x - pts[0][0]) * pts[0][2]
                } else {
                    0.0
                };
        }
        if x >= pts[n - 1][0] {
            return pts[n - 1][1]
                + if extend {
                    (x - pts[n - 1][0]) * pts[n - 1][2]
                } else {
                    0.0
                };
        }
        for k in 0..n - 1 {
            let [x0, y0, m0] = pts[k];
            let [x1, y1, m1] = pts[k + 1];
            if x >= x0 && x <= x1 {
                let h = x1 - x0;
                let t = (x - x0) / h;
                let t2 = t * t;
                let t3 = t2 * t;
                let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
                let h10 = t3 - 2.0 * t2 + t;
                let h01 = -2.0 * t3 + 3.0 * t2;
                let h11 = t3 - t2;
                return h00 * y0 + h10 * h * m0 + h01 * y1 + h11 * h * m1;
            }
        }
        pts[n - 1][1]
    }
}
/// Catalog knot ordering, duplicate policy and midtone convenience control.
pub fn catalog_curve_points(knots: [[f32; 2]; 5], contrast: f32) -> Vec<(f32, f32)> {
    let mut pts: Vec<_> = knots.into_iter().map(|[x, y]| (x, y)).collect();
    if contrast.abs() > 1e-6 {
        pts[2].1 = (0.5 + contrast * 0.25).clamp(0.05, 0.95);
    }
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
    if pts.len() < 2 {
        pts = vec![(0.0, 0.0), (1.0, 1.0)];
    }
    pts
}
/// Build a 256-entry LUT from control points (0..1) using a **smooth
/// monotone-cubic (Fritsch–Carlson) Hermite spline**, mapping input index
/// `i/255` to an output 0..255 byte.
///
/// This mirrors Photoshop's *smooth* curve mode, where the curve bows between
/// knots rather than connecting them with straight segments, while the
/// Fritsch–Carlson tangent limiting guarantees the result is monotone (no
/// overshoot / ringing) when the control points are monotone.
///
/// Sanitization: non-finite control points are dropped and the rest are clamped
/// to 0..1 and sorted by x (with coincident x's collapsed). Fewer than two valid
/// points yields an **identity** LUT, so a NaN-poisoned channel is a safe no-op.
fn curve_lut(points: &[(f32, f32)]) -> [u8; 256] {
    let curve = TransferCurve::new(points);
    let mut lut = [0; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        *slot = (clamp01(curve.sample(i as f32 / 255.0, false)) * 255.0).round() as u8;
    }
    lut
}

/// Curves — Photoshop's per-channel model.
///
/// Control points are `(input, output)` pairs in 0..1. The composite `rgb` curve
/// is applied to **all three channels first**, then each per-channel curve
/// (`red`, `green`, `blue`) is applied on top. An **empty slice** for any channel
/// means identity for that channel. Up to four 256-entry LUTs are precomputed
/// (smooth monotone-cubic interpolation) and composed via LUT chaining.
pub fn curves(
    img: &mut RasterImage,
    rgb: &[(f32, f32)],
    red: &[(f32, f32)],
    green: &[(f32, f32)],
    blue: &[(f32, f32)],
    sel: Option<&Mask>,
) {
    let comp = curve_lut(rgb);
    let lr = curve_lut(red);
    let lg = curve_lut(green);
    let lb = curve_lut(blue);
    apply_point(img, sel, move |px| {
        [
            lr[comp[px[0] as usize] as usize],
            lg[comp[px[1] as usize] as usize],
            lb[comp[px[2] as usize] as usize],
            px[3],
        ]
    });
}

/// Exposure. A photographic stop-based exposure change performed in **linear
/// light**: each channel is linearized from sRGB, multiplied by `2^stops`, then
/// re-encoded to sRGB. This matches Photoshop's behavior far better than scaling
/// the gamma-encoded bytes directly (e.g. +1 stop on a mid value lands near 145,
/// not a naive doubling).
pub fn exposure(img: &mut RasterImage, stops: f32, sel: Option<&Mask>) {
    let stops = match san(stops, -30.0, 30.0) {
        Some(s) => s,
        None => return, // non-finite input → no-op
    };
    let factor = 2f32.powf(stops);
    map_point(img, sel, move |rgb| {
        let mut out = [0.0; 3];
        for i in 0..3 {
            let lin = srgb_to_linear(rgb[i]) * factor;
            out[i] = clamp01(linear_to_srgb(lin));
        }
        out
    });
}

/// Hue/Saturation. `hue_deg` rotates hue; `sat` (-1..1) scales saturation;
/// `lightness` (-1..1) lifts toward white (positive) or crushes toward black
/// (negative). Done in HSL space.
pub fn hue_saturation(
    img: &mut RasterImage,
    hue_deg: f32,
    sat: f32,
    lightness: f32,
    sel: Option<&Mask>,
) {
    if !(hue_deg.is_finite() && sat.is_finite() && lightness.is_finite()) {
        return; // non-finite input → no-op
    }
    map_point(img, sel, move |rgb| {
        let mut hsl = rgb_to_hsl(rgb);
        hsl[0] = (hsl[0] + hue_deg).rem_euclid(360.0);
        hsl[1] = clamp01(hsl[1] * (1.0 + sat.clamp(-1.0, 1.0)));
        let l = hsl[2];
        hsl[2] = if lightness >= 0.0 {
            l + (1.0 - l) * lightness.clamp(0.0, 1.0)
        } else {
            l * (1.0 + lightness.clamp(-1.0, 0.0))
        };
        hsl_to_rgb(hsl)
    });
}

/// Tonal-range weights (shadow, midtone, highlight) for a given luma 0..1.
#[inline]
fn tonal_weights(l: f32) -> (f32, f32, f32) {
    let shadow = (1.0 - 2.0 * l).clamp(0.0, 1.0);
    let highlight = (2.0 * l - 1.0).clamp(0.0, 1.0);
    let mid = (1.0 - shadow - highlight).clamp(0.0, 1.0);
    (shadow, mid, highlight)
}

/// Shared transfer-space tonal color shifts. Native video disables only the
/// raster RGB clamps; tonal masks retain their bounded mathematical definition.
pub fn color_balance_rgb(
    rgb: [f32; 3],
    shifts: [[f32; 3]; 3],
    preserve: bool,
    raster_clamp: bool,
) -> [f32; 3] {
    let (sw, mw, hw) = tonal_weights(luma(rgb));
    let mut out = [0.0; 3];
    for i in 0..3 {
        let shift = shifts[0][i] * sw + shifts[1][i] * mw + shifts[2][i] * hw;
        out[i] = rgb[i] + 0.5 * shift;
        if raster_clamp {
            out[i] = clamp01(out[i]);
        }
    }
    if preserve {
        let original = luma(rgb);
        let after = luma(out);
        if after > 1e-4 {
            let factor = original / after;
            for v in &mut out {
                *v *= factor;
                if raster_clamp {
                    *v = clamp01(*v);
                }
            }
        }
    }
    out
}

/// Color Balance. Per-channel shifts (-1..1 each) for `shadows`, `midtones` and
/// `highlights`, weighted by where each pixel falls in the tonal range. When
/// `preserve_luminosity` is set, each pixel's original luma is restored after the
/// color shift (the shift then only moves chroma), matching Photoshop's
/// *Preserve Luminosity* option (as `photo_filter` does).
pub fn color_balance(
    img: &mut RasterImage,
    shadows: [f32; 3],
    midtones: [f32; 3],
    highlights: [f32; 3],
    preserve_luminosity: bool,
    sel: Option<&Mask>,
) {
    if !(finite3(shadows) && finite3(midtones) && finite3(highlights)) {
        return; // non-finite input → no-op
    }
    let clamp3 = |a: [f32; 3]| {
        [
            a[0].clamp(-1.0, 1.0),
            a[1].clamp(-1.0, 1.0),
            a[2].clamp(-1.0, 1.0),
        ]
    };
    let shadows = clamp3(shadows);
    let midtones = clamp3(midtones);
    let highlights = clamp3(highlights);
    map_point(img, sel, move |rgb| {
        color_balance_rgb(
            rgb,
            [shadows, midtones, highlights],
            preserve_luminosity,
            true,
        )
    });
}

/// Vibrance. Boosts saturation more on low-saturation pixels (and less on
/// already-saturated ones). `amount` -1..1.
pub fn vibrance(img: &mut RasterImage, amount: f32, sel: Option<&Mask>) {
    let amt = match san(amount, -1.0, 1.0) {
        Some(a) => a,
        None => return, // non-finite input → no-op
    };
    map_point(img, sel, move |rgb| {
        let mut hsl = rgb_to_hsl(rgb);
        let s = hsl[1];
        // Delta proportional to (1 - s): low-sat pixels move more.
        hsl[1] = clamp01(s + amt * (1.0 - s));
        hsl_to_rgb(hsl)
    });
}

/// Desaturate. Replaces each pixel with its **HSL lightness** gray
/// `(max + min) / 2` — this matches Photoshop's *Image > Adjustments >
/// Desaturate*, which uses the HSL lightness midpoint rather than a perceptual
/// (Rec. 601) luma. (Weighted luma still backs `black_and_white`.)
/// Shared encoded-space HSL-lightness math. No clamp: the raster caller owns
/// its u8 boundary, while native video preserves extended floating values.
pub fn desaturate_rgb(rgb: [f32; 3]) -> [f32; 3] {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let g = (max + min) / 2.0;
    [g, g, g]
}
pub fn desaturate(img: &mut RasterImage, sel: Option<&Mask>) {
    map_point(img, sel, desaturate_rgb);
}

/// Black & White. Mixes channels into a gray using normalized `weights`.
pub fn black_and_white(img: &mut RasterImage, weights: [f32; 3], sel: Option<&Mask>) {
    if !finite3(weights) {
        return; // non-finite input → no-op
    }
    let w = normalized_gray_weights(weights);
    map_point(img, sel, move |rgb| {
        channel_mix_rgb(rgb, [w; 3]).map(clamp01)
    });
}

/// Floating encoded-channel negative; callers own color-space/storage boundaries.
/// The Legacy byte fast path below stays unchanged and is exhaustively checked.
pub fn invert_channel(value: f32) -> f32 {
    1.0 - value
}

/// Invert (negative). `v -> 1 - v` per channel.
pub fn invert(img: &mut RasterImage, sel: Option<&Mask>) {
    apply_point(img, sel, |px| {
        [255 - px[0], 255 - px[1], 255 - px[2], px[3]]
    });
}

/// Posterize. Quantizes each channel into `levels` (2..=255) evenly-spaced steps.
/// Shared positive-code-value quantization; callers own input-space conversion.
pub fn posterize_channel(value: f32, levels: u32) -> f32 {
    let steps = (levels.clamp(2, 255) - 1) as f32;
    (value * steps).round() / steps
}
pub fn posterize(img: &mut RasterImage, levels: u32, sel: Option<&Mask>) {
    let n = levels.clamp(2, 255);
    map_point(img, sel, move |rgb| rgb.map(|v| posterize_channel(v, n)));
}

/// Threshold. Pixels with luma >= `level` (0..1) become white, otherwise black.
/// Shared encoded Rec601 threshold decision; preserves the raster comparison.
pub fn threshold_rgb(rgb: [f32; 3], level: f32) -> bool {
    luma(rgb) >= level
}
pub fn threshold(img: &mut RasterImage, level: f32, sel: Option<&Mask>) {
    let t = match san(level, 0.0, 1.0) {
        Some(l) => l,
        None => return, // non-finite input → no-op
    };
    apply_point(img, sel, move |px| {
        let (rgb, a) = to_f(px);
        let v: u8 = if threshold_rgb(rgb, t) { 255 } else { 0 };
        [v, v, v, a]
    });
}

/// Shared transfer-space Photo Filter. The explicit raster_clamp retains the
/// original raster-only rescale clamp; native video keeps extended values.
pub fn photo_filter_rgb(
    rgb: [f32; 3],
    color: [f32; 3],
    density: f32,
    preserve: bool,
    raster_clamp: bool,
) -> [f32; 3] {
    let mut out = [0.0; 3];
    for i in 0..3 {
        let tinted = rgb[i] * color[i];
        out[i] = rgb[i] * (1.0 - density) + tinted * density;
    }
    if preserve {
        let original = luma(rgb);
        let tinted = luma(out);
        if tinted > 1e-4 {
            let factor = original / tinted;
            for v in &mut out {
                *v *= factor;
                if raster_clamp {
                    *v = clamp01(*v);
                }
            }
        }
    }
    out
}

/// Photo Filter. Tints the image toward `color` (0..1 RGB) by `density` (0..1)
/// using a multiply blend. When `preserve_luminosity` is set, the result is
/// rescaled so its luma matches the original.
pub fn photo_filter(
    img: &mut RasterImage,
    color: [f32; 3],
    density: f32,
    preserve_luminosity: bool,
    sel: Option<&Mask>,
) {
    if !finite3(color) || !density.is_finite() {
        return; // non-finite input → no-op
    }
    let d = clamp01(density);
    map_point(img, sel, move |rgb| {
        photo_filter_rgb(rgb, color, d, preserve_luminosity, true)
    });
}

/// Shared unclamped transfer-coordinate matrix math. The raster caller keeps
/// its legacy clamp; native video preserves signed/HDR floating values.
pub fn channel_mix_rgb(rgb: [f32; 3], matrix: [[f32; 3]; 3]) -> [f32; 3] {
    matrix.map(|r| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2])
}
/// Existing Black & White normalization, including the zero-sum fallback.
pub fn normalized_gray_weights(weights: [f32; 3]) -> [f32; 3] {
    let sum = weights[0] + weights[1] + weights[2];
    if sum.abs() < 1e-9 {
        [1.0 / 3.0; 3]
    } else {
        weights.map(|v| v / sum)
    }
}

/// Channel Mixer. Each output channel is the dot product of its weight vector
/// with the input RGB: `out_r = dot(r, rgb)`, etc.
pub fn channel_mixer(
    img: &mut RasterImage,
    r: [f32; 3],
    g: [f32; 3],
    b: [f32; 3],
    sel: Option<&Mask>,
) {
    if !(finite3(r) && finite3(g) && finite3(b)) {
        return; // non-finite input → no-op
    }
    map_point(img, sel, move |rgb| {
        channel_mix_rgb(rgb, [r, g, b]).map(clamp01)
    });
}

/// Gradient Map. Maps each pixel's luma (0..1) to a color sampled from the
/// gradient defined by `stops` (position 0..1, RGB byte color), interpolating
/// linearly between adjacent stops.
pub fn gradient_map(img: &mut RasterImage, stops: &[(f32, [u8; 3])], sel: Option<&Mask>) {
    // Sort defensively by position so callers needn't pre-sort; drop any stop
    // with a non-finite position so NaN cannot poison the mapping.
    let mut s: Vec<(f32, [f32; 3])> = stops
        .iter()
        .filter(|(p, _)| p.is_finite())
        .map(|(p, c)| {
            (
                clamp01(*p),
                [
                    c[0] as f32 / 255.0,
                    c[1] as f32 / 255.0,
                    c[2] as f32 / 255.0,
                ],
            )
        })
        .collect();
    s.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    map_point(img, sel, move |rgb| {
        if s.is_empty() {
            return rgb;
        }
        let l = luma(rgb);
        if l <= s[0].0 {
            return s[0].1;
        }
        if l >= s[s.len() - 1].0 {
            return s[s.len() - 1].1;
        }
        for w in s.windows(2) {
            let (p0, c0) = w[0];
            let (p1, c1) = w[1];
            if l >= p0 && l <= p1 {
                let t = if (p1 - p0).abs() < 1e-9 {
                    0.0
                } else {
                    (l - p0) / (p1 - p0)
                };
                return [
                    c0[0] + t * (c1[0] - c0[0]),
                    c0[1] + t * (c1[1] - c0[1]),
                    c0[2] + t * (c1[2] - c0[2]),
                ];
            }
        }
        s[s.len() - 1].1
    });
}

/// Selective Color (simplified).
///
/// Photoshop's Selective Color adjusts CMYK components within named color
/// families (reds, yellows, … neutrals). This is a deliberately simplified
/// stand-in: pixels whose color is within `range` (0..1 RGB distance) of
/// `target` receive an additive RGB `adjust` (-1..1 per channel), weighted by
/// how close they are to the target. Pixels far from the target are untouched.
pub fn selective_color(
    img: &mut RasterImage,
    target: [f32; 3],
    adjust: [f32; 3],
    range: f32,
    sel: Option<&Mask>,
) {
    if !(finite3(target) && finite3(adjust) && range.is_finite()) {
        return; // non-finite input → no-op
    }
    let r = range.max(1e-4);
    map_point(img, sel, move |rgb| {
        // Euclidean distance in normalized RGB.
        let dist = ((rgb[0] - target[0]).powi(2)
            + (rgb[1] - target[1]).powi(2)
            + (rgb[2] - target[2]).powi(2))
        .sqrt();
        let weight = (1.0 - dist / r).clamp(0.0, 1.0);
        [
            clamp01(rgb[0] + adjust[0] * weight),
            clamp01(rgb[1] + adjust[1] * weight),
            clamp01(rgb[2] + adjust[2] * weight),
        ]
    });
}

/// Default Shadows/Highlights radius (px) when the caller passes 0 (or a
/// negative) — mirrors Photoshop's default ~30 px tonal-region radius.
const SHADOWS_HIGHLIGHTS_DEFAULT_RADIUS: f32 = 30.0;

/// Largest accepted radius, to bound the blur cost on huge images.
const SHADOWS_HIGHLIGHTS_MAX_RADIUS: f32 = 1024.0;

/// Half-width of the tonal-region falloff (0..1 luma). Inside this band from a
/// tonal extreme the correction ramps in smoothly; beyond it the correction
/// fades to zero. Roughly Photoshop's *Tonal Width* default.
const SHADOWS_HIGHLIGHTS_TONAL_WIDTH: f32 = 0.5;

/// Shadows/Highlights — a **local, radius-adaptive** tonal correction, like
/// Photoshop's *Image > Adjustments > Shadows/Highlights*.
///
/// `shadows_amount` (0..1) lifts dark regions; `highlights_amount` (0..1)
/// recovers (darkens) bright regions. Crucially the lift/cut at each pixel is
/// driven by the **local (blurred-neighborhood) luminance** rather than the
/// pixel's own luma: a single-channel luma guide is built and Gaussian-blurred
/// at `radius` px, so the correction adapts to its surroundings and recovers
/// local contrast. Two pixels of identical own-luma but different neighborhoods
/// therefore map differently — the defining property of a local operator.
///
/// `radius` is the neighborhood size in pixels; `0` (or negative) selects the
/// ~30 px default, and the value is clamped to a sane upper bound. Non-finite
/// `radius` or amounts degrade to an identity no-op (never panics). Alpha is
/// preserved and the result is written through the masked blend path.
pub fn shadows_highlights(
    img: &mut RasterImage,
    shadows_amount: f32,
    highlights_amount: f32,
    radius: f32,
    sel: Option<&Mask>,
) {
    let (sa, ha) = match (
        san(shadows_amount, 0.0, 1.0),
        san(highlights_amount, 0.0, 1.0),
    ) {
        (Some(s), Some(h)) => (s, h),
        _ => return, // non-finite amount → no-op
    };
    if !radius.is_finite() {
        return; // non-finite radius → no-op
    }
    // 0 / negative → sensible default; otherwise clamp to a bounded range.
    let radius = if radius > 0.0 {
        radius.min(SHADOWS_HIGHLIGHTS_MAX_RADIUS)
    } else {
        SHADOWS_HIGHLIGHTS_DEFAULT_RADIUS
    };

    let w = img.width;
    let h = img.height;
    let n = (w as usize) * (h as usize);
    if n == 0 {
        return;
    }

    // Build a single-channel luma guide, then blur it to get local luminance.
    let mut guide = vec![0u8; n];
    for (dst, px) in guide.iter_mut().zip(img.pixels.chunks_exact(4)) {
        let l = luma([
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
        ]);
        *dst = (l * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    let local = crate::raster::filter::gaussian_blur_gray(&guide, w, h, radius);

    let tw = SHADOWS_HIGHLIGHTS_TONAL_WIDTH;
    let mut result = img.clone();
    for (i, out_px) in result.pixels.chunks_exact_mut(4).enumerate() {
        let ll = local[i] as f32 / 255.0;
        // Strong shadow lift where the LOCAL region is dark; strong highlight
        // cut where the LOCAL region is bright — each with a smooth falloff.
        let shadow_w = 1.0 - smoothstep(0.0, tw, ll);
        let highlight_w = smoothstep(1.0 - tw, 1.0, ll);
        let lift = sa * shadow_w;
        let cut = ha * highlight_w;
        for c in 0..3 {
            let v = out_px[c] as f32 / 255.0;
            let nv = clamp01(v + lift * (1.0 - v) - cut * v);
            out_px[c] = (nv * 255.0).round() as u8;
        }
        // out_px[3] (alpha) is left untouched by the clone.
    }
    blend_result(img, &result, sel);
}

/// Gamma correction. `out = in^(1/gamma)` per channel (gamma > 1 brightens).
pub fn gamma(img: &mut RasterImage, gamma: f32, sel: Option<&Mask>) {
    if !gamma.is_finite() {
        return; // non-finite input → no-op
    }
    let inv = 1.0 / gamma.clamp(0.01, 9.99);
    map_point(img, sel, move |rgb| {
        [rgb[0].powf(inv), rgb[1].powf(inv), rgb[2].powf(inv)]
    });
}

/// Default histogram-tail clip fraction for the Auto adjustments (0.1%). A small
/// percentile clip on each end stops a single stray pixel (a hot speck, a dust
/// dot) from anchoring the stretch and defeating it.
const AUTO_CLIP_FRACTION: f32 = 0.001;

/// Build a 256-bin luma histogram over the whole image (mask-independent).
fn luma_histogram(img: &RasterImage) -> [u32; 256] {
    let mut hist = [0u32; 256];
    for px in img.pixels.chunks_exact(4) {
        let l = luma([
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
        ]);
        let idx = (l * 255.0).round().clamp(0.0, 255.0) as usize;
        hist[idx] += 1;
    }
    hist
}

/// Find the low/high histogram bins (0..=255) that bound all but a `frac`
/// fraction of pixels at each tail. With `frac == 0` (or fewer pixels than one
/// clip unit) this reduces to the plain min/max occupied bins.
fn percentile_bounds(hist: &[u32; 256], total: u32, frac: f32) -> (usize, usize) {
    if total == 0 {
        return (0, 255);
    }
    let clip = ((total as f32) * frac.max(0.0)).floor() as u32;
    let mut lo = 0usize;
    let mut acc = 0u32;
    for (i, &c) in hist.iter().enumerate() {
        acc += c;
        if acc > clip {
            lo = i;
            break;
        }
    }
    let mut hi = 255usize;
    acc = 0;
    for i in (0..256).rev() {
        acc += hist[i];
        if acc > clip {
            hi = i;
            break;
        }
    }
    if hi < lo {
        std::mem::swap(&mut lo, &mut hi);
    }
    (lo, hi)
}

/// Auto Contrast. Stretches the luma histogram to the full 0..1 range, applying
/// the same linear mapping to every channel. The lo/hi anchors are taken at a
/// small percentile ([`AUTO_CLIP_FRACTION`]) of each tail rather than the
/// absolute min/max, so a lone outlier pixel can't defeat the stretch. The
/// histogram is measured over the whole image (ignoring any selection); results
/// are still written through `apply_point` so the selection masks the *write*.
pub fn auto_contrast(img: &mut RasterImage, sel: Option<&Mask>) {
    let total = (img.pixels.len() / 4) as u32;
    let hist = luma_histogram(img);
    let (lo_i, hi_i) = percentile_bounds(&hist, total, AUTO_CLIP_FRACTION);
    let lo = lo_i as f32 / 255.0;
    let hi = hi_i as f32 / 255.0;
    if hi - lo < 1e-4 {
        return;
    }
    let scale = 1.0 / (hi - lo);
    map_point(img, sel, move |rgb| {
        [
            clamp01((rgb[0] - lo) * scale),
            clamp01((rgb[1] - lo) * scale),
            clamp01((rgb[2] - lo) * scale),
        ]
    });
}

/// Per-channel 256-bin histograms across the whole image (mask-independent).
fn channel_histograms(img: &RasterImage) -> [[u32; 256]; 3] {
    let mut hist = [[0u32; 256]; 3];
    for px in img.pixels.chunks_exact(4) {
        for i in 0..3 {
            hist[i][px[i] as usize] += 1;
        }
    }
    hist
}

/// Auto Levels. Stretches each channel's histogram independently to full range,
/// anchoring the per-channel lo/hi at a small percentile
/// ([`AUTO_CLIP_FRACTION`]) of each tail so single outlier pixels don't defeat
/// the stretch.
pub fn auto_levels(img: &mut RasterImage, sel: Option<&Mask>) {
    let total = (img.pixels.len() / 4) as u32;
    let hists = channel_histograms(img);
    let mut bounds = [(0.0f32, 1.0f32); 3];
    for i in 0..3 {
        let (lo_i, hi_i) = percentile_bounds(&hists[i], total, AUTO_CLIP_FRACTION);
        bounds[i] = (lo_i as f32 / 255.0, hi_i as f32 / 255.0);
    }
    map_point(img, sel, move |rgb| {
        let mut out = [0.0; 3];
        for i in 0..3 {
            let (lo, hi) = bounds[i];
            out[i] = if hi - lo < 1e-4 {
                rgb[i]
            } else {
                clamp01((rgb[i] - lo) / (hi - lo))
            };
        }
        out
    });
}

// ── AdjustmentSpec — adjustments as serializable data ───────────────────────────

use serde::{Deserialize, Serialize};

/// A serializable description of one adjustment and its parameters.
///
/// This makes an adjustment *data* rather than a function call, which is what
/// lets Photonic offer **non-destructive adjustment layers** (a `RasterNode`
/// carrying an `AdjustmentSpec` re-applies it to the composite beneath it every
/// time the document is rendered) and a single uniform `apply_adjustment` MCP
/// tool. The variants mirror the functions in this module 1:1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdjustmentSpec {
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
    },
    Levels {
        in_black: f32,
        in_white: f32,
        gamma: f32,
        out_black: f32,
        out_white: f32,
    },
    Curves {
        rgb: Vec<(f32, f32)>,
        red: Vec<(f32, f32)>,
        green: Vec<(f32, f32)>,
        blue: Vec<(f32, f32)>,
    },
    Exposure {
        stops: f32,
    },
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    ColorBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        preserve_luminosity: bool,
    },
    Vibrance {
        amount: f32,
    },
    Desaturate,
    BlackAndWhite {
        weights: [f32; 3],
    },
    Invert,
    Posterize {
        levels: u32,
    },
    Threshold {
        level: f32,
    },
    PhotoFilter {
        color: [f32; 3],
        density: f32,
        preserve_luminosity: bool,
    },
    ChannelMixer {
        red: [f32; 3],
        green: [f32; 3],
        blue: [f32; 3],
    },
    GradientMap {
        stops: Vec<(f32, [u8; 3])>,
    },
    SelectiveColor {
        target: [f32; 3],
        adjust: [f32; 3],
        range: f32,
    },
    ShadowsHighlights {
        shadows: f32,
        highlights: f32,
        radius: f32,
    },
    Gamma {
        gamma: f32,
    },
    AutoContrast,
    AutoLevels,
}

impl AdjustmentSpec {
    /// Apply this adjustment to `img`, confined to an optional selection `mask`.
    pub fn apply(&self, img: &mut RasterImage, mask: Option<&Mask>) {
        match self {
            AdjustmentSpec::BrightnessContrast {
                brightness,
                contrast,
            } => brightness_contrast(img, *brightness, *contrast, mask),
            AdjustmentSpec::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
            } => levels(
                img, *in_black, *in_white, *gamma, *out_black, *out_white, mask,
            ),
            AdjustmentSpec::Curves {
                rgb,
                red,
                green,
                blue,
            } => curves(img, rgb, red, green, blue, mask),
            AdjustmentSpec::Exposure { stops } => exposure(img, *stops, mask),
            AdjustmentSpec::HueSaturation {
                hue,
                saturation,
                lightness,
            } => hue_saturation(img, *hue, *saturation, *lightness, mask),
            AdjustmentSpec::ColorBalance {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => color_balance(
                img,
                *shadows,
                *midtones,
                *highlights,
                *preserve_luminosity,
                mask,
            ),
            AdjustmentSpec::Vibrance { amount } => vibrance(img, *amount, mask),
            AdjustmentSpec::Desaturate => desaturate(img, mask),
            AdjustmentSpec::BlackAndWhite { weights } => black_and_white(img, *weights, mask),
            AdjustmentSpec::Invert => invert(img, mask),
            AdjustmentSpec::Posterize { levels } => posterize(img, *levels, mask),
            AdjustmentSpec::Threshold { level } => threshold(img, *level, mask),
            AdjustmentSpec::PhotoFilter {
                color,
                density,
                preserve_luminosity,
            } => photo_filter(img, *color, *density, *preserve_luminosity, mask),
            AdjustmentSpec::ChannelMixer { red, green, blue } => {
                channel_mixer(img, *red, *green, *blue, mask)
            }
            AdjustmentSpec::GradientMap { stops } => gradient_map(img, stops, mask),
            AdjustmentSpec::SelectiveColor {
                target,
                adjust,
                range,
            } => selective_color(img, *target, *adjust, *range, mask),
            AdjustmentSpec::ShadowsHighlights {
                shadows,
                highlights,
                radius,
            } => shadows_highlights(img, *shadows, *highlights, *radius, mask),
            AdjustmentSpec::Gamma { gamma: g } => gamma(img, *g, mask),
            AdjustmentSpec::AutoContrast => auto_contrast(img, mask),
            AdjustmentSpec::AutoLevels => auto_levels(img, mask),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rgba: [u8; 4]) -> RasterImage {
        RasterImage::filled(2, 2, rgba)
    }

    #[test]
    fn brightness_plus_one_saturates_white() {
        let mut img = solid([100, 120, 140, 200]);
        brightness_contrast(&mut img, 1.0, 0.0, None);
        assert_eq!(img.pixel(0, 0), [255, 255, 255, 200]);
    }

    #[test]
    fn brightness_minus_one_crushes_black() {
        let mut img = solid([100, 120, 140, 255]);
        brightness_contrast(&mut img, -1.0, 0.0, None);
        let p = img.pixel(0, 0);
        assert_eq!([p[0], p[1], p[2]], [0, 0, 0]);
    }

    #[test]
    fn contrast_zero_is_identity() {
        let mut img = solid([60, 130, 200, 255]);
        brightness_contrast(&mut img, 0.0, 0.0, None);
        assert_eq!(img.pixel(0, 0), [60, 130, 200, 255]);
    }

    #[test]
    fn levels_identity() {
        let mut img = solid([40, 128, 220, 255]);
        levels(&mut img, 0.0, 1.0, 1.0, 0.0, 1.0, None);
        let p = img.pixel(0, 0);
        // Allow ±1 for rounding round-trips.
        assert!((p[0] as i32 - 40).abs() <= 1);
        assert!((p[1] as i32 - 128).abs() <= 1);
        assert!((p[2] as i32 - 220).abs() <= 1);
    }

    #[test]
    fn levels_clips_to_output_black() {
        let mut img = solid([10, 10, 10, 255]);
        // in_black at 0.2 -> these darks map below 0 -> clamp to out_black 0.
        levels(&mut img, 0.2, 1.0, 1.0, 0.0, 1.0, None);
        assert_eq!(img.pixel(0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn curves_identity_lut() {
        let lut = curve_lut(&[(0.0, 0.0), (1.0, 1.0)]);
        assert_eq!(lut[0], 0);
        assert_eq!(lut[255], 255);
        assert_eq!(lut[128], 128);
    }

    #[test]
    fn curves_inverts_with_descending_points() {
        let mut img = solid([0, 128, 255, 255]);
        // Composite (rgb) curve only; per-channel curves identity.
        curves(&mut img, &[(0.0, 1.0), (1.0, 0.0)], &[], &[], &[], None);
        let p = img.pixel(0, 0);
        assert_eq!(p[0], 255);
        assert_eq!(p[2], 0);
    }

    #[test]
    fn curves_per_channel_red_only_leaves_gb_untouched() {
        let mut img = solid([100, 100, 100, 255]);
        // Invert the red channel only; rgb/green/blue identity (empty slices).
        curves(&mut img, &[], &[(0.0, 1.0), (1.0, 0.0)], &[], &[], None);
        let p = img.pixel(0, 0);
        assert_eq!(p[0], 155, "red inverted: 255-100 = 155");
        assert_eq!(p[1], 100, "green untouched");
        assert_eq!(p[2], 100, "blue untouched");
    }

    #[test]
    fn curves_smooth_bows_away_from_linear() {
        // Knots (0,0),(0.25,0.5),(1,1): the smooth monotone-cubic spline bows
        // above the straight segment between the last two knots at the midpoint.
        let pts = [(0.0_f32, 0.0), (0.25, 0.5), (1.0, 1.0)];
        let lut = curve_lut(&pts);
        // Piecewise-linear value at x = 0.5 on segment (0.25,0.5)-(1.0,1.0).
        let t: f32 = (0.5 - 0.25) / (1.0 - 0.25);
        let lin: f32 = 0.5 + t * (1.0 - 0.5);
        let lin_byte = (lin * 255.0).round() as i32;
        let smooth = lut[128] as i32;
        assert!(
            (smooth - lin_byte).abs() >= 5,
            "smooth {} should bow away from linear {}",
            smooth,
            lin_byte
        );
    }

    #[test]
    fn exposure_zero_identity_and_brighten() {
        let mut a = solid([100, 100, 100, 255]);
        exposure(&mut a, 0.0, None);
        // Linear-light round-trip at 0 stops is the identity (within rounding).
        assert!((a.pixel(0, 0)[0] as i32 - 100).abs() <= 1);
        let mut b = solid([100, 100, 100, 255]);
        exposure(&mut b, 1.0, None); // +1 stop in linear light
                                     // +1 stop on 100 in linear light lands ≈138 (NOT the naive ×2 = 200).
        let v = b.pixel(0, 0)[0] as i32;
        assert!(
            (130..=150).contains(&v),
            "expected linear-light ~138, got {}",
            v
        );
        assert!(v < 200, "must not be the naive gamma-space doubling");
    }

    #[test]
    fn hue_saturation_zero_is_identity() {
        let mut img = solid([200, 100, 50, 255]);
        hue_saturation(&mut img, 0.0, 0.0, 0.0, None);
        let p = img.pixel(0, 0);
        assert!((p[0] as i32 - 200).abs() <= 1);
        assert!((p[1] as i32 - 100).abs() <= 1);
        assert!((p[2] as i32 - 50).abs() <= 1);
    }

    #[test]
    fn hue_saturation_desaturates_to_gray() {
        let mut img = solid([200, 100, 50, 255]);
        hue_saturation(&mut img, 0.0, -1.0, 0.0, None);
        let p = img.pixel(0, 0);
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
    }

    #[test]
    fn color_balance_shifts_shadows_red() {
        let mut img = solid([20, 20, 20, 255]); // dark -> shadow weight high
        color_balance(&mut img, [1.0, 0.0, 0.0], [0.0; 3], [0.0; 3], false, None);
        let p = img.pixel(0, 0);
        assert!(p[0] > 20, "red channel should rise, got {}", p[0]);
        assert_eq!(p[1], 20);
        assert_eq!(p[2], 20);
    }

    #[test]
    fn color_balance_preserve_luminosity_holds_luma() {
        let mut img = solid([128, 128, 128, 255]);
        let orig = luma([128.0 / 255.0; 3]);
        // Strong midtone red push, but preserve luminosity.
        color_balance(&mut img, [0.0; 3], [0.6, 0.0, 0.0], [0.0; 3], true, None);
        let p = img.pixel(0, 0);
        let new = luma([
            p[0] as f32 / 255.0,
            p[1] as f32 / 255.0,
            p[2] as f32 / 255.0,
        ]);
        assert!(
            (new - orig).abs() < 0.02,
            "luma drifted: {} vs {}",
            new,
            orig
        );
        assert!(p[0] > p[1], "red should still be pushed relative to green");
    }

    #[test]
    fn vibrance_boosts_saturation() {
        let mut img = solid([150, 120, 110, 255]); // low saturation
        let before = rgb_to_hsl([150.0 / 255.0, 120.0 / 255.0, 110.0 / 255.0])[1];
        vibrance(&mut img, 0.8, None);
        let p = img.pixel(0, 0);
        let after = rgb_to_hsl([
            p[0] as f32 / 255.0,
            p[1] as f32 / 255.0,
            p[2] as f32 / 255.0,
        ])[1];
        assert!(after > before, "{} !> {}", after, before);
    }

    #[test]
    fn desaturate_equalizes_channels() {
        let mut img = solid([10, 200, 90, 255]);
        desaturate(&mut img, None);
        let p = img.pixel(0, 0);
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
        assert_eq!(p[3], 255);
    }

    #[test]
    fn desaturate_pure_red_is_hsl_lightness() {
        // HSL lightness of pure red = (max+min)/2 = 0.5 ≈ 127, NOT the Rec.601
        // luma (~0.299 → 76). Matches Photoshop's Desaturate.
        let mut img = solid([255, 0, 0, 255]);
        desaturate(&mut img, None);
        let p = img.pixel(0, 0);
        assert!(
            (p[0] as i32 - 127).abs() <= 1,
            "expected ~127, got {}",
            p[0]
        );
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
    }

    #[test]
    fn black_and_white_normalizes_weights() {
        let mut img = solid([100, 100, 100, 255]);
        // Unnormalized weights summing to 6 -> still gray 100.
        black_and_white(&mut img, [2.0, 2.0, 2.0], None);
        let p = img.pixel(0, 0);
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
        assert!((p[0] as i32 - 100).abs() <= 1);
    }

    #[test]
    fn invert_twice_is_identity() {
        let mut img = solid([10, 128, 240, 200]);
        invert(&mut img, None);
        invert(&mut img, None);
        assert_eq!(img.pixel(0, 0), [10, 128, 240, 200]);
    }

    #[test]
    fn posterize_two_levels_limits_distinct_values() {
        let mut img = RasterImage::new(4, 1);
        img.set_pixel(0, 0, [0, 0, 0, 255]);
        img.set_pixel(1, 0, [80, 80, 80, 255]);
        img.set_pixel(2, 0, [180, 180, 180, 255]);
        img.set_pixel(3, 0, [255, 255, 255, 255]);
        posterize(&mut img, 2, None);
        use std::collections::HashSet;
        let mut vals: HashSet<u8> = HashSet::new();
        for x in 0..4 {
            vals.insert(img.pixel(x, 0)[0]);
        }
        assert!(vals.len() <= 2, "got {:?}", vals);
        assert!(vals.iter().all(|&v| v == 0 || v == 255));
    }

    #[test]
    fn threshold_yields_only_black_or_white() {
        let mut img = RasterImage::new(3, 1);
        img.set_pixel(0, 0, [10, 10, 10, 255]);
        img.set_pixel(1, 0, [130, 130, 130, 255]);
        img.set_pixel(2, 0, [250, 250, 250, 255]);
        threshold(&mut img, 0.5, None);
        for x in 0..3 {
            let p = img.pixel(x, 0);
            assert!(p[0] == 0 || p[0] == 255);
            assert_eq!(p[0], p[1]);
            assert_eq!(p[1], p[2]);
        }
        assert_eq!(img.pixel(0, 0)[0], 0);
        assert_eq!(img.pixel(2, 0)[0], 255);
    }

    #[test]
    fn photo_filter_density_zero_is_identity() {
        let mut img = solid([120, 90, 60, 255]);
        photo_filter(&mut img, [1.0, 0.5, 0.0], 0.0, false, None);
        assert_eq!(img.pixel(0, 0), [120, 90, 60, 255]);
    }

    #[test]
    fn photo_filter_tints_toward_color() {
        let mut img = solid([200, 200, 200, 255]);
        // Pure-blue filter, full density -> red/green should drop.
        photo_filter(&mut img, [0.0, 0.0, 1.0], 1.0, false, None);
        let p = img.pixel(0, 0);
        assert_eq!(p[0], 0);
        assert_eq!(p[1], 0);
        assert_eq!(p[2], 200);
    }

    #[test]
    fn channel_mixer_swaps_red_and_blue() {
        let mut img = solid([255, 0, 0, 255]);
        // out_r = blue, out_g = green, out_b = red.
        channel_mixer(
            &mut img,
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            None,
        );
        let p = img.pixel(0, 0);
        assert_eq!(p[0], 0);
        assert_eq!(p[2], 255);
    }

    #[test]
    fn gradient_map_endpoints() {
        let stops = [(0.0, [0, 0, 0]), (1.0, [255, 255, 255])];
        let mut black = solid([0, 0, 0, 255]);
        gradient_map(&mut black, &stops, None);
        assert_eq!(black.pixel(0, 0), [0, 0, 0, 255]);
        let mut white = solid([255, 255, 255, 255]);
        gradient_map(&mut white, &stops, None);
        assert_eq!(white.pixel(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn gradient_map_colorizes() {
        // Map dark->red, light->green.
        let stops = [(0.0, [255, 0, 0]), (1.0, [0, 255, 0])];
        let mut img = solid([0, 0, 0, 255]);
        gradient_map(&mut img, &stops, None);
        assert_eq!(img.pixel(0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn selective_color_only_affects_near_target() {
        let mut img = RasterImage::new(2, 1);
        img.set_pixel(0, 0, [255, 0, 0, 255]); // matches red target
        img.set_pixel(1, 0, [0, 0, 255, 255]); // far from target
        selective_color(&mut img, [1.0, 0.0, 0.0], [0.0, 0.5, 0.0], 0.3, None);
        let near = img.pixel(0, 0);
        let far = img.pixel(1, 0);
        assert!(near[1] > 0, "near-target green should rise");
        assert_eq!(far, [0, 0, 255, 255], "far pixel untouched");
    }

    #[test]
    fn shadows_highlights_lifts_dark() {
        let mut img = solid([20, 20, 20, 255]);
        shadows_highlights(&mut img, 0.8, 0.0, 8.0, None);
        assert!(img.pixel(0, 0)[0] > 20);
    }

    #[test]
    fn shadows_highlights_recovers_bright() {
        let mut img = solid([240, 240, 240, 255]);
        shadows_highlights(&mut img, 0.0, 0.8, 8.0, None);
        assert!(img.pixel(0, 0)[0] < 240);
    }

    #[test]
    fn shadows_highlights_is_local_not_global() {
        // A mid-gray patch with IDENTICAL own-luma in both images, but opposite
        // surroundings: black in one, white in the other. A purely global (own-
        // luma) operator would map the patch identically in both. A local
        // operator keys off the blurred neighborhood, so the patch is lifted
        // MORE when surrounded by black (dark local region) than by white.
        let size = 15u32;
        let patch_lo = 6u32; // 3x3 centered patch (6..=8)
        let patch_hi = 8u32;
        let mid = [128u8, 128, 128, 255];

        let build = |bg: [u8; 4]| {
            let mut img = RasterImage::filled(size, size, bg);
            for y in patch_lo..=patch_hi {
                for x in patch_lo..=patch_hi {
                    img.set_pixel(x, y, mid);
                }
            }
            img
        };

        let mut on_black = build([0, 0, 0, 255]);
        let mut on_white = build([255, 255, 255, 255]);

        // Sanity: identical own-luma at the patch center before the op.
        assert_eq!(on_black.pixel(7, 7), mid);
        assert_eq!(on_white.pixel(7, 7), mid);

        // Modest radius so the surround bleeds into the patch's local luminance.
        shadows_highlights(&mut on_black, 0.8, 0.0, 4.0, None);
        shadows_highlights(&mut on_white, 0.8, 0.0, 4.0, None);

        let lifted_black = on_black.pixel(7, 7)[0];
        let lifted_white = on_white.pixel(7, 7)[0];

        // Local adaptation: same own-luma, different result based on neighbors.
        assert!(
            lifted_black > lifted_white,
            "patch surrounded by black should be lifted more ({}) than by white ({})",
            lifted_black,
            lifted_white
        );
        // And it must genuinely have been lifted above its original value.
        assert!(
            lifted_black > 128,
            "black-surround patch lifted, got {}",
            lifted_black
        );
    }

    #[test]
    fn shadows_highlights_nan_radius_is_no_op() {
        let orig = [60, 130, 200, 255];
        let mut img = solid(orig);
        shadows_highlights(&mut img, 0.8, 0.8, f32::NAN, None);
        assert_eq!(img.pixel(0, 0), orig, "NaN radius must be a no-op");

        let mut img2 = solid(orig);
        shadows_highlights(&mut img2, f32::NAN, 0.0, 8.0, None);
        assert_eq!(img2.pixel(0, 0), orig, "NaN amount must be a no-op");
    }

    #[test]
    fn gamma_identity_and_brighten() {
        let mut a = solid([128, 128, 128, 255]);
        gamma(&mut a, 1.0, None);
        assert!((a.pixel(0, 0)[0] as i32 - 128).abs() <= 1);
        let mut b = solid([128, 128, 128, 255]);
        gamma(&mut b, 2.0, None); // brightens midtones
        assert!(b.pixel(0, 0)[0] > 128);
    }

    #[test]
    fn auto_contrast_widens_range() {
        // Low-contrast gradient between 100 and 150.
        let mut img = RasterImage::new(2, 1);
        img.set_pixel(0, 0, [100, 100, 100, 255]);
        img.set_pixel(1, 0, [150, 150, 150, 255]);
        auto_contrast(&mut img, None);
        assert_eq!(img.pixel(0, 0)[0], 0);
        assert_eq!(img.pixel(1, 0)[0], 255);
    }

    #[test]
    fn auto_contrast_flat_image_unchanged() {
        let mut img = solid([100, 100, 100, 255]);
        auto_contrast(&mut img, None);
        assert_eq!(img.pixel(0, 0), [100, 100, 100, 255]);
    }

    #[test]
    fn auto_contrast_ignores_single_outlier_and_stretches_midtones() {
        // 1000 pixels: a tight midtone band 110..140 plus one black (0) and one
        // white (255) outlier. With a 0.1% clip (clip = 1 px/tail), the lone
        // outliers are discarded so the stretch keys off the 110..140 band.
        let mut img = RasterImage::new(1000, 1);
        for x in 0..1000 {
            img.set_pixel(x, 0, [125, 125, 125, 255]);
        }
        img.set_pixel(1, 0, [110, 110, 110, 255]); // band low
        img.set_pixel(2, 0, [140, 140, 140, 255]); // band high
        img.set_pixel(0, 0, [0, 0, 0, 255]); // outlier (clipped)
        img.set_pixel(999, 0, [255, 255, 255, 255]); // outlier (clipped)
        auto_contrast(&mut img, None);
        // Outliers do NOT anchor the stretch: band ends hit full range.
        assert_eq!(img.pixel(1, 0)[0], 0, "band low stretched to black");
        assert_eq!(img.pixel(2, 0)[0], 255, "band high stretched to white");
        // The dominant midtone is genuinely stretched (not left near 125).
        let mid = img.pixel(500, 0)[0];
        assert!((110..=145).contains(&mid), "midtone stretched, got {}", mid);
    }

    #[test]
    fn auto_levels_stretches_each_channel() {
        let mut img = RasterImage::new(2, 1);
        // Red spans 50..200, green spans 80..80 (flat), blue 0..255.
        img.set_pixel(0, 0, [50, 80, 0, 255]);
        img.set_pixel(1, 0, [200, 80, 255, 255]);
        auto_levels(&mut img, None);
        let p0 = img.pixel(0, 0);
        let p1 = img.pixel(1, 0);
        assert_eq!(p0[0], 0);
        assert_eq!(p1[0], 255);
        // Flat green channel stays put.
        assert_eq!(p0[1], 80);
        assert_eq!(p1[1], 80);
    }

    #[test]
    fn mask_limits_effect_to_selection() {
        let mut img = solid([0, 0, 0, 255]);
        let mut m = Mask::empty(2, 2);
        m.set(0, 0, 255);
        invert(&mut img, Some(&m));
        assert_eq!(img.pixel(0, 0), [255, 255, 255, 255]);
        assert_eq!(img.pixel(1, 1), [0, 0, 0, 255]);
    }

    #[test]
    fn nan_params_are_no_ops() {
        // Non-finite parameters must leave the image untouched — never panic and
        // never produce a solid-black (or NaN-poisoned) result.
        let orig = [60, 130, 200, 255];

        let mut a = solid(orig);
        brightness_contrast(&mut a, f32::NAN, 0.0, None);
        assert_eq!(a.pixel(0, 0), orig, "brightness_contrast NaN");

        let mut b = solid(orig);
        levels(&mut b, f32::NAN, 1.0, 1.0, 0.0, 1.0, None);
        assert_eq!(b.pixel(0, 0), orig, "levels NaN");

        let mut c = solid(orig);
        exposure(&mut c, f32::INFINITY, None);
        assert_eq!(c.pixel(0, 0), orig, "exposure inf");

        let mut d = solid(orig);
        // A NaN control point degrades the channel to identity, not constant.
        curves(&mut d, &[(0.0, f32::NAN), (1.0, 1.0)], &[], &[], &[], None);
        assert_eq!(d.pixel(0, 0), orig, "curves NaN point");

        let mut e = solid(orig);
        gamma(&mut e, f32::NAN, None);
        assert_eq!(e.pixel(0, 0), orig, "gamma NaN");

        let mut f = solid(orig);
        hue_saturation(&mut f, f32::NAN, 0.0, 0.0, None);
        assert_eq!(f.pixel(0, 0), orig, "hue_saturation NaN");

        let mut g = solid(orig);
        color_balance(
            &mut g,
            [f32::NAN, 0.0, 0.0],
            [0.0; 3],
            [0.0; 3],
            false,
            None,
        );
        assert_eq!(g.pixel(0, 0), orig, "color_balance NaN");

        let mut h = solid(orig);
        photo_filter(&mut h, [f32::NAN, 0.0, 0.0], 1.0, false, None);
        assert_eq!(h.pixel(0, 0), orig, "photo_filter NaN");

        let mut i = solid(orig);
        threshold(&mut i, f32::NAN, None);
        assert_eq!(i.pixel(0, 0), orig, "threshold NaN");

        let mut j = solid(orig);
        vibrance(&mut j, f32::NAN, None);
        assert_eq!(j.pixel(0, 0), orig, "vibrance NaN");
    }

    #[test]
    fn hsl_roundtrip_is_stable() {
        for &c in &[[200, 100, 50], [10, 220, 130], [0, 0, 0], [255, 255, 255]] {
            let rgb = [
                c[0] as f32 / 255.0,
                c[1] as f32 / 255.0,
                c[2] as f32 / 255.0,
            ];
            let back = hsl_to_rgb(rgb_to_hsl(rgb));
            for i in 0..3 {
                assert!((back[i] - rgb[i]).abs() < 0.01, "{:?} -> {:?}", rgb, back);
            }
        }
    }
}

#[cfg(test)]
mod shared_perceptual_kernel_tests {
    use super::*;
    #[test]
    fn shared_perceptual_kernels_keep_raster_rounding_and_extended_gray() {
        for levels in 2..=255 {
            for code in 0..=255 {
                let actual = posterize_channel(code as f32 / 255.0, levels);
                let expected = ((code as f64 / 255.0 * (levels - 1) as f64).round()
                    / (levels - 1) as f64) as f32;
                assert!((actual - expected).abs() < 1e-6);
            }
        }
        assert_eq!(desaturate_rgb([-2.0, 4.0, 1.0]), [1.0; 3]);
        assert_eq!(desaturate_rgb([-4.0, -2.0, -3.0]), [-3.0; 3]);
        assert_eq!(desaturate_rgb([8.0, 16.0, 4.0]), [10.0; 3]);
        assert!(threshold_rgb([1.0; 3], 1.0));
        assert!(threshold_rgb([0.0; 3], 0.0));
        assert!(threshold_rgb([0.5; 3], 0.5));
        assert!(!threshold_rgb([1.0, 0.0, 0.0], 0.5));
        assert!(threshold_rgb([0.0, 1.0, 0.0], 0.5));
    }
}

#[cfg(test)]
mod shared_channel_matrix_tests {
    use super::*;
    #[test]
    fn shared_channel_matrix_preserves_legacy_clamp_and_extended_native_math() {
        let matrix = [[2.0, -1.0, 0.5], [0.0, 1.0, 0.0], [-1.0, 0.0, 2.0]];
        assert_eq!(channel_mix_rgb([-2.0, 4.0, 3.0], matrix), [-6.5, 4.0, 8.0]);
        assert_eq!(normalized_gray_weights([0.0; 3]), [1.0 / 3.0; 3]);
        assert_eq!(normalized_gray_weights([1.0, 0.0, 1.0]), [0.5, 0.0, 0.5]);
        for code in 0..=255 {
            let rgb = [code as f32 / 255.0, 0.5, 1.0];
            let expected = [
                (2.0 * rgb[0] - rgb[1] + 0.5 * rgb[2]).clamp(0.0, 1.0),
                rgb[1],
                (-rgb[0] + 2.0 * rgb[2]).clamp(0.0, 1.0),
            ];
            assert_eq!(channel_mix_rgb(rgb, matrix).map(clamp01), expected);
        }
    }
}

#[cfg(test)]
mod shared_photo_filter_tests {
    use super::*;
    #[test]
    fn shared_photo_filter_retains_raster_order_clamp_and_extended_math() {
        for rgb in [
            [0.2, 0.4, 0.8],
            [-2.0, 4.0, 3.0],
            [0.000001; 3],
            [8.0, 16.0, 4.0],
        ] {
            for color in [[1.0, 0.5, 0.2], [0.0; 3], [0.2, 0.6, 1.0]] {
                for density in [0.0, 0.25, 1.0] {
                    for preserve in [false, true] {
                        let mut expected = [0.0; 3];
                        for i in 0..3 {
                            expected[i] = rgb[i] * (1.0 - density) + (rgb[i] * color[i]) * density;
                        }
                        if preserve {
                            let original = luma(rgb);
                            let new = luma(expected);
                            if new > 1e-4 {
                                let factor = original / new;
                                for c in &mut expected {
                                    *c = (*c * factor).clamp(0.0, 1.0);
                                }
                            }
                        }
                        assert_eq!(
                            photo_filter_rgb(rgb, color, density, preserve, true),
                            expected
                        );
                    }
                }
            }
        }
        assert_eq!(
            photo_filter_rgb([-2.0, 4.0, 3.0], [1.0; 3], 0.5, false, false),
            [-2.0, 4.0, 3.0]
        );
        assert_eq!(
            photo_filter_rgb([0.0; 3], [0.0; 3], 1.0, true, false),
            [0.0; 3]
        );
    }
}

#[cfg(test)]
mod shared_color_balance_tests {
    use super::*;
    #[test]
    fn shared_color_balance_retains_legacy_order_and_extended_native_output() {
        for rgb in [
            [0.2, 0.4, 0.8],
            [-2.0, 4.0, 3.0],
            [0.000001; 3],
            [8.0, 16.0, 4.0],
        ] {
            for shifts in [
                [[0.0; 3]; 3],
                [[0.4, -0.2, 0.1], [-0.3, 0.4, 0.2], [0.1, -0.4, 0.3]],
                [[1.0, -1.0, 0.0]; 3],
            ] {
                for preserve in [false, true] {
                    let (sw, mw, hw) = tonal_weights(luma(rgb));
                    let mut expected = [0.0; 3];
                    for i in 0..3 {
                        let shift = shifts[0][i] * sw + shifts[1][i] * mw + shifts[2][i] * hw;
                        expected[i] = (rgb[i] + 0.5 * shift).clamp(0.0, 1.0);
                    }
                    if preserve {
                        let original = luma(rgb);
                        let after = luma(expected);
                        if after > 1e-4 {
                            let factor = original / after;
                            for c in &mut expected {
                                *c = (*c * factor).clamp(0.0, 1.0);
                            }
                        }
                    }
                    assert_eq!(color_balance_rgb(rgb, shifts, preserve, true), expected);
                }
            }
        }
        assert_eq!(
            color_balance_rgb([-2.0, 4.0, 3.0], [[0.0; 3]; 3], false, false),
            [-2.0, 4.0, 3.0]
        );
    }
}

#[cfg(test)]
mod shared_transfer_curve_tests {
    use super::*;
    fn prior_curve_lut(points: &[(f32, f32)]) -> [u8; 256] {
        // Default to identity.
        let mut lut = [0u8; 256];
        for (i, l) in lut.iter_mut().enumerate() {
            *l = i as u8;
        }

        // Sanitize: finite-only, clamp to 0..1, sort by x, drop coincident x's.
        let mut pts: Vec<(f32, f32)> = points
            .iter()
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .map(|(x, y)| (clamp01(*x), clamp01(*y)))
            .collect();
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);

        let n = pts.len();
        if n < 2 {
            return lut; // identity — also the NaN/degenerate-input safe path
        }

        // Secant slopes between consecutive knots.
        let mut d = vec![0.0f32; n - 1];
        for i in 0..n - 1 {
            d[i] = (pts[i + 1].1 - pts[i].1) / (pts[i + 1].0 - pts[i].0);
        }

        // Initial Hermite tangents.
        let mut m = vec![0.0f32; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            if d[i - 1] * d[i] <= 0.0 {
                // Local extremum (or flat) — force a flat tangent to preserve monotonicity.
                m[i] = 0.0;
            } else {
                m[i] = (d[i - 1] + d[i]) / 2.0;
            }
        }

        // Fritsch–Carlson monotonicity limiting on each interval.
        for i in 0..n - 1 {
            if d[i].abs() < 1e-12 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
                continue;
            }
            let alpha = m[i] / d[i];
            let beta = m[i + 1] / d[i];
            let s = alpha * alpha + beta * beta;
            if s > 9.0 {
                let tau = 3.0 / s.sqrt();
                m[i] = tau * alpha * d[i];
                m[i + 1] = tau * beta * d[i];
            }
        }

        for (i, slot) in lut.iter_mut().enumerate() {
            let x = i as f32 / 255.0;
            let y = if x <= pts[0].0 {
                pts[0].1
            } else if x >= pts[n - 1].0 {
                pts[n - 1].1
            } else {
                let mut yy = pts[n - 1].1;
                for k in 0..n - 1 {
                    let (x0, y0) = pts[k];
                    let (x1, y1) = pts[k + 1];
                    if x >= x0 && x <= x1 {
                        let h = x1 - x0;
                        let t = (x - x0) / h;
                        let t2 = t * t;
                        let t3 = t2 * t;
                        // Cubic Hermite basis functions.
                        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
                        let h10 = t3 - 2.0 * t2 + t;
                        let h01 = -2.0 * t3 + 3.0 * t2;
                        let h11 = t3 - t2;
                        yy = h00 * y0 + h10 * h * m[k] + h01 * y1 + h11 * h * m[k + 1];
                        break;
                    }
                }
                yy
            };
            *slot = (clamp01(y) * 255.0).round() as u8;
        }
        lut
    }

    #[test]
    fn shared_transfer_curve_exact_legacy_lut_and_extended_endpoints() {
        for points in [
            vec![],
            vec![(0.5, 0.25)],
            vec![
                (0.0, 0.0),
                (0.25, 0.25),
                (0.5, 0.5),
                (0.75, 0.75),
                (1.0, 1.0),
            ],
            vec![(1.0, 0.0), (0.0, 1.0), (0.5, 0.3)],
            vec![(0.2, 0.4), (0.4, 0.1), (0.6, 0.8), (0.9, 0.5)],
            vec![(0.0, 0.0), (0.0, 1.0), (1.0, 0.2)],
            vec![(f32::NAN, 0.0), (-1.0, 2.0), (2.0, -1.0)],
            vec![(0.0, 0.0), (0.000002, 1.0), (0.5, 0.4), (1.0, 1.0)],
        ] {
            assert_eq!(curve_lut(&points), prior_curve_lut(&points));
        }
        let curve = TransferCurve::new(&[(0.0, 0.1), (1.0, 0.9)]);
        assert!((curve.sample(-1.0, true) + 0.7).abs() < 1e-6);
        assert!((curve.sample(2.0, true) - 1.7).abs() < 1e-6);
        assert_eq!(curve.sample(-1.0, false), 0.1);
        assert_eq!(curve.sample(2.0, false), 0.9);
        for contrast in [-1.0, 0.0, 0.0000005, 0.5, 1.0] {
            let points = catalog_curve_points(
                [
                    [0.0, 0.0],
                    [0.25, 0.25],
                    [0.5, 0.8],
                    [0.75, 0.75],
                    [1.0, 1.0],
                ],
                contrast,
            );
            assert_eq!(
                points[2].1,
                if contrast.abs() > 1e-6 {
                    0.5 + contrast * 0.25
                } else {
                    0.8
                }
            );
        }
        assert_eq!(
            catalog_curve_points([[0.5, 0.2]; 5], 0.0),
            vec![(0.0, 0.0), (1.0, 1.0)]
        );
    }
}

#[cfg(test)]
mod shared_levels_tests {
    use super::*;
    #[test]
    fn shared_levels_retains_legacy_math_and_native_residual() {
        for bw in [[0.0f32, 1.0], [0.1, 0.8], [0.5, 0.5], [0.8, 0.2]] {
            for gamma in [0.01f32, 0.5, 1.0, 2.2, 9.99] {
                for output in [[0.0, 1.0], [0.1, 0.9], [1.0, 0.0], [0.3, 0.3]] {
                    for rgb in [[0.0, 0.1, 0.5], [0.2, 0.8, 1.0], [-2.0, 4.0, 16.0]] {
                        let expected = rgb.map(|v| {
                            let n = ((v - bw[0]) / (bw[1] - bw[0]).max(1e-4)).clamp(0.0, 1.0);
                            (output[0]
                                + n.powf(1.0 / gamma.clamp(0.01, 9.99)) * (output[1] - output[0]))
                                .clamp(0.0, 1.0)
                        });
                        assert_eq!(levels_rgb(rgb, bw, gamma, output, false), expected);
                    }
                    let px = [51u8, 128, 230, 77];
                    let mut image = RasterImage::filled(1, 1, px);
                    levels(&mut image, bw[0], bw[1], gamma, output[0], output[1], None);
                    let expected = std::array::from_fn(|i| {
                        if i == 3 {
                            px[3]
                        } else {
                            let n = ((px[i] as f32 / 255.0 - bw[0]) / (bw[1] - bw[0]).max(1e-4))
                                .clamp(0.0, 1.0);
                            ((output[0]
                                + n.powf(1.0 / gamma.clamp(0.01, 9.99)) * (output[1] - output[0]))
                                .clamp(0.0, 1.0)
                                * 255.0)
                                .round() as u8
                        }
                    });
                    assert_eq!(image.pixel(0, 0), expected);
                }
            }
        }
        assert_eq!(
            levels_rgb([-2.0, 4.0, 16.0], [0.0, 1.0], 0.01, [0.0, 1.0], true),
            [-2.0, 4.0, 16.0]
        );
        assert_eq!(
            levels_rgb([-2.0, 4.0, 16.0], [0.0, 1.0], 9.99, [0.0, 1.0], true),
            [-2.0, 4.0, 16.0]
        );
    }
}

/// Photonic's extended HSL component transform, for straight linear Rec.709.
/// The adaptive envelope is exactly [0,1] on bounded input and continuous at
/// its boundaries. It avoids clipping signed/HDR values or mistaking all HDR
/// colors for fully saturated ones. Legacy raster HSL remains unchanged.
/// Lightness endpoints target nominal black/white while extended residuals
/// fade with the lightness adjustment. Achromatic vibrance has no hue to boost.
pub fn extended_linear_hsl_rgb(
    rgb: [f32; 3],
    hue_deg: f32,
    saturation: f32,
    lightness: f32,
    vibrance_amount: Option<f32>,
) -> [f32; 3] {
    if vibrance_amount == Some(0.0)
        || (vibrance_amount.is_none() && hue_deg == 0.0 && saturation == 0.0 && lightness == 0.0)
    {
        return rgb;
    }
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let relative_chroma = (max - min) / min.abs().max(max.abs()).max(1.0);
    if vibrance_amount.is_some() && relative_chroma <= 1e-6 {
        return rgb;
    }
    let low = 2.0 * min.min(0.0);
    let high = 1.0 + 2.0 * (max - 1.0).max(0.0);
    let span = high - low;
    let mut hsl = rgb_to_hsl(rgb.map(|v| (v - low) / span));
    hsl[0] = (hsl[0] + hue_deg).rem_euclid(360.0);
    hsl[1] = match vibrance_amount {
        Some(amount) => {
            // Smoothly suppress the arbitrary hue near achromatic input. All
            // non-neutral 8-bit bounded RGB lies above this tiny transition.
            let t = ((relative_chroma - 1e-6) / (1e-4 - 1e-6)).clamp(0.0, 1.0);
            let gate = t * t * (3.0 - 2.0 * t);
            (hsl[1] + amount * (1.0 - hsl[1]) * gate).clamp(0.0, 1.0)
        }
        None => (hsl[1] * (1.0 + saturation)).clamp(0.0, 1.0),
    };
    let t = lightness.abs();
    let keep = 1.0 - t;
    let physical_lightness =
        keep * (max + min) * 0.5 + t * if lightness >= 0.0 { 1.0 } else { 0.0 };
    if hsl[1] == 0.0 {
        return [physical_lightness; 3];
    }
    let new_low = keep * low;
    let new_high = 1.0 + keep * (high - 1.0);
    hsl[2] = (physical_lightness - new_low) / (new_high - new_low);
    hsl_to_rgb(hsl).map(|v| new_low + v * (new_high - new_low))
}

#[cfg(test)]
mod extended_linear_hsl_tests {
    use super::*;
    #[test]
    fn native_linear_hsl_bounded_legacy_byte_agreement() {
        for rgb in [
            [20u8, 100, 230],
            [230, 60, 30],
            [10, 200, 50],
            [200, 170, 210],
            [0, 128, 255],
            [255, 0, 128],
        ] {
            let f = rgb.map(|v| v as f32 / 255.0);
            for (h, s, l) in [
                (-180.0, -1.0, 0.0),
                (60.0, 0.5, 0.4),
                (30.0, 0.3, -0.6),
                (0.0, 0.0, 1.0),
                (0.0, 0.0, -1.0),
            ] {
                let mut image = RasterImage::new(1, 1);
                image.set_pixel(0, 0, [rgb[0], rgb[1], rgb[2], 128]);
                hue_saturation(&mut image, h, s, l, None);
                let expected = extended_linear_hsl_rgb(f, h, s, l, None)
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                assert_eq!(
                    image.pixel(0, 0),
                    [expected[0], expected[1], expected[2], 128]
                );
            }
            for amount in [-1.0, 0.0, 0.5, 1.0] {
                let mut image = RasterImage::new(1, 1);
                image.set_pixel(0, 0, [rgb[0], rgb[1], rgb[2], 128]);
                vibrance(&mut image, amount, None);
                let expected = extended_linear_hsl_rgb(f, 0.0, 0.0, 0.0, Some(amount))
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                assert_eq!(
                    image.pixel(0, 0),
                    [expected[0], expected[1], expected[2], 128]
                );
            }
        }
    }
    #[test]
    fn native_linear_hsl_extended_endpoints_neutrals_and_continuity() {
        for rgb in [
            [2.0, 2.1, 2.05],
            [-3.0, -2.9, -2.95],
            [-0.5, 3.0, 0.25],
            [0.2, 0.4, 0.6],
            [65504.0, -65504.0, 1.0],
        ] {
            assert_eq!(extended_linear_hsl_rgb(rgb, 0.0, 0.0, 0.0, None), rgb);
            let gray = extended_linear_hsl_rgb(rgb, 0.0, -1.0, 0.0, None);
            assert!((gray[0] - gray[1]).abs() < 0.001 && (gray[1] - gray[2]).abs() < 0.001);
            assert_eq!(
                gray[0],
                (rgb[0].max(rgb[1]).max(rgb[2]) + rgb[0].min(rgb[1]).min(rgb[2])) * 0.5
            );
            for light in [-1.0, 1.0] {
                let p = extended_linear_hsl_rgb(rgb, 30.0, 0.5, light, None);
                let goal = if light > 0.0 { 1.0 } else { 0.0 };
                assert!(p.iter().all(|v| (*v - goal).abs() < 0.00001));
            }
        }
        for v in [-8.0, 0.0, 0.18, 1.0, 8.0, 65504.0] {
            for amount in [-1.0, 0.5, 1.0] {
                assert_eq!(
                    extended_linear_hsl_rgb([v; 3], 0.0, 0.0, 0.0, Some(amount)),
                    [v; 3]
                );
            }
        }
        for rgb in [[2.0, 2.1, 2.05], [-3.0, -2.9, -2.95]] {
            let p = extended_linear_hsl_rgb(rgb, 0.0, 0.0, 0.0, Some(0.5));
            let spread = |x: [f32; 3]| x[0].max(x[1]).max(x[2]) - x[0].min(x[1]).min(x[2]);
            assert!(spread(p) > spread(rgb));
        }
        for edge in [0.0f32, 1.0] {
            for vib in [None, Some(0.5)] {
                let a = extended_linear_hsl_rgb([edge - 0.000001, 0.25, 0.75], 30.0, 0.3, 0.2, vib);
                let b = extended_linear_hsl_rgb([edge + 0.000001, 0.25, 0.75], 30.0, 0.3, 0.2, vib);
                assert!(a.iter().zip(b).all(|(a, b)| (*a - b).abs() < 0.00002));
            }
        }
        let mut previous = extended_linear_hsl_rgb([0.1; 3], 0.0, 0.0, 0.0, Some(1.0));
        for i in 1..=200 {
            let d = i as f32 * 0.000001;
            let p =
                extended_linear_hsl_rgb([0.1, 0.1 + d, 0.1 + d * 0.5], 0.0, 0.0, 0.0, Some(1.0));
            assert!(p.iter().zip(previous).all(|(a, b)| (*a - b).abs() < 0.01));
            previous = p;
        }
    }
}

#[cfg(test)]
mod floating_invert_tests {
    use super::*;
    #[test]
    fn native_transfer_invert_exhaustive_legacy_byte_agreement() {
        for channel in 0..=255u8 {
            for alpha in [0, 128, 255] {
                let mut image = RasterImage::new(1, 1);
                let rgb = [channel, 255 - channel, channel / 2];
                image.set_pixel(0, 0, [rgb[0], rgb[1], rgb[2], alpha]);
                invert(&mut image, None);
                let expected =
                    rgb.map(|v| (invert_channel(v as f32 / 255.0) * 255.0).round() as u8);
                assert_eq!(
                    image.pixel(0, 0),
                    [expected[0], expected[1], expected[2], alpha]
                );
            }
        }
        for value in [-4.0, -1.0, 0.0, 0.25, 1.0, 4.0] {
            assert_eq!(invert_channel(invert_channel(value)), value);
        }
    }
}
