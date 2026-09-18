//! Working format (linear, premultiplied `Rgba16Float`) → encoder pix_fmt
//! (03-render-color-pipeline.md §3.5, steps 2-6). The exact inverse of
//! [`photonic_render::color::yuv_to_working`] (§3.2/§3.3) — reuses that
//! module's constants rather than introducing independent literals, so §4.4
//! rule 1 ("identical constants, same literal source") holds transitively:
//! [`rgb_to_ycbcr`] derives the BT.709/601 luma weights from the *same*
//! forward-matrix constants `yuv_to_working` already uses.
//!
//! Input is an `f32` RGBA readback buffer — the CPU-side representation of a
//! GPU `Rgba16Float` texture after readback (03 §4.4 rule 3 keeps all CPU
//! reference math in `f32`, not `f16`). The evaluator (built concurrently)
//! supplies this buffer; this module has no GPU/wgpu dependency of its own.
//!
//! Two delivery families, per codec (03 §3.5 covers the YUV family; the RGB
//! family is this module's necessary extension — see the [`EncodePlanes`]
//! doc comment):
//! - **YUV** (H.264/AV1/VP9/ProRes/GIF): unpremultiply → BT.709 OETF →
//!   RGB→YUV matrix → range compression → pack planes.
//! - **RGB** (PNG/APNG): unpremultiply → sRGB OETF → straight RGBA8. PNG
//!   sequence output round-trips as a plain image asset on re-import (05 §3.5
//!   "round-trips as an image-sequence import"), which decodes via the sRGB
//!   EOTF (§4.2 boundary table row 2), not BT.709 — so encoding it with the
//!   sRGB OETF is what makes that round-trip exact, matching §6.1's explicit
//!   carve-out ("sRGB tagging equivalence... for web-consumed still/PNG-sequence
//!   outputs").

use photonic_render::color::{
    self, bt709_oetf, srgb_oetf, Colorimetry, Matrix, Range, CHROMA_CENTRE, CODE_MAX,
    LIMITED_CHROMA_MIN, LIMITED_CHROMA_SCALE, LIMITED_LUMA_MIN, LIMITED_LUMA_SCALE,
};

/// Guards unpremultiply-by-zero the same way the present pass does (03 §5
/// step 2: `rgb = rgb_premult / max(a, epsilon)`).
pub const UNPREMULTIPLY_EPSILON: f32 = 1e-4;

/// Unpremultiply a linear premultiplied RGB triple by its alpha (§3.5 step 2).
/// Fully-transparent pixels (`a <= epsilon`) unpremultiply to black — their
/// color is unobservable regardless, and this matches the forward direction's
/// convention (`yuv_to_working` multiplies by `a_raw`, so `a == 0` also
/// forces `rgb == 0` there).
#[inline]
pub fn unpremultiply(rgb_premult: [f32; 3], a: f32) -> [f32; 3] {
    if a <= UNPREMULTIPLY_EPSILON {
        return [0.0, 0.0, 0.0];
    }
    let inv_a = 1.0 / a;
    [
        rgb_premult[0] * inv_a,
        rgb_premult[1] * inv_a,
        rgb_premult[2] * inv_a,
    ]
}

/// RGB (scene-linear) → Y'CbCr (video-signal domain, §3.5 step 4): the exact
/// analytic inverse of the matrix half of
/// [`photonic_render::color::yuv_to_working`]. Returns `(y', cb, cr)` with
/// chroma centred on 0 (not yet range-compressed — see [`range_compress`]).
///
/// Derivation: `yuv_to_working`'s forward matrix is `R = Y' + CR_R·cr`,
/// `B = Y' + CB_B·cb`, which is the standard YCbCr form with
/// `CR_R == 2(1 - Kr)` and `CB_B == 2(1 - Kb)` for luma weights `Kr`/`Kb`
/// (`Kg = 1 - Kr - Kb`). Solving for `Kr`/`Kb` from the *existing* `CR_R`/
/// `CB_B` constants (rather than hardcoding new luma-weight literals) keeps
/// this the same "one source of truth" the forward path already is.
pub fn rgb_to_ycbcr(rgb: [f32; 3], matrix: Matrix) -> (f32, f32, f32) {
    let (cr_r, cb_b) = match matrix {
        Matrix::Bt709 => (color::BT709_CR_R, color::BT709_CB_B),
        Matrix::Bt601 => (color::BT601_CR_R, color::BT601_CB_B),
    };
    let kb = 1.0 - cb_b / 2.0;
    let kr = 1.0 - cr_r / 2.0;
    let kg = 1.0 - kr - kb;
    let [r, g, b] = rgb;
    let yp = kr * r + kg * g + kb * b;
    let cb = (b - yp) / cb_b;
    let cr = (r - yp) / cr_r;
    (yp, cb, cr)
}

