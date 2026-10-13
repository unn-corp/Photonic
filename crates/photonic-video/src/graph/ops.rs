//! CPU pixel kernels for the reference evaluator (`eval_cpu`), one per
//! `GraphOp` family (02 §1 module map's `graph/ops/`).
//!
//! Everything here operates on [`Image`] — an f32, premultiplied, linear-light
//! Rec.709 RGBA buffer, the CPU twin of the GPU `Rgba16Float` working texture
//! (D-09). Determinism is the whole point: same inputs ⇒ byte-identical output
//! (02 §2, 03 §4.4 rule 2 — the operation order matches the shader path). Blend
//! math reuses `photonic_core::raster::blend` so `Merge` agrees with the CPU
//! compositor where they overlap (03 §4.4 rule 4).
//!
//! ## Operand space (03 §4.5, normative)
//!
//! This module is the named reference implementation of §4.5. Every operator —
//! and every future 30-effect-catalogue entry — arrives at its input in one
//! defined encoding/alpha state; the four rules, stated once here, exist so no
//! op re-decides them (closes 27 A-1 / A-3):
//!
//! 1. **Linear light** (§4.5.1). Blending happens in linear-light Rec.709 on
//!    every compositor. [`Image`] storage is linear; there is no transfer curve
//!    on the operands. Deliberate divergence from Photoshop/CSS.
//! 2. **Straight alpha for blend** (§4.5.2). Blend functions take straight
//!    (non-premultiplied) colour: unpremultiply → blend → re-premultiply using
//!    the W3C form `Cs' = (1-αb)·Cs + αb·B(Cb,Cs)`,
//!    `co = αs·Cs' + (1-αs)·bottom_premul`. [`merge_pixel`] is the reference.
//! 3. **Unpremult → op → repremult for grade / per-channel non-linear ops**
//!    (§4.5.3). Any op that is non-linear in RGB (grade, invert, …) must run on
//!    straight colour: [`invert`] and `grade::apply_grade_cpu` both do this.
//! 4. **sRGB render target for the fixed-function / `COMPOSITE_SHADER` path**
//!    (§4.5.4). The GPU vector document renders to an sRGB target so the
//!    hardware blend unit lands in linear.
//!
//! Two helpers — [`ALPHA_EPS`], [`unpremultiply`], [`repremultiply`] — are the
//! single citable primitives every op is expected to reuse for rules 2 and 3.

use glam::{Mat3, Vec2};
use photonic_core::layer::BlendMode;
use photonic_core::raster::blend::blend_rgb;

use crate::graph::ir::{
    DeinterlaceMethod, FieldOrder, FitMode, LinearColor, Sampling, StabilizeWarp, WipeDirection,
};

/// An f32 premultiplied linear-Rec.709 RGBA image — the CPU reference working
/// buffer. Row-major, `pixels.len() == width * height`.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// `[r, g, b, a]` premultiplied linear, row-major.
    pub pixels: Vec<[f32; 4]>,
}

impl Image {
    /// A transparent-black image (all zero, premultiplied).
    pub fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        Image {
            width,
            height,
            pixels: vec![[0.0; 4]; (width * height) as usize],
        }
    }

    /// A uniform image of `color` (already premultiplied linear).
    pub fn filled(width: u32, height: u32, color: LinearColor) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        Image {
            width,
            height,
            pixels: vec![[color.r, color.g, color.b, color.a]; (width * height) as usize],
        }
    }

    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        let x = x.min(self.width - 1);
        let y = y.min(self.height - 1);
        self.pixels[(y * self.width + x) as usize]
    }

    #[inline]
    pub(crate) fn set(&mut self, x: u32, y: u32, v: [f32; 4]) {
        self.pixels[(y * self.width + x) as usize] = v;
    }

    /// Bilinear sample at pixel coordinates `(px, py)` (clamped at the edges).
    /// Operates directly on premultiplied values — correct because premultiplied
    /// linear is closed under linear interpolation (no fringing).
    pub fn sample_bilinear(&self, px: f32, py: f32) -> [f32; 4] {
        self.sample_bilinear_with_border(px, py, false)
    }

    fn sample_bilinear_with_border(&self, px: f32, py: f32, transparent: bool) -> [f32; 4] {
        let w = self.width as i32;
        let h = self.height as i32;
        let x = px - 0.5;
        let y = py - 0.5;
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;
        let at = |ix: i32, iy: i32| -> [f32; 4] {
            if transparent && (ix < 0 || ix >= w || iy < 0 || iy >= h) {
                return [0.0; 4];
            }
            let cx = ix.clamp(0, w - 1) as u32;
            let cy = iy.clamp(0, h - 1) as u32;
            self.pixel(cx, cy)
        };
        let p00 = at(x0, y0);
        let p10 = at(x0 + 1, y0);
        let p01 = at(x0, y0 + 1);
        let p11 = at(x0 + 1, y0 + 1);
        let mut out = [0.0f32; 4];
        for c in 0..4 {
            let top = p00[c] * (1.0 - fx) + p10[c] * fx;
            let bot = p01[c] * (1.0 - fx) + p11[c] * fx;
            out[c] = top * (1.0 - fy) + bot * fy;
        }
        out
    }
}

/// `SolidColor`: a full-frame constant (premultiplied linear).
pub fn solid(width: u32, height: u32, color: LinearColor) -> Image {
    Image::filled(width, height, color)
}

/// `Transform2D`: resample `input` under the affine `mat` (dest ← src via the
/// inverse), producing a same-sized image. Identity is an exact passthrough.
pub fn transform2d(input: &Image, mat: Mat3, sampling: Sampling) -> Image {
    transform2d_to_canvas(input, mat, sampling, input.width, input.height)
}

/// Resample directly from native pixels using the authored canvas coordinates.
pub(crate) fn transform2d_to_canvas(
    input: &Image,
    mat: Mat3,
    sampling: Sampling,
    width: u32,
    height: u32,
) -> Image {
    transform2d_to_canvas_with_border(input, mat, sampling, width, height, false)
}

pub(crate) fn transform2d_to_canvas_with_border(
    input: &Image,
    mat: Mat3,
    sampling: Sampling,
    width: u32,
    height: u32,
    transparent_border: bool,
) -> Image {
    if !transform_matrix_is_valid(mat) {
        return Image::new(width, height);
    }
    if mat == Mat3::IDENTITY && input.width == width && input.height == height {
        return input.clone();
    }
    let inv = mat.inverse();
    let mut out = Image::new(width, height);
    let source_scale = Vec2::new(
        input.width as f32 / out.width as f32,
        input.height as f32 / out.height as f32,
    );
    for y in 0..out.height {
        for x in 0..out.width {
            let dst = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let src = inv.transform_point2(dst) * source_scale;
            let v = match sampling {
                Sampling::Bilinear => {
                    input.sample_bilinear_with_border(src.x, src.y, transparent_border)
                }
                Sampling::Nearest => {
                    if transparent_border
                        && (src.x < 0.0
                            || src.x >= input.width as f32
                            || src.y < 0.0
                            || src.y >= input.height as f32)
                    {
                        out.set(x, y, [0.0; 4]);
                        continue;
                    }
                    let sx = (src.x.floor() as i32).clamp(0, input.width as i32 - 1) as u32;
                    let sy = (src.y.floor() as i32).clamp(0, input.height as i32 - 1) as u32;
                    input.pixel(sx, sy)
                }
            };
            out.set(x, y, v);
        }
    }
    out
}

/// `StabilizeWarp`: the D-12 gyro-correction resample (22 §6.4 step 7).
///
/// Inverse-mapped, like every resampling op here — for each *output* pixel,
/// work out where in the source its light came from:
///
/// 1. undo the crop zoom about the frame centre;
/// 2. unproject through the lens into a ray in the virtual (stabilized) camera;
/// 3. rotate that ray into the real camera's frame;
/// 4. project it back through the *same* lens to a source pixel;
/// 5. sample.
///
/// Steps 2 and 4 use one lens model, so an identity rotation at zoom 1 is an
/// exact passthrough rather than a resample that softens the image — the
/// property [`StabilizeWarp::is_identity`] lets the compiler exploit and the
/// tests pin down.
///
/// This is the normative reference the WGSL twin must match (02 §2), so it is
/// written to be transliterated: no iterator chains, no early exits the shader
/// cannot express, and a fixed Newton iteration count in the fisheye inverse.
pub fn stabilize_warp(input: &Image, warp: &StabilizeWarp, sampling: Sampling) -> Image {
    if !warp.is_valid() {
        return Image::new(input.width, input.height);
    }
    if warp.is_identity() {
        return input.clone();
    }
    let (w, h) = (input.width as f32, input.height as f32);
    // Intrinsics are normalized; scale to this frame's actual size.
    let (fx, fy) = (warp.intrinsics[0] * w, warp.intrinsics[1] * h);
    let (cx, cy) = (warp.intrinsics[2] * w, warp.intrinsics[3] * h);
    let r = warp.rotation;
    let inv_zoom = 1.0 / warp.zoom;
    let mut out = Image::new(input.width, input.height);

    for y in 0..out.height {
        for x in 0..out.width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            // 1 — undo the crop zoom about the frame centre.
            let ox = w * 0.5 + (px - w * 0.5) * inv_zoom;
            let oy = h * 0.5 + (py - h * 0.5) * inv_zoom;

            // 2 — unproject to a ray in the virtual camera.
            let u = (ox - cx) / fx;
            let v = (oy - cy) / fy;
            let ray = if warp.fisheye {
                let theta_d = (u * u + v * v).sqrt();
                if theta_d < 1e-12 {
                    [0.0, 0.0, 1.0]
                } else {
                    let theta = undistort_theta_f32(theta_d, &warp.k);
                    let s = theta.sin();
                    [s * u / theta_d, s * v / theta_d, theta.cos()]
                }
            } else {
                let n = (u * u + v * v + 1.0).sqrt();
                [u / n, v / n, 1.0 / n]
            };

            // 3 — rotate into the real camera's frame (row-major multiply).
            let sx3 = r[0] * ray[0] + r[1] * ray[1] + r[2] * ray[2];
            let sy3 = r[3] * ray[0] + r[4] * ray[1] + r[5] * ray[2];
            let sz3 = r[6] * ray[0] + r[7] * ray[1] + r[8] * ray[2];

            // 4 — project back through the same lens.
            let (sx, sy, valid) = if warp.fisheye {
                let rr = (sx3 * sx3 + sy3 * sy3).sqrt();
                if rr < 1e-12 {
                    (cx, cy, true)
                } else {
                    let theta = rr.atan2(sz3);
                    let theta_d = distort_theta_f32(theta, &warp.k);
                    let scale = theta_d / rr;
                    (fx * sx3 * scale + cx, fy * sy3 * scale + cy, true)
                }
            } else if sz3 <= 1e-12 {
                // Behind the camera: no forward projection exists.
                (0.0, 0.0, false)
            } else {
                (fx * (sx3 / sz3) + cx, fy * (sy3 / sz3) + cy, true)
            };

            // 5 — sample, or leave the hole showing if asked to.
            let outside = !valid || sx < 0.0 || sy < 0.0 || sx > w - 1.0 || sy > h - 1.0;
            let texel = if !valid || (outside && warp.transparent_edges) {
                [0.0, 0.0, 0.0, 0.0]
            } else {
                match sampling {
                    Sampling::Bilinear => input.sample_bilinear(sx, sy),
                    Sampling::Nearest => {
                        let ix = (sx.floor() as i32).clamp(0, input.width as i32 - 1) as u32;
                        let iy = (sy.floor() as i32).clamp(0, input.height as i32 - 1) as u32;
                        input.pixel(ix, iy)
                    }
                }
            };
            out.set(x, y, texel);
        }
    }
    out
}

/// `θ_d = θ(1 + k1θ² + k2θ⁴ + k3θ⁶ + k4θ⁸)` in `f32`, matching the WGSL twin.
///
/// The `f64` version in [`crate::graph::stabilize::lens`] drives analysis,
/// where accumulated precision matters. This one drives per-pixel rendering,
/// where matching the shader bit-for-bit matters more.
fn distort_theta_f32(theta: f32, k: &[f32; 4]) -> f32 {
    let t2 = theta * theta;
    let t4 = t2 * t2;
    let t6 = t4 * t2;
    let t8 = t4 * t4;
    theta * (1.0 + k[0] * t2 + k[1] * t4 + k[2] * t6 + k[3] * t8)
}

/// Newton inverse of [`distort_theta_f32`], at a fixed iteration count so the
/// CPU and GPU do identical arithmetic.
fn undistort_theta_f32(theta_d: f32, k: &[f32; 4]) -> f32 {
    let mut theta = theta_d;
    for _ in 0..crate::graph::stabilize::lens::UNDISTORT_ITERATIONS {
        let t2 = theta * theta;
        let t4 = t2 * t2;
        let t6 = t4 * t2;
        let t8 = t4 * t4;
        let f = theta * (1.0 + k[0] * t2 + k[1] * t4 + k[2] * t6 + k[3] * t8) - theta_d;
        let df = 1.0 + 3.0 * k[0] * t2 + 5.0 * k[1] * t4 + 7.0 * k[2] * t6 + 9.0 * k[3] * t8;
        if df.abs() < 1e-12 {
            break;
        }
        theta -= f / df;
    }
    theta
}

/// Inversion policy shared by the CPU and GPU transform paths. Near-singular
/// or non-finite matrices render transparent rather than sampling undefined
/// coordinates.
pub(crate) fn transform_matrix_is_valid(mat: Mat3) -> bool {
    let determinant = mat.determinant();
    mat.to_cols_array().into_iter().all(f32::is_finite)
        && determinant.is_finite()
        && determinant.abs() > 1e-8
}

/// `Resize`: scale `input` to `(w, h)` under `fit` (bilinear). `Stretch` maps the
/// whole frame; `Fit`/`Fill` apply a uniform scale (letterbox / crop).
pub fn resize(input: &Image, w: u32, h: u32, fit: FitMode) -> Image {
    let (w, h) = (w.max(1), h.max(1));
    let mut out = Image::new(w, h);
    let (iw, ih) = (input.width as f32, input.height as f32);
    let (ow, oh) = (w as f32, h as f32);
    let (sx, sy, ox, oy, content_w, content_h, letterbox) = match fit {
        FitMode::Stretch => (iw / ow, ih / oh, 0.0, 0.0, ow, oh, false),
        FitMode::Fit => {
            let scale = (ow / iw).min(oh / ih);
            let content_w = iw * scale;
            let content_h = ih * scale;
            (
                1.0 / scale,
                1.0 / scale,
                (ow - content_w) * 0.5,
                (oh - content_h) * 0.5,
                content_w,
                content_h,
                true,
            )
        }
        FitMode::Fill => {
            let scale = (ow / iw).max(oh / ih);
            let content_w = iw * scale;
            let content_h = ih * scale;
            (
                1.0 / scale,
                1.0 / scale,
                (ow - content_w) * 0.5,
                (oh - content_h) * 0.5,
                content_w,
                content_h,
                false,
            )
        }
    };
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 + 0.5;
            let dy = y as f32 + 0.5;
            if letterbox && (dx < ox || dx >= ox + content_w || dy < oy || dy >= oy + content_h) {
                continue;
            }
            let sxp = (dx - ox) * sx;
            let syp = (dy - oy) * sy;
            out.set(x, y, input.sample_bilinear(sxp, syp));
        }
    }
    out
}

/// `Crop`: remove normalized margins while preserving the output canvas.
///
/// The cropped region remains at its original position and the removed edges
/// become transparent. This makes Crop useful as a mask in a graph while
/// leaving the canvas dimensions available to the following compositor.
pub fn crop(
    input: &Image,
    width: u32,
    height: u32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> Image {
    let (width, height) = (width.max(1), height.max(1));
    let mut out = Image::new(width, height);
    let left = left.clamp(0.0, 1.0) * width as f32;
    let top = top.clamp(0.0, 1.0) * height as f32;
    let right = width as f32 - right.clamp(0.0, 1.0) * width as f32;
    let bottom = height as f32 - bottom.clamp(0.0, 1.0) * height as f32;
    if right <= left || bottom <= top {
        return out;
    }

    for y in 0..height {
        for x in 0..width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            if px >= left && px < right && py >= top && py < bottom {
                let sx =
                    (px * input.width as f32 / width as f32).clamp(0.5, input.width as f32 - 0.5);
                let sy = (py * input.height as f32 / height as f32)
                    .clamp(0.5, input.height as f32 - 0.5);
                out.set(x, y, input.sample_bilinear(sx, sy));
            }
        }
    }
    out
}

/// `Effect{Invert}` (08 §3 / §2 `Invert` row): invert the straight (unpremult)
/// linear color, preserving alpha, then re-premultiply — the CPU twin of the GPU
/// invert pass (`eval::Passes::invert`). Straight color is clamped to `[0,1]`
/// before inversion so the op is well-defined for out-of-range working values,
/// matching the shader's `1 - clamp(c, 0, 1)`. For an opaque pixel this reduces
/// to `1 - rgb`.
pub fn invert(input: &Image) -> Image {
    let mut out = Image::new(input.width, input.height);
    for (o, p) in out.pixels.iter_mut().zip(input.pixels.iter()) {
        let a = p[3];
        // 03 §4.5.3: invert is per-channel non-linear, so operate on straight
        // colour. At α <= ALPHA_EPS carry `[0;3]` through (premultiplied RGB is
        // already 0 there); no pixel moves relative to the pre-helper code.
        let straight = unpremultiply(*p)
            .map(|s| {
                [
                    s[0].clamp(0.0, 1.0),
                    s[1].clamp(0.0, 1.0),
                    s[2].clamp(0.0, 1.0),
                ]
            })
            .unwrap_or([0.0, 0.0, 0.0]);
        *o = repremultiply([1.0 - straight[0], 1.0 - straight[1], 1.0 - straight[2]], a);
    }
    out
}

