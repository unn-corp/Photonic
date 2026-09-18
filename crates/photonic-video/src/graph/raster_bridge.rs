//! Raster-kernel bridge into the video catalogue (K-B16 / 30 §4).
//!
//! Converts the video working buffer ([`super::ops::Image`] — linear
//! premultiplied f32) to/from [`RasterImage`] under the effect's
//! [`OperandSpace`](photonic_core::timeline::effect_manifest::OperandSpace),
//! then dispatches the existing `photonic_core::raster` CPU oracle.
//!
//! # Operand contract (30 §4.2)
//!
//! | Space | Conversion |
//! |---|---|
//! | `TransferStraight` | unpremult → sRGB encode → kernel → linear decode → premult |
//! | `LinearStraight`   | unpremult → *linear* 0..1 as u8 → kernel → back → premult |
//!
//! LinearStraight avoids the gamma encode that would halo spatial ops. The
//! photo kernels still see 8-bit quantised values; GPU twins operate in true
//! linear f32 and are the interactive path. Transfer ops (levels, curves,
//! posterize…) must run in the encoded domain — matching the photo editor.
//!
//! Bridged effects lower as `EffectKind::Unknown(tag)`. The GPU evaluator has
//! WGSL twins for the neighbourhood/point ops listed in [`BRIDGED_IDS`].

use photonic_core::raster::adjust::{
    black_and_white, channel_mixer, color_balance, curves, desaturate, hue_saturation, invert,
    levels, photo_filter, posterize, threshold, vibrance,
};
use photonic_core::raster::advanced::{
    chromatic_aberration, clarity, lens_blur, reduce_noise, smart_sharpen, surface_blur, vignette,
};
use photonic_core::raster::filter::{
    add_noise, box_blur, emboss, find_edges, gaussian_blur, high_pass, median, mosaic, motion_blur,
    unsharp_mask,
};
use photonic_core::raster::repair::dust_and_scratches;
use photonic_core::raster::warp::{perspective, pinch, ripple, spherize};
use photonic_core::timeline::effect_manifest::{manifest, EffectId, OperandSpace};
use photonic_core::RasterImage;

use super::ops::Image;
use crate::contract::ResolvedParams;

/// Stable EffectId strings this bridge can evaluate on CPU.
pub const BRIDGED_IDS: &[&str] = &[
    // Blur / sharpen / noise
    "blur.box",
    "blur.gaussian",
    "blur.motion",
    "blur.surface",
    "blur.lens",
    "sharpen.unsharp_raster",
    "sharpen.smart",
    "filter.high_pass",
    "filter.median",
    "noise.reduce",
    "repair.dust_and_scratches",
    // Stylize
    "stylize.emboss",
    "stylize.find_edges",
    "stylize.mosaic",
    "stylize.grain",
    "stylize.vignette",
    "stylize.chromatic_aberration",
    // Color (Transfer + Linear)
    "color.levels",
    "color.curves",
    "color.posterize",
    "color.threshold",
    "color.channel_mixer",
    "color.hue_saturation",
    "color.vibrance",
    "color.desaturate",
    "color.black_and_white",
    "color.invert_raster",
    "color.clarity",
    "color.photo_filter",
    "color.color_balance",
    // Geometry / warp
    "geo.pinch",
    "geo.spherize",
    "geo.ripple",
    "geo.perspective",
    // Util (30 §5.1) — pure working-buffer ops, no RasterImage detour
    "util.unpremultiply",
    "util.alpha_view",
    "util.drop_shadow",
    "util.outline",
];

pub fn is_bridged(id: &str) -> bool {
    BRIDGED_IDS.contains(&id)
}

/// Operand space for a bridged id — from the manifest when present, else Linear.
pub fn operand_space(id: &str) -> OperandSpace {
    manifest(EffectId::new(id.to_string()))
        .map(|m| m.space)
        .unwrap_or(OperandSpace::LinearStraight)
}

/// Apply a bridged raster kernel. Returns `None` if `id` is not bridged.
pub fn apply(id: &str, input: &Image, params: &ResolvedParams) -> Option<Image> {
    if !is_bridged(id) {
        return None;
    }
    // Util ops work directly in linear premult working space.
    if id.starts_with("util.") {
        return dispatch_util(id, input, params);
    }
    let space = operand_space(id);
    let mut raster = image_to_raster(input, space);
    dispatch(id, &mut raster, params)?;
    Some(raster_to_image(&raster, space))
}