/// Range compression (§3.5 step 5): the exact inverse of
/// [`photonic_render::color::range_expand`]. `cb`/`cr` are centred-on-0
/// inputs (as returned by [`rgb_to_ycbcr`]); outputs are raw 0..1 code
/// values ready to quantize to 8-bit.
///
/// v1 always compresses to [`Range::Limited`] for every built-in preset (05
/// §3.5's catalog never requests full-range output). 03 §3.5 step 5 names an
/// "`ExportPreset` flag" for full-range output that 05 §3.1's schema does not
/// actually define — a cross-doc gap, not resolved here; this function still
/// supports `Range::Full` so the flag can be threaded through later without
/// changing this module.
pub fn range_compress(yp: f32, cb: f32, cr: f32, range: Range) -> (f32, f32, f32) {
    match range {
        Range::Full => (yp, cb + CHROMA_CENTRE, cr + CHROMA_CENTRE),
        Range::Limited => {
            let y = (yp * LIMITED_LUMA_SCALE + LIMITED_LUMA_MIN) / CODE_MAX;
            let cbc = ((cb + CHROMA_CENTRE) * LIMITED_CHROMA_SCALE + LIMITED_CHROMA_MIN) / CODE_MAX;
            let crc = ((cr + CHROMA_CENTRE) * LIMITED_CHROMA_SCALE + LIMITED_CHROMA_MIN) / CODE_MAX;
            (y, cbc, crc)
        }
    }
}

/// One pixel, working → YUV signal (§3.5 steps 2, 3, 4, 5 combined):
/// unpremultiply → BT.709 OETF → RGB→YUV matrix → range compression. Alpha
/// passes through straight, unclamped-transfer (§3.2 step 4's convention,
/// mirrored on encode) but *is* range-clamped to a valid 0..1 code.
#[inline]
pub fn working_pixel_to_yuv_codes(rgba_premult: [f32; 4], target: Colorimetry) -> [f32; 4] {
    let [r, g, b, a] = rgba_premult;
    let lin = unpremultiply([r, g, b], a);
    let signal = [bt709_oetf(lin[0]), bt709_oetf(lin[1]), bt709_oetf(lin[2])];
    let (yp, cb, cr) = rgb_to_ycbcr(signal, target.matrix);
    let (y, cb, cr) = range_compress(yp, cb, cr, target.range);
    [
        y.clamp(0.0, 1.0),
        cb.clamp(0.0, 1.0),
        cr.clamp(0.0, 1.0),
        a.clamp(0.0, 1.0),
    ]
}

/// One pixel, working → straight sRGB RGBA (PNG/APNG path): unpremultiply →
/// sRGB OETF. No YUV matrix, no range compression — PNG carries full-range
/// straight-alpha RGB8 directly.
#[inline]
pub fn working_pixel_to_srgb_rgba(rgba_premult: [f32; 4]) -> [f32; 4] {
    let [r, g, b, a] = rgba_premult;
    let lin = unpremultiply([r, g, b], a);
    [
        srgb_oetf(lin[0]).clamp(0.0, 1.0),
        srgb_oetf(lin[1]).clamp(0.0, 1.0),
        srgb_oetf(lin[2]).clamp(0.0, 1.0),
        a.clamp(0.0, 1.0),
    ]
}

#[inline]
fn quantize(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * CODE_MAX).round() as u8
}