/// Rec.709 luma of a straight (unpremultiplied) linear RGB triple — the same
/// weights the GPU key/grade shaders use (`grade_gpu::luma709`), so the CPU and
/// GPU key passes agree on brightness.
#[inline]
pub fn luma709(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

// ── K-G6 deinterlace ─────────────────────────────────────────────────────────

/// Deinterlace `input` to progressive (K-G6). Operates on the current frame
/// only (spatial methods). Field order selects which field is kept for
/// [`DeinterlaceMethod::OneField`].
pub fn deinterlace(input: &Image, method: DeinterlaceMethod, field_order: FieldOrder) -> Image {
    match method {
        DeinterlaceMethod::OneField => deinterlace_one_field(input, field_order),
        DeinterlaceMethod::LinearBlend => deinterlace_linear_blend(input),
        DeinterlaceMethod::YadifSpatial => deinterlace_yadif_spatial(input, field_order),
    }
}

/// Keep the dominant field; replace the other field's lines by averaging
/// neighbours (soft vertical double).
fn deinterlace_one_field(input: &Image, field_order: FieldOrder) -> Image {
    let keep_even = matches!(field_order, FieldOrder::TopFirst);
    let mut out = input.clone();
    let h = input.height as i32;
    let w = input.width;
    for y in 0..h {
        let is_even = y % 2 == 0;
        if is_even == keep_even {
            continue;
        }
        let y_prev = (y - 1).clamp(0, h - 1) as u32;
        let y_next = (y + 1).clamp(0, h - 1) as u32;
        for x in 0..w {
            let a = input.pixel(x, y_prev);
            let b = input.pixel(x, y_next);
            out.set(
                x,
                y as u32,
                [
                    (a[0] + b[0]) * 0.5,
                    (a[1] + b[1]) * 0.5,
                    (a[2] + b[2]) * 0.5,
                    (a[3] + b[3]) * 0.5,
                ],
            );
        }
    }
    out
}

/// Average each line with its vertical neighbours (cheap comb reduction).
fn deinterlace_linear_blend(input: &Image) -> Image {
    let mut out = Image::new(input.width, input.height);
    let h = input.height as i32;
    for y in 0..h {
        let y0 = (y - 1).clamp(0, h - 1) as u32;
        let y1 = y as u32;
        let y2 = (y + 1).clamp(0, h - 1) as u32;
        for x in 0..input.width {
            let a = input.pixel(x, y0);
            let b = input.pixel(x, y1);
            let c = input.pixel(x, y2);
            out.set(
                x,
                y1,
                [
                    (a[0] + b[0] * 2.0 + c[0]) * 0.25,
                    (a[1] + b[1] * 2.0 + c[1]) * 0.25,
                    (a[2] + b[2] * 2.0 + c[2]) * 0.25,
                    (a[3] + b[3] * 2.0 + c[3]) * 0.25,
                ],
            );
        }
    }
    out
}

/// Spatial edge-adaptive interpolate for the "missing" field (YADIF spatial
/// half). Keeps the dominant field, interpolates the other with a simple
/// edge-directed vertical predictor.
fn deinterlace_yadif_spatial(input: &Image, field_order: FieldOrder) -> Image {
    let keep_even = matches!(field_order, FieldOrder::TopFirst);
    let mut out = input.clone();
    let h = input.height as i32;
    let w = input.width as i32;
    for y in 0..h {
        let is_even = y % 2 == 0;
        if is_even == keep_even {
            continue;
        }
        let yp = (y - 1).clamp(0, h - 1);
        let yn = (y + 1).clamp(0, h - 1);
        for x in 0..w {
            let xl = (x - 1).clamp(0, w - 1);
            let xr = (x + 1).clamp(0, w - 1);
            // Vertical predictor from prev/next field lines.
            let mut px = [0.0f32; 4];
            for c in 0..4 {
                let above = input.pixel(x as u32, yp as u32)[c];
                let below = input.pixel(x as u32, yn as u32)[c];
                let diag_a = (input.pixel(xl as u32, yp as u32)[c]
                    + input.pixel(xr as u32, yn as u32)[c])
                    * 0.5;
                let diag_b = (input.pixel(xr as u32, yp as u32)[c]
                    + input.pixel(xl as u32, yn as u32)[c])
                    * 0.5;
                let vert = (above + below) * 0.5;
                // Pick the median of the three predictors (spatial edge adapt).
                let mut preds = [vert, diag_a, diag_b];
                preds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                px[c] = preds[1];
            }
            out.set(x as u32, y as u32, px);
        }
    }
    out
}

// ── Shared blur primitive (K-0.2 + proposal 209 large-radius quality) ────────

/// Cap on the 1-D Gaussian half-width in *sample taps* (not texels when step>1).
/// Matches the WGSL blur historical cap (`min(ceil(sigma*3), 128)`).
const BLUR_RADIUS_CAP: i32 = 128;

/// Prefer multi-iteration over a single pass once σ would need more than this
/// many samples at step=1 (proposal 209: keep kernels well-shaped).
const BLUR_MAX_PASS_SIGMA: f32 = 12.0;

/// Hard cap on step size (proposal 209: >4 causes banding).
const BLUR_STEP_MAX: f32 = 4.0;

/// Max H+V iteration pairs for one logical blur.
const BLUR_MAX_ITERS: u32 = 16;

/// Plan for one logical Gaussian blur (proposal 209).
///
/// Multi-pass: `σ_eff² ≈ iters · σ_pass²` ⇒ `σ_pass = σ_eff / √iters`.
/// Step: when a single pass still needs a wide kernel, space taps by `step`
/// (bilinear/nearest clamp still smooths) with `step ≤ BLUR_STEP_MAX`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlurPlan {
    /// Sigma applied on each H+V pair.
    pub sigma_pass: f32,
    /// Number of full H+V pairs (≥1 when not identity).
    pub iterations: u32,
    /// Texel spacing between kernel taps (1.0 = contiguous).
    pub step: f32,
}

impl BlurPlan {
    /// Identity / early-out plan.
    pub const IDENTITY: Self = Self {
        sigma_pass: 0.0,
        iterations: 0,
        step: 1.0,
    };
}

/// Choose step + iterations so large σ stays Gaussian-like (proposal 209).
pub fn blur_plan(sigma: f32) -> BlurPlan {
    let sigma = if sigma.is_finite() {
        sigma.max(0.0)
    } else {
        0.0
    };
    if sigma < 0.5 {
        return BlurPlan::IDENTITY;
    }
    // Stack iterations until each pass's σ is ≤ MAX_PASS_SIGMA.
    let iters = if sigma <= BLUR_MAX_PASS_SIGMA {
        1u32
    } else {
        let n = (sigma / BLUR_MAX_PASS_SIGMA).powi(2).ceil() as u32;
        n.clamp(2, BLUR_MAX_ITERS)
    };
    let sigma_pass = sigma / (iters as f32).sqrt();
    // Within one pass, if radius (at step 1) exceeds TAP budget, raise step.
    let radius_at_1 = (sigma_pass * 3.0).ceil().max(1.0);
    let step = if radius_at_1 > BLUR_RADIUS_CAP as f32 {
        (radius_at_1 / BLUR_RADIUS_CAP as f32)
            .ceil()
            .clamp(1.0, BLUR_STEP_MAX)
    } else {
        1.0
    };
    BlurPlan {
        sigma_pass,
        iterations: iters,
        step,
    }
}

/// Build a normalized 1-D Gaussian kernel for `sigma` with optional `step`.
/// Sample positions are at `i * step` texels; weights use the true distance so
/// the discrete kernel still approximates N(0,σ²). Empty when `sigma < 0.5`.
pub fn gaussian_kernel_1d(sigma: f32) -> Vec<f32> {
    gaussian_kernel_1d_stepped(sigma, 1.0)
}

/// Like [`gaussian_kernel_1d`] but with texels-per-tap spacing `step` (≥1).
pub fn gaussian_kernel_1d_stepped(sigma: f32, step: f32) -> Vec<f32> {
    let sigma = if sigma.is_finite() {
        sigma.max(0.0)
    } else {
        0.0
    };
    if sigma < 0.5 {
        return Vec::new();
    }
    let step = if step.is_finite() { step.max(1.0) } else { 1.0 };
    // Cover ~3σ in texels; number of taps is coverage/step, capped.
    let radius_texels = (sigma * 3.0).ceil().max(1.0);
    let radius_taps = ((radius_texels / step).ceil() as i32).clamp(1, BLUR_RADIUS_CAP);
    let two_s2 = 2.0 * sigma * sigma;
    let mut k = Vec::with_capacity((2 * radius_taps + 1) as usize);
    let mut sum = 0.0f32;
    for i in -radius_taps..=radius_taps {
        let dist = i as f32 * step;
        let w = (-(dist * dist) / two_s2).exp();
        k.push(w);
        sum += w;
    }
    if sum > 0.0 {
        for w in &mut k {
            *w /= sum;
        }
    }
    k
}

/// One axis of a separable Gaussian. `step` spaces taps in texels.
fn separable_blur_axis(input: &Image, kernel: &[f32], horizontal: bool, step: f32) -> Image {
    let (w, h) = (input.width as i32, input.height as i32);
    let radius = (kernel.len() as i32) / 2;
    let step_i = step.max(1.0);
    let mut out = Image::new(input.width, input.height);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 4];
            for (ki, &kw) in kernel.iter().enumerate() {
                let off = ((ki as i32 - radius) as f32 * step_i).round() as i32;
                let (sx, sy) = if horizontal {
                    ((x + off).clamp(0, w - 1), y)
                } else {
                    (x, (y + off).clamp(0, h - 1))
                };
                let p = input.pixel(sx as u32, sy as u32);
                for c in 0..4 {
                    acc[c] += p[c] * kw;
                }
            }
            out.set(x as u32, y as u32, acc);
        }
    }
    out
}

/// Single H+V pair at the given plan parameters (no multi-iter).
fn blur_one_pass(input: &Image, sigma: f32, step: f32) -> Image {
    let kernel = gaussian_kernel_1d_stepped(sigma, step);
    if kernel.is_empty() {
        return input.clone();
    }
    let tmp = separable_blur_axis(input, &kernel, true, step);
    separable_blur_axis(&tmp, &kernel, false, step)
}

/// `Effect{Blur}` (08 §3 / K-0.2 / proposal 209): separable Gaussian over
/// premultiplied linear RGBA. `radius` is the sigma in pixels (registry
/// `params.radius`). Large σ uses multi-iteration + step (see [`blur_plan`]).
/// `sigma < 0.5` is a bit-exact identity (matches the GPU early-out).
/// WGSL twin: multi-pass `eval::Passes::gaussian_blur`.
pub fn blur(input: &Image, radius: f32) -> Image {
    let plan = blur_plan(radius);
    if plan.iterations == 0 {
        return input.clone();
    }
    let mut img = input.clone();
    for _ in 0..plan.iterations {
        img = blur_one_pass(&img, plan.sigma_pass, plan.step);
    }
    img
}

// ── Coverage feather via approximate SDF (proposal 208) ─────────────────────

/// Soften a hard coverage matte (stored in **alpha**) with a soft band of width
/// `feather_px` (logical pixels). Uses a CPU Jump-Flood-style distance field
/// and a smoothstep around the zero isosurface (proposal 208). RGB is
/// re-premultiplied by the new alpha so the result stays valid premult.
///
/// `feather_px < 0.5` is a bit-exact identity. Far exterior stays 0; deep
/// interior stays at the original interior alpha.
pub fn feather_coverage(input: &Image, feather_px: f32) -> Image {
    let feather = if feather_px.is_finite() {
        feather_px.max(0.0)
    } else {
        0.0
    };
    if feather < 0.5 {
        return input.clone();
    }
    let dist = coverage_signed_distance(input);
    let w = input.width;
    let h = input.height;
    let mut out = Image::new(w, h);
    let half = feather;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let d = dist[i];
            // d < 0 inside; smoothstep from -half..+half
            let t = ((d + half) / (2.0 * half)).clamp(0.0, 1.0);
            // Hermite smoothstep
            let a = t * t * (3.0 - 2.0 * t);
            // Outside → 0, inside → 1, soft band between.
            let cover = 1.0 - a;
            let src = input.pixel(x, y);
            // Unpremultiply RGB if original alpha > 0, re-premultiply by cover.
            let rgb = if src[3] > 1e-6 {
                [src[0] / src[3], src[1] / src[3], src[2] / src[3]]
            } else {
                [0.0, 0.0, 0.0]
            };
            out.set(
                x,
                y,
                [rgb[0] * cover, rgb[1] * cover, rgb[2] * cover, cover],
            );
        }
    }
    out
}

/// Approximate signed distance (negative inside coverage) via Jump Flooding.
/// Coverage threshold is alpha ≥ 0.5. Units: pixels.
///
/// Shared with `raster_bridge::util_outline` (30 §5 requires the outline be
/// SDF-based). Jump Flooding is O(W·H·log max(W,H)) regardless of radius, which
/// is what lets both consumers drop a per-pixel neighbourhood search.
pub(crate) fn coverage_signed_distance(input: &Image) -> Vec<f32> {
    let w = input.width as i32;
    let h = input.height as i32;
    let n = (w * h) as usize;
    // Seed: store nearest boundary site as (sx, sy); invalid = (-1,-1).
    let mut seed_in: Vec<(i32, i32)> = vec![(-1, -1); n];
    let mut seed_out: Vec<(i32, i32)> = vec![(-1, -1); n];
    let idx = |x: i32, y: i32| -> usize { (y * w + x) as usize };
    let inside = |x: i32, y: i32| -> bool { input.pixel(x as u32, y as u32)[3] >= 0.5 };
    // Init: boundary pixels (inside next to outside, or vice versa) seed themselves.
    for y in 0..h {
        for x in 0..w {
            let inn = inside(x, y);
            let mut boundary = false;
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let nx = x + dx;
                let ny = y + dy;
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    if inn {
                        boundary = true;
                    }
                    continue;
                }
                if inside(nx, ny) != inn {
                    boundary = true;
                }
            }
            if boundary {
                if inn {
                    seed_in[idx(x, y)] = (x, y);
                } else {
                    seed_out[idx(x, y)] = (x, y);
                }
            }
        }
    }
    // Also seed pure-interior/exterior from nearest boundary via JFA.
    jfa_propagate(&mut seed_in, w, h);
    jfa_propagate(&mut seed_out, w, h);
    // For interior pixels that never got a seed, run JFA on inverted seeds too:
    // combine: for each pixel, if inside use dist to outside seed, else to inside.
    // Re-seed non-boundary from the other field's boundary by swapping.
    // Simpler approach: after propagate, any empty cell inherits from neighbours
    // was done by JFA. Fill remaining empties with far distance.
    let mut dist = vec![0.0f32; n];
    let far = (w.max(h) as f32) * 2.0;
    for y in 0..h {
        for x in 0..w {
            let i = idx(x, y);
            let inn = inside(x, y);
            if inn {
                let (sx, sy) = seed_out[i];
                let d = if sx < 0 {
                    // Deep interior with no outside seed found — large negative.
                    -far
                } else {
                    -(((x - sx) as f32).hypot((y - sy) as f32))
                };
                dist[i] = d;
            } else {
                let (sx, sy) = seed_in[i];
                let d = if sx < 0 {
                    far
                } else {
                    ((x - sx) as f32).hypot((y - sy) as f32)
                };
                dist[i] = d;
            }
        }
    }
    dist
}

/// Jump Flooding propagation of nearest-seed coordinates (Rong & Tan).
fn jfa_propagate(seeds: &mut [(i32, i32)], w: i32, h: i32) {
    let mut step = 1;
    while step < w.max(h) {
        step *= 2;
    }
    step /= 2;
    let idx = |x: i32, y: i32| -> usize { (y * w + x) as usize };
    while step >= 1 {
        let prev = seeds.to_vec();
        for y in 0..h {
            for x in 0..w {
                let i = idx(x, y);
                let mut best = prev[i];
                let mut best_d = if best.0 < 0 {
                    f32::INFINITY
                } else {
                    ((x - best.0) as f32).hypot((y - best.1) as f32)
                };
                for dy in [-step, 0, step] {
                    for dx in [-step, 0, step] {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = x + dx;
                        let ny = y + dy;
                        if nx < 0 || ny < 0 || nx >= w || ny >= h {
                            continue;
                        }
                        let cand = prev[idx(nx, ny)];
                        if cand.0 < 0 {
                            continue;
                        }
                        let d = ((x - cand.0) as f32).hypot((y - cand.1) as f32);
                        if d < best_d {
                            best_d = d;
                            best = cand;
                        }
                    }
                }
                seeds[i] = best;
            }
        }
        step /= 2;
    }
}

/// Native straight-color detail enhancement. Blur premultiplied color and
/// coverage together, normalize the blur by its coverage, then restore each
/// source pixel's original alpha. Input obeys the premultiplied color contract.
/// Amount zero / subpixel identity radii preserve the original values exactly.
pub fn native_unsharp(input: &Image, amount: f32, sigma: f32) -> Image {
    if amount == 0.0 || sigma < 0.5 {
        return input.clone();
    }
    let blurred = blur(input, sigma);
    let mut out = input.clone();
    for ((dst, src), low) in out
        .pixels
        .iter_mut()
        .zip(&input.pixels)
        .zip(&blurred.pixels)
    {
        if src[3] <= 0.0 {
            *dst = [0.0; 4];
            continue;
        }
        if low[3] > 1e-8 {
            for c in 0..3 {
                dst[c] = src[c] + amount * (src[c] - low[c] * src[3] / low[3]);
            }
        }
        dst[3] = src[3];
    }
    out
}

/// `Effect{Sharpen}` (08 §3 / K-0.2): unsharp mask
/// `out = src + amount · (src − blur(src, radius))` in premultiplied space.
/// `amount == 0` or `radius < 0.5` is a bit-exact identity. Registry paths:
/// `params.amount`, `params.radius`.
pub fn sharpen(input: &Image, amount: f32, radius: f32) -> Image {
    let amount = if amount.is_finite() { amount } else { 0.0 };
    if amount == 0.0 {
        return input.clone();
    }
    let blurred = blur(input, radius);
    if blurred.pixels == input.pixels {
        return input.clone();
    }
    let mut out = Image::new(input.width, input.height);
    for ((o, s), b) in out
        .pixels
        .iter_mut()
        .zip(input.pixels.iter())
        .zip(blurred.pixels.iter())
    {
        *o = [
            s[0] + amount * (s[0] - b[0]),
            s[1] + amount * (s[1] - b[1]),
            s[2] + amount * (s[2] - b[2]),
            s[3] + amount * (s[3] - b[3]),
        ];
    }
    out
}