/// Back-compat: radius/amount-only dispatch used by older call sites/tests.
pub fn apply_simple(id: &str, input: &Image, radius: f32, amount: f32) -> Option<Image> {
    let params = ResolvedParams {
        entries: vec![
            (
                photonic_core::timeline::PropPath::new("params.radius"),
                photonic_core::timeline::PropValue::Float(radius as f64),
            ),
            (
                photonic_core::timeline::PropPath::new("params.amount"),
                photonic_core::timeline::PropValue::Float(amount as f64),
            ),
        ],
    };
    apply(id, input, &params)
}

fn dispatch(id: &str, raster: &mut RasterImage, params: &ResolvedParams) -> Option<()> {
    let f = |path: &str, d: f32| params.f32_or(path, d);
    match id {
        "blur.box" => {
            let r = f("params.radius", 1.0).max(0.0).round() as u32;
            box_blur(raster, r.max(1), None);
        }
        "blur.gaussian" => {
            gaussian_blur(raster, f("params.radius", 1.0).max(0.0), None);
        }
        "blur.motion" => {
            let dist = f("params.distance", 8.0).max(0.0).round() as u32;
            motion_blur(raster, f("params.angle", 0.0), dist, None);
        }
        "sharpen.unsharp_raster" => {
            unsharp_mask(
                raster,
                f("params.radius", 1.0).max(0.0),
                f("params.amount", 1.0).max(0.0),
                0,
                None,
            );
        }
        "filter.high_pass" => high_pass(raster, f("params.radius", 2.0).max(0.1), None),
        "filter.median" => {
            let r = f("params.radius", 1.0).max(0.0).round() as u32;
            median(raster, r.max(1), None);
        }
        "stylize.emboss" => emboss(raster, None),
        "stylize.find_edges" => find_edges(raster, None),
        "stylize.mosaic" => {
            let b = f("params.block", 8.0).max(1.0).round() as u32;
            mosaic(raster, b, None);
        }
        "stylize.grain" => {
            let seed = f("params.seed", 1.0).max(0.0).round() as u64;
            add_noise(
                raster,
                f("params.amount", 0.1).clamp(0.0, 1.0),
                f("params.monochrome", 1.0) > 0.5,
                seed,
                None,
            );
        }
        "stylize.vignette" => {
            vignette(
                raster,
                f("params.amount", -0.5).clamp(-1.0, 1.0),
                f("params.feather", 0.5).clamp(0.0, 1.0),
                None,
            );
        }
        "stylize.chromatic_aberration" => {
            chromatic_aberration(raster, f("params.amount", 2.0), None);
        }
        "color.levels" => {
            levels(
                raster,
                f("params.in_black", 0.0),
                f("params.in_white", 1.0),
                f("params.gamma", 1.0),
                f("params.out_black", 0.0),
                f("params.out_white", 1.0),
                None,
            );
        }
        "color.posterize" => {
            let n = f("params.levels", 4.0).round().clamp(2.0, 255.0) as u32;
            posterize(raster, n, None);
        }
        "color.threshold" => threshold(raster, f("params.level", 0.5), None),
        "color.channel_mixer" => {
            // Identity matrix by default.
            let r = [
                f("params.rr", 1.0),
                f("params.rg", 0.0),
                f("params.rb", 0.0),
            ];
            let g = [
                f("params.gr", 0.0),
                f("params.gg", 1.0),
                f("params.gb", 0.0),
            ];
            let b = [
                f("params.br", 0.0),
                f("params.bg", 0.0),
                f("params.bb", 1.0),
            ];
            channel_mixer(raster, r, g, b, None);
        }
        "color.hue_saturation" => {
            hue_saturation(
                raster,
                f("params.hue", 0.0),
                f("params.saturation", 0.0),
                f("params.lightness", 0.0),
                None,
            );
        }
        "color.vibrance" => vibrance(raster, f("params.amount", 0.0), None),
        "color.desaturate" => desaturate(raster, None),
        "color.black_and_white" => {
            black_and_white(
                raster,
                [
                    f("params.wr", 0.299),
                    f("params.wg", 0.587),
                    f("params.wb", 0.114),
                ],
                None,
            );
        }
        "color.invert_raster" => invert(raster, None),
        "color.clarity" => clarity(raster, f("params.amount", 0.0), None),
        "color.curves" => {
            // Up to 5 authored knots (p0..p4). Non-zero contrast overrides p2.y
            // as a midtone pivot (authoring convenience).
            let contrast = f("params.contrast", 0.0).clamp(-1.0, 1.0);
            let mut pts = vec![
                (f("params.p0x", 0.0), f("params.p0y", 0.0)),
                (f("params.p1x", 0.25), f("params.p1y", 0.25)),
                (f("params.p2x", 0.5), f("params.p2y", 0.5)),
                (f("params.p3x", 0.75), f("params.p3y", 0.75)),
                (f("params.p4x", 1.0), f("params.p4y", 1.0)),
            ];
            if contrast.abs() > 1e-6 {
                pts[2].1 = (0.5 + contrast * 0.25).clamp(0.05, 0.95);
            }
            // Drop duplicate x's after sort (curves() also sanitizes).
            pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
            if pts.len() < 2 {
                pts = vec![(0.0, 0.0), (1.0, 1.0)];
            }
            curves(raster, &pts, &[], &[], &[], None);
        }
        "color.photo_filter" => {
            photo_filter(
                raster,
                [
                    f("params.r", 1.0).clamp(0.0, 1.0),
                    f("params.g", 0.5).clamp(0.0, 1.0),
                    f("params.b", 0.2).clamp(0.0, 1.0),
                ],
                f("params.density", 0.25).clamp(0.0, 1.0),
                f("params.preserve_luminosity", 1.0) > 0.5,
                None,
            );
        }
        "color.color_balance" => {
            let s = [
                f("params.shadows_r", 0.0),
                f("params.shadows_g", 0.0),
                f("params.shadows_b", 0.0),
            ];
            let m = [
                f("params.midtones_r", 0.0),
                f("params.midtones_g", 0.0),
                f("params.midtones_b", 0.0),
            ];
            let h = [
                f("params.highlights_r", 0.0),
                f("params.highlights_g", 0.0),
                f("params.highlights_b", 0.0),
            ];
            color_balance(
                raster,
                s,
                m,
                h,
                f("params.preserve_luminosity", 1.0) > 0.5,
                None,
            );
        }
        "blur.surface" => {
            let r = f("params.radius", 2.0).max(0.0).round() as u32;
            surface_blur(raster, r, f("params.threshold", 0.25).clamp(0.0, 1.0), None);
        }
        "blur.lens" => lens_blur(raster, f("params.radius", 4.0).max(0.0), None),
        "sharpen.smart" => {
            smart_sharpen(
                raster,
                f("params.amount", 1.0).max(0.0),
                f("params.radius", 1.0).max(0.0),
                f("params.threshold", 0.0).clamp(0.0, 255.0) as u8,
                None,
            );
        }
        "noise.reduce" => reduce_noise(raster, f("params.strength", 0.5).clamp(0.0, 1.0), None),
        "repair.dust_and_scratches" => {
            dust_and_scratches(
                raster,
                f("params.radius", 1.0).max(0.0).round() as u32,
                f("params.threshold", 16.0).clamp(0.0, 255.0) as u8,
                None,
            );
        }
        "geo.pinch" => pinch(raster, f("params.amount", 0.0).clamp(-1.0, 1.0), None),
        "geo.spherize" => spherize(raster, f("params.amount", 0.0).clamp(-1.0, 1.0), None),
        "geo.ripple" => {
            ripple(
                raster,
                f("params.amplitude", 4.0),
                f("params.wavelength", 16.0).max(1.0),
                None,
            );
        }
        "geo.perspective" => {
            // Normalized corner destinations (0..1 of frame): TL TR BR BL.
            // Defaults are identity (full rect).
            let w = raster.width as f32;
            let h = raster.height as f32;
            let dst = [
                (f("params.tl_x", 0.0) * w, f("params.tl_y", 0.0) * h),
                (f("params.tr_x", 1.0) * w, f("params.tr_y", 0.0) * h),
                (f("params.br_x", 1.0) * w, f("params.br_y", 1.0) * h),
                (f("params.bl_x", 0.0) * w, f("params.bl_y", 1.0) * h),
            ];
            *raster = perspective(raster, dst);
        }
        _ => return None,
    }
    Some(())
}