/// Encoder-ready plane data for one frame. Mirrors
/// [`crate::decode::DecodedPlanes`]'s `Yuv420`/`Yuva444` shapes (decode's
/// `PixFmt` enum only distinguishes those two), plus two variants decode has
/// no reason to have: `Yuva420` (VP9/WebM alpha — libvpx-vp9 rejects
/// `yuva444p` as "not widely supported"; `yuva420p` is the broadly-compatible
/// real-world convention, confirmed against the shipped ffmpeg build) and
/// `Rgba8` (PNG/APNG — not a YUV format at all). Kept local to `export`
/// rather than extending `crate::decode::PixFmt` (out of this story's scope).
#[derive(Clone, Debug, PartialEq)]
pub enum EncodePlanes {
    /// 4:2:0, no alpha (H.264, AV1, GIF-intermediate).
    Yuv420 {
        width: u32,
        height: u32,
        y: Vec<u8>,
        cb: Vec<u8>,
        cr: Vec<u8>,
    },
    /// 4:2:0 chroma + full-res alpha (VP9/WebM alpha, CAP-021).
    Yuva420 {
        width: u32,
        height: u32,
        y: Vec<u8>,
        cb: Vec<u8>,
        cr: Vec<u8>,
        a: Vec<u8>,
    },
    /// 4:4:4 + alpha, no chroma downsample (ProRes 4444).
    Yuva444 {
        width: u32,
        height: u32,
        y: Vec<u8>,
        cb: Vec<u8>,
        cr: Vec<u8>,
        a: Vec<u8>,
    },
    /// Straight-alpha sRGB RGBA8, interleaved (PNG/APNG).
    Rgba8 {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

impl EncodePlanes {
    /// Stream planes in FFmpeg wire order without allocating a combined copy.
    pub fn write_to(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        match self {
            Self::Yuv420 { y, cb, cr, .. } => {
                writer.write_all(y)?;
                writer.write_all(cb)?;
                writer.write_all(cr)
            }
            Self::Yuva420 { y, cb, cr, a, .. } | Self::Yuva444 { y, cb, cr, a, .. } => {
                writer.write_all(y)?;
                writer.write_all(cb)?;
                writer.write_all(cr)?;
                writer.write_all(a)
            }
            Self::Rgba8 { rgba, .. } => writer.write_all(rgba),
        }
    }

    pub fn dims(&self) -> (u32, u32) {
        match *self {
            EncodePlanes::Yuv420 { width, height, .. }
            | EncodePlanes::Yuva420 { width, height, .. }
            | EncodePlanes::Yuva444 { width, height, .. }
            | EncodePlanes::Rgba8 { width, height, .. } => (width, height),
        }
    }

    /// The raw byte layout ffmpeg's rawvideo demuxer expects for this plane
    /// set, in ffmpeg `-pix_fmt` wire order (packed for `Rgba8`, planar
    /// Y/Cb/Cr(/A) otherwise).
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            EncodePlanes::Yuv420 { y, cb, cr, .. } => {
                let mut out = Vec::with_capacity(y.len() + cb.len() + cr.len());
                out.extend_from_slice(y);
                out.extend_from_slice(cb);
                out.extend_from_slice(cr);
                out
            }
            EncodePlanes::Yuva420 { y, cb, cr, a, .. } => {
                let mut out = Vec::with_capacity(y.len() + cb.len() + cr.len() + a.len());
                out.extend_from_slice(y);
                out.extend_from_slice(cb);
                out.extend_from_slice(cr);
                out.extend_from_slice(a);
                out
            }
            EncodePlanes::Yuva444 { y, cb, cr, a, .. } => {
                let mut out = Vec::with_capacity(y.len() + cb.len() + cr.len() + a.len());
                out.extend_from_slice(y);
                out.extend_from_slice(cb);
                out.extend_from_slice(cr);
                out.extend_from_slice(a);
                out
            }
            EncodePlanes::Rgba8 { rgba, .. } => rgba.clone(),
        }
    }

