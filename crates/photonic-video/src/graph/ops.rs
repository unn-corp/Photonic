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
        let w = self.width as i32;
        let h = self.height as i32;
        let x = px - 0.5;
        let y = py - 0.5;
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;
        let at = |ix: i32, iy: i32| -> [f32; 4] {
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
                Sampling::Bilinear => input.sample_bilinear(src.x, src.y),
                Sampling::Nearest => {
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