/// Util catalogue kernels operate on linear premult `Image` (no u8 detour).
fn dispatch_util(id: &str, input: &Image, params: &ResolvedParams) -> Option<Image> {
    let f = |path: &str, d: f32| params.f32_or(path, d);
    match id {
        "util.unpremultiply" => Some(util_unpremultiply(input)),
        "util.alpha_view" => {
            // mode: 0 = alpha-as-luma, 1 = premul RGB, 2 = straight RGB
            let mode = f("params.mode", 0.0).round() as i32;
            Some(util_alpha_view(input, mode))
        }
        "util.drop_shadow" => Some(util_drop_shadow(
            input,
            f("params.x", 4.0),
            f("params.y", 4.0),
            f("params.radius", 3.0).max(0.0),
            [f("params.r", 0.0), f("params.g", 0.0), f("params.b", 0.0)],
            f("params.opacity", 0.5).clamp(0.0, 1.0),
        )),
        "util.outline" => Some(util_outline(
            input,
            f("params.thickness", 2.0).max(0.0),
            [f("params.r", 1.0), f("params.g", 1.0), f("params.b", 1.0)],
            f("params.opacity", 1.0).clamp(0.0, 1.0),
        )),
        _ => None,
    }
}

fn util_unpremultiply(input: &Image) -> Image {
    let mut out = Image::new(input.width, input.height);
    for (i, p) in input.pixels.iter().enumerate() {
        let a = p[3].clamp(0.0, 1.0);
        if a > 1e-6 {
            out.pixels[i] = [p[0] / a, p[1] / a, p[2] / a, a];
        } else {
            out.pixels[i] = [0.0, 0.0, 0.0, 0.0];
        }
    }
    out
}