/// `Effect{Glow}` (08 §3 / K-0.2): extract pixels whose straight Rec.709 luma
/// exceeds `threshold`, blur that extract by `radius`, tint by `tint_linear`
/// (straight linear RGB, alpha ignored — glow is a light add), scale by
/// `intensity`, and screen-add over the source in premultiplied space:
/// `out = src + glow * (1 − src)` componentwise on RGB, alpha =
/// `src.a + glow.a * (1 − src.a)`. Registry: `params.radius/threshold/intensity/tint`.
pub fn glow(
    input: &Image,
    radius: f32,
    threshold: f32,
    intensity: f32,
    tint_linear: [f32; 3],
) -> Image {
    let threshold = if threshold.is_finite() {
        threshold.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let intensity = if intensity.is_finite() {
        intensity.max(0.0)
    } else {
        0.0
    };
    if intensity == 0.0 {
        return input.clone();
    }
    // Bright extract: scale each premult pixel by a luma keep factor.
    let mut extract = Image::new(input.width, input.height);
    for (o, p) in extract.pixels.iter_mut().zip(input.pixels.iter()) {
        let straight = unpremultiply(*p).unwrap_or([0.0; 3]);
        let keep = if luma709(straight) >= threshold {
            1.0
        } else {
            0.0
        };
        *o = [p[0] * keep, p[1] * keep, p[2] * keep, p[3] * keep];
    }
    let blurred = blur(&extract, radius);
    let mut out = Image::new(input.width, input.height);
    for ((o, s), g) in out
        .pixels
        .iter_mut()
        .zip(input.pixels.iter())
        .zip(blurred.pixels.iter())
    {
        // Tint the glow: multiply RGB by tint, scale by intensity; keep α from the blur.
        let gr = g[0] * tint_linear[0] * intensity;
        let gg = g[1] * tint_linear[1] * intensity;
        let gb = g[2] * tint_linear[2] * intensity;
        let ga = (g[3] * intensity).clamp(0.0, 1.0);
        // Premultiplied "screen" / additive-over: src + glow*(1-src) on RGB;
        // classic over on alpha.
        *o = [
            s[0] + gr * (1.0 - s[0]).max(0.0),
            s[1] + gg * (1.0 - s[1]).max(0.0),
            s[2] + gb * (1.0 - s[2]).max(0.0),
            s[3] + ga * (1.0 - s[3]),
        ];
    }
    out
}

/// Native AP1 adaptation of the catalog screen-add glow contract.
pub fn native_glow(
    input: &Image,
    radius: f32,
    threshold: f32,
    intensity: f32,
    tint_linear: [f32; 3],
) -> Image {
    let threshold = if threshold.is_finite() {
        threshold.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let intensity = if intensity.is_finite() {
        intensity.max(0.0)
    } else {
        0.0
    };
    if intensity == 0.0 {
        return input.clone();
    }
    // Bright extract: scale each premult pixel by a luma keep factor.
    let mut extract = Image::new(input.width, input.height);
    for (o, p) in extract.pixels.iter_mut().zip(input.pixels.iter()) {
        let straight = unpremultiply(*p).unwrap_or([0.0; 3]);
        let keep =
            if (straight[0] * 0.27222872 + straight[1] * 0.67408174 + straight[2] * 0.053689517)
                >= threshold
            {
                1.0
            } else {
                0.0
            };
        *o = [p[0] * keep, p[1] * keep, p[2] * keep, p[3] * keep];
    }
    let blurred = blur(&extract, radius);
    let mut out = Image::new(input.width, input.height);
    for ((o, s), g) in out
        .pixels
        .iter_mut()
        .zip(input.pixels.iter())
        .zip(blurred.pixels.iter())
    {
        // Tint the glow: multiply RGB by tint, scale by intensity; keep α from the blur.
        let gr = g[0] * tint_linear[0] * intensity;
        let gg = g[1] * tint_linear[1] * intensity;
        let gb = g[2] * tint_linear[2] * intensity;
        let ga = (g[3] * intensity).clamp(0.0, 1.0);
        // Premultiplied "screen" / additive-over: src + glow*(1-src) on RGB;
        // classic over on alpha.
        *o = [
            s[0] + gr * (1.0 - s[0]).max(0.0),
            s[1] + gg * (1.0 - s[1]).max(0.0),
            s[2] + gb * (1.0 - s[2]).max(0.0),
            s[3] + ga * (1.0 - s[3]),
        ];
    }
    out
}

/// sRGB EOTF (gamma → scene-linear), the standard breakpoint form. An authoring
/// `Color` param (key colour, tint) is sRGB display-domain; the working space is
/// scene-linear Rec.709 (D-09), so a key/tint colour converts on the way in.
/// Shared so the CPU kernels here and the GPU uniforms (`eval.rs`) start from the
/// identical linear key value (a prerequisite for GPU/CPU parity).
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB OETF (scene-linear → gamma), the exact inverse of [`srgb_to_linear`].
///
/// Needed by kernels that must reason in the *perceptual* domain rather than the
/// linear working space — deflicker measures and corrects there, because equal
/// visibility of residual error matters more than equal energy (see
/// [`crate::graph::deflicker`]). Sharing the breakpoint form with the EOTF above
/// keeps the round trip exact.
#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// A well-defined `smoothstep` (Hermite) matching WGSL's `smoothstep`: the band
/// width is floored at a tiny epsilon so `edge0 == edge1` (a hard key with zero
/// softness) degrades to a near-step instead of dividing by zero — the CPU and
/// GPU must floor identically or a zero-softness key would diverge.
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The smallest key/feather band width (see [`smoothstep`]); a zero-width band
/// becomes this so the edge is a near-hard step, never a divide-by-zero. Shared
/// with the GPU twins (`eval.rs`) so both floor the band identically.
pub(crate) const KEY_BAND_EPS: f32 = 1e-4;

/// AP1 luminance-based alpha key. Extended RGB is retained in surviving pixels.
pub fn native_luma_key(input: &Image, threshold: f32, softness: f32, invert: bool) -> Image {
    let hi = threshold + softness.max(KEY_BAND_EPS);
    let mut out = Image::new(input.width, input.height);
    for (o, p) in out.pixels.iter_mut().zip(&input.pixels) {
        let rgb = unpremultiply(*p).unwrap_or([0.0; 3]);
        let y = rgb[0] * 0.27222872 + rgb[1] * 0.67408174 + rgb[2] * 0.053689517;
        let mut keep = smoothstep(threshold, hi, y);
        if invert {
            keep = 1.0 - keep;
        }
        *o = p.map(|c| c * keep);
    }
    out
}

/// `Effect{LumaKey}` (08 §3): scale each pixel's alpha by a luma-driven keep
/// factor. `keep = smoothstep(threshold, threshold + softness, luma)` over the
/// straight linear Rec.709 luma; `invert` flips it (key the brights instead of
/// the darks). Because storage is premultiplied, scaling alpha by `keep` scales
/// the whole `[r,g,b,a]` pixel by `keep` (straight colour is unchanged). The
/// WGSL twin is `eval::Passes::luma_key`.
pub fn luma_key(input: &Image, threshold: f32, softness: f32, invert: bool) -> Image {
    let hi = threshold + softness.max(KEY_BAND_EPS);
    let mut out = Image::new(input.width, input.height);
    for (o, p) in out.pixels.iter_mut().zip(input.pixels.iter()) {
        let straight = unpremultiply(*p).unwrap_or([0.0; 3]);
        let mut keep = smoothstep(threshold, hi, luma709(straight));
        if invert {
            keep = 1.0 - keep;
        }
        *o = [p[0] * keep, p[1] * keep, p[2] * keep, p[3] * keep];
    }
    out
}

/// `Effect{ChromaKey}` (08 §3): key out pixels near `key_linear` (straight linear
/// Rec.709). `keep = smoothstep(tolerance, tolerance + edge_softness, dist)` on
/// the Euclidean colour distance, so pixels within `tolerance` drop out and the
/// `edge_softness` band feathers the matte edge. `spill_suppress` desaturates the
/// key's dominant channel toward the mean of the other two (classic green-spill
/// removal) by that fraction, on the kept straight colour, before re-premultiply.
/// The WGSL twin is `eval::Passes::chroma_key`.
pub fn chroma_key(
    input: &Image,
    key_linear: [f32; 3],
    tolerance: f32,
    edge_softness: f32,
    spill_suppress: f32,
) -> Image {
    let hi = tolerance + edge_softness.max(KEY_BAND_EPS);
    // Dominant key channel (argmax) — the spill channel. Ties resolve to the
    // lowest index deterministically on both CPU and GPU (identical key values).
    let dom = if key_linear[0] >= key_linear[1] && key_linear[0] >= key_linear[2] {
        0
    } else if key_linear[1] >= key_linear[2] {
        1
    } else {
        2
    };
    let mut out = Image::new(input.width, input.height);
    for (o, p) in out.pixels.iter_mut().zip(input.pixels.iter()) {
        let straight = unpremultiply(*p).unwrap_or([0.0; 3]);
        let d = [
            straight[0] - key_linear[0],
            straight[1] - key_linear[1],
            straight[2] - key_linear[2],
        ];
        let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let keep = smoothstep(tolerance, hi, dist);
        // Spill suppression on the dominant key channel.
        let mut c = straight;
        let others = ((dom + 1) % 3, (dom + 2) % 3);
        let mean = 0.5 * (c[others.0] + c[others.1]);
        if c[dom] > mean {
            c[dom] += (mean - c[dom]) * spill_suppress;
        }
        let a = p[3] * keep;
        *o = [c[0] * a, c[1] * a, c[2] * a, a];
    }
    out
}

/// `Effect{MaskShapeGen}` (08 §3): a 0-input generator emitting an ellipse matte
/// (premultiplied white inside, transparent outside, `feather`ed edge). `center`
/// and `size` are canvas-normalized (size is the ellipse's half-axes as a
/// fraction of width/height); `rotation` is radians; `feather` is the inner edge
/// band as a fraction of the radius. This is the ellipse form; the graph node's
/// `MaskShapeKind` (rect vs. ellipse) is not yet carried in `ResolvedParams`, so
/// every mask-shape node renders as an ellipse for now. WGSL twin:
/// `eval::Passes::mask_shape`.
pub fn mask_shape(
    width: u32,
    height: u32,
    center: [f32; 2],
    size: [f32; 2],
    rotation: f32,
    feather: f32,
) -> Image {
    let (width, height) = (width.max(1), height.max(1));
    let mut out = Image::new(width, height);
    let (cos_r, sin_r) = ((-rotation).cos(), (-rotation).sin());
    let inner = 1.0 - feather.clamp(0.0, 1.0).max(KEY_BAND_EPS);
    for y in 0..height {
        for x in 0..width {
            let uv = [
                (x as f32 + 0.5) / width as f32,
                (y as f32 + 0.5) / height as f32,
            ];
            let d = [uv[0] - center[0], uv[1] - center[1]];
            // Rotate the offset into the ellipse's local frame (by −rotation).
            let rd = [d[0] * cos_r - d[1] * sin_r, d[0] * sin_r + d[1] * cos_r];
            let e = [
                if size[0].abs() > ALPHA_EPS {
                    rd[0] / size[0]
                } else {
                    f32::INFINITY
                },
                if size[1].abs() > ALPHA_EPS {
                    rd[1] / size[1]
                } else {
                    f32::INFINITY
                },
            ];
            let r = (e[0] * e[0] + e[1] * e[1]).sqrt();
            let a = 1.0 - smoothstep(inner, 1.0, r);
            out.set(x, y, [a, a, a, a]);
        }
    }
    out
}

/// Native ellipse/rectangle AP1 white image generator.
pub fn native_mask_shape(
    width: u32,
    height: u32,
    rect: bool,
    center: [f32; 2],
    size: [f32; 2],
    rotation: f32,
    feather: f32,
) -> Image {
    let (width, height) = (width.max(1), height.max(1));
    let mut out = Image::new(width, height);
    let (cos_r, sin_r) = ((-(rotation as f64)).cos(), (-(rotation as f64)).sin());
    let inner = 1.0 - (feather as f64).clamp(0.0, 1.0).max(KEY_BAND_EPS as f64);
    for y in 0..height {
        for x in 0..width {
            let uv = [
                (x as f64 + 0.5) / width as f64,
                (y as f64 + 0.5) / height as f64,
            ];
            let d = [uv[0] - center[0] as f64, uv[1] - center[1] as f64];
            // Rotate the offset into the ellipse's local frame (by −rotation).
            let rd = [d[0] * cos_r - d[1] * sin_r, d[0] * sin_r + d[1] * cos_r];
            let e = [
                if size[0].abs() > ALPHA_EPS {
                    rd[0] / size[0] as f64
                } else {
                    f64::INFINITY
                },
                if size[1].abs() > ALPHA_EPS {
                    rd[1] / size[1] as f64
                } else {
                    f64::INFINITY
                },
            ];
            let r = if rect {
                e[0].abs().max(e[1].abs())
            } else {
                (e[0] * e[0] + e[1] * e[1]).sqrt()
            };
            let t = ((r - inner) / (1.0 - inner)).clamp(0.0, 1.0);
            let a = (1.0 - t * t * (3.0 - 2.0 * t)) as f32;
            out.set(x, y, [a, a, a, a]);
        }
    }
    out
}

/// `Merge`: composite `top` over `bottom` with global `opacity`, under `mode`
/// (02 §2 `Merge`). Premultiplied linear source-over with a blend function
/// (W3C compositing model); `Normal` reduces to plain `over`. Blend math reuses
/// [`blend_rgb`] (03 §4.4 rule 4). Output is the size of `bottom` (or `top` when
/// bottom is smaller); mismatched sizes sample `top` at matching pixels.
pub fn merge(top: &Image, bottom: &Image, mode: BlendMode, opacity: f32) -> Image {
    let opacity = opacity.clamp(0.0, 1.0);
    let w = top.width.max(bottom.width);
    let h = top.height.max(bottom.height);
    let mut out = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let tp = sample_clamped(top, x, y);
            let bp = sample_clamped(bottom, x, y);
            out.set(x, y, merge_pixel(tp, bp, mode, opacity));
        }
    }
    out
}

/// Native Normal is direct premultiplied source-over, matching the GPU fast
/// path. It must not divide by alpha and discard valid extended RGB below the
/// artistic blend unpremultiply cutoff. Legacy `merge` remains unchanged.
pub fn native_normal_merge(top: &Image, bottom: &Image, opacity: f32) -> Image {
    let opacity = opacity.clamp(0.0, 1.0);
    let mut out = Image::new(top.width.max(bottom.width), top.height.max(bottom.height));
    for y in 0..out.height {
        for x in 0..out.width {
            let tp = sample_clamped(top, x, y);
            let bp = sample_clamped(bottom, x, y);
            let alpha = tp[3] * opacity;
            let mut pixel = [0.0; 4];
            for c in 0..3 {
                pixel[c] = (tp[c] * opacity + bp[c] * (1.0 - alpha)).clamp(-65504.0, 65504.0);
            }
            pixel[3] = alpha + bp[3] * (1.0 - alpha);
            out.set(x, y, pixel);
        }
    }
    out
}

/// Native SoftLight evaluates the extended cubic in f64 before finite half-storage
/// saturation. Valid signed premultiplied inputs near the alpha cutoff otherwise
/// overflow the f32 straight-color intermediate, including NaN at zero opacity.
/// Legacy uses `merge` unchanged. GPU uses an algebraically factored f32 overlap.
pub fn native_soft_light_merge(top: &Image, bottom: &Image, opacity: f32) -> Image {
    let opacity = opacity.clamp(0.0, 1.0) as f64;
    let mut out = Image::new(top.width.max(bottom.width), top.height.max(bottom.height));
    for y in 0..out.height {
        for x in 0..out.width {
            let tp = sample_clamped(top, x, y);
            let bp = sample_clamped(bottom, x, y);
            if opacity == 0.0 {
                out.set(x, y, bp);
                continue;
            }
            let sa = tp[3] as f64;
            let ba = bp[3] as f64;
            let a = sa * opacity;
            let mut pixel = [0.0; 4];
            for c in 0..3 {
                let cs = if tp[3] > ALPHA_EPS {
                    tp[c] as f64 / sa
                } else {
                    0.0
                };
                let cb = if bp[3] > ALPHA_EPS {
                    bp[c] as f64 / ba
                } else {
                    0.0
                };
                let blended = if cs <= 0.5 {
                    cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
                } else {
                    let d = if cb <= 0.25 {
                        ((16.0 * cb - 12.0) * cb + 4.0) * cb
                    } else {
                        cb.sqrt()
                    };
                    cb + (2.0 * cs - 1.0) * (d - cb)
                };
                pixel[c] = (a * ((1.0 - ba) * cs + ba * blended) + (1.0 - a) * bp[c] as f64)
                    .clamp(-65504.0, 65504.0) as f32;
            }
            pixel[3] = (a + ba * (1.0 - a)) as f32;
            out.set(x, y, pixel);
        }
    }
    out
}

/// Native AP1 arithmetic blends, with the same premultiplied source-over
/// convention as `merge`, but no display-range clipping of the blend function.
pub fn native_arithmetic_merge(
    top: &Image,
    bottom: &Image,
    mode: BlendMode,
    opacity: f32,
) -> Image {
    let opacity = opacity.clamp(0.0, 1.0);
    let mut out = Image::new(top.width.max(bottom.width), top.height.max(bottom.height));
    for y in 0..out.height {
        for x in 0..out.width {
            let tp = sample_clamped(top, x, y);
            let bp = sample_clamped(bottom, x, y);
            let cs = unpremultiply(tp).unwrap_or([0.0; 3]);
            let cb = unpremultiply(bp).unwrap_or([0.0; 3]);
            let a_s = tp[3] * opacity;
            let a_b = bp[3];
            let mut pixel = [0.0; 4];
            for c in 0..3 {
                let blended = match mode {
                    BlendMode::LinearDodge => cb[c] + cs[c],
                    BlendMode::LinearBurn => cb[c] + cs[c] - 1.0,
                    BlendMode::Subtract => cb[c] - cs[c],
                    BlendMode::LinearLight => cb[c] + 2.0 * cs[c] - 1.0,
                    _ => unreachable!("native arithmetic mode validated before evaluation"),
                };
                pixel[c] = a_s * ((1.0 - a_b) * cs[c] + a_b * blended) + (1.0 - a_s) * bp[c];
            }
            pixel[3] = a_s + a_b * (1.0 - a_s);
            out.set(x, y, pixel);
        }
    }
    out
}

/// AP1-Y replacement with luminance-preserving unit-cube clipping. Inputs are
/// finite bounded creative RGB coordinates, not display-encoded HSL colors.
fn native_set_lum(c: [f32; 3], desired: f32) -> [f32; 3] {
    let lum = |v: [f32; 3]| v[0] * 0.27222872 + v[1] * 0.67408174 + v[2] * 0.053689517;
    let shift = desired - lum(c);
    let mut out = c.map(|channel| channel + shift);
    let l = lum(out).clamp(0.0, 1.0);
    let low = out[0].min(out[1]).min(out[2]);
    let high = out[0].max(out[1]).max(out[2]);
    if low < 0.0 {
        out = out.map(|channel| l + (channel - l) * l / (l - low));
    }
    if high > 1.0 {
        out = out.map(|channel| l + (channel - l) * (1.0 - l) / (high - l));
    }
    out.map(|channel| channel.clamp(0.0, 1.0))
}

fn native_nonseparable_rgb(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    use photonic_core::raster::blend::set_sat;
    let lum = |v: [f32; 3]| v[0] * 0.27222872 + v[1] * 0.67408174 + v[2] * 0.053689517;
    let sat = |v: [f32; 3]| v[0].max(v[1]).max(v[2]) - v[0].min(v[1]).min(v[2]);
    match mode {
        BlendMode::Hue => native_set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => native_set_lum(set_sat(cb, sat(cs)), lum(cb)),
        BlendMode::Color => native_set_lum(cs, lum(cb)),
        BlendMode::Luminosity => native_set_lum(cb, lum(cs)),
        _ => unreachable!("native nonseparable mode validated"),
    }
}

/// Bounded artistic AP1 blends. Clipping affects the overlap function only,
/// never unblended source/backdrop contributions. CPU reuses the catalog math.
pub fn native_bounded_merge(top: &Image, bottom: &Image, mode: BlendMode, opacity: f32) -> Image {
    assert!(super::ir::native_bounded_blend(mode));
    let opacity = opacity.clamp(0.0, 1.0);
    let mut out = Image::new(top.width.max(bottom.width), top.height.max(bottom.height));
    for y in 0..out.height {
        for x in 0..out.width {
            let tp = sample_clamped(top, x, y);
            let bp = sample_clamped(bottom, x, y);
            let cs = unpremultiply(tp).unwrap_or([0.0; 3]);
            let cb = unpremultiply(bp).unwrap_or([0.0; 3]);
            let a_s = tp[3] * opacity;
            let a_b = bp[3];
            let nonseparable = super::ir::native_nonseparable_blend(mode).then(|| {
                native_nonseparable_rgb(
                    mode,
                    cb.map(|c| c.clamp(0.0, 1.0)),
                    cs.map(|c| c.clamp(0.0, 1.0)),
                )
            });
            let mut pixel = [0.0; 4];
            for c in 0..3 {
                let b = cb[c].clamp(0.0, 1.0);
                let s = cs[c].clamp(0.0, 1.0);
                let blended = if let Some(rgb) = nonseparable {
                    rgb[c]
                } else if mode == BlendMode::HardMix {
                    // Algebraic VividLight >= 0.5 avoids division roundoff
                    // changing the discontinuous threshold at complementary colors.
                    if (b == 0.0 && s == 1.0) || b < 1.0 - s {
                        0.0
                    } else {
                        1.0
                    }
                } else {
                    photonic_core::raster::blend::blend_channel(mode, b, s)
                };
                pixel[c] = a_s * ((1.0 - a_b) * cs[c] + a_b * blended) + (1.0 - a_s) * bp[c];
            }
            pixel[3] = a_s + a_b * (1.0 - a_s);
            out.set(x, y, pixel);
        }
    }
    out
}

