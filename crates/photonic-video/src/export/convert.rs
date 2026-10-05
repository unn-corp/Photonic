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
    signal_to_yuv_codes(signal, a, target)
}

/// Premultiplied BT.709 video-signal RGB to normalized Y′CbCr codes. This is
/// the packing stage after a managed SDR video output transform; applying the
/// legacy working-pixel converter here would run the BT.709 OETF twice.
pub fn video_signal_pixel_to_yuv_codes(rgba_premult: [f32; 4], target: Colorimetry) -> [f32; 4] {
    let [r, g, b, a] = rgba_premult;
    signal_to_yuv_codes(unpremultiply([r, g, b], a), a, target)
}

fn signal_to_yuv_codes(signal: [f32; 3], a: f32, target: Colorimetry) -> [f32; 4] {
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

#[inline]
fn quantize_12(code: f32) -> [u8; 2] {
    (code.clamp(0.0, 4095.0).round() as u16).to_le_bytes()
}

#[inline]
fn quantize_10(code: f32) -> [u8; 2] {
    (code.clamp(0.0, 1023.0).round() as u16).to_le_bytes()
}

/// Floating working frame → 10-bit 4:2:2 planar YUV for opaque ProRes HQ.
/// Chroma is averaged over each horizontal pixel pair before quantization;
/// odd final columns keep their own sample. No 8-bit intermediate is used.
pub fn working_frame_to_yuv422p10(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    target: Colorimetry,
) -> EncodePlanes {
    frame_to_yuv422p10(rgba_premult, width, height, target, true)
}

/// 10-bit 4:2:2 plane packing for already BT.709-encoded video pixels.
pub fn video_signal_frame_to_yuv422p10(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    target: Colorimetry,
) -> EncodePlanes {
    frame_to_yuv422p10(rgba_premult, width, height, target, false)
}

fn frame_to_yuv422p10(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    target: Colorimetry,
    apply_oetf: bool,
) -> EncodePlanes {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(rgba_premult.len(), w * h * 4, "rgba buffer size mismatch");
    let mut y = Vec::with_capacity(w * h * 2);
    let mut cb = Vec::with_capacity(w.div_ceil(2) * h * 2);
    let mut cr = Vec::with_capacity(w.div_ceil(2) * h * 2);
    for row in 0..h {
        let mut row_cb = Vec::with_capacity(w);
        let mut row_cr = Vec::with_capacity(w);
        for col in 0..w {
            let offset = (row * w + col) * 4;
            let rgba = &rgba_premult[offset..offset + 4];
            let linear = unpremultiply([rgba[0], rgba[1], rgba[2]], rgba[3]);
            let signal = if apply_oetf {
                linear.map(bt709_oetf)
            } else {
                linear
            };
            let (luma, blue, red) = rgb_to_ycbcr(signal, target.matrix);
            let (yc, bc, rc) = match target.range {
                Range::Limited => (
                    64.0 + 876.0 * luma,
                    64.0 + 896.0 * (blue + 0.5),
                    64.0 + 896.0 * (red + 0.5),
                ),
                Range::Full => (1023.0 * luma, 1023.0 * (blue + 0.5), 1023.0 * (red + 0.5)),
            };
            y.extend_from_slice(&quantize_10(yc));
            row_cb.push(bc);
            row_cr.push(rc);
        }
        for pair in 0..w.div_ceil(2) {
            let first = pair * 2;
            let last = (first + 1).min(w - 1);
            cb.extend_from_slice(&quantize_10((row_cb[first] + row_cb[last]) * 0.5));
            cr.extend_from_slice(&quantize_10((row_cr[first] + row_cr[last]) * 0.5));
        }
    }
    EncodePlanes::Yuv422P10 {
        width,
        height,
        y,
        cb,
        cr,
    }
}

/// Quantize straight from the floating working image to 12-bit planar 4:4:4
/// YUVA. Limited-range video uses 256–3760 luma and 256–3840 chroma codes, with
/// full-range alpha. No intermediate 8-bit quantization occurs.
pub fn working_frame_to_yuva444p12(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    target: Colorimetry,
) -> EncodePlanes {
    frame_to_yuva444p12(rgba_premult, width, height, target, true)
}

/// 12-bit 4:4:4:4 plane packing for already BT.709-encoded video pixels.
pub fn video_signal_frame_to_yuva444p12(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    target: Colorimetry,
) -> EncodePlanes {
    frame_to_yuva444p12(rgba_premult, width, height, target, false)
}

fn frame_to_yuva444p12(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    target: Colorimetry,
    apply_oetf: bool,
) -> EncodePlanes {
    let pixels = width as usize * height as usize;
    assert_eq!(rgba_premult.len(), pixels * 4, "rgba buffer size mismatch");
    let mut y = Vec::with_capacity(pixels * 2);
    let mut cb = Vec::with_capacity(pixels * 2);
    let mut cr = Vec::with_capacity(pixels * 2);
    let mut a = Vec::with_capacity(pixels * 2);
    for rgba in rgba_premult.chunks_exact(4) {
        let alpha = rgba[3].clamp(0.0, 1.0);
        let linear = unpremultiply([rgba[0], rgba[1], rgba[2]], alpha);
        let signal = if apply_oetf {
            linear.map(bt709_oetf)
        } else {
            linear
        };
        let (luma, blue, red) = rgb_to_ycbcr(signal, target.matrix);
        let (yc, bc, rc) = match target.range {
            Range::Limited => (
                256.0 + 3504.0 * luma,
                256.0 + 3584.0 * (blue + 0.5),
                256.0 + 3584.0 * (red + 0.5),
            ),
            Range::Full => (4095.0 * luma, 4095.0 * (blue + 0.5), 4095.0 * (red + 0.5)),
        };
        y.extend_from_slice(&quantize_12(yc));
        cb.extend_from_slice(&quantize_12(bc));
        cr.extend_from_slice(&quantize_12(rc));
        a.extend_from_slice(&quantize_12(4095.0 * alpha));
    }
    EncodePlanes::Yuva444P12 {
        width,
        height,
        y,
        cb,
        cr,
        a,
    }
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
    /// Planar 10-bit 4:2:2, little-endian u16 samples for opaque ProRes HQ.
    Yuv422P10 {
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
    /// Planar 12-bit 4:4:4 + alpha, little-endian u16 samples for ProRes 4444.
    /// Each sample stores a right-aligned code in 0..4095.
    Yuva444P12 {
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
            Self::Yuv420 { y, cb, cr, .. } | Self::Yuv422P10 { y, cb, cr, .. } => {
                writer.write_all(y)?;
                writer.write_all(cb)?;
                writer.write_all(cr)
            }
            Self::Yuva420 { y, cb, cr, a, .. }
            | Self::Yuva444 { y, cb, cr, a, .. }
            | Self::Yuva444P12 { y, cb, cr, a, .. } => {
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
            | EncodePlanes::Yuv422P10 { width, height, .. }
            | EncodePlanes::Yuva420 { width, height, .. }
            | EncodePlanes::Yuva444 { width, height, .. }
            | EncodePlanes::Yuva444P12 { width, height, .. }
            | EncodePlanes::Rgba8 { width, height, .. } => (width, height),
        }
    }

    /// The raw byte layout ffmpeg's rawvideo demuxer expects for this plane
    /// set, in ffmpeg `-pix_fmt` wire order (packed for `Rgba8`, planar
    /// Y/Cb/Cr(/A) otherwise).
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            EncodePlanes::Yuv420 { y, cb, cr, .. } | EncodePlanes::Yuv422P10 { y, cb, cr, .. } => {
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
            EncodePlanes::Yuva444 { y, cb, cr, a, .. }
            | EncodePlanes::Yuva444P12 { y, cb, cr, a, .. } => {
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
            EncodePlanes::Yuv422P10 { .. } => "yuv422p10le",
            EncodePlanes::Yuva420 { .. } => "yuva420p",
            EncodePlanes::Yuva444 { .. } => "yuva444p",
            EncodePlanes::Yuva444P12 { .. } => "yuva444p12le",
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
    frame_to_yuv_planes(
        rgba_premult,
        width,
        height,
        target,
        alpha,
        alpha_444,
        working_pixel_to_yuv_codes,
    )
}

/// 8-bit YUV plane packing for premultiplied BT.709 video-signal pixels.
/// This is an isolated managed export component, not active export dispatch.
pub fn video_signal_frame_to_yuv_planes(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    target: Colorimetry,
    alpha: bool,
    alpha_444: bool,
) -> EncodePlanes {
    frame_to_yuv_planes(
        rgba_premult,
        width,
        height,
        target,
        alpha,
        alpha_444,
        video_signal_pixel_to_yuv_codes,
    )
}

fn frame_to_yuv_planes(
    rgba_premult: &[f32],
    width: u32,
    height: u32,
    target: Colorimetry,
    alpha: bool,
    alpha_444: bool,
    convert_pixel: fn([f32; 4], Colorimetry) -> [f32; 4],
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
        let [yc, cbc, crc, ac] = convert_pixel(px, target);
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
    frame_to_rgba8(rgba_premult, width, height, true)
}

/// Pack premultiplied sRGB display code values as straight RGBA8 without a
/// second sRGB OETF. PNG export must request an sRGB display graph output.
pub fn srgb_display_frame_to_rgba8(rgba_premult: &[f32], width: u32, height: u32) -> EncodePlanes {
    frame_to_rgba8(rgba_premult, width, height, false)
}

fn frame_to_rgba8(rgba_premult: &[f32], width: u32, height: u32, apply_oetf: bool) -> EncodePlanes {
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
        let out = if apply_oetf {
            working_pixel_to_srgb_rgba(px)
        } else {
            let alpha = px[3];
            let straight = unpremultiply([px[0], px[1], px[2]], alpha);
            [straight[0], straight[1], straight[2], alpha]
        };
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
    fn managed_video_signal_packing_skips_second_transfer() {
        let target = Colorimetry {
            matrix: Matrix::Bt709,
            range: Range::Limited,
        };
        let alpha = 0.5;
        let linear = [0.18_f32, 0.06, 0.8];
        let encoded = linear.map(bt709_oetf);
        let legacy = working_pixel_to_yuv_codes(
            [
                linear[0] * alpha,
                linear[1] * alpha,
                linear[2] * alpha,
                alpha,
            ],
            target,
        );
        let managed = video_signal_pixel_to_yuv_codes(
            [
                encoded[0] * alpha,
                encoded[1] * alpha,
                encoded[2] * alpha,
                alpha,
            ],
            target,
        );
        for (a, b) in managed.iter().zip(legacy) {
            assert!((a - b).abs() < 1e-6);
        }
        let doubled = working_pixel_to_yuv_codes(
            [
                encoded[0] * alpha,
                encoded[1] * alpha,
                encoded[2] * alpha,
                alpha,
            ],
            target,
        );
        assert!((managed[0] - doubled[0]).abs() > 0.1);
        assert_eq!(video_signal_pixel_to_yuv_codes([0.0; 4], target)[3], 0.0);
    }

    #[test]
    fn managed_video_8bit_plane_packing_matches_legacy_signal_once() {
        let target = Colorimetry::BT709_LIMITED;
        let linear: Vec<f32> = [
            [0.0, 0.0, 0.0, 1.0],
            [0.09, 0.015, 0.4, 0.5],
            [0.8, 0.2, 0.01, 1.0],
            [0.025, 0.175, 0.05, 0.25],
            [0.0, 0.0, 0.0, 0.0],
            [0.2, 0.2, 0.2, 1.0],
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut encoded = Vec::with_capacity(linear.len());
        for pixel in linear.chunks_exact(4) {
            let alpha = pixel[3];
            encoded.extend(
                pixel[..3]
                    .iter()
                    .map(|value| bt709_oetf(*value / alpha.max(1e-4)) * alpha),
            );
            encoded.push(alpha);
        }
        for (alpha, alpha_444) in [(false, false), (true, false), (true, true)] {
            let legacy = working_frame_to_yuv_planes(&linear, 3, 2, target, alpha, alpha_444);
            let managed =
                video_signal_frame_to_yuv_planes(&encoded, 3, 2, target, alpha, alpha_444);
            assert_eq!(managed.to_bytes(), legacy.to_bytes());
            assert_eq!(managed.ffmpeg_pix_fmt(), legacy.ffmpeg_pix_fmt());
        }
        let legacy_10 = working_frame_to_yuv422p10(&linear, 3, 2, target);
        let managed_10 = video_signal_frame_to_yuv422p10(&encoded, 3, 2, target);
        assert_eq!(managed_10.to_bytes(), legacy_10.to_bytes());
        assert_eq!(managed_10.ffmpeg_pix_fmt(), "yuv422p10le");
        let legacy_12 = working_frame_to_yuva444p12(&linear, 3, 2, target);
        let managed_12 = video_signal_frame_to_yuva444p12(&encoded, 3, 2, target);
        assert_eq!(managed_12.to_bytes(), legacy_12.to_bytes());
        assert_eq!(managed_12.ffmpeg_pix_fmt(), "yuva444p12le");
        let mut srgb = Vec::with_capacity(linear.len());
        for pixel in linear.chunks_exact(4) {
            let alpha = pixel[3];
            srgb.extend(
                pixel[..3]
                    .iter()
                    .map(|value| srgb_oetf(*value / alpha.max(1e-4)) * alpha),
            );
            srgb.push(alpha);
        }
        assert_eq!(
            srgb_display_frame_to_rgba8(&srgb, 3, 2).to_bytes(),
            working_frame_to_rgba8(&linear, 3, 2).to_bytes()
        );
    }

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
            EncodePlanes::Yuv422P10 {
                width: 1,
                height: 1,
                y: dummy(2),
                cb: dummy(2),
                cr: dummy(2)
            }
            .ffmpeg_pix_fmt(),
            "yuv422p10le"
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
            EncodePlanes::Yuva444P12 {
                width: 1,
                height: 1,
                y: dummy(2),
                cb: dummy(2),
                cr: dummy(2),
                a: dummy(2)
            }
            .ffmpeg_pix_fmt(),
            "yuva444p12le"
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
            working_frame_to_yuv422p10(&rgba, 5, 3, Colorimetry::BT709_LIMITED),
            working_frame_to_yuv_planes(&rgba, 5, 3, Colorimetry::BT709_LIMITED, false, false),
            working_frame_to_yuv_planes(&rgba, 5, 3, Colorimetry::BT709_LIMITED, true, false),
            working_frame_to_yuv_planes(&rgba, 5, 3, Colorimetry::BT709_LIMITED, true, true),
            working_frame_to_yuva444p12(&rgba, 5, 3, Colorimetry::BT709_LIMITED),
        ];
        for planes in cases {
            let mut bytes = Vec::new();
            planes.write_to(&mut bytes).unwrap();
            assert_eq!(bytes, planes.to_bytes());
        }
    }

    #[test]
    fn prores_422_uses_10_bit_codes_and_odd_width_chroma() {
        let width = 513u32;
        let mut rgba = Vec::with_capacity(width as usize * 4);
        for index in 0..width {
            let value = index as f32 / (width - 1) as f32;
            rgba.extend_from_slice(&[value, value, value, 1.0]);
        }
        let EncodePlanes::Yuv422P10 { y, cb, cr, .. } =
            working_frame_to_yuv422p10(&rgba, width, 1, Colorimetry::BT709_LIMITED)
        else {
            panic!("10-bit 4:2:2 planes")
        };
        assert_eq!(y.len(), width as usize * 2);
        assert_eq!(cb.len(), width.div_ceil(2) as usize * 2);
        assert_eq!(cr.len(), cb.len());
        let codes: Vec<_> = y
            .chunks_exact(2)
            .map(|sample| u16::from_le_bytes([sample[0], sample[1]]))
            .collect();
        assert_eq!(codes[0], 64);
        assert_eq!(*codes.last().unwrap(), 940);
        assert!(
            codes
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 256
        );
        assert!(cb
            .chunks_exact(2)
            .all(|sample| u16::from_le_bytes([sample[0], sample[1]]) == 512));
    }

    #[test]
    fn twelve_bit_yuva_uses_video_codes_without_eight_bit_quantization() {
        let mut rgba = Vec::new();
        for index in 0..512 {
            let value = index as f32 / 511.0;
            rgba.extend_from_slice(&[value, value, value, 1.0]);
        }
        let EncodePlanes::Yuva444P12 { y, cb, cr, a, .. } =
            working_frame_to_yuva444p12(&rgba, 512, 1, Colorimetry::BT709_LIMITED)
        else {
            panic!("expected twelve-bit planes")
        };
        let codes: Vec<u16> = y
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        assert_eq!(codes[0], 256);
        assert_eq!(codes[511], 3760);
        assert!(codes.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            codes
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 256
        );
        for plane in [&cb, &cr] {
            assert!(plane
                .chunks_exact(2)
                .all(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]) == 2048));
        }
        assert!(a
            .chunks_exact(2)
            .all(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]) == 4095));
        let EncodePlanes::Yuva444P12 { a, .. } =
            working_frame_to_yuva444p12(&[0.25, 0.25, 0.25, 0.5], 1, 1, Colorimetry::BT709_LIMITED)
        else {
            unreachable!()
        };
        assert_eq!(u16::from_le_bytes([a[0], a[1]]), 2048);
    }
}