fn util_alpha_view(input: &Image, mode: i32) -> Image {
    let mut out = Image::new(input.width, input.height);
    for (i, p) in input.pixels.iter().enumerate() {
        let a = p[3].clamp(0.0, 1.0);
        out.pixels[i] = match mode {
            1 => *p, // premul RGB as-is
            2 => {
                // straight RGB (unpremult) with full alpha so it is visible
                if a > 1e-6 {
                    [p[0] / a, p[1] / a, p[2] / a, 1.0]
                } else {
                    [0.0, 0.0, 0.0, 1.0]
                }
            }
            _ => [a, a, a, 1.0], // alpha as luma
        };
    }
    out
}

fn sample_clamped(img: &Image, x: i32, y: i32) -> [f32; 4] {
    let x = x.clamp(0, img.width as i32 - 1) as u32;
    let y = y.clamp(0, img.height as i32 - 1) as u32;
    img.pixel(x, y)
}

fn util_drop_shadow(
    input: &Image,
    ox: f32,
    oy: f32,
    radius: f32,
    color: [f32; 3],
    opacity: f32,
) -> Image {
    let w = input.width;
    let h = input.height;
    let mut alpha = vec![0.0f32; (w * h) as usize];
    let dx = ox.round() as i32;
    let dy = oy.round() as i32;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let s = sample_clamped(input, x - dx, y - dy);
            alpha[(y as u32 * w + x as u32) as usize] = s[3];
        }
    }
    // Box blur alpha (separable, radius in px).
    let r = radius.round().max(0.0) as i32;
    if r > 0 {
        let mut tmp = alpha.clone();
        let k = 2 * r + 1;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let mut acc = 0.0;
                for i in -r..=r {
                    let xx = (x + i).clamp(0, w as i32 - 1);
                    acc += alpha[(y as u32 * w + xx as u32) as usize];
                }
                tmp[(y as u32 * w + x as u32) as usize] = acc / k as f32;
            }
        }
        alpha = tmp;
        let mut tmp = alpha.clone();
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let mut acc = 0.0;
                for j in -r..=r {
                    let yy = (y + j).clamp(0, h as i32 - 1);
                    acc += alpha[(yy as u32 * w + x as u32) as usize];
                }
                tmp[(y as u32 * w + x as u32) as usize] = acc / k as f32;
            }
        }
        alpha = tmp;
    }
    let mut out = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let sa = (alpha[i] * opacity).clamp(0.0, 1.0);
            let shadow = [color[0] * sa, color[1] * sa, color[2] * sa, sa];
            let top = input.pixel(x, y);
            // Premultiplied over: top over shadow.
            let oa = top[3] + shadow[3] * (1.0 - top[3]);
            let or = top[0] + shadow[0] * (1.0 - top[3]);
            let og = top[1] + shadow[1] * (1.0 - top[3]);
            let ob = top[2] + shadow[2] * (1.0 - top[3]);
            out.set(x, y, [or, og, ob, oa]);
        }
    }
    out
}

/// Antialiasing half-width for the outline's outer edge, in pixels
/// (proposal 208 §4.1 `aa_px`, default 1.0). Deliberately a constant rather
/// than a manifest param: `OUTLINE_PARAMS` is part of the frozen catalogue
/// schema, and 30 §5 asks for analytic antialiasing, not a user knob.
const OUTLINE_AA_PX: f32 = 1.0;