/// Whole-color scene-linear AP1 selection. These weights are the AP1-to-XYZ Y
/// row; they intentionally differ from Legacy's artistic 0.30/0.59/0.11 luma.
pub fn native_luminance_merge(top: &Image, bottom: &Image, mode: BlendMode, opacity: f32) -> Image {
    let opacity = opacity.clamp(0.0, 1.0);
    let mut out = Image::new(top.width.max(bottom.width), top.height.max(bottom.height));
    let y_ap1 = |rgb: [f32; 3]| rgb[0] * 0.27222872 + rgb[1] * 0.67408174 + rgb[2] * 0.053689517;
    for y in 0..out.height {
        for x in 0..out.width {
            let tp = sample_clamped(top, x, y);
            let bp = sample_clamped(bottom, x, y);
            let cs = unpremultiply(tp).unwrap_or([0.0; 3]);
            let cb = unpremultiply(bp).unwrap_or([0.0; 3]);
            let by = y_ap1(cb);
            let sy = y_ap1(cs);
            let backdrop = match mode {
                BlendMode::DarkerColor => by <= sy,
                BlendMode::LighterColor => by >= sy,
                _ => unreachable!("native whole-color mode validated"),
            };
            let blended = if backdrop { cb } else { cs };
            let a_s = tp[3] * opacity;
            let a_b = bp[3];
            let mut pixel = [0.0; 4];
            for c in 0..3 {
                pixel[c] = a_s * ((1.0 - a_b) * cs[c] + a_b * blended[c]) + (1.0 - a_s) * bp[c];
            }
            pixel[3] = a_s + a_b * (1.0 - a_s);
            out.set(x, y, pixel);
        }
    }
    out
}

#[inline]
fn sample_clamped(img: &Image, x: u32, y: u32) -> [f32; 4] {
    let cx = x.min(img.width - 1);
    let cy = y.min(img.height - 1);
    img.pixel(cx, cy)
}

/// One premultiplied `over`-with-blend pixel (W3C compositing). `tp`/`bp` are
/// premultiplied linear; the result is premultiplied linear.
#[inline]
fn merge_pixel(tp: [f32; 4], bp: [f32; 4], mode: BlendMode, opacity: f32) -> [f32; 4] {
    let a_s = tp[3] * opacity; // effective source alpha
    let a_b = bp[3];
    // At α <= ALPHA_EPS the premultiplied RGB is already 0, so carrying `[0;3]`
    // through is the "carry RGB unchanged" outcome (03 §4.5.2) and moves no pixel.
    let cs = unpremultiply(tp).unwrap_or([0.0; 3]);
    let cb = unpremultiply(bp).unwrap_or([0.0; 3]);
    // Backdrop-blended source color: Cs' = (1-αb)·Cs + αb·B(Cb, Cs).
    let blended = if mode == BlendMode::Normal {
        cs
    } else {
        blend_rgb(mode, cb, cs)
    };
    let mut out = [0.0f32; 4];
    for c in 0..3 {
        // Backdrop-blended straight source color, then premultiplied source-over:
        // Cs' = (1-αb)·Cs + αb·B(Cb, Cs);  co = αs·Cs' + (1-αs)·(backdrop premult).
        let cs_prime = (1.0 - a_b) * cs[c] + a_b * blended[c];
        out[c] = a_s * cs_prime + (1.0 - a_s) * bp[c];
    }
    out[3] = a_s + a_b * (1.0 - a_s);
    out
}

/// Normalised sweep coordinate in `0..1` for a directional transition at pixel
/// (`x`,`y`) of a `w`×`h` frame: the fraction of the way along `direction`'s axis
/// measured from the edge the incoming layer enters. Pixel-centered (`(i+0.5)/n`)
/// so the CPU and GPU agree on where the edge sits. Shared by [`wipe`].
#[inline]
fn sweep_coord(dir: WipeDirection, x: u32, y: u32, w: u32, h: u32) -> f32 {
    let xn = (x as f32 + 0.5) / w as f32;
    let yn = (y as f32 + 0.5) / h as f32;
    match dir {
        WipeDirection::LeftToRight => xn,
        WipeDirection::RightToLeft => 1.0 - xn,
        WipeDirection::TopToBottom => yn,
        WipeDirection::BottomToTop => 1.0 - yn,
    }
}

/// Analytical luma-map wipe (26 K-B7): per-pixel switch time from
/// [`crate::graph::luma_wipe`], blended with [`crate::graph::luma_wipe::soft_mix`].
/// Premultiplied lerp; `t == 0` → `outgoing`, `t == 1` → `incoming`. WGSL twin is
/// `eval::Passes::luma_wipe`.
pub fn luma_wipe(
    incoming: &Image,
    outgoing: &Image,
    kind: crate::graph::luma_wipe::LumaWipeKind,
    softness: f32,
    invert: bool,
    t: f32,
) -> Image {
    use crate::graph::luma_wipe::{luma_at, soft_mix};
    let w = incoming.width.max(outgoing.width);
    let h = incoming.height.max(outgoing.height);
    let mut out = Image::new(w, h);
    let wf = w.max(1) as f32;
    let hf = h.max(1) as f32;
    for y in 0..h {
        for x in 0..w {
            let u = (x as f32 + 0.5) / wf;
            let v = (y as f32 + 0.5) / hf;
            let m = luma_at(kind, u, v, invert);
            let reveal = soft_mix(t, m, softness);
            let ip = sample_clamped(incoming, x, y);
            let op = sample_clamped(outgoing, x, y);
            let mut px = [0.0f32; 4];
            for c in 0..4 {
                px[c] = op[c] * (1.0 - reveal) + ip[c] * reveal;
            }
            out.set(x, y, px);
        }
    }
    out
}

/// `WipeMix` (08 §2.0b): a directional `smoothstep` wipe between `incoming` and
/// `outgoing` at eased factor `t`; `softness` is the edge half-width (canvas-
/// normalised). The edge is remapped to sweep the full `[-s, 1+s]` range so the
/// endpoints stay bit-exact for **any** softness: `t == 0` returns `outgoing`,
/// `t == 1` returns `incoming`. Premultiplied linear is closed under linear
/// interpolation, so the blend is a plain componentwise lerp (no fringing). The
/// WGSL twin is `eval::Passes::wipe`.
pub fn wipe(
    incoming: &Image,
    outgoing: &Image,
    dir: WipeDirection,
    softness: f32,
    t: f32,
) -> Image {
    let w = incoming.width.max(outgoing.width);
    let h = incoming.height.max(outgoing.height);
    let mut out = Image::new(w, h);
    let s = softness.max(0.0);
    let edge = -s + t * (1.0 + 2.0 * s);
    let hw = s.max(KEY_BAND_EPS);
    for y in 0..h {
        for x in 0..w {
            let p = sweep_coord(dir, x, y, w, h);
            // `reveal`: 1 = fully incoming, 0 = fully outgoing.
            let reveal = 1.0 - smoothstep(edge - hw, edge + hw, p);
            let ip = sample_clamped(incoming, x, y);
            let op = sample_clamped(outgoing, x, y);
            let mut v = [0.0f32; 4];
            for c in 0..4 {
                v[c] = op[c] * (1.0 - reveal) + ip[c] * reveal;
            }
            out.set(x, y, v);
        }
    }
    out
}

/// `PushMix` (08 §2.0b): both layers translate along `direction` by `t`, the
/// incoming sliding in from the entering edge as the outgoing slides out, sampled
/// with [`transform2d`]'s pixel-center / edge-clamp bilinear semantics (03 §4.5).
/// `t == 0` returns `outgoing`, `t == 1` returns `incoming`, bit-exact. The WGSL
/// twin is `eval::Passes::push`.
pub fn push(incoming: &Image, outgoing: &Image, dir: WipeDirection, t: f32) -> Image {
    let w = incoming.width.max(outgoing.width);
    let h = incoming.height.max(outgoing.height);
    let mut out = Image::new(w, h);
    // `horizontal`: the sweep axis is x (else y). `forward`: the incoming enters
    // from the low-coordinate edge, so screen coord increases *into* the outgoing.
    let (horizontal, forward) = match dir {
        WipeDirection::LeftToRight => (true, true),
        WipeDirection::RightToLeft => (true, false),
        WipeDirection::TopToBottom => (false, true),
        WipeDirection::BottomToTop => (false, false),
    };
    for y in 0..h {
        for x in 0..w {
            let (u, npx) = if horizontal {
                ((x as f32 + 0.5) / w as f32, w as f32)
            } else {
                ((y as f32 + 0.5) / h as f32, h as f32)
            };
            // Mirror the axis for the reverse directions, run the forward selection,
            // then unmirror the sampled coordinate.
            let uu = if forward { u } else { 1.0 - u };
            let (use_incoming, src_u) = if uu >= t {
                (false, uu - t) // outgoing shifted by +t
            } else {
                (true, uu - t + 1.0) // incoming sliding in from the low edge
            };
            let sample_along = if forward { src_u } else { 1.0 - src_u };
            let src = if use_incoming { incoming } else { outgoing };
            let (px, py) = if horizontal {
                (sample_along * npx, y as f32 + 0.5)
            } else {
                (x as f32 + 0.5, sample_along * npx)
            };
            out.set(x, y, src.sample_bilinear(px, py));
        }
    }
    out
}

/// α below which RGB is carried through unchanged instead of divided (03 §4.5.2).
///
/// For premultiplied storage α == 0 implies RGB == 0, so callers of
/// [`unpremultiply`] that substitute `[0.0; 3]` for the `None` case reproduce
/// exactly the "carry RGB through unchanged" outcome the spec describes for
/// straight-alpha buffers — the two agree in practice.
pub const ALPHA_EPS: f32 = 1e-6;

/// Straight (unpremultiplied) linear RGB from a premultiplied pixel, or `None`
/// when α <= [`ALPHA_EPS`] (03 §4.5.2). The inverse is [`repremultiply`]. Shared
/// as the single citable primitive so every operator (grade, blend, future
/// catalogue effects) unpremultiplies one way (03 §4.5).
#[inline]
pub fn unpremultiply(p: [f32; 4]) -> Option<[f32; 3]> {
    let a = p[3];
    if a > ALPHA_EPS {
        Some([p[0] / a, p[1] / a, p[2] / a])
    } else {
        None
    }
}