    /// The matching ffmpeg `-pix_fmt` token for [`Self::to_bytes`]'s layout.
    pub fn ffmpeg_pix_fmt(&self) -> &'static str {
        match self {
            EncodePlanes::Yuv420 { .. } => "yuv420p",
            EncodePlanes::Yuva420 { .. } => "yuva420p",
            EncodePlanes::Yuva444 { .. } => "yuva444p",
            EncodePlanes::Rgba8 { .. } => "rgba",
        }
    }
}

/// Chroma plane dimensions for 4:2:0 subsampling, rounding up on odd sizes
/// (matches `crate::decode::PixFmt::frame_bytes`'s `(w+1)/2` convention).
fn chroma_dims(width: u32, height: u32) -> (usize, usize) {
    ((width as usize).div_ceil(2), (height as usize).div_ceil(2))
}

/// Box-downsample a full-res chroma plane (0..1 float, row-major) to 4:2:0.
/// Each output sample averages the up-to-4 input samples in its 2×2 site;
/// odd trailing rows/columns average over fewer samples (never out of
/// bounds) — a real box filter, not a nearest-neighbor decimation.
fn box_downsample_chroma(full: &[f32], width: u32, height: u32) -> Vec<f32> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = chroma_dims(width, height);
    let mut out = vec![0.0f32; cw * ch];
    for cy in 0..ch {
        let y0 = cy * 2;
        let y1 = (y0 + 1).min(h - 1);
        for cx in 0..cw {
            let x0 = cx * 2;
            let x1 = (x0 + 1).min(w - 1);
            let mut sum = 0.0f32;
            let mut n = 0u32;
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    sum += full[yy * w + xx];
                    n += 1;
                }
            }
            out[cy * cw + cx] = sum / n as f32;
        }
    }
    out
}

/// Full working-frame → YUV(+A) planes (§3.5 steps 2-6), the per-pixel
/// pipeline plus (for non-4:4:4 outputs) the 4:2:0 box downsample.
///
/// `rgba_premult` is row-major, linear premultiplied RGBA, `width*height*4`
/// `f32`s. `alpha_444` selects `Yuva444` (ProRes) over `Yuva420` (VP9) when
/// `alpha` is set — callers pick per target codec (encoder.rs owns that
/// choice; this function just executes whichever shape is asked for).
pub fn working_frame_to_yuv_planes(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    target: Colorimetry,
    alpha: bool,
    alpha_444: bool,
) -> EncodePlanes {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(
        rgba_premult.len(),
        w * h * 4,
        "rgba buffer size mismatch: expected {}x{}x4, got {}",
        w,
        h,
        rgba_premult.len()
    );

    let mut y_plane = vec![0u8; w * h];
    let mut cb_full = vec![0.0f32; w * h];
    let mut cr_full = vec![0.0f32; w * h];
    let mut a_plane = vec![0u8; w * h];

    for i in 0..w * h {
        let px = [
            rgba_premult[i * 4],
            rgba_premult[i * 4 + 1],
            rgba_premult[i * 4 + 2],
            rgba_premult[i * 4 + 3],
        ];
        let [yc, cbc, crc, ac] = working_pixel_to_yuv_codes(px, target);
        y_plane[i] = quantize(yc);
        cb_full[i] = cbc;
        cr_full[i] = crc;
        a_plane[i] = quantize(ac);
    }

    if alpha && alpha_444 {
        let cb: Vec<u8> = cb_full.iter().map(|&v| quantize(v)).collect();
        let cr: Vec<u8> = cr_full.iter().map(|&v| quantize(v)).collect();
        return EncodePlanes::Yuva444 {
            width,
            height,
            y: y_plane,
            cb,
            cr,
            a: a_plane,
        };
    }

    let cb: Vec<u8> = box_downsample_chroma(&cb_full, width, height)
        .into_iter()
        .map(quantize)
        .collect();
    let cr: Vec<u8> = box_downsample_chroma(&cr_full, width, height)
        .into_iter()
        .map(quantize)
        .collect();

    if alpha {
        EncodePlanes::Yuva420 {
            width,
            height,
            y: y_plane,
            cb,
            cr,
            a: a_plane,
        }
    } else {
        EncodePlanes::Yuv420 {
            width,
            height,
            y: y_plane,
            cb,
            cr,
        }
    }
}