/// `util.outline` (30 §5, proposal 208 §4.1 `OutlineFromSdf`) — a stroke band
/// around the coverage isosurface, composited **behind** the source.
///
/// 30 §5 requires this be signed-distance-based "not the reference's rasterised
/// dilation: analytic antialiasing, smooth at hard angles, no thickness
/// ceiling". The original implementation was that rejected dilation: a
/// per-pixel `(2t+1)²` box search for the nearest opaque pixel. It had three
/// defects this rewrite closes.
///
/// 1. **Cost.** O(W·H·thickness²) — at the manifest's own 64px maximum that is
///    16,641 samples *per pixel*. The shared Jump-Flood field
///    ([`ops::coverage_signed_distance`]) is O(W·H·log max(W,H)) and does not
///    grow with thickness at all, so the "no thickness ceiling" clause becomes
///    true rather than aspirational.
/// 2. **Boxy corners.** Distances were quantised to integer pixel offsets and
///    thresholded at `alpha > 0.5`, so a diagonal or a corner stepped. The
///    distance field is continuous, and the band edge now resolves with a
///    smoothstep of half-width [`OUTLINE_AA_PX`].
/// 3. **It was a glow, not a stroke.** Coverage was `1 - d/thickness`, a linear
///    ramp across the *whole* band, so the outline had no solid core and faded
///    from the shape edge outward. The band is now solid out to `thickness` and
///    only antialiases at its outer rim.
///
/// The band runs from the isosurface outward; the portion under the source is
/// hidden by the composite, so the visible ring is `thickness` px wide.
fn util_outline(input: &Image, thickness: f32, color: [f32; 3], opacity: f32) -> Image {
    let w = input.width;
    let h = input.height;
    if thickness <= 0.0 || !thickness.is_finite() || opacity <= 0.0 {
        return input.clone();
    }
    // Negative inside the shape, positive outside, in pixels.
    let dist = super::ops::coverage_signed_distance(input);
    let mut out = Image::new(w, h);
    let aa = OUTLINE_AA_PX.max(1e-4);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let center = input.pixel(x, y);
            // Solid from the isosurface out to `thickness`, then a smoothstep
            // of half-width `aa` centred on the outer edge.
            let t = ((dist[i] - (thickness - aa)) / (2.0 * aa)).clamp(0.0, 1.0);
            let band = 1.0 - t * t * (3.0 - 2.0 * t);
            let ea = band * opacity;
            let outline = [color[0] * ea, color[1] * ea, color[2] * ea, ea];
            // Premultiplied over: source over outline (outline behind content).
            let ia = center[3];
            out.set(
                x,
                y,
                [
                    center[0] + outline[0] * (1.0 - ia),
                    center[1] + outline[1] * (1.0 - ia),
                    center[2] + outline[2] * (1.0 - ia),
                    ia + outline[3] * (1.0 - ia),
                ],
            );
        }
    }
    out
}