/// Re-premultiply straight linear RGB by α (03 §4.5.2), the inverse of
/// [`unpremultiply`]. Returns the full premultiplied `[r, g, b, a]`.
#[inline]
pub fn repremultiply(rgb: [f32; 3], a: f32) -> [f32; 4] {
    [rgb[0] * a, rgb[1] * a, rgb[2] * a, a]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_transform_border_does_not_change_legacy_clamp() {
        let image = Image::filled(
            2,
            1,
            LinearColor {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let moved = Mat3::from_translation(Vec2::new(1.0, 0.0));
        let legacy = transform2d_to_canvas(&image, moved, Sampling::Nearest, 2, 1);
        let native =
            transform2d_to_canvas_with_border(&image, moved, Sampling::Nearest, 2, 1, true);
        assert_eq!(legacy.pixel(0, 0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(native.pixel(0, 0), [0.0; 4]);
        assert_eq!(native.pixel(1, 0), [1.0, 0.0, 0.0, 1.0]);
        let bilinear = transform2d_to_canvas_with_border(
            &image,
            Mat3::from_translation(Vec2::new(0.5, 0.0)),
            Sampling::Bilinear,
            2,
            1,
            true,
        );
        assert_eq!(bilinear.pixel(0, 0), [0.5, 0.0, 0.0, 0.5]);
    }

    fn premult(r: f32, g: f32, b: f32, a: f32) -> [f32; 4] {
        [r * a, g * a, b * a, a]
    }

    // ── D-12 stabilization warp ─────────────────────────────────────────

    /// A deterministic test pattern with structure in both axes, so a warp
    /// that mirrors, transposes, or shifts is visible rather than plausible.
    fn ramp(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let u = x as f32 / (w - 1).max(1) as f32;
                let v = y as f32 / (h - 1).max(1) as f32;
                img.set(x, y, [u, v, u * v, 1.0]);
            }
        }
        img
    }

    fn pinhole_warp(w: f32, h: f32) -> StabilizeWarp {
        StabilizeWarp::identity(w, h)
    }

    #[test]
    fn identity_warp_is_an_exact_passthrough() {
        // The invariant the whole lens design is built around: unprojecting and
        // reprojecting through the *same* model must cancel exactly, so a
        // stabilized clip with no correction is bit-identical to its source and
        // costs no sharpness.
        let src = ramp(64, 36);
        let out = stabilize_warp(&src, &pinhole_warp(64.0, 36.0), Sampling::Bilinear);
        assert_eq!(out, src);
    }

    #[test]
    fn identity_fisheye_warp_is_also_a_passthrough() {
        let src = ramp(64, 36);
        let mut warp = pinhole_warp(64.0, 36.0);
        warp.fisheye = true;
        warp.k = [0.02, -0.004, 0.0007, -0.00005];
        let out = stabilize_warp(&src, &warp, Sampling::Bilinear);
        assert_eq!(out, src, "is_identity() short-circuits before any resample");
    }

    #[test]
    fn near_identity_fisheye_round_trip_stays_faithful() {
        // Force the real math to run (not the short circuit) by nudging the
        // rotation by a hair, then check the result is still essentially the
        // source. This is what proves project/unproject actually invert, rather
        // than the short circuit hiding a broken pair.
        let src = ramp(64, 36);
        let mut warp = pinhole_warp(64.0, 36.0);
        warp.fisheye = true;
        warp.k = [0.02, -0.004, 0.0007, -0.00005];
        warp.rotation[0] = 1.0 - 1e-6;
        let out = stabilize_warp(&src, &warp, Sampling::Bilinear);
        let worst = out
            .pixels
            .iter()
            .zip(src.pixels.iter())
            .map(|(a, b)| (0..4).map(|c| (a[c] - b[c]).abs()).fold(0.0f32, f32::max))
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "worst channel deviation was {worst}");
    }

    #[test]
    fn invalid_warp_renders_transparent_not_garbage() {
        let src = ramp(16, 16);
        for bad in [
            StabilizeWarp {
                zoom: f32::NAN,
                ..pinhole_warp(16.0, 16.0)
            },
            StabilizeWarp {
                zoom: 0.5, // a "zoom" that shrinks exposes edges by construction
                ..pinhole_warp(16.0, 16.0)
            },
            StabilizeWarp {
                intrinsics: [0.0, 0.5, 0.5, 0.5],
                ..pinhole_warp(16.0, 16.0)
            },
            StabilizeWarp {
                rotation: [f32::INFINITY; 9],
                ..pinhole_warp(16.0, 16.0)
            },
        ] {
            let out = stabilize_warp(&src, &bad, Sampling::Bilinear);
            assert!(
                out.pixels.iter().all(|p| *p == [0.0; 4]),
                "an invalid warp must render transparent, matching Transform2D's policy"
            );
        }
    }

    #[test]
    fn zoom_magnifies_about_the_frame_centre() {
        let src = ramp(64, 64);
        let warp = StabilizeWarp {
            zoom: 2.0,
            ..pinhole_warp(64.0, 64.0)
        };
        let out = stabilize_warp(&src, &warp, Sampling::Bilinear);
        // The centre pixel is a fixed point of a centred zoom.
        let c = 32u32;
        for ch in 0..4 {
            assert!(
                (out.pixel(c, c)[ch] - src.pixel(c, c)[ch]).abs() < 0.02,
                "centre moved on channel {ch}"
            );
        }
        // A point a quarter-frame out should now show what was an eighth out.
        let probe = out.pixel(48, 32);
        let expect = src.pixel(40, 32);
        assert!(
            (probe[0] - expect[0]).abs() < 0.03,
            "2x zoom should halve the offset from centre: {probe:?} vs {expect:?}"
        );
    }

    #[test]
    fn rotation_shifts_content_in_the_expected_direction() {
        // A yaw to the right must pull content from further left in the source.
        let src = ramp(64, 64);
        let yaw = 5f32.to_radians();
        let (s, c) = (yaw.sin(), yaw.cos());
        let warp = StabilizeWarp {
            // Row-major rotation about Y.
            rotation: [c, 0.0, s, 0.0, 1.0, 0.0, -s, 0.0, c],
            ..pinhole_warp(64.0, 64.0)
        };
        let out = stabilize_warp(&src, &warp, Sampling::Bilinear);
        // Channel 0 is the horizontal ramp, so it reads directly as position.
        let before = src.pixel(32, 32)[0];
        let after = out.pixel(32, 32)[0];
        assert!(
            after > before + 0.01,
            "a +Y rotation should sample further right: {before} -> {after}"
        );
    }

    #[test]
    fn transparent_edges_leaves_holes_instead_of_smearing() {
        let src = ramp(64, 64);
        let yaw = 25f32.to_radians();
        let (s, c) = (yaw.sin(), yaw.cos());
        let rotation = [c, 0.0, s, 0.0, 1.0, 0.0, -s, 0.0, c];

        let holed = stabilize_warp(
            &src,
            &StabilizeWarp {
                rotation,
                transparent_edges: true,
                ..pinhole_warp(64.0, 64.0)
            },
            Sampling::Bilinear,
        );
        let clamped = stabilize_warp(
            &src,
            &StabilizeWarp {
                rotation,
                transparent_edges: false,
                ..pinhole_warp(64.0, 64.0)
            },
            Sampling::Bilinear,
        );
        assert!(
            holed.pixels.iter().any(|p| p[3] == 0.0),
            "a large rotation must expose transparent pixels in TransparentEdges"
        );
        assert!(
            clamped.pixels.iter().all(|p| p[3] > 0.0),
            "clamping mode must not punch holes"
        );
    }

    #[test]
    fn nearest_and_bilinear_agree_on_structure() {
        // Not a parity test — just that the two sampling modes are both wired
        // and land in the same place, so a mode swap is not a silent shift.
        let src = ramp(64, 64);
        let yaw = 3f32.to_radians();
        let (s, c) = (yaw.sin(), yaw.cos());
        let warp = StabilizeWarp {
            rotation: [c, 0.0, s, 0.0, 1.0, 0.0, -s, 0.0, c],
            ..pinhole_warp(64.0, 64.0)
        };
        let bl = stabilize_warp(&src, &warp, Sampling::Bilinear);
        let nn = stabilize_warp(&src, &warp, Sampling::Nearest);
        let worst = bl
            .pixels
            .iter()
            .zip(nn.pixels.iter())
            .map(|(a, b)| (a[0] - b[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst < 0.05,
            "modes disagree by {worst}, likely a coordinate bug"
        );
    }

    #[test]
    fn warp_preserves_image_dimensions() {
        let src = ramp(37, 23);
        let warp = StabilizeWarp {
            zoom: 1.4,
            ..pinhole_warp(37.0, 23.0)
        };
        let out = stabilize_warp(&src, &warp, Sampling::Bilinear);
        assert_eq!((out.width, out.height), (37, 23));
    }

    #[test]
    fn solid_is_uniform() {
        let img = solid(
            4,
            4,
            LinearColor {
                r: 0.25,
                g: 0.5,
                b: 0.75,
                a: 1.0,
            },
        );
        assert_eq!(img.pixels.len(), 16);
        assert!(img.pixels.iter().all(|p| *p == [0.25, 0.5, 0.75, 1.0]));
    }

    #[test]
    fn identity_transform_is_passthrough() {
        let img = solid(
            3,
            3,
            LinearColor {
                r: 0.1,
                g: 0.2,
                b: 0.3,
                a: 1.0,
            },
        );
        let out = transform2d(&img, Mat3::IDENTITY, Sampling::Bilinear);
        assert_eq!(out, img);
    }

    #[test]
    fn singular_transform_is_transparent() {
        let img = Image {
            width: 2,
            height: 2,
            pixels: vec![premult(1.0, 0.0, 0.0, 1.0); 4],
        };
        let out = transform2d(
            &img,
            Mat3::from_scale(Vec2::new(0.0, 1.0)),
            Sampling::Nearest,
        );
        assert_eq!(out, Image::new(2, 2));
    }

    #[test]
    fn merge_opaque_over_replaces_backdrop() {
        // Opaque red over opaque blue with Normal, opacity 1 → red.
        let top = solid(
            2,
            2,
            LinearColor {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let bottom = solid(
            2,
            2,
            LinearColor {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            },
        );
        let out = merge(&top, &bottom, BlendMode::Normal, 1.0);
        for p in &out.pixels {
            assert!((p[0] - 1.0).abs() < 1e-6);
            assert!(p[1].abs() < 1e-6);
            assert!(p[2].abs() < 1e-6);
            assert!((p[3] - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn merge_half_opacity_is_linear_blend() {
        // Opaque white over opaque black at opacity 0.5 → premultiplied 0.5 grey.
        let top = solid(
            1,
            1,
            LinearColor {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
        );
        let bottom = solid(
            1,
            1,
            LinearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let out = merge(&top, &bottom, BlendMode::Normal, 0.5);
        let p = out.pixels[0];
        #[allow(clippy::needless_range_loop)]
        for c in 0..3 {
            assert!((p[c] - 0.5).abs() < 1e-6, "channel {c} = {}", p[c]);
        }
        assert!((p[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn merge_transparent_top_keeps_backdrop() {
        let top = solid(
            1,
            1,
            LinearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
        );
        let bottom = solid(
            1,
            1,
            LinearColor {
                r: 0.3,
                g: 0.4,
                b: 0.5,
                a: 1.0,
            },
        );
        let out = merge(&top, &bottom, BlendMode::Normal, 1.0);
        let p = out.pixels[0];
        assert!((p[0] - 0.3).abs() < 1e-6);
        assert!((p[1] - 0.4).abs() < 1e-6);
        assert!((p[2] - 0.5).abs() < 1e-6);
        assert!((p[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn merge_over_transparent_backdrop_premultiplies_source() {
        // Opaque red over transparent, opacity 1 → premultiplied red, alpha 1.
        let top = solid(
            1,
            1,
            LinearColor {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let bottom = Image::new(1, 1);
        let out = merge(&top, &bottom, BlendMode::Normal, 1.0);
        assert_eq!(out.pixels[0], premult(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn invert_opaque_is_one_minus_rgb() {
        let img = solid(
            2,
            2,
            LinearColor {
                r: 0.2,
                g: 0.6,
                b: 0.9,
                a: 1.0,
            },
        );
        let out = invert(&img);
        for p in &out.pixels {
            assert!((p[0] - 0.8).abs() < 1e-6, "r={}", p[0]);
            assert!((p[1] - 0.4).abs() < 1e-6, "g={}", p[1]);
            assert!((p[2] - 0.1).abs() < 1e-6, "b={}", p[2]);
            assert!((p[3] - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn invert_preserves_premultiplied_alpha() {
        // A 50%-transparent white premult pixel [0.5,0.5,0.5,0.5] → straight white
        // → inverted straight black → premult [0,0,0,0.5], alpha unchanged.
        let mut img = Image::new(1, 1);
        img.pixels[0] = [0.5, 0.5, 0.5, 0.5];
        let out = invert(&img);
        let p = out.pixels[0];
        for (c, &v) in p[..3].iter().enumerate() {
            assert!(v.abs() < 1e-6, "channel {c} = {v}");
        }
        assert!((p[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn unpremultiply_repremultiply_round_trips() {
        // For α > ALPHA_EPS the round trip is the identity within 1e-6; below the
        // threshold `unpremultiply` reports `None` (03 §4.5.2).
        let straight = [0.3f32, 0.6, 0.85];
        for a in [1e-7f32, 1e-6, 0.01, 0.5, 1.0] {
            let premul = repremultiply(straight, a);
            assert!((premul[3] - a).abs() < 1e-9, "alpha carried for α={a}");
            match unpremultiply(premul) {
                Some(back) => {
                    assert!(a > ALPHA_EPS, "α={a} should have been below threshold");
                    for c in 0..3 {
                        assert!(
                            (back[c] - straight[c]).abs() <= 1e-6,
                            "α={a} ch{c}: {} vs {}",
                            back[c],
                            straight[c]
                        );
                    }
                }
                None => assert!(a <= ALPHA_EPS, "α={a} wrongly reported transparent"),
            }
        }
    }

    #[test]
    fn merge_pixel_unchanged_after_helper_extraction() {
        // Re-assert the four existing merge vectors bit-for-bit, proving the
        // `unpremultiply`/`repremultiply` extraction moved no pixel (T7).
        // 1. opaque red over opaque blue, Normal, opacity 1 → red.
        let red = premult(1.0, 0.0, 0.0, 1.0);
        let blue = premult(0.0, 0.0, 1.0, 1.0);
        assert_eq!(
            merge_pixel(red, blue, BlendMode::Normal, 1.0),
            [1.0, 0.0, 0.0, 1.0]
        );
        // 2. opaque white over opaque black, opacity 0.5 → premult 0.5 grey.
        let white = premult(1.0, 1.0, 1.0, 1.0);
        let black = premult(0.0, 0.0, 0.0, 1.0);
        let g = merge_pixel(white, black, BlendMode::Normal, 0.5);
        for c in 0..3 {
            assert!((g[c] - 0.5).abs() < 1e-6, "ch{c} = {}", g[c]);
        }
        assert!((g[3] - 1.0).abs() < 1e-6);
        // 3. transparent top keeps backdrop.
        let clear = premult(0.0, 0.0, 0.0, 0.0);
        let backdrop = premult(0.3, 0.4, 0.5, 1.0);
        let keep = merge_pixel(clear, backdrop, BlendMode::Normal, 1.0);
        assert!((keep[0] - 0.3).abs() < 1e-6);
        assert!((keep[1] - 0.4).abs() < 1e-6);
        assert!((keep[2] - 0.5).abs() < 1e-6);
        assert!((keep[3] - 1.0).abs() < 1e-6);
        // 4. opaque red over transparent → premultiplied red, alpha 1.
        assert_eq!(
            merge_pixel(red, [0.0; 4], BlendMode::Normal, 1.0),
            premult(1.0, 0.0, 0.0, 1.0)
        );
    }

    #[test]
    fn luma_key_drops_darks_and_keeps_brights() {
        // Bright opaque white (luma 1) with threshold 0.5 → fully kept.
        let bright = solid(
            2,
            2,
            LinearColor {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
        );
        let kept = luma_key(&bright, 0.5, 0.1, false);
        for p in &kept.pixels {
            assert!((p[3] - 1.0).abs() < 1e-6, "bright kept, a={}", p[3]);
        }
        // Dark opaque (luma 0) → keyed out (alpha 0, premult rgb 0 too).
        let dark = solid(
            2,
            2,
            LinearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let dropped = luma_key(&dark, 0.5, 0.1, false);
        for p in &dropped.pixels {
            assert!(p[3].abs() < 1e-6, "dark dropped, a={}", p[3]);
        }
        // Invert flips it: the bright pixel is now the one keyed out.
        let inv = luma_key(&bright, 0.5, 0.1, true);
        for p in &inv.pixels {
            assert!(p[3].abs() < 1e-6, "invert drops brights, a={}", p[3]);
        }
    }

    #[test]
    fn chroma_key_drops_the_key_colour_and_keeps_far_colours() {
        // Key on pure green (already linear here). A green pixel → keyed out.
        let key = [0.0, 1.0, 0.0];
        let green = solid(
            2,
            2,
            LinearColor {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let dropped = chroma_key(&green, key, 0.2, 0.1, 0.0);
        for p in &dropped.pixels {
            assert!(p[3].abs() < 1e-6, "green keyed out, a={}", p[3]);
        }
        // A red pixel is far from green → kept (alpha unchanged).
        let red = solid(
            2,
            2,
            LinearColor {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let kept = chroma_key(&red, key, 0.2, 0.1, 0.0);
        for p in &kept.pixels {
            assert!((p[3] - 1.0).abs() < 1e-6, "red kept, a={}", p[3]);
        }
    }

    #[test]
    fn chroma_key_spill_suppress_desaturates_dominant_channel() {
        // A greenish pixel (green dominant) kept but with heavy spill suppression
        // pulls green down toward the mean of red/blue.
        let key = [0.0, 1.0, 0.0];
        let spilled = solid(
            1,
            1,
            LinearColor {
                r: 0.2,
                g: 0.8,
                b: 0.2,
                a: 1.0,
            },
        );
        // Low tolerance: the colour distance to green sits outside the key radius,
        // so the pixel is KEPT — letting the spill-suppression result be observed.
        let out = chroma_key(&spilled, key, 0.1, 0.05, 1.0);
        let p = out.pixels[0];
        // a≈1, so straight ≈ premult. Green should have dropped to ≈ mean(0.2,0.2)=0.2.
        assert!((p[3] - 1.0).abs() < 1e-4, "kept, a={}", p[3]);
        assert!(
            (p[1] - 0.2).abs() < 1e-3,
            "green suppressed to mean, g={}",
            p[1]
        );
        assert!((p[0] - 0.2).abs() < 1e-3, "red untouched, r={}", p[0]);
    }

    // ── Blur / Sharpen / Glow (K-0.2) ────────────────────────────────────────

    #[test]
    fn blur_sigma_below_half_is_identity() {
        let img = solid(
            4,
            4,
            LinearColor {
                r: 0.7,
                g: 0.2,
                b: 0.1,
                a: 1.0,
            },
        );
        let out = blur(&img, 0.4);
        assert_eq!(out.pixels, img.pixels);
    }

    #[test]
    fn blur_plan_small_is_single_pass() {
        let p = blur_plan(2.0);
        assert_eq!(p.iterations, 1);
        assert!((p.sigma_pass - 2.0).abs() < 1e-5);
        assert!((p.step - 1.0).abs() < 1e-5);
    }

    #[test]
    fn blur_plan_large_uses_multi_iter_and_capped_step() {
        let p = blur_plan(48.0);
        assert!(p.iterations >= 2, "iters={}", p.iterations);
        assert!(p.iterations <= 16);
        assert!(p.sigma_pass <= BLUR_MAX_PASS_SIGMA + 1e-3);
        assert!(p.step >= 1.0 && p.step <= BLUR_STEP_MAX + 1e-5);
        // Effective variance: n * σ_pass² ≈ σ²
        let eff = (p.iterations as f32).sqrt() * p.sigma_pass;
        assert!(
            (eff - 48.0).abs() < 0.5,
            "effective sigma {eff} should ≈ 48"
        );
    }

    #[test]
    fn blur_flattens_a_checker_edge() {
        // 2×1: left white, right black → blur mixes them.
        let mut img = Image::new(2, 1);
        img.set(0, 0, [1.0, 1.0, 1.0, 1.0]);
        img.set(1, 0, [0.0, 0.0, 0.0, 1.0]);
        let out = blur(&img, 1.0);
        let left = out.pixel(0, 0);
        let right = out.pixel(1, 0);
        assert!(left[0] < 1.0, "left pulled down by right neighbour");
        assert!(right[0] > 0.0, "right pulled up by left neighbour");
        // Alpha stays ~1 (both inputs opaque).
        assert!((left[3] - 1.0).abs() < 1e-4);
        assert!((right[3] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn blur_large_sigma_mixes_far_pixels() {
        // 16×1: left white, right black. Large σ must pull the left edge down.
        let mut img = Image::new(16, 1);
        for x in 0..8 {
            img.set(x, 0, [1.0, 1.0, 1.0, 1.0]);
        }
        for x in 8..16 {
            img.set(x, 0, [0.0, 0.0, 0.0, 1.0]);
        }
        let out = blur(&img, 24.0);
        let left = out.pixel(0, 0);
        assert!(
            left[0] < 0.95,
            "large blur should soften far-left, got r={}",
            left[0]
        );
    }

    #[test]
    fn feather_coverage_identity_below_half() {
        let mut img = Image::new(8, 8);
        for y in 2..6 {
            for x in 2..6 {
                img.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let out = feather_coverage(&img, 0.4);
        assert_eq!(out.pixels, img.pixels);
    }

    #[test]
    fn feather_coverage_softens_edge() {
        let mut img = Image::new(16, 16);
        for y in 4..12 {
            for x in 4..12 {
                img.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let out = feather_coverage(&img, 4.0);
        // Deep interior still solid.
        let c = out.pixel(8, 8);
        assert!(c[3] > 0.9, "interior alpha={}", c[3]);
        // Far exterior still empty.
        let e = out.pixel(0, 0);
        assert!(e[3] < 0.1, "exterior alpha={}", e[3]);
        // Edge band is soft (not binary).
        let edge = out.pixel(4, 8);
        assert!(
            edge[3] > 0.05 && edge[3] < 0.95,
            "edge should be soft, alpha={}",
            edge[3]
        );
    }

    #[test]
    fn sharpen_zero_amount_is_identity() {
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
        assert_eq!(sharpen(&img, 0.0, 2.0).pixels, img.pixels);
    }

    #[test]
    fn sharpen_boosts_contrast_at_an_edge() {
        let mut img = Image::new(4, 1);
        for x in 0..2 {
            img.set(x, 0, [0.0, 0.0, 0.0, 1.0]);
        }
        for x in 2..4 {
            img.set(x, 0, [1.0, 1.0, 1.0, 1.0]);
        }
        let out = sharpen(&img, 1.0, 1.0);
        // The bright side of the edge should be at least as bright as the source
        // (unsharp boost); the dark side at most as bright.
        assert!(out.pixel(3, 0)[0] >= img.pixel(3, 0)[0] - 1e-5);
        assert!(out.pixel(0, 0)[0] <= img.pixel(0, 0)[0] + 1e-5);
    }

    #[test]
    fn glow_zero_intensity_is_identity() {
        let img = solid(
            4,
            4,
            LinearColor {
                r: 0.9,
                g: 0.9,
                b: 0.9,
                a: 1.0,
            },
        );
        assert_eq!(
            glow(&img, 2.0, 0.0, 0.0, [1.0, 1.0, 1.0]).pixels,
            img.pixels
        );
    }

    #[test]
    fn mask_shape_is_opaque_inside_and_transparent_outside() {
        // A centered ellipse covering ~half the frame: the center is opaque, the
        // far corner is outside → transparent.
        let m = mask_shape(32, 32, [0.5, 0.5], [0.3, 0.3], 0.0, 0.05);
        let center = m.pixel(16, 16);
        assert!(
            (center[3] - 1.0).abs() < 1e-4,
            "center opaque, a={}",
            center[3]
        );
        let corner = m.pixel(0, 0);
        assert!(
            corner[3].abs() < 1e-4,
            "corner transparent, a={}",
            corner[3]
        );
        // Premultiplied white inside: rgb == alpha.
        for c in 0..3 {
            assert!((center[c] - center[3]).abs() < 1e-6, "premult white ch{c}");
        }
    }

    #[test]
    fn bilinear_sample_midpoint_averages() {
        // A 2×1 image: black left, white right. Sampling the seam averages.
        let mut img = Image::new(2, 1);
        img.pixels[0] = [0.0, 0.0, 0.0, 1.0];
        img.pixels[1] = [1.0, 1.0, 1.0, 1.0];
        let mid = img.sample_bilinear(1.0, 0.5); // pixel boundary between the two
        #[allow(clippy::needless_range_loop)]
        for c in 0..3 {
            assert!((mid[c] - 0.5).abs() < 1e-6, "channel {c} = {}", mid[c]);
        }
    }

    // ── Directional wipe / push transitions (K-0.4) ──────────────────────────
    const RED: LinearColor = LinearColor {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    const BLUE: LinearColor = LinearColor {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };
    const DIRS: [WipeDirection; 4] = [
        WipeDirection::LeftToRight,
        WipeDirection::RightToLeft,
        WipeDirection::TopToBottom,
        WipeDirection::BottomToTop,
    ];

    /// K-B7: luma-wipe endpoints are bit-exact even with softness.
    #[test]
    fn luma_wipe_endpoints_are_exact() {
        let incoming = solid(4, 4, RED);
        let outgoing = solid(4, 4, BLUE);
        let out0 = luma_wipe(
            &incoming,
            &outgoing,
            crate::graph::luma_wipe::LumaWipeKind::LinearH,
            0.1,
            false,
            0.0,
        );
        let out1 = luma_wipe(
            &incoming,
            &outgoing,
            crate::graph::luma_wipe::LumaWipeKind::LinearH,
            0.1,
            false,
            1.0,
        );
        assert_eq!(out0.pixels, outgoing.pixels, "t=0 == outgoing");
        assert_eq!(out1.pixels, incoming.pixels, "t=1 == incoming");
    }

    /// The wipe endpoints are bit-exact for every direction (even with softness):
    /// `t == 0` is the outgoing frame, `t == 1` the incoming.
    #[test]
    fn wipe_endpoints_are_exact_for_every_direction() {
        let incoming = solid(8, 8, RED);
        let outgoing = solid(8, 8, BLUE);
        for dir in DIRS {
            let at0 = wipe(&incoming, &outgoing, dir, 0.25, 0.0);
            assert_eq!(at0.pixels, outgoing.pixels, "t=0 == outgoing ({dir:?})");
            let at1 = wipe(&incoming, &outgoing, dir, 0.25, 1.0);
            assert_eq!(at1.pixels, incoming.pixels, "t=1 == incoming ({dir:?})");
        }
    }

    /// A hard (softness 0) left→right wipe at `t = 0.5` splits an 8-wide row: the
    /// edge sits at `p = 0.5` (x = 3.5), so x < 4 shows the incoming and x ≥ 4 the
    /// outgoing.
    #[test]
    fn wipe_midpoint_boundary_splits_incoming_and_outgoing() {
        let incoming = solid(8, 1, RED);
        let outgoing = solid(8, 1, BLUE);
        let out = wipe(&incoming, &outgoing, WipeDirection::LeftToRight, 0.0, 0.5);
        assert_eq!(
            out.pixel(0, 0),
            [1.0, 0.0, 0.0, 1.0],
            "left edge is incoming"
        );
        assert_eq!(
            out.pixel(7, 0),
            [0.0, 0.0, 1.0, 1.0],
            "right edge is outgoing"
        );
    }

    /// The push endpoints are bit-exact for every direction: `t == 0` is outgoing,
    /// `t == 1` incoming.
    #[test]
    fn push_endpoints_are_exact_for_every_direction() {
        let incoming = solid(8, 8, RED);
        let outgoing = solid(8, 8, BLUE);
        for dir in DIRS {
            let at0 = push(&incoming, &outgoing, dir, 0.0);
            assert_eq!(at0.pixels, outgoing.pixels, "t=0 == outgoing ({dir:?})");
            let at1 = push(&incoming, &outgoing, dir, 1.0);
            assert_eq!(at1.pixels, incoming.pixels, "t=1 == incoming ({dir:?})");
        }
    }

    /// A left→right push at `t = 0.5` shows the incoming trailing into the left of
    /// the row and the outgoing leading out the right (boundary at u = 0.5).
    #[test]
    fn push_midpoint_boundary_splits_incoming_and_outgoing() {
        let incoming = solid(8, 1, RED);
        let outgoing = solid(8, 1, BLUE);
        let out = push(&incoming, &outgoing, WipeDirection::LeftToRight, 0.5);
        assert_eq!(
            out.pixel(0, 0),
            [1.0, 0.0, 0.0, 1.0],
            "left edge is incoming"
        );
        assert_eq!(
            out.pixel(7, 0),
            [0.0, 0.0, 1.0, 1.0],
            "right edge is outgoing"
        );
    }
}

#[cfg(test)]
mod deinterlace_tests {
    use super::*;
    use crate::graph::ir::{DeinterlaceMethod, FieldOrder};

    /// Build a 4×4 comb pattern: even rows white, odd rows black.
    fn comb_frame() -> Image {
        let mut img = Image::new(4, 4);
        for y in 0..4u32 {
            for x in 0..4u32 {
                let v = if y % 2 == 0 {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    [0.0, 0.0, 0.0, 1.0]
                };
                img.set(x, y, v);
            }
        }
        img
    }

    #[test]
    fn linear_blend_softens_comb() {
        let src = comb_frame();
        let out = deinterlace(&src, DeinterlaceMethod::LinearBlend, FieldOrder::TopFirst);
        // Middle of an odd row should no longer be pure black.
        let mid = out.pixel(2, 1);
        assert!(mid[0] > 0.1, "blend should lift odd-row black: {mid:?}");
        assert!(mid[0] < 0.95, "blend should not be pure white: {mid:?}");
    }

    #[test]
    fn one_field_keeps_even_rows() {
        let src = comb_frame();
        let out = deinterlace(&src, DeinterlaceMethod::OneField, FieldOrder::TopFirst);
        assert_eq!(out.pixel(1, 0), [1.0, 1.0, 1.0, 1.0]);
        // Odd row reconstructed from even neighbours → white.
        let o = out.pixel(1, 1);
        assert!(
            (o[0] - 1.0).abs() < 1e-5,
            "odd row should match field: {o:?}"
        );
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    #[test]
    fn crop_removes_normalized_margins_without_resizing_canvas() {
        let mut source = Image::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                source.set(x, y, [x as f32, y as f32, 0.0, 1.0]);
            }
        }

        let output = crop(&source, 4, 4, 0.25, 0.25, 0.25, 0.25);

        assert_eq!(output.width, 4);
        assert_eq!(output.height, 4);
        assert_eq!(output.pixel(0, 0), [0.0; 4]);
        assert_eq!(output.pixel(1, 1), [1.0, 1.0, 0.0, 1.0]);
        assert_eq!(output.pixel(2, 2), [2.0, 2.0, 0.0, 1.0]);
        assert_eq!(output.pixel(3, 3), [0.0; 4]);
    }

    #[test]
    fn fit_adds_transparent_letterbox_bars() {
        let source = Image::filled(
            4,
            2,
            LinearColor {
                r: 0.8,
                g: 0.2,
                b: 0.1,
                a: 1.0,
            },
        );
        let output = resize(&source, 4, 4, FitMode::Fit);

        assert_eq!(output.pixel(1, 0), [0.0; 4], "top bar must be transparent");
        assert_eq!(
            output.pixel(1, 3),
            [0.0; 4],
            "bottom bar must be transparent"
        );
        assert_eq!(output.pixel(1, 1), [0.8, 0.2, 0.1, 1.0]);
        assert_eq!(output.pixel(1, 2), [0.8, 0.2, 0.1, 1.0]);
    }

    #[test]
    fn fill_crops_the_overflow_from_the_center() {
        let mut source = Image::new(4, 2);
        for y in 0..source.height {
            for x in 0..source.width {
                source.set(x, y, [x as f32, y as f32, 0.0, 1.0]);
            }
        }
        let output = resize(&source, 2, 2, FitMode::Fill);

        // A 4×2 -> 2×2 cover scale is 1, so the output coordinates are 1.5
        // and 2.5 in source space. With pixel centers at n + 0.5, those land
        // on source columns 1 and 2: the outer columns are cropped, not stretched in.
        assert_eq!(output.pixel(0, 0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(output.pixel(1, 0), [2.0, 0.0, 0.0, 1.0]);
    }
}

#[cfg(test)]
mod native_luminance_tests {
    use super::*;
    #[test]
    fn ap1_whole_color_selection_differs_from_legacy_luma_and_keeps_cpu_ties() {
        let mut top = Image::new(1, 1);
        let mut bottom = Image::new(1, 1);
        top.pixels[0] = [0.0, 0.0, 1.0, 1.0];
        bottom.pixels[0] = [0.25, 0.0, 0.0, 1.0];
        for mode in [BlendMode::DarkerColor, BlendMode::LighterColor] {
            let native = native_luminance_merge(&top, &bottom, mode, 1.0).pixels[0];
            let legacy = merge(&top, &bottom, mode, 1.0).pixels[0];
            let (expected, old) = if mode == BlendMode::DarkerColor {
                (top.pixels[0], bottom.pixels[0])
            } else {
                (bottom.pixels[0], top.pixels[0])
            };
            assert_eq!(native, expected);
            assert_eq!(legacy, old);
        }
        // Commuting equal f32 products produce an exact CPU Y tie for different RGBs.
        // GPU half quantization can separate this particular pair, so it is a CPU tie test.
        top.pixels[0] = [0.67408174, 0.0, 0.0, 1.0];
        bottom.pixels[0] = [0.0, 0.27222872, 0.0, 1.0];
        for mode in [BlendMode::DarkerColor, BlendMode::LighterColor] {
            assert_eq!(
                native_luminance_merge(&top, &bottom, mode, 1.0).pixels[0],
                bottom.pixels[0]
            );
        }
    }
}

/// Native scene-linear radial gain. No display-space conversion or RGB clamp.
/// Parameters are validated by the IR boundary; alpha is never modified.
pub fn native_vignette(input: &Image, amount: f32, feather: f32) -> Image {
    let mut out = input.clone();
    let cx = (input.width as f32 - 1.0) * 0.5;
    let cy = (input.height as f32 - 1.0) * 0.5;
    let max_distance = (cx * cx + cy * cy).sqrt().max(1e-4);
    let span = feather.max(1e-4);
    let inner = 1.0 - span;
    for y in 0..input.height {
        for x in 0..input.width {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d = (dx * dx + dy * dy).sqrt() / max_distance;
            let t = ((d - inner) / span).clamp(0.0, 1.0);
            let gain = (1.0 + amount * t * t * (3.0 - 2.0 * t)).max(0.0);
            let pixel = &mut out.pixels[(y * input.width + x) as usize];
            for channel in &mut pixel[..3] {
                *channel *= gain;
            }
        }
    }
    out
}

/// Stable integer noise. This is a creative uniform grain, not sensor or film emulation.
pub fn native_grain(
    input: &Image,
    amount: f32,
    monochrome: bool,
    seed: u32,
    pixel_scale: [f32; 2],
) -> Image {
    let mut out = input.clone();
    if amount == 0.0 {
        return out;
    }
    for y in 0..input.height {
        for x in 0..input.width {
            let ax = ((x as f32 + 0.5) / pixel_scale[0]).floor() as u32;
            let ay = ((y as f32 + 0.5) / pixel_scale[1]).floor() as u32;
            let p = &mut out.pixels[(y * input.width + x) as usize];
            for c in 0..3 {
                let mut h = seed
                    ^ 0xa511e9b3
                    ^ ax.wrapping_mul(0x9e3779b9)
                    ^ ay.wrapping_mul(0x85ebca6b)
                    ^ (if monochrome { 0u32 } else { c as u32 }).wrapping_mul(0xc2b2ae35);
                h ^= h >> 16;
                h = h.wrapping_mul(0x7feb352d);
                h ^= h >> 15;
                h = h.wrapping_mul(0x846ca68b);
                h ^= h >> 16;
                let n = (h >> 8) as f32 * (2.0 / 16_777_215.0) - 1.0;
                p[c] += n * amount * p[3];
            }
        }
    }
    out
}

/// Alpha-aware scene-linear bilateral denoise, with a bounded authored radius.
/// Range distances use straight AP1; accumulation is coverage-weighted.
pub fn native_denoise(input: &Image, strength: f32, spatial_scale: f32) -> Image {
    let mut out = input.clone();
    if strength == 0.0 {
        return out;
    }
    let radius = (2.0 * spatial_scale).ceil().clamp(1.0, 8.0) as i32;
    let sigma_s = ((1.5 + 1.5 * strength) * spatial_scale).max(1e-4);
    let sigma_r = (10.0 + 70.0 * strength) / 255.0;
    for y in 0..input.height {
        for x in 0..input.width {
            let center = input.pixel(x, y);
            if center[3] <= 0.0 {
                out.set(x, y, [0.0; 4]);
                continue;
            }
            let rgb = [
                center[0] / center[3],
                center[1] / center[3],
                center[2] / center[3],
            ];
            let mut acc = [0.0f32; 3];
            let mut coverage = 0.0f32;
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let sample = input.pixel(
                        (x as i32 + dx).clamp(0, input.width as i32 - 1) as u32,
                        (y as i32 + dy).clamp(0, input.height as i32 - 1) as u32,
                    );
                    if sample[3] <= 0.0 {
                        continue;
                    }
                    let mut dist = 0.0;
                    for c in 0..3 {
                        let d = sample[c] / sample[3] - rgb[c];
                        dist += d * d;
                    }
                    let weight = (-(dx * dx + dy * dy) as f32 / (2.0 * sigma_s * sigma_s)
                        - dist / (2.0 * sigma_r * sigma_r))
                        .exp();
                    for c in 0..3 {
                        acc[c] += sample[c] * weight;
                    }
                    coverage += sample[3] * weight;
                }
            }
            if coverage > 0.0 {
                out.set(
                    x,
                    y,
                    [
                        acc[0] / coverage * center[3],
                        acc[1] / coverage * center[3],
                        acc[2] / coverage * center[3],
                        center[3],
                    ],
                );
            }
        }
    }
    out
}

/// Bounded static polygon payload accepted by the managed graph.
pub fn native_polygon_vertices_valid(vertices: &[[f32; 2]]) -> bool {
    photonic_core::timeline::MaskShapeKind::validate_vertices(vertices).is_ok()
}

/// Independent f64 polygon reference. Vertices are normalized within the shape's
/// local box; center/size have the same center/half-extent convention as ellipse
/// and rectangle. Even-odd fill supports concave and self-intersecting paths.
/// Feather is an inward distance in local half-extent units, never an expansion.
#[allow(clippy::too_many_arguments)]
pub fn native_polygon_mask(
    width: u32,
    height: u32,
    vertices: &[[f32; 2]],
    center: [f32; 2],
    size: [f32; 2],
    rotation: f32,
    feather: f32,
) -> Image {
    let (width, height) = (width.max(1), height.max(1));
    let mut out = Image::new(width, height);
    if !native_polygon_vertices_valid(vertices)
        || size.iter().any(|v| !v.is_finite() || *v <= ALPHA_EPS)
    {
        return out;
    }
    let (sin, cos) = (-(rotation as f64)).sin_cos();
    for y in 0..height {
        for x in 0..width {
            let dx = (x as f64 + 0.5) / width as f64 - center[0] as f64;
            let dy = (y as f64 + 0.5) / height as f64 - center[1] as f64;
            let p = [
                (dx * cos - dy * sin) / size[0] as f64,
                (dx * sin + dy * cos) / size[1] as f64,
            ];
            let mut inside = false;
            let mut distance = f64::INFINITY;
            for i in 0..vertices.len() {
                let a = vertices[i].map(|v| v as f64 * 2.0 - 1.0);
                let b = vertices[(i + 1) % vertices.len()].map(|v| v as f64 * 2.0 - 1.0);
                let edge = [b[0] - a[0], b[1] - a[1]];
                let d = [p[0] - a[0], p[1] - a[1]];
                let len = edge[0] * edge[0] + edge[1] * edge[1];
                let t = if len > 0.0 {
                    ((d[0] * edge[0] + d[1] * edge[1]) / len).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                distance = distance.min((d[0] - t * edge[0]).hypot(d[1] - t * edge[1]));
                if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) * edge[0] / edge[1]
                {
                    inside = !inside;
                }
            }
            let alpha = if inside {
                if feather <= 0.0 {
                    1.0
                } else {
                    let t = (distance / feather as f64).clamp(0.0, 1.0);
                    t * t * (3.0 - 2.0 * t)
                }
            } else {
                0.0
            };
            out.set(x, y, [alpha as f32; 4]);
        }
    }
    out
}

/// Centered line convolution of premultiplied AP1 RGBA. At most 129 bilinear
/// samples span the entire requested displacement; no channel is clipped.
/// IR validation supplies finite bounded authored displacement components.
pub fn native_motion_blur(input: &Image, delta: [f32; 2]) -> Image {
    if input.width == 0 || input.height == 0 || delta == [0.0, 0.0] {
        return input.clone();
    }
    let steps = native_motion_blur_steps(delta);
    let mut out = input.clone();
    for y in 0..input.height {
        for x in 0..input.width {
            let mut sum = [0.0f32; 4];
            for i in 0..=steps {
                let t = i as f32 / steps as f32 - 0.5;
                let sx = (x as f32 + delta[0] * t).clamp(0.0, (input.width - 1) as f32);
                let sy = (y as f32 + delta[1] * t).clamp(0.0, (input.height - 1) as f32);
                let x0 = sx.floor() as u32;
                let y0 = sy.floor() as u32;
                let x1 = (x0 + 1).min(input.width - 1);
                let y1 = (y0 + 1).min(input.height - 1);
                let fx = sx - x0 as f32;
                let fy = sy - y0 as f32;
                let a = input.pixel(x0, y0);
                let b = input.pixel(x1, y0);
                let c = input.pixel(x0, y1);
                let d = input.pixel(x1, y1);
                for k in 0..4 {
                    let top = a[k] * (1.0 - fx) + b[k] * fx;
                    let bottom = c[k] * (1.0 - fx) + d[k] * fx;
                    sum[k] += top * (1.0 - fy) + bottom * fy;
                }
            }
            out.set(x, y, sum.map(|v| v / (steps + 1) as f32));
        }
    }
    out
}

/// Resolve sampling density on the CPU so f32 GPU square-root rounding cannot
/// change the discrete sample count at an integer footprint boundary.
pub(crate) fn native_motion_blur_steps(delta: [f32; 2]) -> u32 {
    f64::from(delta[0])
        .hypot(f64::from(delta[1]))
        .ceil()
        .clamp(1.0, 128.0) as u32
}

/// Average each logical cell once, then replicate its premultiplied AP1 mean.
/// Partial edge cells exclude pixels outside the image. No range clipping.
pub fn native_mosaic(input: &Image, block: [u32; 2]) -> Image {
    let [bw, bh] = block.map(|v| v.max(1));
    let mut out = input.clone();
    for by in (0..input.height).step_by(bh as usize) {
        for bx in (0..input.width).step_by(bw as usize) {
            let ex = bx.saturating_add(bw).min(input.width);
            let ey = by.saturating_add(bh).min(input.height);
            let mut sum = [0.0f64; 4];
            for y in by..ey {
                for x in bx..ex {
                    let p = input.pixel(x, y);
                    for c in 0..4 {
                        sum[c] += f64::from(p[c]);
                    }
                }
            }
            let n = f64::from((ex - bx) * (ey - by));
            let mean = sum.map(|v| (v / n) as f32);
            for y in by..ey {
                for x in bx..ex {
                    out.set(x, y, mean);
                }
            }
        }
    }
    out
}

/// Sliding-window equal-weight convolution, with clamped logical edges.
/// Accumulate in f64; preserve signed/HDR premultiplied channels and coverage.
pub fn native_box_blur(input: &Image, radius: [u32; 2]) -> Image {
    let mut source = input.clone();
    for (axis, radius) in radius.into_iter().enumerate() {
        if radius == 0 {
            continue;
        }
        let radius = radius.min(500) as i64;
        let (length, lines) = if axis == 0 {
            (input.width, input.height)
        } else {
            (input.height, input.width)
        };
        let mut out = source.clone();
        let at = |position: i64, line: u32| {
            let p = position.clamp(0, i64::from(length) - 1) as u32;
            if axis == 0 {
                source.pixel(p, line)
            } else {
                source.pixel(line, p)
            }
        };
        for line in 0..lines {
            let mut sum = [0.0f64; 4];
            for offset in -radius..=radius {
                let p = at(offset, line);
                for c in 0..4 {
                    sum[c] += f64::from(p[c]);
                }
            }
            for position in 0..length {
                let mean = sum.map(|v| (v / (2 * radius + 1) as f64) as f32);
                if axis == 0 {
                    out.set(position, line, mean)
                } else {
                    out.set(line, position, mean)
                }
                let remove = at(i64::from(position) - radius, line);
                let add = at(i64::from(position) + radius + 1, line);
                for c in 0..4 {
                    sum[c] += f64::from(add[c]) - f64::from(remove[c]);
                }
            }
        }
        source = out;
    }
    source
}

/// Fixed-canvas geometric inverse sampling with authored-pixel centers.
pub fn native_geometric_warp(
    input: &Image,
    kind: super::ir::NativeWarpKind,
    amount: f32,
    wavelength: f32,
    pixel_scale: [f32; 2],
) -> Image {
    use super::ir::NativeWarpKind;
    if amount == 0.0 || input.width == 0 || input.height == 0 {
        return input.clone();
    }
    let scale = Vec2::from_array(pixel_scale);
    let size = Vec2::new(input.width as f32, input.height as f32) / scale;
    let center = size * 0.5;
    let radius = size.min_element() * 0.5;
    let mut out = input.clone();
    for y in 0..input.height {
        for x in 0..input.width {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / scale;
            let mut q = p;
            if kind == NativeWarpKind::Ripple {
                let phase = std::f32::consts::TAU;
                q += amount
                    * Vec2::new(
                        (phase * (p.y / wavelength).fract()).sin(),
                        (phase * (p.x / wavelength).fract()).sin(),
                    );
            } else {
                let delta = p - center;
                let distance = delta.length();
                if distance > 0.0 && distance < radius {
                    let n = distance / radius;
                    let factor = if kind == NativeWarpKind::Pinch {
                        1.0 + amount * (1.0 - n).powi(2)
                    } else if amount >= 0.0 {
                        1.0 + amount * (n.asin() / std::f32::consts::FRAC_PI_2 / n - 1.0)
                    } else {
                        1.0 + (-amount) * ((n * std::f32::consts::FRAC_PI_2).sin() / n - 1.0)
                    };
                    q = center + delta * factor;
                }
            }
            let sample = q * scale;
            out.set(x, y, input.sample_bilinear(sample.x, sample.y));
        }
    }
    out
}

/// Resolve authored-scale Gaussian radius, including Clarity's canvas-relative size.
pub(crate) fn native_detail_sigma(
    kind: super::ir::NativeDetailKind,
    sigma: f32,
    pixel_scale: [f32; 2],
    w: u32,
    h: u32,
) -> f32 {
    let base = if kind == super::ir::NativeDetailKind::Clarity {
        ((w as f32 / pixel_scale[0]).min(h as f32 / pixel_scale[1]) / 8.0).clamp(3.0, 60.0)
    } else {
        sigma
    };
    base * pixel_scale[0].min(pixel_scale[1])
}
pub fn native_detail(
    input: &Image,
    kind: super::ir::NativeDetailKind,
    amount: f32,
    sigma: f32,
    threshold: f32,
    pixel_scale: [f32; 2],
) -> Image {
    use super::ir::NativeDetailKind;
    let sigma = native_detail_sigma(kind, sigma, pixel_scale, input.width, input.height);
    if kind != NativeDetailKind::HighPass && (amount == 0.0 || sigma < 0.5) {
        return input.clone();
    }
    let blurred = blur(input, sigma);
    let mut out = input.clone();
    for ((dst, src), low) in out
        .pixels
        .iter_mut()
        .zip(&input.pixels)
        .zip(&blurred.pixels)
    {
        if src[3] <= 0.0 {
            *dst = [0.0; 4];
            continue;
        }
        let straight = [src[0] / src[3], src[1] / src[3], src[2] / src[3]];
        let luma = straight[0] * 0.27222872 + straight[1] * 0.67408174 + straight[2] * 0.053689517;
        let bell = (1.0 - (luma - 0.5).abs() * 2.0).clamp(0.0, 1.0);
        for c in 0..3 {
            let diff = if low[3] > 1e-8 {
                straight[c] - low[c] / low[3]
            } else {
                0.0
            };
            let value = match kind {
                NativeDetailKind::HighPass => 128.0 / 255.0 + diff,
                NativeDetailKind::Clarity => straight[c] + amount * bell * diff,
                NativeDetailKind::SmartSharpen => {
                    straight[c]
                        + amount
                            * ((diff.abs() - threshold) / (32.0 / 255.0)).clamp(0.0, 1.0)
                            * diff
                }
            };
            dst[c] = value * src[3];
        }
        dst[3] = src[3];
    }
    out
}

/// AP1 luminance relief/edges and coverage-aware radial channel displacement.
pub fn native_stylize(
    input: &Image,
    kind: super::ir::NativeStylizeKind,
    amount: f32,
    pixel_scale: [f32; 2],
) -> Image {
    use super::ir::NativeStylizeKind;
    if kind == NativeStylizeKind::ChromaticAberration && amount == 0.0 {
        return input.clone();
    }
    let scale = Vec2::from_array(pixel_scale);
    let size = Vec2::new(input.width as f32, input.height as f32) / scale;
    let center = size * 0.5;
    let max_dist = ((size.x - 1.0) * 0.5)
        .hypot((size.y - 1.0) * 0.5)
        .max(0.0001);
    let sample = |p: Vec2| {
        let c = input.sample_bilinear(p.x, p.y);
        if c[3] > 1e-8 {
            [c[0] / c[3], c[1] / c[3], c[2] / c[3]]
        } else {
            [0.0; 3]
        }
    };
    let mut out = input.clone();
    for y in 0..input.height {
        for x in 0..input.width {
            let src = input.pixel(x, y);
            if src[3] <= 0.0 {
                out.set(x, y, [0.0; 4]);
                continue;
            }
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if kind == NativeStylizeKind::ChromaticAberration {
                let authored = p / scale;
                let offset = (authored - center) * (amount / max_dist);
                let red = sample((authored + offset) * scale);
                let blue = sample((authored - offset) * scale);
                out.set(x, y, [red[0] * src[3], src[1], blue[2] * src[3], src[3]]);
            } else {
                let mut gx = 0.0;
                let mut gy = 0.0;
                let emboss = [[-1.0, -1.0, 0.0], [-1.0, 0.0, 1.0], [0.0, 1.0, 1.0]];
                let sobel_x = [[-1.0, 0.0, 1.0], [-2.0, 0.0, 2.0], [-1.0, 0.0, 1.0]];
                let sobel_y = [[-1.0, -2.0, -1.0], [0.0, 0.0, 0.0], [1.0, 2.0, 1.0]];
                for ky in 0..3 {
                    for kx in 0..3 {
                        let q = p + Vec2::new(kx as f32 - 1.0, ky as f32 - 1.0) * scale;
                        let c = sample(q);
                        let l = c[0] * 0.27222872 + c[1] * 0.67408174 + c[2] * 0.053689517;
                        if kind == NativeStylizeKind::Emboss {
                            gx += l * emboss[ky][kx];
                        } else {
                            gx += l * sobel_x[ky][kx];
                            gy += l * sobel_y[ky][kx];
                        }
                    }
                }
                let gray = if kind == NativeStylizeKind::Emboss {
                    128.0 / 255.0 + gx
                } else {
                    gx.hypot(gy)
                };
                out.set(x, y, [gray * src[3], gray * src[3], gray * src[3], src[3]]);
            }
        }
    }
    out
}

/// Inverse projective map of a bounded convex normalized destination quadrilateral.
/// Solve in binary64 with partial pivoting; reject unstable maps rather than fallback.
pub fn native_perspective_inverse(corners: [f32; 8]) -> Option<[f32; 9]> {
    if corners
        .iter()
        .any(|v| !v.is_finite() || !(-1.0..=2.0).contains(v))
    {
        return None;
    }
    let pts: [[f64; 2]; 4] =
        std::array::from_fn(|i| [corners[i * 2] as f64, corners[i * 2 + 1] as f64]);
    let mut sign = 0.0f64;
    for i in 0..4 {
        let a = pts[i];
        let b = pts[(i + 1) % 4];
        let c = pts[(i + 2) % 4];
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        if cross.abs() < 1e-6 {
            return None;
        }
        if i == 0 {
            sign = cross.signum();
        } else if sign != cross.signum() {
            return None;
        }
    }
    let target = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let mut a = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let [x, y] = pts[i];
        let [u, v] = target[i];
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -x * u, -y * u, u];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -x * v, -y * v, v];
    }
    for col in 0..8 {
        let pivot = (col..8).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-10 {
            return None;
        }
        a.swap(col, pivot);
        let divisor = a[col][col];
        for k in col..9 {
            a[col][k] /= divisor;
        }
        for row in 0..8 {
            if row == col {
                continue;
            }
            let factor = a[row][col];
            for k in col..9 {
                a[row][k] -= factor * a[col][k];
            }
        }
    }
    let mut out = [0.0f32; 9];
    for i in 0..8 {
        out[i] = a[i][8] as f32;
    }
    out[8] = 1.0;
    if !native_perspective_matrix_valid(&out) {
        return None;
    }
    let denominators = pts.map(|[x, y]| out[6] as f64 * x + out[7] as f64 * y + out[8] as f64);
    if denominators
        .iter()
        .any(|v| v.abs() < 1e-6 || v.signum() != denominators[0].signum())
    {
        return None;
    }
    // Verify the f32 map still reproduces its four corner correspondences.
    for (i, [x, y]) in pts.into_iter().enumerate() {
        let d = denominators[i];
        let u = (out[0] as f64 * x + out[1] as f64 * y + out[2] as f64) / d;
        let v = (out[3] as f64 * x + out[4] as f64 * y + out[5] as f64) / d;
        if (u - target[i][0]).abs() > 1e-4 || (v - target[i][1]).abs() > 1e-4 {
            return None;
        }
    }
    Some(out)
}
pub fn native_perspective_matrix_valid(h: &[f32; 9]) -> bool {
    if h.iter().any(|v| !v.is_finite() || v.abs() > 1e6) {
        return false;
    }
    let h = h.map(f64::from);
    let determinant = h[0] * (h[4] * h[8] - h[5] * h[7]) - h[1] * (h[3] * h[8] - h[5] * h[6])
        + h[2] * (h[3] * h[7] - h[4] * h[6]);
    determinant.abs() > 1e-8
}
/// True homography, fixed canvas and transparent exterior; colors stay premultiplied.
pub fn native_perspective(input: &Image, h: &[f32; 9]) -> Image {
    if *h == [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        return input.clone();
    }
    let mut out = Image::new(input.width, input.height);
    if !native_perspective_matrix_valid(h) {
        return out;
    }
    for y in 0..input.height {
        for x in 0..input.width {
            let px = (x as f32 + 0.5) / input.width as f32;
            let py = (y as f32 + 0.5) / input.height as f32;
            let d = h[6] * px + h[7] * py + h[8];
            if d.abs() < 1e-6 {
                continue;
            }
            let u = (h[0] * px + h[1] * py + h[2]) / d;
            let v = (h[3] * px + h[4] * py + h[5]) / d;
            if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                out.set(
                    x,
                    y,
                    input.sample_bilinear(u * input.width as f32, v * input.height as f32),
                );
            }
        }
    }
    out
}

/// The qualified native box kernel bounds evaluation radius to 500 pixels.
pub fn native_shadow_radius(radius: u32, pixel_scale: [f32; 2]) -> [u32; 2] {
    pixel_scale.map(|scale| (radius as f32 * scale).round().clamp(0.0, 500.0) as u32)
}
/// Alpha shift and equal-weight blur, then original over the tinted AP1 shadow.
pub fn native_drop_shadow(
    input: &Image,
    offset: [f32; 2],
    radius: u32,
    color: [f32; 3],
    opacity: f32,
    pixel_scale: [f32; 2],
) -> Image {
    if opacity == 0.0 {
        return input.clone();
    }
    let mut alpha = Image::new(input.width, input.height);
    for y in 0..input.height {
        for x in 0..input.width {
            let px =
                (x as f32 + 0.5 - offset[0] * pixel_scale[0]).clamp(0.5, input.width as f32 - 0.5);
            let py =
                (y as f32 + 0.5 - offset[1] * pixel_scale[1]).clamp(0.5, input.height as f32 - 0.5);
            alpha.set(x, y, [0.0, 0.0, 0.0, input.sample_bilinear(px, py)[3]]);
        }
    }
    let alpha = native_box_blur(&alpha, native_shadow_radius(radius, pixel_scale));
    let mut out = input.clone();
    for (i, c) in input.pixels.iter().enumerate() {
        let a = alpha.pixels[i][3] * opacity * (1.0 - c[3]);
        out.pixels[i] = [
            c[0] + color[0] * a,
            c[1] + color[1] * a,
            c[2] + color[2] * a,
            c[3] + a,
        ];
    }
    out
}

/// Encoded-space creative kernels share raster math. Only Posterize/Threshold clip.
pub fn native_transfer_artistic(
    input: &Image,
    kind: super::ir::NativeTransferArtisticKind,
    parameter: f32,
) -> Image {
    use super::ir::NativeTransferArtisticKind;
    let to_srgb = crate::color::native::ap1_to_linear_srgb();
    let to_ap1 = crate::color::native::linear_srgb_to_ap1();
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        if p[3] <= 0.0 {
            out.pixels[i] = [0.0; 4];
            continue;
        }
        let straight = [
            p[0] as f64 / p[3] as f64,
            p[1] as f64 / p[3] as f64,
            p[2] as f64 / p[3] as f64,
        ];
        let linear = if straight[0] == straight[1] && straight[1] == straight[2] {
            straight
        } else {
            to_srgb.transform(straight)
        };
        if kind == NativeTransferArtisticKind::Desaturate
            && straight[0] == straight[1]
            && straight[1] == straight[2]
        {
            out.pixels[i] = *p;
            continue;
        }
        let mut rgb = linear.map(|v| {
            let v = if matches!(
                kind,
                NativeTransferArtisticKind::Desaturate | NativeTransferArtisticKind::Invert
            ) {
                v
            } else {
                v.clamp(0.0, 1.0)
            };
            if v == 1.0 {
                1.0
            } else {
                crate::color::native::encode_srgb(v)
            }
        });
        match kind {
            NativeTransferArtisticKind::Posterize => {
                rgb = rgb.map(|v| {
                    photonic_core::raster::adjust::posterize_channel(v as f32, parameter as u32)
                        as f64
                });
            }
            NativeTransferArtisticKind::Threshold => {
                rgb = [if photonic_core::raster::adjust::threshold_rgb(
                    rgb.map(|v| v as f32),
                    parameter,
                ) {
                    1.0
                } else {
                    0.0
                }; 3];
            }
            NativeTransferArtisticKind::Invert => {
                rgb = rgb.map(|v| photonic_core::raster::adjust::invert_channel(v as f32) as f64);
            }
            NativeTransferArtisticKind::Desaturate => {
                rgb = photonic_core::raster::adjust::desaturate_rgb(rgb.map(|v| v as f32))
                    .map(f64::from);
            }
        }
        let rgb = to_ap1.transform(rgb.map(crate::color::native::decode_srgb));
        let storage = |value: f64| {
            let v = value as f32 * p[3];
            if kind == NativeTransferArtisticKind::Invert {
                v.clamp(-65504.0, 65504.0)
            } else {
                v
            }
        };
        out.pixels[i] = [storage(rgb[0]), storage(rgb[1]), storage(rgb[2]), p[3]];
    }
    out
}

/// Shared extended transfer-space channel mix, not AP1-primary matrix math.
pub fn native_transfer_matrix(input: &Image, matrix: [[f32; 3]; 3]) -> Image {
    let identity = matrix == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let a = crate::color::native::ap1_to_linear_srgb();
    let b = crate::color::native::linear_srgb_to_ap1();
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        if p[3] <= 0.0 {
            out.pixels[i] = [0.0; 4];
            continue;
        }
        if identity {
            continue;
        }
        let rgb = a
            .transform([
                p[0] as f64 / p[3] as f64,
                p[1] as f64 / p[3] as f64,
                p[2] as f64 / p[3] as f64,
            ])
            .map(crate::color::native::encode_srgb)
            .map(|v| v as f32);
        let rgb = photonic_core::raster::adjust::channel_mix_rgb(rgb, matrix);
        let rgb = b.transform(rgb.map(|v| crate::color::native::decode_srgb(v as f64)));
        out.pixels[i] = [
            rgb[0] as f32 * p[3],
            rgb[1] as f32 * p[3],
            rgb[2] as f32 * p[3],
            p[3],
        ];
    }
    out
}

/// Tint in extended signed transfer coordinates, sharing raster pointwise math.
pub fn native_photo_filter(input: &Image, color: [f32; 3], density: f32, preserve: bool) -> Image {
    let identity = density == 0.0 || color == [1.0; 3];
    let a = crate::color::native::ap1_to_linear_srgb();
    let b = crate::color::native::linear_srgb_to_ap1();
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        if p[3] <= 0.0 {
            out.pixels[i] = [0.0; 4];
            continue;
        }
        if identity {
            continue;
        }
        let rgb = a
            .transform([
                p[0] as f64 / p[3] as f64,
                p[1] as f64 / p[3] as f64,
                p[2] as f64 / p[3] as f64,
            ])
            .map(crate::color::native::encode_srgb)
            .map(|v| v as f32);
        let rgb =
            photonic_core::raster::adjust::photo_filter_rgb(rgb, color, density, preserve, false);
        let rgb = b.transform(rgb.map(|v| crate::color::native::decode_srgb(v as f64)));
        out.pixels[i] = [
            rgb[0] as f32 * p[3],
            rgb[1] as f32 * p[3],
            rgb[2] as f32 * p[3],
            p[3],
        ];
    }
    out
}

/// Extended transfer-space tonal shifts with shared raster pointwise math.
pub fn native_color_balance(input: &Image, shifts: [[f32; 3]; 3], preserve: bool) -> Image {
    let identity = shifts == [[0.0; 3]; 3];
    let a = crate::color::native::ap1_to_linear_srgb();
    let b = crate::color::native::linear_srgb_to_ap1();
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        if p[3] <= 0.0 {
            out.pixels[i] = [0.0; 4];
            continue;
        }
        if identity {
            continue;
        }
        let rgb = a
            .transform([
                p[0] as f64 / p[3] as f64,
                p[1] as f64 / p[3] as f64,
                p[2] as f64 / p[3] as f64,
            ])
            .map(crate::color::native::encode_srgb)
            .map(|v| v as f32);
        let rgb = photonic_core::raster::adjust::color_balance_rgb(rgb, shifts, preserve, false);
        let rgb = b.transform(rgb.map(|v| crate::color::native::decode_srgb(v as f64)));
        out.pixels[i] = [
            rgb[0] as f32 * p[3],
            rgb[1] as f32 * p[3],
            rgb[2] as f32 * p[3],
            p[3],
        ];
    }
    out
}

/// Floating catalog curve without raster quantization or 0..1 clipping.
pub fn native_transfer_curves(input: &Image, knots: [[f32; 2]; 5], contrast: f32) -> Image {
    use photonic_core::raster::adjust::{catalog_curve_points, TransferCurve};
    let curve = TransferCurve::new(&catalog_curve_points(knots, contrast));
    let identity = curve.is_identity();
    let a = crate::color::native::ap1_to_linear_srgb();
    let b = crate::color::native::linear_srgb_to_ap1();
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        if p[3] <= 0.0 {
            out.pixels[i] = [0.0; 4];
            continue;
        }
        if identity {
            continue;
        }
        let rgb = a
            .transform([
                p[0] as f64 / p[3] as f64,
                p[1] as f64 / p[3] as f64,
                p[2] as f64 / p[3] as f64,
            ])
            .map(crate::color::native::encode_srgb)
            .map(|v| curve.sample(v as f32, true));
        let rgb = b.transform(rgb.map(|v| crate::color::native::decode_srgb(v as f64)));
        out.pixels[i] = [
            (rgb[0] * p[3] as f64).clamp(-65504.0, 65504.0) as f32,
            (rgb[1] * p[3] as f64).clamp(-65504.0, 65504.0) as f32,
            (rgb[2] * p[3] as f64).clamp(-65504.0, 65504.0) as f32,
            p[3],
        ];
    }
    out
}

/// Levels with bounded gamma plus extended linear residual, in signed transfer space.
pub fn native_levels(input: &Image, black_white: [f32; 2], gamma: f32, output: [f32; 2]) -> Image {
    let identity = black_white == [0.0, 1.0] && gamma == 1.0 && output == [0.0, 1.0];
    let a = crate::color::native::ap1_to_linear_srgb();
    let b = crate::color::native::linear_srgb_to_ap1();
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        if p[3] <= 0.0 {
            out.pixels[i] = [0.0; 4];
            continue;
        }
        if identity {
            continue;
        }
        let rgb = a
            .transform([
                p[0] as f64 / p[3] as f64,
                p[1] as f64 / p[3] as f64,
                p[2] as f64 / p[3] as f64,
            ])
            .map(crate::color::native::encode_srgb)
            .map(|v| v as f32);
        let rgb = photonic_core::raster::adjust::levels_rgb(rgb, black_white, gamma, output, true);
        let rgb = b.transform(rgb.map(|v| crate::color::native::decode_srgb(v as f64)));
        out.pixels[i] = [
            (rgb[0] * p[3] as f64).clamp(-65504.0, 65504.0) as f32,
            (rgb[1] * p[3] as f64).clamp(-65504.0, 65504.0) as f32,
            (rgb[2] * p[3] as f64).clamp(-65504.0, 65504.0) as f32,
            p[3],
        ];
    }
    out
}

/// Alpha inspection with shared Legacy float math and finite native storage.
pub fn native_alpha_view(input: &Image, mode: u8) -> Image {
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        let mut q = super::raster_bridge::alpha_view_pixel(*p, mode as i32);
        if mode == 2 {
            for v in &mut q[..3] {
                *v = v.clamp(-65504.0, 65504.0);
            }
        }
        out.pixels[i] = q;
    }
    out
}

/// Native coverage-outline JFA. The coverage isosurface is alpha >= 0.5.
/// Distances use authored pixels, including nonuniform Draft scales. Invalid
/// fields stay infinite so an empty image never creates a spurious solid band.
/// JFA is approximate, not an exact Euclidean distance transform.
pub fn native_outline_distances(input: &Image, pixel_scale: [f32; 2]) -> Vec<f32> {
    let (w, h) = (input.width as i32, input.height as i32);
    let idx = |x: i32, y: i32| (y * w + x) as usize;
    let inside = |x: i32, y: i32| input.pixel(x as u32, y as u32)[3] >= 0.5;
    let mut seeds = vec![[-1i32; 2]; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            if inside(x, y)
                && [[-1, 0], [1, 0], [0, -1], [0, 1]].iter().any(|[dx, dy]| {
                    let (nx, ny) = (x + dx, y + dy);
                    nx < 0 || ny < 0 || nx >= w || ny >= h || !inside(nx, ny)
                })
            {
                seeds[idx(x, y)] = [x, y];
            }
        }
    }
    let distance = |x: i32, y: i32, p: [i32; 2]| {
        let dx = (x - p[0]) as f32 / pixel_scale[0];
        let dy = (y - p[1]) as f32 / pixel_scale[1];
        dx * dx + dy * dy
    };
    let mut step = 1;
    while step < w.max(h) {
        step *= 2;
    }
    step /= 2;
    while step >= 1 {
        let prev = seeds.clone();
        for y in 0..h {
            for x in 0..w {
                let i = idx(x, y);
                let mut best = prev[i];
                let mut d = if best[0] < 0 {
                    f32::INFINITY
                } else {
                    distance(x, y, best)
                };
                for dy in [-step, 0, step] {
                    for dx in [-step, 0, step] {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (x + dx, y + dy);
                        if nx < 0 || ny < 0 || nx >= w || ny >= h {
                            continue;
                        }
                        let p = prev[idx(nx, ny)];
                        if p[0] < 0 {
                            continue;
                        }
                        let nd = distance(x, y, p);
                        if nd < d {
                            d = nd;
                            best = p;
                        }
                    }
                }
                seeds[i] = best;
            }
        }
        step /= 2;
    }
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            if inside(x, y) {
                -1.0
            } else {
                let p = seeds[idx(x, y)];
                if p[0] < 0 {
                    f32::INFINITY
                } else {
                    distance(x, y, p).sqrt()
                }
            }
        })
        .collect()
}
pub fn native_outline(
    input: &Image,
    thickness: f32,
    color: [f32; 3],
    opacity: f32,
    pixel_scale: [f32; 2],
) -> Image {
    if thickness == 0.0 || opacity == 0.0 {
        return input.clone();
    }
    let distance = native_outline_distances(input, pixel_scale);
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        let t = ((distance[i] - (thickness - 1.0)) / 2.0).clamp(0.0, 1.0);
        let coverage = (1.0 - t * t * (3.0 - 2.0 * t)) * opacity * (1.0 - p[3]);
        out.pixels[i] = [
            (p[0] + color[0] * coverage).clamp(-65504.0, 65504.0),
            (p[1] + color[1] * coverage).clamp(-65504.0, 65504.0),
            (p[2] + color[2] * coverage).clamp(-65504.0, 65504.0),
            p[3] + coverage,
        ];
    }
    out
}

/// Catalog HSL in straight linear Rec.709 with Photonic's signed/HDR envelope.
pub fn native_linear_hsl(input: &Image, vibrance: bool, controls: [f32; 3]) -> Image {
    let identity = controls == [0.0; 3];
    let a = crate::color::native::ap1_to_linear_srgb();
    let b = crate::color::native::linear_srgb_to_ap1();
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        if p[3] <= 0.0 {
            out.pixels[i] = [0.0; 4];
            continue;
        }
        if identity {
            continue;
        }
        let rgb = a
            .transform([
                p[0] as f64 / p[3] as f64,
                p[1] as f64 / p[3] as f64,
                p[2] as f64 / p[3] as f64,
            ])
            .map(|v| v as f32);
        let min = rgb[0].min(rgb[1]).min(rgb[2]);
        let max = rgb[0].max(rgb[1]).max(rgb[2]);
        if vibrance && max - min <= 1e-6 * min.abs().max(max.abs()).max(1.0) {
            continue;
        }
        let rgb = if vibrance {
            photonic_core::raster::adjust::extended_linear_hsl_rgb(
                rgb,
                0.0,
                0.0,
                0.0,
                Some(controls[0]),
            )
        } else {
            photonic_core::raster::adjust::extended_linear_hsl_rgb(
                rgb,
                controls[0],
                controls[1],
                controls[2],
                None,
            )
        };
        let ap1 = b.transform(rgb.map(f64::from));
        out.pixels[i] = [
            (ap1[0] * p[3] as f64).clamp(-65504.0, 65504.0) as f32,
            (ap1[1] * p[3] as f64).clamp(-65504.0, 65504.0) as f32,
            (ap1[2] * p[3] as f64).clamp(-65504.0, 65504.0) as f32,
            p[3],
        ];
    }
    out
}

/// Exact authored-tap bilateral Surface Blur in linear Rec.709 range space.
/// Weighted AP1 accumulation commutes with the linear basis conversion.
/// The GPU entry point additionally enforces the explicit per-frame work budget.
pub fn native_surface_blur(
    input: &Image,
    radius: u32,
    threshold: f32,
    pixel_scale: [f32; 2],
) -> Image {
    if radius == 0 || threshold == 0.0 {
        return input.clone();
    }
    let radius = radius.min(64) as i32;
    let sigma_s = (radius as f32).max(1.0);
    let sigma_r = threshold.max(1.0 / 255.0);
    let inv_s = 0.5 / (sigma_s * sigma_s);
    let inv_r = 0.5 / (sigma_r * sigma_r);
    let matrix = crate::color::native::ap1_to_linear_srgb();
    let mut out = input.clone();
    for y in 0..input.height {
        for x in 0..input.width {
            let center = input.pixel(x, y);
            if center[3] <= 0.0 {
                out.set(x, y, [0.0; 4]);
                continue;
            }
            let mut sum = [0.0f32; 4];
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let sample = input.sample_bilinear(
                        x as f32 + 0.5 + dx as f32 * pixel_scale[0],
                        y as f32 + 0.5 + dy as f32 * pixel_scale[1],
                    );
                    let d = matrix
                        .transform([
                            (sample[0] - center[0]) as f64,
                            (sample[1] - center[1]) as f64,
                            (sample[2] - center[2]) as f64,
                        ])
                        .map(|v| v as f32);
                    let range = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                    let weight =
                        (-((dx * dx + dy * dy) as f32) * inv_s).exp() * (-range * inv_r).exp();
                    for c in 0..4 {
                        sum[c] += sample[c] * weight;
                    }
                }
            }
            if sum[3] > 0.0 {
                let factor = center[3] / sum[3];
                out.set(
                    x,
                    y,
                    [
                        (sum[0] * factor).clamp(-65504.0, 65504.0),
                        (sum[1] * factor).clamp(-65504.0, 65504.0),
                        (sum[2] * factor).clamp(-65504.0, 65504.0),
                        center[3],
                    ],
                );
            }
        }
    }
    out
}

/// Discrete circular aperture average of premultiplied AP1 RGBA.
/// Fractional radius determines inclusion, not a rounded/separable kernel.
pub fn native_lens_blur(input: &Image, radius: f32, pixel_scale: [f32; 2]) -> Image {
    if radius < 1.0 {
        return input.clone();
    }
    let radius = radius.min(500.0);
    let bound = radius.ceil() as i32;
    let radius2 = radius * radius;
    let mut out = input.clone();
    for y in 0..input.height {
        for x in 0..input.width {
            let mut sum = [0.0f64; 4];
            let mut count = 0u32;
            for dy in -bound..=bound {
                for dx in -bound..=bound {
                    if (dx * dx + dy * dy) as f32 > radius2 {
                        continue;
                    }
                    let p = input.sample_bilinear(
                        x as f32 + 0.5 + dx as f32 * pixel_scale[0],
                        y as f32 + 0.5 + dy as f32 * pixel_scale[1],
                    );
                    for c in 0..4 {
                        sum[c] += p[c] as f64;
                    }
                    count += 1;
                }
            }
            out.set(
                x,
                y,
                [
                    (sum[0] / count as f64).clamp(-65504.0, 65504.0) as f32,
                    (sum[1] / count as f64).clamp(-65504.0, 65504.0) as f32,
                    (sum[2] / count as f64).clamp(-65504.0, 65504.0) as f32,
                    (sum[3] / count as f64) as f32,
                ],
            );
        }
    }
    out
}

/// Native form of the established diagnostic, not a straight-storage conversion.
pub fn native_unpremultiply(input: &Image) -> Image {
    let mut out = input.clone();
    for p in &mut out.pixels {
        *p = super::raster_bridge::unpremultiply_pixel(*p);
        for c in &mut p[..3] {
            *c = c.clamp(-65504.0, 65504.0);
        }
    }
    out
}

/// Independent per-channel rank in straight linear Rec.709, including coverage.
pub fn native_rank_filter(
    input: &Image,
    kind: super::ir::NativeRankKind,
    radius: u32,
    threshold: u8,
    pixel_scale: [f32; 2],
) -> Image {
    if radius == 0 {
        return input.clone();
    }
    let r =
        super::native_rank::effective_radius(radius, input.width, input.height, pixel_scale) as i32;
    let to709 = crate::color::native::ap1_to_linear_srgb();
    let toap1 = crate::color::native::linear_srgb_to_ap1();
    let operand = |p: [f32; 4]| {
        let a = (p[3] as f64).clamp(0.0, 1.0);
        let rgb = if a > 1e-6 {
            to709.0.map(|row| {
                p[1] as f64 / a
                    + row[0] * (p[0] as f64 - p[1] as f64) / a
                    + row[2] * (p[2] as f64 - p[1] as f64) / a
            })
        } else {
            [0.0; 3]
        };
        [rgb[0], rgb[1], rgb[2], a]
    };
    let mut out = input.clone();
    let mut window = Vec::new();
    let mut channel = Vec::new();
    for y in 0..input.height {
        for x in 0..input.width {
            window.clear();
            for dy in -r..=r {
                for dx in -r..=r {
                    window.push(operand(input.sample_bilinear(
                        x as f32 + 0.5 + dx as f32 * pixel_scale[0],
                        y as f32 + 0.5 + dy as f32 * pixel_scale[1],
                    )));
                }
            }
            let mut med = [0.0; 4];
            for c in 0..4 {
                channel.clear();
                channel.extend(window.iter().map(|p| p[c]));
                let mid = channel.len() / 2;
                med[c] = *channel.select_nth_unstable_by(mid, f64::total_cmp).1;
            }
            if kind == super::ir::NativeRankKind::DustScratches {
                let orig = operand(input.pixel(x, y));
                for c in 0..3 {
                    if (orig[c] - med[c]).abs() <= threshold as f64 / 255.0 {
                        med[c] = orig[c];
                    }
                }
                med[3] = orig[3];
            }
            let rgb = toap1
                .0
                .map(|row| med[1] + row[0] * (med[0] - med[1]) + row[2] * (med[2] - med[1]));
            out.set(
                x,
                y,
                [
                    (rgb[0] * med[3]).clamp(-65504.0, 65504.0) as f32,
                    (rgb[1] * med[3]).clamp(-65504.0, 65504.0) as f32,
                    (rgb[2] * med[3]).clamp(-65504.0, 65504.0) as f32,
                    med[3] as f32,
                ],
            );
        }
    }
    out
}

/// Resolved native correction. Exposure acts on bounded encoded709 values,
/// preserving signed/HDR linear residuals. Bands divide linear RGB first.
/// Anchored AP1 delta reconstruction avoids large neutral HDR cancellation.
pub fn native_deflicker(input: &Image, gain: [f32; 3], band: [f32; 3]) -> Image {
    if gain == [1.0; 3] && band[1].abs() <= 1e-6 {
        return input.clone();
    }
    let to709 = crate::color::native::ap1_to_linear_srgb();
    let toap1 = crate::color::native::linear_srgb_to_ap1();
    let mut out = input.clone();
    for (i, p) in input.pixels.iter().enumerate() {
        if p[3] <= 1e-6 {
            continue;
        }
        let y = i as u32 / input.width.max(1);
        let b = if band[1].abs() > 1e-6 {
            (1.0 / (1.0
                + band[1] as f64
                    * (std::f64::consts::TAU * band[0] as f64 * y as f64
                        / input.height.max(1) as f64
                        + band[2] as f64)
                        .cos()))
            .clamp(0.5, 2.0)
        } else {
            1.0
        };
        let alpha = p[3] as f64;
        let base = [
            p[0] as f64 * b / alpha,
            p[1] as f64 * b / alpha,
            p[2] as f64 * b / alpha,
        ];
        let linear = to709
            .0
            .map(|row| base[1] + row[0] * (base[0] - base[1]) + row[2] * (base[2] - base[1]));
        let mut delta = [0.0; 3];
        for c in 0..3 {
            if gain[c] == 1.0 {
                continue;
            }
            let bounded = linear[c].clamp(0.0, 1.0);
            let encoded = crate::color::native::encode_srgb(bounded);
            let t = ((encoded - 0.75) / 0.25).clamp(0.0, 1.0);
            let g = if gain[c] > 1.0 {
                1.0 + (gain[c] as f64 - 1.0) * (1.0 - t * t)
            } else {
                gain[c] as f64
            };
            let corrected = crate::color::native::decode_srgb((encoded * g).clamp(0.0, 1.0));
            delta[c] = corrected - bounded;
        }
        let change = toap1
            .0
            .map(|row| delta[1] + row[0] * (delta[0] - delta[1]) + row[2] * (delta[2] - delta[1]));
        for c in 0..3 {
            out.pixels[i][c] = ((base[c] + change[c]) * alpha).clamp(-65504.0, 65504.0) as f32;
        }
    }
    out
}

/// f64 pixel reference for closed Bezier mattes. Flattening is shared authoring
/// geometry; ray crossings, distances and feather arithmetic are independent
/// from WGSL. A half-pixel symmetric transition supplies minimum antialiasing.
pub fn bezier_matte(
    width: u32,
    height: u32,
    window: &photonic_core::timeline::BezierWindow,
) -> Image {
    let mut out = Image::new(width, height);
    let Ok(vertices) = window.flatten(width, height) else {
        return out;
    };
    let feather = (f64::from(window.feather) * f64::from(width.min(height))).max(0.5);
    for y in 0..height {
        for x in 0..width {
            let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
            let mut inside = false;
            let mut distance = f64::INFINITY;
            for i in 0..vertices.len() {
                let a = vertices[i].map(f64::from);
                let b = vertices[(i + 1) % vertices.len()].map(f64::from);
                let edge = [b[0] - a[0], b[1] - a[1]];
                let delta = [p[0] - a[0], p[1] - a[1]];
                let len = edge[0] * edge[0] + edge[1] * edge[1];
                let t = if len > 0.0 {
                    ((delta[0] * edge[0] + delta[1] * edge[1]) / len).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                distance = distance.min((delta[0] - t * edge[0]).hypot(delta[1] - t * edge[1]));
                if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) * edge[0] / edge[1]
                {
                    inside = !inside;
                }
            }
            let signed = if inside { distance } else { -distance };
            let t = ((signed + feather) / (2.0 * feather)).clamp(0.0, 1.0);
            let mut v = (t * t * (3.0 - 2.0 * t)) as f32;
            if window.invert {
                v = 1.0 - v;
            }
            out.set(x, y, [v, v, v, 1.0]);
        }
    }
    out
}

#[cfg(test)]
mod native_nonseparable_tests {
    use super::*;
    #[test]
    fn native_nonseparable_uses_ap1_y_and_keeps_legacy_unchanged() {
        let mut source = Image::new(1, 1);
        source.pixels[0] = [0.0, 1.0, 0.0, 1.0];
        let mut bottom = Image::new(1, 1);
        bottom.pixels[0] = [1.0, 0.0, 0.0, 1.0];
        for mode in [
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ] {
            let pixel = native_bounded_merge(&source, &bottom, mode, 1.0).pixels[0];
            assert!(pixel
                .into_iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(&v)));
            assert_eq!(pixel[3], 1.0);
            let actual = pixel[0] * 0.27222872 + pixel[1] * 0.67408174 + pixel[2] * 0.053689517;
            let expected = if mode == BlendMode::Luminosity {
                0.67408174
            } else {
                0.27222872
            };
            assert!((actual - expected).abs() < 1e-6, "{mode:?}: {pixel:?}");
        }
        let legacy = merge(&source, &bottom, BlendMode::Color, 1.0).pixels[0];
        let native = native_bounded_merge(&source, &bottom, BlendMode::Color, 1.0).pixels[0];
        assert!((legacy[1] - 0.3 / 0.59).abs() < 1e-6);
        assert!((native[1] - 0.27222872 / 0.67408174).abs() < 1e-6);
        assert!((legacy[1] - native[1]).abs() > 0.1);
    }
}

#[cfg(test)]
mod native_normal_storage_tests {
    use super::*;
    #[test]
    fn native_normal_preserves_small_coverage_and_legacy_cutoff_is_unchanged() {
        let mut count = 0;
        for alpha in [0.0, 16.0 / 16777216.0, 17.0 / 16777216.0, 0.5, 1.0] {
            for opacity in [0.0, 0.125, 0.5, 1.0] {
                for magnitude in [0.25, 1.0, 65504.0] {
                    let mut top = Image::new(1, 1);
                    let mut bottom = Image::new(1, 1);
                    top.pixels[0] = if alpha == 0.0 {
                        [0.0; 4]
                    } else {
                        [magnitude, -magnitude, magnitude * 0.5, alpha]
                    };
                    bottom.pixels[0] = [-magnitude, magnitude, magnitude, 0.5];
                    let got = native_normal_merge(&top, &bottom, opacity).pixels[0];
                    let a = alpha as f64 * opacity as f64;
                    for (c, actual) in got.iter().enumerate().take(3) {
                        let expected = (top.pixels[0][c] as f64 * opacity as f64
                            + bottom.pixels[0][c] as f64 * (1.0 - a))
                            .clamp(-65504.0, 65504.0);
                        assert!(
                            actual.is_finite()
                                && (*actual as f64 - expected).abs()
                                    <= (expected.abs() * 2e-7).max(0.01)
                        );
                    }
                    assert!((got[3] as f64 - (a + 0.5 * (1.0 - a))).abs() < 1e-7);
                    if alpha > 0.0 && alpha <= ALPHA_EPS && opacity > 0.0 {
                        let old = merge(&top, &bottom, BlendMode::Normal, opacity).pixels[0];
                        for (c, actual) in old.iter().enumerate().take(3) {
                            assert_eq!(*actual, bottom.pixels[0][c] * (1.0 - alpha * opacity));
                        }
                        assert_ne!(old, got, "Legacy cutoff stays distinct");
                    }
                    count += 1;
                }
            }
        }
        assert_eq!(count, 60);
    }
}