/// Full working-frame → straight sRGB RGBA8 (PNG/APNG path, no YUV).
pub fn working_frame_to_rgba8(rgba_premult: &[f32], width: u32, height: u32) -> EncodePlanes {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(rgba_premult.len(), w * h * 4, "rgba buffer size mismatch");
    let mut rgba = vec![0u8; w * h * 4];
    for i in 0..w * h {
        let px = [
            rgba_premult[i * 4],
            rgba_premult[i * 4 + 1],
            rgba_premult[i * 4 + 2],
            rgba_premult[i * 4 + 3],
        ];
        let out = working_pixel_to_srgb_rgba(px);
        rgba[i * 4] = quantize(out[0]);
        rgba[i * 4 + 1] = quantize(out[1]);
        rgba[i * 4 + 2] = quantize(out[2]);
        rgba[i * 4 + 3] = quantize(out[3]);
    }
    EncodePlanes::Rgba8 {
        width,
        height,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_render::color::{range_expand, yuv_to_working};

    #[test]
    fn unpremultiply_zero_alpha_is_black() {
        assert_eq!(unpremultiply([0.7, 0.3, 0.9], 0.0), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn unpremultiply_full_alpha_is_identity() {
        let got = unpremultiply([0.7, 0.3, 0.9], 1.0);
        for (a, b) in got.iter().zip([0.7, 0.3, 0.9]) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    /// §4.4 rule 1's transitive check: `rgb_to_ycbcr`'s matrix is the exact
    /// analytic inverse of `yuv_to_working`'s forward matrix, verified by
    /// round-tripping through the *forward* formula reconstructed from the
    /// same public `photonic_render::color` constants (not re-derived here).
    #[test]
    fn rgb_to_ycbcr_inverts_the_forward_matrix() {
        let samples = [
            [0.0_f32, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.8, 0.2, 0.1],
            [0.1, 0.6, 0.9],
            [0.5, 0.5, 0.5],
        ];
        for matrix in [Matrix::Bt709, Matrix::Bt601] {
            let (cr_r, cb_g, cr_g, cb_b) = match matrix {
                Matrix::Bt709 => (
                    color::BT709_CR_R,
                    color::BT709_CB_G,
                    color::BT709_CR_G,
                    color::BT709_CB_B,
                ),
                Matrix::Bt601 => (
                    color::BT601_CR_R,
                    color::BT601_CB_G,
                    color::BT601_CR_G,
                    color::BT601_CB_B,
                ),
            };
            for rgb in samples {
                let (yp, cb, cr) = rgb_to_ycbcr(rgb, matrix);
                // Reconstruct via the exact forward formula `yuv_to_working` uses.
                let r2 = yp + cr_r * cr;
                let g2 = yp - cb_g * cb - cr_g * cr;
                let b2 = yp + cb_b * cb;
                assert!((r2 - rgb[0]).abs() < 1e-5, "{matrix:?} R round-trip");
                assert!((g2 - rgb[1]).abs() < 1e-5, "{matrix:?} G round-trip");
                assert!((b2 - rgb[2]).abs() < 1e-5, "{matrix:?} B round-trip");
            }
        }
    }

    /// `range_compress` is the exact inverse of `photonic_render::color::range_expand`.
    #[test]
    fn range_compress_inverts_range_expand() {
        for range in [Range::Full, Range::Limited] {
            for (yp, cb, cr) in [(0.0_f32, -0.5, -0.5), (1.0, 0.5, 0.5), (0.42, 0.1, -0.2)] {
                let (y_code, cb_code, cr_code) = range_compress(yp, cb, cr, range);
                let (yp2, cb2, cr2) = range_expand(y_code, cb_code, cr_code, range);
                assert!((yp2 - yp).abs() < 1e-5, "{range:?} y' round-trip");
                assert!((cb2 - cb).abs() < 1e-5, "{range:?} cb round-trip");
                assert!((cr2 - cr).abs() < 1e-5, "{range:?} cr round-trip");
            }
        }
    }

    /// The full pipeline round-trip named in 03 §3.5's determinism note:
    /// working → encode → (decode's own `yuv_to_working`) reproduces the
    /// source within the §4.4-rule-3 tolerance (1e-3, linear-light).
    #[test]
    fn working_pixel_round_trips_through_decodes_yuv_to_working() {
        let cm = Colorimetry::BT709_LIMITED;
        let samples = [
            [0.0_f32, 0.0, 0.0, 0.0],
            [0.5, 0.5, 0.5, 1.0],
            [0.8, 0.2, 0.1, 1.0],
            [0.1, 0.6, 0.9, 0.5],
            [1.0, 1.0, 1.0, 1.0],
        ];
        for rgba in samples {
            let codes = working_pixel_to_yuv_codes(rgba, cm);
            let back = yuv_to_working(codes[0], codes[1], codes[2], codes[3], cm);
            for i in 0..4 {
                assert!(
                    (back[i] - rgba[i]).abs() < 1e-3,
                    "channel {i}: {rgba:?} -> codes {codes:?} -> back {back:?}"
                );
            }
        }
    }

    #[test]
    fn quantize_reference_points() {
        assert_eq!(quantize(0.0), 0);
        assert_eq!(quantize(1.0), 255);
        assert_eq!(quantize(-1.0), 0, "clamps below 0");
        assert_eq!(quantize(2.0), 255, "clamps above 1");
    }

    #[test]
    fn working_frame_to_yuv_planes_yuv420_dims_match_pix_fmt_frame_bytes() {
        use crate::decode::PixFmt;
        let (w, h) = (5u32, 3u32); // odd dims exercise the chroma rounding.
        let rgba = vec![0.5f32; (w * h * 4) as usize];
        let planes =
            working_frame_to_yuv_planes(&rgba, w, h, Colorimetry::BT709_LIMITED, false, false);
        match &planes {
            EncodePlanes::Yuv420 { y, cb, cr, .. } => {
                assert_eq!(
                    y.len() + cb.len() + cr.len(),
                    PixFmt::Yuv420p.frame_bytes(w, h)
                );
                assert_eq!(cb.len(), 3 * 2, "5x3 -> 3x2 chroma via div_ceil(2)");
                // (5+1)/2=3,(3+1)/2=2
            }
            other => panic!("expected Yuv420, got {other:?}"),
        }
    }

    #[test]
    fn working_frame_to_yuv_planes_alpha_444_yields_yuva444_full_res_chroma() {
        let (w, h) = (4u32, 2u32);
        let rgba = vec![0.5f32; (w * h * 4) as usize];
        let planes =
            working_frame_to_yuv_planes(&rgba, w, h, Colorimetry::BT709_LIMITED, true, true);
        match &planes {
            EncodePlanes::Yuva444 { y, cb, cr, a, .. } => {
                assert_eq!(y.len(), (w * h) as usize);
                assert_eq!(cb.len(), (w * h) as usize, "4:4:4 chroma is full-res");
                assert_eq!(cr.len(), (w * h) as usize);
                assert_eq!(a.len(), (w * h) as usize);
            }
            other => panic!("expected Yuva444, got {other:?}"),
        }
    }

    #[test]
    fn working_frame_to_yuv_planes_alpha_420_yields_yuva420_subsampled_chroma_full_res_alpha() {
        let (w, h) = (4u32, 2u32);
        let rgba = vec![0.5f32; (w * h * 4) as usize];
        let planes =
            working_frame_to_yuv_planes(&rgba, w, h, Colorimetry::BT709_LIMITED, true, false);
        match &planes {
            EncodePlanes::Yuva420 { y, cb, cr, a, .. } => {
                assert_eq!(y.len(), (w * h) as usize);
                assert_eq!(cb.len(), 2, "2x2 chroma downsample for 4x2");
                assert_eq!(cr.len(), 2);
                assert_eq!(
                    a.len(),
                    (w * h) as usize,
                    "alpha stays full-res in yuva420p"
                );
            }
            other => panic!("expected Yuva420, got {other:?}"),
        }
    }

    #[test]
    fn box_downsample_averages_a_uniform_2x2_block() {
        // 4x2 plane, left half = 0.0, right half = 1.0 -> two 2x1 chroma cells
        // (height 2 -> chroma height 1), each cell's average is exact.
        let w = 4u32;
        let h = 2u32;
        let mut full = vec![0.0f32; (w * h) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                full[y * w as usize + x] = if x < 2 { 0.0 } else { 1.0 };
            }
        }
        let down = box_downsample_chroma(&full, w, h);
        assert_eq!(down.len(), 2); // cw=2, ch=1
        assert!((down[0] - 0.0).abs() < 1e-6);
        assert!((down[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn box_downsample_handles_odd_trailing_edge() {
        // 3x1: chroma width = 2. Last chroma cell covers only column 2 (1 sample).
        let full = vec![0.2f32, 0.4, 0.9];
        let down = box_downsample_chroma(&full, 3, 1);
        assert_eq!(down.len(), 2);
        assert!((down[0] - 0.3).abs() < 1e-6, "avg of 0.2,0.4");
        assert!((down[1] - 0.9).abs() < 1e-6, "single trailing sample");
    }

    #[test]
    fn rgba8_path_is_straight_alpha_srgb_not_yuv() {
        let rgba = vec![1.0f32, 0.0, 0.0, 0.5]; // premultiplied red at half alpha
        let planes = working_frame_to_rgba8(&rgba, 1, 1);
        match planes {
            EncodePlanes::Rgba8 { rgba, .. } => {
                // Unpremultiply: 1.0/0.5 = 2.0 -> clamped to 1.0 before sRGB encode.
                assert_eq!(rgba[0], 255, "unpremultiplied+clamped red channel");
                assert_eq!(rgba[1], 0);
                assert_eq!(rgba[2], 0);
                assert_eq!(rgba[3], 128, "alpha passes through straight (0.5 -> ~128)");
            }
            other => panic!("expected Rgba8, got {other:?}"),
        }
    }

    #[test]
    fn ffmpeg_pix_fmt_tokens_match_plane_shapes() {
        let dummy = |n: usize| vec![0u8; n];
        assert_eq!(
            EncodePlanes::Yuv420 {
                width: 1,
                height: 1,
                y: dummy(1),
                cb: dummy(1),
                cr: dummy(1)
            }
            .ffmpeg_pix_fmt(),
            "yuv420p"
        );
        assert_eq!(
            EncodePlanes::Yuva420 {
                width: 1,
                height: 1,
                y: dummy(1),
                cb: dummy(1),
                cr: dummy(1),
                a: dummy(1)
            }
            .ffmpeg_pix_fmt(),
            "yuva420p"
        );
        assert_eq!(
            EncodePlanes::Yuva444 {
                width: 1,
                height: 1,
                y: dummy(1),
                cb: dummy(1),
                cr: dummy(1),
                a: dummy(1)
            }
            .ffmpeg_pix_fmt(),
            "yuva444p"
        );
        assert_eq!(
            EncodePlanes::Rgba8 {
                width: 1,
                height: 1,
                rgba: dummy(4)
            }
            .ffmpeg_pix_fmt(),
            "rgba"
        );
    }

    #[test]
    fn streamed_planes_match_packed_wire_bytes() {
        let rgba = [0.1, 0.2, 0.3, 0.5].repeat(15);
        let cases = [
            working_frame_to_rgba8(&rgba, 5, 3),
            working_frame_to_yuv_planes(&rgba, 5, 3, Colorimetry::BT709_LIMITED, false, false),
            working_frame_to_yuv_planes(&rgba, 5, 3, Colorimetry::BT709_LIMITED, true, false),
            working_frame_to_yuv_planes(&rgba, 5, 3, Colorimetry::BT709_LIMITED, true, true),
        ];
        for planes in cases {
            let mut bytes = Vec::new();
            planes.write_to(&mut bytes).unwrap();
            assert_eq!(bytes, planes.to_bytes());
        }
    }
}