/// Working buffer → raster, honouring operand space.
fn image_to_raster(img: &Image, space: OperandSpace) -> RasterImage {
    let mut out = RasterImage::new(img.width, img.height);
    for (i, p) in img.pixels.iter().enumerate() {
        let a = p[3].clamp(0.0, 1.0);
        let (r, g, b) = if a > 1e-6 {
            (p[0] / a, p[1] / a, p[2] / a)
        } else {
            (0.0, 0.0, 0.0)
        };
        let o = i * 4;
        match space {
            OperandSpace::TransferStraight => {
                out.pixels[o] = linear_to_srgb_u8(r);
                out.pixels[o + 1] = linear_to_srgb_u8(g);
                out.pixels[o + 2] = linear_to_srgb_u8(b);
            }
            OperandSpace::LinearStraight => {
                // Linear 0..1 quantised to u8 — no OETF (avoids blur halos).
                out.pixels[o] = linear_to_u8(r);
                out.pixels[o + 1] = linear_to_u8(g);
                out.pixels[o + 2] = linear_to_u8(b);
            }
        }
        out.pixels[o + 3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Raster → working buffer, inverse of [`image_to_raster`].
fn raster_to_image(img: &RasterImage, space: OperandSpace) -> Image {
    let mut out = Image::new(img.width, img.height);
    for (i, p) in out.pixels.iter_mut().enumerate() {
        let o = i * 4;
        let a = img.pixels[o + 3] as f32 / 255.0;
        let (r, g, b) = match space {
            OperandSpace::TransferStraight => (
                srgb_u8_to_linear(img.pixels[o]),
                srgb_u8_to_linear(img.pixels[o + 1]),
                srgb_u8_to_linear(img.pixels[o + 2]),
            ),
            OperandSpace::LinearStraight => (
                u8_to_linear(img.pixels[o]),
                u8_to_linear(img.pixels[o + 1]),
                u8_to_linear(img.pixels[o + 2]),
            ),
        };
        *p = [r * a, g * a, b * a, a];
    }
    out
}

fn linear_to_u8(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8
}

fn u8_to_linear(c: u8) -> f32 {
    c as f32 / 255.0
}

fn linear_to_srgb_u8(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

fn srgb_u8_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ir::LinearColor;
    use crate::graph::ops::solid;

    #[test]
    fn bridged_ids_are_recognized() {
        assert!(is_bridged("stylize.emboss"));
        assert!(is_bridged("color.levels"));
        assert!(is_bridged("blur.motion"));
        assert!(is_bridged("geo.pinch"));
        assert!(is_bridged("geo.perspective"));
        assert!(is_bridged("color.curves"));
        assert!(is_bridged("sharpen.smart"));
        assert!(is_bridged("noise.reduce"));
        assert!(is_bridged("blur.surface"));
        assert!(!is_bridged("nope.fake"));
        assert_eq!(BRIDGED_IDS.len(), 38);
        assert!(is_bridged("util.outline"));
        assert!(is_bridged("util.drop_shadow"));
    }

    #[test]
    fn curves_contrast_moves_midtones() {
        let img = solid(
            4,
            4,
            LinearColor {
                r: 0.5,
                g: 0.5,
                b: 0.5,
                a: 1.0,
            },
        );
        let params = ResolvedParams {
            entries: vec![(
                photonic_core::timeline::PropPath::new("params.contrast"),
                photonic_core::timeline::PropValue::Float(0.8),
            )],
        };
        let out = apply("color.curves", &img, &params).expect("bridged");
        // Positive contrast lifts mid above 0.5 in transfer space.
        assert!(
            out.pixel(0, 0)[0] > 0.5,
            "contrast curve should lift mid: {:?}",
            out.pixel(0, 0)
        );
    }

    #[test]
    fn curves_multipoint_identity_at_default_knots() {
        let img = solid(
            4,
            4,
            LinearColor {
                r: 0.3,
                g: 0.5,
                b: 0.7,
                a: 1.0,
            },
        );
        // Explicit identity knots, contrast 0.
        let params = ResolvedParams {
            entries: vec![
                (
                    photonic_core::timeline::PropPath::new("params.contrast"),
                    photonic_core::timeline::PropValue::Float(0.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.p0x"),
                    photonic_core::timeline::PropValue::Float(0.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.p0y"),
                    photonic_core::timeline::PropValue::Float(0.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.p4x"),
                    photonic_core::timeline::PropValue::Float(1.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.p4y"),
                    photonic_core::timeline::PropValue::Float(1.0),
                ),
            ],
        };
        let out = apply("color.curves", &img, &params).expect("bridged");
        for (a, b) in img.pixels.iter().zip(out.pixels.iter()) {
            for c in 0..3 {
                assert!(
                    (a[c] - b[c]).abs() < 0.04,
                    "identity curves channel {c}: {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn unpremultiply_restores_straight_rgb() {
        let mut img = Image::new(2, 1);
        img.set(0, 0, [0.25, 0.0, 0.0, 0.5]); // premul red at 50%
        img.set(1, 0, [0.0, 0.0, 0.0, 0.0]);
        let out = apply("util.unpremultiply", &img, &ResolvedParams::default()).expect("bridged");
        assert!((out.pixel(0, 0)[0] - 0.5).abs() < 1e-4);
        assert!((out.pixel(0, 0)[3] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn alpha_view_shows_alpha_as_luma() {
        let mut img = Image::new(1, 1);
        img.set(0, 0, [0.1, 0.2, 0.3, 0.75]);
        let out = apply("util.alpha_view", &img, &ResolvedParams::default()).expect("bridged");
        assert!((out.pixel(0, 0)[0] - 0.75).abs() < 1e-4);
        assert!((out.pixel(0, 0)[3] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn outline_grows_opaque_silhouette() {
        let mut img = Image::new(8, 8);
        // Center pixel opaque white.
        img.set(4, 4, [1.0, 1.0, 1.0, 1.0]);
        let params = ResolvedParams {
            entries: vec![
                (
                    photonic_core::timeline::PropPath::new("params.thickness"),
                    photonic_core::timeline::PropValue::Float(2.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.r"),
                    photonic_core::timeline::PropValue::Float(1.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.g"),
                    photonic_core::timeline::PropValue::Float(0.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.b"),
                    photonic_core::timeline::PropValue::Float(0.0),
                ),
            ],
        };
        let out = apply("util.outline", &img, &params).expect("bridged");
        // Neighbour of the center should pick up red outline.
        assert!(
            out.pixel(5, 4)[0] > 0.1 || out.pixel(4, 5)[0] > 0.1,
            "outline should paint neighbours"
        );
    }

    #[test]
    fn pinch_identity_at_zero() {
        let img = solid(
            8,
            8,
            LinearColor {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 1.0,
            },
        );
        let params = ResolvedParams {
            entries: vec![(
                photonic_core::timeline::PropPath::new("params.amount"),
                photonic_core::timeline::PropValue::Float(0.0),
            )],
        };
        let out = apply("geo.pinch", &img, &params).expect("bridged");
        for (a, b) in img.pixels.iter().zip(out.pixels.iter()) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 0.02, "pinch 0 identity");
            }
        }
        let _ = out;
    }

    #[test]
    fn transfer_space_for_levels() {
        assert_eq!(
            operand_space("color.levels"),
            OperandSpace::TransferStraight
        );
        assert_eq!(operand_space("blur.box"), OperandSpace::LinearStraight);
    }

    #[test]
    fn emboss_changes_a_solid_edge_image() {
        let mut img = Image::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                let v = if x < 4 { 0.0 } else { 1.0 };
                img.set(x, y, [v, v, v, 1.0]);
            }
        }
        let out = apply_simple("stylize.emboss", &img, 0.0, 0.0).expect("bridged");
        assert_ne!(out.pixels, img.pixels, "emboss must alter an edge image");
    }

    #[test]
    fn box_blur_softens_a_checker() {
        let mut img = Image::new(4, 1);
        img.set(0, 0, [1.0, 1.0, 1.0, 1.0]);
        img.set(1, 0, [0.0, 0.0, 0.0, 1.0]);
        img.set(2, 0, [1.0, 1.0, 1.0, 1.0]);
        img.set(3, 0, [0.0, 0.0, 0.0, 1.0]);
        let out = apply_simple("blur.box", &img, 1.0, 0.0).expect("bridged");
        assert!(out.pixel(0, 0)[0] < 1.0);
    }

    #[test]
    fn levels_identity_at_defaults() {
        let img = solid(
            4,
            4,
            LinearColor {
                r: 0.4,
                g: 0.5,
                b: 0.6,
                a: 1.0,
            },
        );
        let params = ResolvedParams {
            entries: vec![
                (
                    photonic_core::timeline::PropPath::new("params.in_black"),
                    photonic_core::timeline::PropValue::Float(0.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.in_white"),
                    photonic_core::timeline::PropValue::Float(1.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.gamma"),
                    photonic_core::timeline::PropValue::Float(1.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.out_black"),
                    photonic_core::timeline::PropValue::Float(0.0),
                ),
                (
                    photonic_core::timeline::PropPath::new("params.out_white"),
                    photonic_core::timeline::PropValue::Float(1.0),
                ),
            ],
        };
        let out = apply("color.levels", &img, &params).expect("bridged");
        for (a, b) in img.pixels.iter().zip(out.pixels.iter()) {
            for c in 0..3 {
                assert!(
                    (a[c] - b[c]).abs() < 0.03,
                    "levels identity channel {c}: {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn posterize_reduces_unique_levels() {
        let mut img = Image::new(8, 1);
        for x in 0..8 {
            let v = x as f32 / 7.0;
            img.set(x, 0, [v, v, v, 1.0]);
        }
        let params = ResolvedParams {
            entries: vec![(
                photonic_core::timeline::PropPath::new("params.levels"),
                photonic_core::timeline::PropValue::Float(2.0),
            )],
        };
        let out = apply("color.posterize", &img, &params).expect("bridged");
        // 2 levels → only near-black and near-white.
        let mut lows = 0;
        let mut highs = 0;
        for p in &out.pixels {
            if p[0] < 0.25 {
                lows += 1;
            } else if p[0] > 0.75 {
                highs += 1;
            }
        }
        assert!(lows > 0 && highs > 0, "posterize 2 must produce both poles");
    }

    #[test]
    fn linear_roundtrip_near_identity() {
        let img = solid(
            4,
            4,
            LinearColor {
                r: 0.5,
                g: 0.25,
                b: 0.75,
                a: 1.0,
            },
        );
        let r = image_to_raster(&img, OperandSpace::LinearStraight);
        let back = raster_to_image(&r, OperandSpace::LinearStraight);
        for (a, b) in img.pixels.iter().zip(back.pixels.iter()) {
            for c in 0..4 {
                assert!(
                    (a[c] - b[c]).abs() < 0.01,
                    "linear channel {c}: {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn solid_roundtrip_is_near_identity_transfer() {
        let img = solid(
            4,
            4,
            LinearColor {
                r: 0.5,
                g: 0.25,
                b: 0.75,
                a: 1.0,
            },
        );
        let r = image_to_raster(&img, OperandSpace::TransferStraight);
        let back = raster_to_image(&r, OperandSpace::TransferStraight);
        for (a, b) in img.pixels.iter().zip(back.pixels.iter()) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 0.02, "channel {c}: {a:?} vs {b:?}");
            }
        }
    }

    // ── util.outline on the SDF path (30 §5, proposal 208 §6 T3) ────────────

    /// An opaque white square inset by `inset` px inside a `n`x`n` transparent
    /// frame — enough margin that a thickness-4 outline never clips the border.
    fn square_matte(n: u32, inset: u32) -> Image {
        let mut img = Image::new(n, n);
        for y in inset..n - inset {
            for x in inset..n - inset {
                img.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        img
    }

    fn outline(img: &Image, thickness: f32) -> Image {
        util_outline(img, thickness, [1.0, 0.0, 0.0], 1.0)
    }

    /// The defect this rewrite exists to fix: the old `1 - d/thickness` ramp
    /// gave the outline no solid core, so it read as a glow. The band is now
    /// fully opaque from the shape edge out to `thickness - aa`, where the
    /// antialiased rim (centred on `thickness`) begins.
    #[test]
    fn outline_band_is_solid_out_to_thickness() {
        let img = square_matte(40, 12);
        let out = outline(&img, 4.0);
        // Row through the square's middle; the left edge sits at x == 12.
        let y = 20;
        for dx in 1..=3u32 {
            let a = out.pixel(12 - dx, y)[3];
            assert!(
                a > 0.95,
                "{dx}px outside the edge must be solid outline, got alpha {a}"
            );
        }
    }

    /// Beyond `thickness` (plus the antialiasing half-width) the band is gone,
    /// so the stroke has the width the user asked for.
    #[test]
    fn outline_band_ends_at_thickness() {
        let img = square_matte(40, 12);
        let out = outline(&img, 4.0);
        let y = 20;
        // 6px out is past thickness 4 + aa 1 → transparent.
        assert!(
            out.pixel(12 - 6, y)[3] < 0.05,
            "band should not extend past thickness + aa"
        );
        // The rim in between is partially covered, not a hard step.
        let rim = out.pixel(12 - 5, y)[3];
        assert!(
            (0.0..=0.95).contains(&rim),
            "outer rim should be antialiased, got {rim}"
        );
    }

    /// 30 §5's "smooth at hard angles": the field is Euclidean, so corners come
    /// out round. Both samples below sit at the same *Chebyshev* offset (4, the
    /// box-search metric) from the shape — a boxy dilation would cover them
    /// identically. Euclidean distance separates them: the axial pixel is 4px
    /// away and still inside the stroke's antialiased rim, while the diagonal
    /// one is 5.66px from the corner and fully outside it.
    #[test]
    fn outline_corner_is_round_not_boxy() {
        let img = square_matte(40, 12);
        let out = outline(&img, 4.0);
        // Corner of the square is (12, 12).
        let diagonal = out.pixel(12 - 4, 12 - 4)[3];
        let axial = out.pixel(12 - 4, 20)[3];
        assert!(
            axial > 0.4,
            "4px out along an axis is still within the stroke rim, got {axial}"
        );
        assert!(
            diagonal < 0.05,
            "4px out diagonally is 5.66px from the corner — past a 4px stroke; \
             a boxy dilation would cover it like the axial sample. got {diagonal}"
        );
        assert!(
            axial > diagonal + 0.3,
            "the corner must fall off faster than the flat edge (round, not \
             boxy): axial {axial} vs diagonal {diagonal}"
        );
    }

    /// The source is composited over its own outline, so opaque interior
    /// pixels are returned untouched.
    #[test]
    fn outline_leaves_the_source_interior_untouched() {
        let img = square_matte(40, 12);
        let out = outline(&img, 4.0);
        assert_eq!(out.pixel(20, 20), [1.0, 1.0, 1.0, 1.0]);
    }

    /// Degenerate params are a bit-exact identity rather than a full JFA pass.
    #[test]
    fn outline_zero_thickness_or_opacity_is_identity() {
        let img = square_matte(24, 8);
        assert_eq!(
            util_outline(&img, 0.0, [1.0, 0.0, 0.0], 1.0).pixels,
            img.pixels
        );
        assert_eq!(
            util_outline(&img, 4.0, [1.0, 0.0, 0.0], 0.0).pixels,
            img.pixels
        );
        assert_eq!(
            util_outline(&img, f32::NAN, [1.0, 0.0, 0.0], 1.0).pixels,
            img.pixels
        );
    }

    /// Thickness is not capped by a search window any more (30 §5 "no thickness
    /// ceiling"), so a large stroke still reaches its full width.
    #[test]
    fn outline_honours_large_thickness() {
        let img = square_matte(64, 24);
        let out = outline(&img, 16.0);
        let y = 32;
        assert!(
            out.pixel(24 - 15, y)[3] > 0.95,
            "15px out must still be solid"
        );
        assert!(out.pixel(24 - 18, y)[3] < 0.05, "18px out must be clear");
    }
}
