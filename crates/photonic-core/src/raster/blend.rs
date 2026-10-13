//! Blend-mode math and layer compositing.
//!
//! Implements the 16 [`BlendMode`](crate::layer::BlendMode) variants with the
//! exact formulas from the W3C Compositing & Blending spec (which Photoshop and
//! CSS `mix-blend-mode` both follow), so on-screen, exported, and Photoshop
//! results agree. Separable modes run per channel; non-separable modes (Hue,
//! Saturation, Color, Luminosity) operate on the RGB triple.

use super::image::RasterImage;
use super::mask::Mask;
use crate::layer::BlendMode;

/// Blend a single channel pair (backdrop `cb`, source `cs`), all in 0..1.
#[inline]
pub fn blend_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::ColorDodge => {
            if cb == 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        BlendMode::HardLight => hard_light(cb, cs),
        BlendMode::SoftLight => soft_light(cb, cs),
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        // ── Photoshop extras (separable) ──────────────────────────────────────
        BlendMode::LinearDodge => (cb + cs).min(1.0),
        BlendMode::LinearBurn => (cb + cs - 1.0).max(0.0),
        BlendMode::Subtract => (cb - cs).max(0.0),
        BlendMode::Divide => {
            if cs <= 0.0 {
                1.0
            } else {
                (cb / cs).min(1.0)
            }
        }
        // Vivid Light: burn for shadows, dodge for highlights.
        BlendMode::VividLight => {
            if cs <= 0.5 {
                blend_channel(BlendMode::ColorBurn, cb, (2.0 * cs).min(1.0))
            } else {
                blend_channel(BlendMode::ColorDodge, cb, (2.0 * (cs - 0.5)).min(1.0))
            }
        }
        // Linear Light: linear burn/dodge about mid-grey.
        BlendMode::LinearLight => (cb + 2.0 * cs - 1.0).clamp(0.0, 1.0),
        // Pin Light: darken for shadows, lighten for highlights.
        BlendMode::PinLight => {
            if cs <= 0.5 {
                cb.min(2.0 * cs)
            } else {
                cb.max(2.0 * cs - 1.0)
            }
        }
        // Hard Mix: threshold Vivid Light to 0/1.
        BlendMode::HardMix => {
            if blend_channel(BlendMode::VividLight, cb, cs) < 0.5 {
                0.0
            } else {
                1.0
            }
        }
        // Non-separable modes are handled in `blend_rgb`; per-channel falls back
        // to normal so callers that only do separable work stay correct.
        BlendMode::Hue
        | BlendMode::Saturation
        | BlendMode::Color
        | BlendMode::Luminosity
        | BlendMode::DarkerColor
        | BlendMode::LighterColor => cs,
    }
}

#[inline]
fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb * (2.0 * cs)
    } else {
        screen(cb, 2.0 * cs - 1.0)
    }
}

#[inline]
fn screen(cb: f32, cs: f32) -> f32 {
    cb + cs - cb * cs
}

#[inline]
fn soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

#[inline]
fn is_separable(mode: BlendMode) -> bool {
    !matches!(
        mode,
        BlendMode::Hue
            | BlendMode::Saturation
            | BlendMode::Color
            | BlendMode::Luminosity
            | BlendMode::DarkerColor
            | BlendMode::LighterColor
    )
}

/// Blend two RGB triples (0..1) under `mode`, returning the blended RGB
/// (before alpha compositing).
pub fn blend_rgb(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    if is_separable(mode) {
        [
            blend_channel(mode, cb[0], cs[0]),
            blend_channel(mode, cb[1], cs[1]),
            blend_channel(mode, cb[2], cs[2]),
        ]
    } else {
        match mode {
            BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
            BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
            BlendMode::Color => set_lum(cs, lum(cb)),
            BlendMode::Luminosity => set_lum(cb, lum(cs)),
            // Darker/Lighter Color pick the whole backdrop or source by luminosity.
            BlendMode::DarkerColor => {
                if lum(cb) <= lum(cs) {
                    cb
                } else {
                    cs
                }
            }
            BlendMode::LighterColor => {
                if lum(cb) >= lum(cs) {
                    cb
                } else {
                    cs
                }
            }
            _ => cs,
        }
    }
}

// ── Non-separable helpers (W3C spec) ────────────────────────────────────────────

#[inline]
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        for ch in c.iter_mut() {
            *ch = l + (*ch - l) * l / (l - n).max(1e-6);
        }
    }
    if x > 1.0 {
        for ch in c.iter_mut() {
            *ch = l + (*ch - l) * (1.0 - l) / (x - l).max(1e-6);
        }
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

#[inline]
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// Set the saturation of finite unit-range `c` to unit-range `s` per W3C SetSat.
/// This pure RGB-chroma step is shared by Legacy and Native AP1 compositing.
pub fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    // indices of min, mid, max
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap());
    let (lo, mid, hi) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[hi] > c[lo] {
        out[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        out[hi] = s;
    }
    out[lo] = 0.0;
    out
}

/// Composite `top` onto `base` ("source-over") at integer offset `(ox, oy)`,
/// with global `opacity` (0..1), `mode`, and an optional coverage `mask` (in the
/// same coordinate space as `top`). Straight-alpha throughout.
#[allow(clippy::too_many_arguments)]
pub fn composite(
    base: &mut RasterImage,
    top: &RasterImage,
    ox: i64,
    oy: i64,
    opacity: f32,
    mode: BlendMode,
    mask: Option<&Mask>,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }
    for ty in 0..top.height {
        let by = oy + ty as i64;
        if by < 0 || by >= base.height as i64 {
            continue;
        }
        for tx in 0..top.width {
            let bx = ox + tx as i64;
            if bx < 0 || bx >= base.width as i64 {
                continue;
            }
            let s = top.pixel(tx, ty);
            let mut sa = (s[3] as f32 / 255.0) * opacity;
            if let Some(m) = mask {
                sa *= m.coverage(tx, ty);
            }
            if sa <= 0.0 {
                continue;
            }
            let b = base.pixel(bx as u32, by as u32);
            let ba = b[3] as f32 / 255.0;
            let cb = [
                b[0] as f32 / 255.0,
                b[1] as f32 / 255.0,
                b[2] as f32 / 255.0,
            ];
            let cs = [
                s[0] as f32 / 255.0,
                s[1] as f32 / 255.0,
                s[2] as f32 / 255.0,
            ];

            // Blended source color, then mixed with raw source by backdrop alpha
            // (spec: Cs = (1 - ab)·Cs + ab·B(Cb,Cs)).
            let blended = blend_rgb(mode, cb, cs);
            let mixed = [
                (1.0 - ba) * cs[0] + ba * blended[0],
                (1.0 - ba) * cs[1] + ba * blended[1],
                (1.0 - ba) * cs[2] + ba * blended[2],
            ];

            // source-over alpha composite (straight alpha)
            let oa = sa + ba * (1.0 - sa);
            let mut out = [0u8; 4];
            if oa > 0.0 {
                for c in 0..3 {
                    let co = (mixed[c] * sa + cb[c] * ba * (1.0 - sa)) / oa;
                    out[c] = (co * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
            out[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
            base.set_pixel(bx as u32, by as u32, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_over_opaque() {
        let mut base = RasterImage::filled(2, 2, [0, 0, 0, 255]);
        let top = RasterImage::filled(2, 2, [255, 255, 255, 255]);
        composite(&mut base, &top, 0, 0, 1.0, BlendMode::Normal, None);
        assert_eq!(base.pixel(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn half_opacity_blends() {
        let mut base = RasterImage::filled(1, 1, [0, 0, 0, 255]);
        let top = RasterImage::filled(1, 1, [255, 255, 255, 255]);
        composite(&mut base, &top, 0, 0, 0.5, BlendMode::Normal, None);
        let p = base.pixel(0, 0);
        assert!((p[0] as i32 - 128).abs() <= 1);
    }

    #[test]
    fn multiply_darkens() {
        let mut base = RasterImage::filled(1, 1, [128, 128, 128, 255]);
        let top = RasterImage::filled(1, 1, [128, 128, 128, 255]);
        composite(&mut base, &top, 0, 0, 1.0, BlendMode::Multiply, None);
        let p = base.pixel(0, 0);
        assert!((p[0] as i32 - 64).abs() <= 2);
    }

    #[test]
    fn screen_is_symmetric_lighten() {
        assert!((blend_channel(BlendMode::Screen, 0.5, 0.5) - 0.75).abs() < 1e-5);
    }

    #[test]
    fn mask_limits_compositing() {
        let mut base = RasterImage::filled(2, 1, [0, 0, 0, 255]);
        let top = RasterImage::filled(2, 1, [255, 255, 255, 255]);
        let mut mask = Mask::empty(2, 1);
        mask.set(0, 0, 255); // only left pixel
        composite(&mut base, &top, 0, 0, 1.0, BlendMode::Normal, Some(&mask));
        assert_eq!(base.pixel(0, 0), [255, 255, 255, 255]);
        assert_eq!(base.pixel(1, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn color_takes_source_hue() {
        // Color mode = hue+sat of source, luminosity of backdrop. Gray backdrop,
        // red source → reddish result (r > g, r > b).
        let out = blend_rgb(BlendMode::Color, [0.5, 0.5, 0.5], [1.0, 0.0, 0.0]);
        assert!(out[0] > out[1] && out[0] > out[2]);
    }

    #[test]
    fn photoshop_extra_modes_match_formulas() {
        use BlendMode::*;
        let ch = blend_channel;
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-5;
        assert!(approx(ch(LinearDodge, 0.3, 0.3), 0.6));
        assert!(approx(ch(LinearDodge, 0.8, 0.8), 1.0)); // clamps to 1
        assert!(approx(ch(LinearBurn, 0.8, 0.8), 0.6));
        assert!(approx(ch(LinearBurn, 0.2, 0.2), 0.0)); // clamps to 0
        assert!(approx(ch(Subtract, 0.8, 0.3), 0.5));
        assert!(approx(ch(Subtract, 0.3, 0.8), 0.0));
        assert!(approx(ch(Divide, 0.5, 1.0), 0.5));
        assert!(approx(ch(Divide, 0.5, 0.0), 1.0)); // divide-by-zero guard
        assert!(approx(ch(PinLight, 0.5, 0.2), 0.4)); // darken side
        assert!(approx(ch(PinLight, 0.5, 0.8), 0.6)); // lighten side
        assert!(approx(ch(LinearLight, 0.5, 0.5), 0.5));
        assert_eq!(ch(HardMix, 0.5, 0.8), 1.0); // thresholds to 0/1

        // Non-separable whole-colour picks by luminosity.
        let darker = blend_rgb(DarkerColor, [0.8, 0.8, 0.8], [0.2, 0.2, 0.2]);
        assert!(darker[0] < 0.5, "DarkerColor keeps the darker whole colour");
        let lighter = blend_rgb(LighterColor, [0.8, 0.8, 0.8], [0.2, 0.2, 0.2]);
        assert!(
            lighter[0] > 0.5,
            "LighterColor keeps the lighter whole colour"
        );
    }
}
