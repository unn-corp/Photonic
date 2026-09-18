//! Color-grade evaluation: keyframe resolution + the CPU reference for every
//! `GradeOpKind` (07 §3), engine-independent.
//!
//! Two responsibilities:
//! 1. [`resolve`] turns an authoring [`Grade`] (base params + keyframe tracks)
//!    into a `Vec<ResolvedGradeOp>` at a given [`Tick`], using core's
//!    time-ignorant [`anim::eval`]. `Wheels` is compiled to its CDL equivalent
//!    here (07 §3.5), and `Lut3d` asset refs are resolved to a ready table via a
//!    caller-supplied provider (the engine's `MediaPool`; tests pass a closure).
//! 2. [`apply_grade_cpu`] runs the resolved stack over an RGBA `f32` buffer with
//!    the exact §3 formulas — the golden reference the GPU kernels
//!    ([`crate::grade_gpu`]) must match within tolerance (07 §6.1).
//!
//! **Working space (07 §3, D-09):** premultiplied linear-Rec.709 *storage*. Per
//! 03 §4.5.3 / 27 A-3 grade operators run on **straight (non-premultiplied)**
//! colour: [`apply_grade_cpu`] unpremultiplies each pixel by its alpha, runs the
//! op, then re-premultiplies. Alpha itself is never touched by any grade op. At
//! α <= [`ALPHA_EPS`] the RGB is carried through unchanged instead of dividing
//! (03 §4.5.2). For opaque pixels (α == 1, every current golden fixture) the
//! round trip is the exact identity, so the blessed corpus does not move. The §3
//! "Option B" enc/dec pair is the sRGB transfer pair — [`enc`] (linear→sRGB) /
//! [`dec`] (sRGB→linear) — applied *internally* around CDL/Wheels/Contrast/LUT
//! only, never around Exposure/WhiteBalance (07 §3.1/§3.3); it sits *inside* the
//! straight-alpha round trip. Luma weighting is **Rec.709**
//! (0.2126/0.7152/0.0722), deliberately not the raster 601 weights (07 §3).

use std::sync::Arc;

use photonic_core::timeline::{
    anim::{self, PropValue},
    AssetId, CdlParams, Grade, GradeMask, GradeOpParams, LutInterp, Tick, WindowShape,
};

use crate::lut::Lut3d;

// ── shared color math (07 §3; the enc/dec pair mirrors raster/adjust.rs:70-90) ─

/// Rec.709 luma weights (07 §3) — NOT the raster 601 weights.
pub const LUMA709_R: f32 = 0.2126;
pub const LUMA709_G: f32 = 0.7152;
pub const LUMA709_B: f32 = 0.0722;

/// White-balance axis gain (07 §3.3): the ±1 UI range spans a "quick balance".
pub const WB_K: f32 = 0.4;

/// sRGB EOTF (encoded→linear) breakpoint — the `dec()` side (raster/adjust.rs:74).
pub const SRGB_EOTF_THRESHOLD: f32 = 0.04045;

// The remaining sRGB coefficients are the single-source-of-truth constants in
// `crate::color`, reused so the enc/dec pair here and the WGSL kernels share
// one definition (07 §3 "avoids a second gamma curve").
use crate::color::{SRGB_ALPHA, SRGB_BETA, SRGB_GAMMA_INV, SRGB_OETF_THRESHOLD, SRGB_SLOPE};

#[inline]
fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// `enc()` (07 §3): linear working value → sRGB-encoded. Identical piecewise
/// formula to `raster/adjust.rs`'s `linear_to_srgb`.
#[inline]
pub fn enc(v: f32) -> f32 {
    let v = clamp01(v);
    if v <= SRGB_OETF_THRESHOLD {
        SRGB_SLOPE * v
    } else {
        SRGB_ALPHA * v.powf(1.0 / SRGB_GAMMA_INV) - SRGB_BETA
    }
}

/// `dec()` (07 §3): sRGB-encoded → linear working value. Identical piecewise
/// formula to `raster/adjust.rs`'s `srgb_to_linear`.
#[inline]
pub fn dec(v: f32) -> f32 {
    let v = clamp01(v);
    if v <= SRGB_EOTF_THRESHOLD {
        v / SRGB_SLOPE
    } else {
        ((v + SRGB_BETA) / SRGB_ALPHA).powf(SRGB_GAMMA_INV)
    }
}

/// Rec.709 relative luminance of an RGB triple (07 §3).
#[inline]
pub fn luma709(rgb: [f32; 3]) -> f32 {
    LUMA709_R * rgb[0] + LUMA709_G * rgb[1] + LUMA709_B * rgb[2]
}

/// GLSL `smoothstep` (raster/adjust.rs:38) — the soft-edge primitive for gates
/// and power-window falloff.
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if (edge1 - edge0).abs() < 1e-9 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// RGB (0..1) → HSL (hue degrees 0..360, sat/light 0..1). Mirrors
/// `raster/adjust.rs:117-138`.
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

/// HSL → RGB. Mirrors `raster/adjust.rs:141-158`.
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

/// Build a 256-entry `f32` LUT from control points via the Fritsch–Carlson
/// monotone-cubic Hermite spline (07 §3.6) — the exact algorithm at
/// `raster/adjust.rs:248-335`, kept in `f32` for GPU-texture parity. Fewer than
/// two valid points yields identity.
pub fn curve_lut(points: &[(f32, f32)]) -> [f32; 256] {
    let mut lut = [0.0f32; 256];
    for (i, l) in lut.iter_mut().enumerate() {
        *l = i as f32 / 255.0;
    }
    let mut pts: Vec<(f32, f32)> = points
        .iter()
        .filter(|(x, y)| x.is_finite() && y.is_finite())
        .map(|(x, y)| (clamp01(*x), clamp01(*y)))
        .collect();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);

    let n = pts.len();
    if n < 2 {
        return lut;
    }
    let mut d = vec![0.0f32; n - 1];
    for i in 0..n - 1 {
        d[i] = (pts[i + 1].1 - pts[i].1) / (pts[i + 1].0 - pts[i].0);
    }
    let mut m = vec![0.0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        if d[i - 1] * d[i] <= 0.0 {
            m[i] = 0.0;
        } else {
            m[i] = (d[i - 1] + d[i]) / 2.0;
        }
    }
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
        *slot = clamp01(y);
    }
    lut
}

/// Sample a 256-entry LUT at normalized `v` (0..1) with linear interpolation —
/// the CPU twin of a 256-texel 1D texture sampled with `FilterMode::Linear`
/// (07 §3.6).
#[inline]
pub fn sample_lut256(lut: &[f32; 256], v: f32) -> f32 {
    let p = clamp01(v) * 255.0;
    let i0 = p.floor() as usize;
    let i1 = (i0 + 1).min(255);
    let f = p - i0 as f32;
    lut[i0] + (lut[i1] - lut[i0]) * f
}

// ── resolved (keyframe-evaluated) IR types (07 §2) ──────────────────────────

/// One keyframe-resolved grade operator — the IR-side sibling of the authoring
/// `GradeOp` (07 §2). Re-exported by `photonic-video::contract` as the payload
/// of the `Grade` IR op.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedGradeOp {
    pub payload: ResolvedGradePayload,
    /// Resolved spatial mask (07 §4); `None` = full frame.
    pub mask: Option<ResolvedMask>,
}

/// Resolved per-kind payload. `Wheels` is absent — it is compiled to
/// [`ResolvedGradePayload::Cdl`] at resolve time (07 §3.5).
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedGradePayload {
    Exposure { stops: f32 },
    Contrast { pivot: f32, amount: f32 },
    WhiteBalance { temp: f32, tint: f32 },
    Cdl(ResolvedCdl),
    Curves(Box<ResolvedCurves>),
    HslQualifier(ResolvedHslQualifier),
    Lut3d(ResolvedLut3d),
}

/// Resolved ASC CDL (07 §3.4). Also the compiled form of `Wheels` and the
/// `HslQualifier` secondary correction.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ResolvedCdl {
    pub slope: [f32; 3],
    pub offset: [f32; 3],
    pub power: [f32; 3],
    pub sat: f32,
}

impl ResolvedCdl {
    /// Compile Lift/Gamma/Gain to CDL-equivalent via the standard identity
    /// (07 §3.5): slope = gain − lift, offset = lift, power = 1/gamma.
    pub fn from_wheels(lift: [f32; 3], gamma: [f32; 3], gain: [f32; 3], sat: f32) -> Self {
        let mut slope = [0.0; 3];
        let mut power = [0.0; 3];
        for c in 0..3 {
            slope[c] = gain[c] - lift[c];
            // Guard a zero/near-zero gamma (registry range is 0.1..4) so 1/gamma
            // stays finite.
            power[c] = 1.0 / gamma[c].max(1e-4);
        }
        ResolvedCdl {
            slope,
            offset: lift,
            power,
            sat,
        }
    }
}

impl From<CdlParams> for ResolvedCdl {
    fn from(p: CdlParams) -> Self {
        ResolvedCdl {
            slope: p.slope,
            offset: p.offset,
            power: p.power,
            sat: p.sat,
        }
    }
}

/// Resolved curves (07 §3.6): master + per-channel 256-entry LUTs, plus the two
/// optional HSL curves (hue-vs-hue, hue-vs-sat) as 256-entry LUTs sampled by
/// normalized hue.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCurves {
    pub master: [f32; 256],
    pub red: [f32; 256],
    pub green: [f32; 256],
    pub blue: [f32; 256],
    pub hue_vs_hue: Option<[f32; 256]>,
    pub hue_vs_sat: Option<[f32; 256]>,
}

/// Resolved HSL qualifier (07 §3.7): three soft range gates + a CDL secondary.
/// Range endpoints are normalized 0..1 (hue = deg/360).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ResolvedHslQualifier {
    pub hue: [f32; 2],
    pub sat: [f32; 2],
    pub lum: [f32; 2],
    pub softness: f32,
    pub correction: ResolvedCdl,
}

/// Resolved 3D LUT op (07 §3.8): a shared parsed table + intensity + interp.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedLut3d {
    pub table: Arc<Lut3d>,
    pub intensity: f32,
    pub tetrahedral: bool,
}

/// Resolved power-window mask (07 §4.1). `RotoMatte` masks resolve away (to
/// `None`) in v1 — no roto source exists yet (07 §4.2).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ResolvedMask {
    pub rectangle: bool,
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub rotation: f32,
    pub softness: f32,
    pub invert: bool,
}

impl ResolvedMask {
    /// Per-pixel window weight at normalized sequence coord `(x, y)` (07 §4.1).
    #[inline]
    pub fn weight(&self, x: f32, y: f32) -> f32 {
        let dx = x - self.center[0];
        let dy = y - self.center[1];
        let (s, co) = self.rotation.sin_cos();
        // Rotate into window-local space.
        let xl = dx * co + dy * s;
        let yl = -dx * s + dy * co;
        let sx = self.size[0].max(1e-4);
        let sy = self.size[1].max(1e-4);
        let d = if self.rectangle {
            (xl / sx).abs().max((yl / sy).abs())
        } else {
            ((xl / sx).powi(2) + (yl / sy).powi(2)).sqrt()
        };
        let soft = self.softness.max(0.0);
        let w = 1.0 - smoothstep(1.0 - soft, 1.0 + soft, d);
        if self.invert {
            1.0 - w
        } else {
            w
        }
    }
}

// ── resolve (authoring Grade → resolved ops at a tick) ──────────────────────

/// Resolve a [`Grade`] at `tick` into the ordered resolved op stack (07 §2).
///
/// Disabled ops and a bypassed grade contribute nothing. `Lut3d` ops call
/// `lut_provider(asset)`; a `None` result (offline/unresolvable asset) drops the
/// op (identity), matching the offline-placeholder asset rule (07 §1). `Unknown`
/// forward-compat ops are skipped (loaded inert, never applied).
pub fn resolve(
    grade: &Grade,
    tick: Tick,
    lut_provider: impl Fn(AssetId) -> Option<Arc<Lut3d>>,
) -> Vec<ResolvedGradeOp> {
    if grade.bypass {
        return Vec::new();
    }
    let mut out = Vec::new();
    for op in &grade.ops {
        if !op.enabled {
            continue;
        }
        let params = eval_params(&op.params, tick);
        let payload = match &params {
            GradeOpParams::Exposure { stops } => ResolvedGradePayload::Exposure { stops: *stops },
            GradeOpParams::Contrast { pivot, amount } => ResolvedGradePayload::Contrast {
                pivot: *pivot,
                amount: *amount,
            },
            GradeOpParams::WhiteBalance { temp, tint } => ResolvedGradePayload::WhiteBalance {
                temp: *temp,
                tint: *tint,
            },
            GradeOpParams::Cdl {
                slope,
                offset,
                power,
                sat,
            } => ResolvedGradePayload::Cdl(ResolvedCdl {
                slope: *slope,
                offset: *offset,
                power: *power,
                sat: *sat,
            }),
            GradeOpParams::Wheels {
                lift,
                gamma,
                gain,
                sat,
            } => ResolvedGradePayload::Cdl(ResolvedCdl::from_wheels(*lift, *gamma, *gain, *sat)),
            GradeOpParams::Curves {
                master,
                red,
                green,
                blue,
                hue_vs_hue,
                hue_vs_sat,
            } => ResolvedGradePayload::Curves(Box::new(ResolvedCurves {
                master: curve_lut(master),
                red: curve_lut(red),
                green: curve_lut(green),
                blue: curve_lut(blue),
                hue_vs_hue: (!hue_vs_hue.is_empty()).then(|| curve_lut(hue_vs_hue)),
                hue_vs_sat: (!hue_vs_sat.is_empty()).then(|| curve_lut(hue_vs_sat)),
            })),
            GradeOpParams::HslQualifier {
                hue,
                sat,
                lum,
                softness,
                correction,
            } => ResolvedGradePayload::HslQualifier(ResolvedHslQualifier {
                hue: *hue,
                sat: *sat,
                lum: *lum,
                softness: *softness,
                correction: (*correction).into(),
            }),
            GradeOpParams::Lut3d {
                asset,
                intensity,
                interp,
            } => match lut_provider(*asset) {
                Some(table) => ResolvedGradePayload::Lut3d(ResolvedLut3d {
                    table,
                    intensity: *intensity,
                    tetrahedral: matches!(interp, LutInterp::Tetrahedral),
                }),
                None => continue, // offline LUT asset → op is inert this frame
            },
            GradeOpParams::Unknown(_) => continue, // forward-compat inert op
        };
        out.push(ResolvedGradeOp {
            payload,
            mask: resolve_mask(op.mask.as_ref()),
        });
    }
    out
}

/// Resolve an op's `GradeMask`. Only `PowerWindow` is real in v1 (07 §4.1);
/// `RotoMatte` resolves to `None` (full frame) since no roto source exists yet.
fn resolve_mask(mask: Option<&GradeMask>) -> Option<ResolvedMask> {
    match mask? {
        GradeMask::PowerWindow {
            shape,
            center,
            size,
            rotation,
            softness,
            invert,
        } => Some(ResolvedMask {
            rectangle: matches!(shape, WindowShape::Rectangle),
            center: *center,
            size: *size,
            rotation: *rotation,
            softness: *softness,
            invert: *invert,
        }),
        GradeMask::RotoMatte { .. } => None,
    }
}

/// Evaluate an op's animated params at `tick`, overriding the base value for
/// each keyframed scalar path via core's [`anim::eval`].
fn eval_params(props: &anim::AnimProps<GradeOpParams>, tick: Tick) -> GradeOpParams {
    let mut p = props.base.clone();
    if props.tracks.is_empty() {
        return p;
    }
    for track in &props.tracks {
        if track.orphaned || track.keyframes.is_empty() {
            continue;
        }
        if let Some(slot) = scalar_field_mut(&mut p, track.property.as_str()) {
            let base = PropValue::Float(*slot as f64);
            if let PropValue::Float(v) = anim::eval(track, &base, tick) {
                *slot = v as f32;
            }
        }
    }
    p
}

/// Map an animatable `PropPath` to the mutable scalar field it addresses, for
/// the kind in `p`. Paths mirror `prop_registry.rs`'s grade blocks. `None` for
/// paths that do not resolve for this kind (e.g. structural curve vectors).
fn scalar_field_mut<'a>(p: &'a mut GradeOpParams, path: &str) -> Option<&'a mut f32> {
    use GradeOpParams::*;
    match p {
        Exposure { stops } => (path == "params.stops").then_some(stops),
        Contrast { pivot, amount } => match path {
            "params.pivot" => Some(pivot),
            "params.amount" => Some(amount),
            _ => None,
        },
        WhiteBalance { temp, tint } => match path {
            "params.temp" => Some(temp),
            "params.tint" => Some(tint),
            _ => None,
        },
        Cdl {
            slope,
            offset,
            power,
            sat,
        } => match path {
            "params.slope[0]" => Some(&mut slope[0]),
            "params.slope[1]" => Some(&mut slope[1]),
            "params.slope[2]" => Some(&mut slope[2]),
            "params.offset[0]" => Some(&mut offset[0]),
            "params.offset[1]" => Some(&mut offset[1]),
            "params.offset[2]" => Some(&mut offset[2]),
            "params.power[0]" => Some(&mut power[0]),
            "params.power[1]" => Some(&mut power[1]),
            "params.power[2]" => Some(&mut power[2]),
            "params.sat" => Some(sat),
            _ => None,
        },
        Wheels {
            lift,
            gamma,
            gain,
            sat,
        } => match path {
            "params.lift[0]" => Some(&mut lift[0]),
            "params.lift[1]" => Some(&mut lift[1]),
            "params.lift[2]" => Some(&mut lift[2]),
            "params.gamma[0]" => Some(&mut gamma[0]),
            "params.gamma[1]" => Some(&mut gamma[1]),
            "params.gamma[2]" => Some(&mut gamma[2]),
            "params.gain[0]" => Some(&mut gain[0]),
            "params.gain[1]" => Some(&mut gain[1]),
            "params.gain[2]" => Some(&mut gain[2]),
            "params.sat" => Some(sat),
            _ => None,
        },
        HslQualifier {
            softness,
            correction,
            ..
        } => match path {
            "params.softness" => Some(softness),
            "params.correction.sat" => Some(&mut correction.sat),
            _ => None,
        },
        Lut3d { intensity, .. } => (path == "params.intensity").then_some(intensity),
        Curves { .. } | Unknown(_) => None,
    }
}

// ── CPU reference apply (07 §3; operand space 03 §4.5.3) ────────────────────

/// α below which a premultiplied pixel's RGB is carried through unchanged
/// instead of divided out (03 §4.5.2). Must equal the WGSL `1e-6` literal in
/// [`crate::grade_gpu`] — guarded by `shaders_share_constants_with_rust`.
pub(crate) const ALPHA_EPS: f32 = 1e-6;

/// Straight (non-premultiplied) linear RGB from a premultiplied pixel, or
/// `None` when α <= [`ALPHA_EPS`] (03 §4.5.3). The inverse is
/// [`repremultiply3`]. Shared so the GPU parity path and any future per-channel
/// non-linear op reuse one definition (03 §4.5).
#[inline]
pub(crate) fn unpremultiply3(px: &[f32; 4]) -> Option<[f32; 3]> {
    let a = px[3];
    if a <= ALPHA_EPS {
        return None;
    }
    Some([px[0] / a, px[1] / a, px[2] / a])
}

/// Re-premultiply straight linear RGB by α (03 §4.5.3), the inverse of
/// [`unpremultiply3`].
#[inline]
pub(crate) fn repremultiply3(rgb: [f32; 3], a: f32) -> [f32; 3] {
    [rgb[0] * a, rgb[1] * a, rgb[2] * a]
}

/// Apply a resolved grade stack to a premultiplied-linear RGBA `f32` buffer.
/// Per 03 §4.5.3 each op runs on **straight** colour: unpremultiply → operate →
/// re-premultiply, once per op (the outer loop re-reads the buffer, so the round
/// trip is per-op, not one wrap around the stack). `width`/`height` size the
/// normalized coordinates power-window masks use (07 §4.1). Alpha is preserved
/// exactly; at α <= [`ALPHA_EPS`] the pixel is left untouched.
pub fn apply_grade_cpu(pixels: &mut [f32], width: u32, height: u32, ops: &[ResolvedGradeOp]) {
    let w = width.max(1) as f32;
    let h = height.max(1) as f32;
    for op in ops {
        for (i, px) in pixels.chunks_exact_mut(4).enumerate() {
            let idx = i as u32;
            let x = ((idx % width.max(1)) as f32 + 0.5) / w;
            let y = ((idx / width.max(1)) as f32 + 0.5) / h;
            let mask_w = op.mask.map(|m| m.weight(x, y)).unwrap_or(1.0);
            let a = px[3];
            // 03 §4.5.3: grade operates on straight colour. Fully transparent
            // pixels (α <= ALPHA_EPS) carry RGB through unchanged rather than
            // dividing (03 §4.5.2); for premultiplied storage that RGB is ~0.
            let Some(rgb) = unpremultiply3(&[px[0], px[1], px[2], a]) else {
                continue;
            };
            let out = repremultiply3(apply_op(&op.payload, rgb, mask_w), a);
            px[0] = out[0];
            px[1] = out[1];
            px[2] = out[2];
            // px[3] alpha preserved.
        }
    }
}

/// Final per-pixel color for one resolved op, already blended by the mask weight
/// (and, for the qualifier, its own gate). `mask_w == 1.0` when the op has no
/// mask.
pub fn apply_op(payload: &ResolvedGradePayload, rgb: [f32; 3], mask_w: f32) -> [f32; 3] {
    match payload {
        ResolvedGradePayload::HslQualifier(q) => {
            let corrected = apply_cdl(rgb, &q.correction);
            let w = qualifier_gate(q, rgb) * mask_w;
            lerp3(rgb, corrected, w)
        }
        _ => {
            let corrected = match payload {
                ResolvedGradePayload::Exposure { stops } => apply_exposure(rgb, *stops),
                ResolvedGradePayload::Contrast { pivot, amount } => {
                    apply_contrast(rgb, *pivot, *amount)
                }
                ResolvedGradePayload::WhiteBalance { temp, tint } => {
                    apply_white_balance(rgb, *temp, *tint)
                }
                ResolvedGradePayload::Cdl(c) => apply_cdl(rgb, c),
                ResolvedGradePayload::Curves(c) => apply_curves(rgb, c),
                ResolvedGradePayload::Lut3d(l) => apply_lut3d(rgb, l),
                // Unreachable by control flow: the outer `match payload` handles
                // `HslQualifier` in its first arm, so this inner match (the `_`
                // branch) can never see it, regardless of input data.
                ResolvedGradePayload::HslQualifier(_) => unreachable!(),
            };
            lerp3(rgb, corrected, mask_w)
        }
    }
}

#[inline]
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// 07 §3.1 — a physical light multiply in scene-linear, no enc/dec roundtrip.
#[inline]
pub fn apply_exposure(rgb: [f32; 3], stops: f32) -> [f32; 3] {
    let f = 2f32.powf(stops);
    [rgb[0] * f, rgb[1] * f, rgb[2] * f]
}

/// 07 §3.2 — contrast in encoded space; same slope formula as
/// `raster/adjust.rs:185-189` (clamped to avoid a divide-by-zero at amount=1).
#[inline]
pub fn apply_contrast(rgb: [f32; 3], pivot: f32, amount: f32) -> [f32; 3] {
    let slope = if amount >= 0.0 {
        1.0 / (1.0 - amount.min(0.999))
    } else {
        1.0 + amount
    };
    let mut out = [0.0; 3];
    for c in 0..3 {
        let e = enc(rgb[c]);
        out[c] = dec(clamp01((e - pivot) * slope + pivot));
    }
    out
}

/// 07 §3.3 — white balance as a scene-linear per-channel gain.
#[inline]
pub fn apply_white_balance(rgb: [f32; 3], temp: f32, tint: f32) -> [f32; 3] {
    let gain = [
        1.0 + WB_K * temp,
        1.0 - 0.5 * WB_K * tint,
        1.0 - WB_K * temp,
    ];
    [rgb[0] * gain[0], rgb[1] * gain[1], rgb[2] * gain[2]]
}

/// 07 §3.4 — ASC CDL v1.2: slope/offset/power in encoded space, then saturation
/// about the corrected value's own Rec.709 luma.
#[inline]
pub fn apply_cdl(rgb: [f32; 3], c: &ResolvedCdl) -> [f32; 3] {
    let mut corr = [0.0f32; 3];
    for ch in 0..3 {
        let e = enc(rgb[ch]);
        // Gamut-clamp the SOP base to >= 0 before the power (a negative base
        // raised to a fractional power is undefined) — the "clamp_or_gamut" step.
        let base = (e * c.slope[ch] + c.offset[ch]).max(0.0);
        corr[ch] = base.powf(c.power[ch]);
    }
    let l = luma709(corr);
    let mut out = [0.0f32; 3];
    for ch in 0..3 {
        out[ch] = dec(clamp01(l + c.sat * (corr[ch] - l)));
    }
    out
}

/// 07 §3.6 — master then per-channel LUTs (sampled in the linear working value),
/// then the optional hue-vs-hue / hue-vs-sat HSL pass.
pub fn apply_curves(rgb: [f32; 3], c: &ResolvedCurves) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    let luts = [&c.red, &c.green, &c.blue];
    for ch in 0..3 {
        let m = sample_lut256(&c.master, rgb[ch]);
        out[ch] = sample_lut256(luts[ch], m);
    }
    if c.hue_vs_hue.is_some() || c.hue_vs_sat.is_some() {
        let mut hsl = rgb_to_hsl(out);
        let hue_x = hsl[0] / 360.0;
        if let Some(hh) = &c.hue_vs_hue {
            let delta = (sample_lut256(hh, hue_x) - 0.5) * 360.0;
            hsl[0] = (hsl[0] + delta).rem_euclid(360.0);
        }
        if let Some(hs) = &c.hue_vs_sat {
            let mult = sample_lut256(hs, hue_x) * 2.0;
            hsl[1] = clamp01(hsl[1] * mult);
        }
        out = hsl_to_rgb(hsl);
    }
    out
}

/// 07 §3.8 — sample the parsed table in enc()-space, mix by intensity, decode.
#[inline]
pub fn apply_lut3d(rgb: [f32; 3], l: &ResolvedLut3d) -> [f32; 3] {
    let e = [enc(rgb[0]), enc(rgb[1]), enc(rgb[2])];
    let sampled = if l.tetrahedral {
        l.table.sample_tetrahedral(e)
    } else {
        l.table.sample_trilinear(e)
    };
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        out[c] = dec(e[c] + (sampled[c] - e[c]) * l.intensity);
    }
    out
}

/// The `hue*sat*lum` qualifier gate (07 §3.7), computed on the pixel's own HSL.
/// Ranges are normalized 0..1 (hue = deg/360).
fn qualifier_gate(q: &ResolvedHslQualifier, rgb: [f32; 3]) -> f32 {
    let hsl = rgb_to_hsl(rgb);
    let h = hsl[0] / 360.0;
    let hue_g = hue_gate(h, q.hue[0], q.hue[1], q.softness);
    let sat_g = range_gate(hsl[1], q.sat[0], q.sat[1], q.softness);
    let lum_g = range_gate(hsl[2], q.lum[0], q.lum[1], q.softness);
    hue_g * sat_g * lum_g
}

/// Soft-edged range gate: 1 inside `[lo, hi]`, `smoothstep` falloff of width
/// `soft` on each side (07 §3.7, reusing the smoothstep primitive).
#[inline]
fn range_gate(v: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    if soft <= 1e-6 {
        return if v >= lo && v <= hi { 1.0 } else { 0.0 };
    }
    let rise = smoothstep(lo - soft, lo, v);
    let fall = 1.0 - smoothstep(hi, hi + soft, v);
    (rise * fall).clamp(0.0, 1.0)
}

/// Hue gate on the 0..1 circle: evaluate the range gate at `h` and its ±1
/// aliases so a band near the 0/1 seam still catches. v1 expects `lo <= hi`
/// (non-wrapping selection); wrap-around ranges are a v1 limitation (07 §3.7).
#[inline]
fn hue_gate(h: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    range_gate(h, lo, hi, soft)
        .max(range_gate(h - 1.0, lo, hi, soft))
        .max(range_gate(h + 1.0, lo, hi, soft))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::raster::{adjust, image::RasterImage};
    use photonic_core::timeline::{
        anim::{AnimProps, Interp, Keyframe, PropertyTrack},
        grade::{Grade, GradeOp, GradeOpKind},
    };

    fn none_provider(_: AssetId) -> Option<Arc<Lut3d>> {
        None
    }

    // ── shared math ─────────────────────────────────────────────────────────

    #[test]
    fn enc_dec_round_trip() {
        for v in [0.0f32, 0.001, 0.05, 0.18, 0.5, 0.9, 1.0] {
            assert!((dec(enc(v)) - v).abs() < 1e-4, "roundtrip {v}");
        }
    }

    #[test]
    fn luma709_is_not_601() {
        // Pure green: 709 weight 0.7152, well above the 601 weight 0.587.
        let l = luma709([0.0, 1.0, 0.0]);
        assert!((l - 0.7152).abs() < 1e-6);
    }

    // ── resolve / keyframes ───────────────────────────────────────────────────

    #[test]
    fn bypass_yields_no_ops() {
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        grade.bypass = true;
        assert!(resolve(&grade, Tick(0), none_provider).is_empty());
    }

    #[test]
    fn disabled_op_skipped() {
        let mut grade = Grade::new();
        let mut op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        op.enabled = false;
        grade.ops.push(op);
        assert!(resolve(&grade, Tick(0), none_provider).is_empty());
    }

    #[test]
    fn wheels_compiles_to_cdl_identity() {
        // Lift 0, gamma 1, gain 1 → CDL slope 1 / offset 0 / power 1 (identity).
        let cdl = ResolvedCdl::from_wheels([0.0; 3], [1.0; 3], [1.0; 3], 1.0);
        assert_eq!(cdl.slope, [1.0; 3]);
        assert_eq!(cdl.offset, [0.0; 3]);
        assert_eq!(cdl.power, [1.0; 3]);
        // And that identity CDL leaves a mid gray unchanged.
        let out = apply_cdl([0.3, 0.3, 0.3], &cdl);
        for c in 0..3 {
            assert!((out[c] - 0.3).abs() < 1e-4, "{out:?}");
        }
    }

    #[test]
    fn keyframed_exposure_interpolates() {
        let mut grade = Grade::new();
        let mut props = AnimProps::new(GradeOpParams::Exposure { stops: 0.0 });
        let mut track = PropertyTrack::new("params.stops");
        track.insert_keyframe(Keyframe::new(
            Tick(0),
            PropValue::Float(0.0),
            Interp::Linear,
        ));
        track.insert_keyframe(Keyframe::new(
            Tick(100),
            PropValue::Float(2.0),
            Interp::Linear,
        ));
        props.tracks.push(track);
        grade.ops.push(GradeOp {
            id: photonic_core::timeline::GradeOpId::new(),
            enabled: true,
            kind: GradeOpKind::Exposure,
            params: props,
            mask: None,
        });
        // At t=50, stops interpolates to 1.0.
        let ops = resolve(&grade, Tick(50), none_provider);
        assert_eq!(ops.len(), 1);
        match ops[0].payload {
            ResolvedGradePayload::Exposure { stops } => {
                assert!((stops - 1.0).abs() < 1e-6, "stops {stops}")
            }
            _ => panic!("wrong payload"),
        }
    }

    // ── operand space: straight-alpha round trip (03 §4.5.3) ─────────────────

    /// A CDL + Contrast + LUT stack — the three ops whose enc/dec non-linearity
    /// makes the round trip observable.
    fn cdl_contrast_lut_stack() -> Vec<ResolvedGradeOp> {
        let cdl = ResolvedCdl {
            slope: [1.1, 1.0, 0.9],
            offset: [0.02, 0.0, -0.01],
            power: [1.0, 1.0, 1.0],
            sat: 1.2,
        };
        let lut = ResolvedLut3d {
            table: Arc::new(Lut3d::identity(17)),
            intensity: 1.0,
            tetrahedral: false,
        };
        vec![
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Cdl(cdl),
                mask: None,
            },
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Contrast {
                    pivot: 0.4,
                    amount: 0.3,
                },
                mask: None,
            },
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Lut3d(lut),
                mask: None,
            },
        ]
    }

    #[test]
    fn grade_is_invariant_under_opacity() {
        let ops = cdl_contrast_lut_stack();
        let straight = [0.6f32, 0.3, 0.15];
        // Opaque pixel: premultiplied storage == straight colour.
        let mut opaque = [straight[0], straight[1], straight[2], 1.0];
        apply_grade_cpu(&mut opaque, 1, 1, &ops);
        // Same colour at α = 0.35, stored premultiplied.
        let a = 0.35f32;
        let mut partial = [straight[0] * a, straight[1] * a, straight[2] * a, a];
        apply_grade_cpu(&mut partial, 1, 1, &ops);
        // Alpha is untouched by every grade op.
        assert_eq!(opaque[3], 1.0);
        assert_eq!(partial[3], a);
        // Unpremultiplied results agree: the grade did not depend on alpha.
        for c in 0..3 {
            let got = partial[c] / a;
            assert!(
                (got - opaque[c]).abs() < 1e-6,
                "channel {c}: partial {got} vs opaque {}",
                opaque[c]
            );
        }
    }

    #[test]
    fn opaque_grade_is_bit_identical_to_legacy() {
        let ops = cdl_contrast_lut_stack();
        let rgb0 = [0.42f32, 0.71, 0.13];
        // Legacy path (pre-03 §4.5.3): ops applied directly to the stored RGB,
        // no unpremultiply round trip. For α == 1 storage == straight, so we
        // reproduce it by folding `apply_op` over the stack — this is exactly
        // what the old `apply_grade_cpu` computed.
        let mut legacy = rgb0;
        for op in &ops {
            legacy = apply_op(&op.payload, legacy, 1.0);
        }
        let mut px = [rgb0[0], rgb0[1], rgb0[2], 1.0];
        apply_grade_cpu(&mut px, 1, 1, &ops);
        // Exact equality: the opaque golden corpus cannot move.
        assert_eq!(
            [px[0], px[1], px[2]],
            legacy,
            "opaque grade must be bit-identical to legacy"
        );
        assert_eq!(px[3], 1.0);
    }

    #[test]
    fn fully_transparent_pixel_is_untouched() {
        let ops = cdl_contrast_lut_stack();
        let mut px = [0.0f32, 0.0, 0.0, 0.0];
        let before = px;
        apply_grade_cpu(&mut px, 1, 1, &ops);
        assert_eq!(px, before, "α==0 pixel must pass through byte-for-byte");
    }

    // ── per-op formulas ───────────────────────────────────────────────────────

    #[test]
    fn exposure_plus_one_doubles_linear() {
        let out = apply_exposure([0.2, 0.4, 0.1], 1.0);
        assert!((out[0] - 0.4).abs() < 1e-6);
        assert!((out[1] - 0.8).abs() < 1e-6);
        assert!((out[2] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn contrast_zero_is_identity() {
        let out = apply_contrast([0.2, 0.5, 0.8], 0.5, 0.0);
        for c in 0..3 {
            let want = [0.2, 0.5, 0.8][c];
            assert!((out[c] - want).abs() < 1e-4, "{out:?}");
        }
    }

    #[test]
    fn white_balance_warm_shifts_red_up_blue_down() {
        let out = apply_white_balance([0.5, 0.5, 0.5], 0.5, 0.0);
        assert!(out[0] > 0.5, "red up");
        assert!(out[2] < 0.5, "blue down");
        assert!((out[1] - 0.5).abs() < 1e-6, "green fixed at tint 0");
    }

    #[test]
    fn cdl_identity_is_noop() {
        let id = ResolvedCdl {
            slope: [1.0; 3],
            offset: [0.0; 3],
            power: [1.0; 3],
            sat: 1.0,
        };
        let out = apply_cdl([0.25, 0.5, 0.75], &id);
        for c in 0..3 {
            let want = [0.25, 0.5, 0.75][c];
            assert!((out[c] - want).abs() < 1e-4, "{out:?}");
        }
    }

    #[test]
    fn cdl_lgg_identity_round_trip() {
        // 07 §6.1 hook: Wheels(identity) → CDL → apply == input (through the LGG
        // identity), within enc/dec rounding.
        let cdl = ResolvedCdl::from_wheels([0.0; 3], [1.0; 3], [1.0; 3], 1.0);
        for v in [0.1f32, 0.35, 0.6, 0.85] {
            let out = apply_cdl([v, v, v], &cdl);
            for c in 0..3 {
                assert!((out[c] - v).abs() < 1e-3, "v={v} out={out:?}");
            }
        }
    }

    #[test]
    fn cdl_saturation_zero_is_gray() {
        // sat = 0 collapses to luma on all channels (in encoded space, decoded).
        let cdl = ResolvedCdl {
            slope: [1.0; 3],
            offset: [0.0; 3],
            power: [1.0; 3],
            sat: 0.0,
        };
        let out = apply_cdl([0.8, 0.2, 0.4], &cdl);
        assert!((out[0] - out[1]).abs() < 1e-5, "gray {out:?}");
        assert!((out[1] - out[2]).abs() < 1e-5, "gray {out:?}");
    }

    // ── curves equivalence with raster/adjust.rs (07 §6.1) ────────────────────

    /// Build a 256×1 gray ramp image (pixel i = [i,i,i,255]).
    fn gray_ramp() -> RasterImage {
        let mut img = RasterImage::new(256, 1);
        for i in 0..256u32 {
            img.set_pixel(i, 0, [i as u8, i as u8, i as u8, 255]);
        }
        img
    }

    #[test]
    fn curves_master_matches_adjust_spline() {
        // A single (composite) curve: master only, per-channel identity. The
        // grade LUT sampled at exact byte positions must equal adjust::curves'
        // byte output — proving the same Fritsch–Carlson spline.
        let pts = [(0.0f32, 0.0), (0.25, 0.5), (1.0, 1.0)];
        let mut img = gray_ramp();
        adjust::curves(&mut img, &pts, &[], &[], &[], None);

        let resolved = ResolvedCurves {
            master: curve_lut(&pts),
            red: curve_lut(&[]),
            green: curve_lut(&[]),
            blue: curve_lut(&[]),
            hue_vs_hue: None,
            hue_vs_sat: None,
        };
        for i in 0..256u32 {
            let v = i as f32 / 255.0;
            let out = apply_curves([v, v, v], &resolved);
            let got = (out[0] * 255.0).round() as u8;
            let want = img.pixel(i, 0)[0];
            assert!(
                (got as i32 - want as i32).abs() <= 1,
                "i={i} got={got} want={want}"
            );
        }
    }

    #[test]
    fn curves_per_channel_red_matches_adjust() {
        // Red-only curve, master identity: isolates a single per-channel LUT.
        let red_pts = [(0.0f32, 1.0), (1.0, 0.0)]; // invert red
        let mut img = gray_ramp();
        adjust::curves(&mut img, &[], &red_pts, &[], &[], None);
        let resolved = ResolvedCurves {
            master: curve_lut(&[]),
            red: curve_lut(&red_pts),
            green: curve_lut(&[]),
            blue: curve_lut(&[]),
            hue_vs_hue: None,
            hue_vs_sat: None,
        };
        for i in 0..256u32 {
            let v = i as f32 / 255.0;
            let out = apply_curves([v, v, v], &resolved);
            let got_r = (out[0] * 255.0).round() as u8;
            let want_r = img.pixel(i, 0)[0];
            assert!(
                (got_r as i32 - want_r as i32).abs() <= 1,
                "i={i} r got={got_r} want={want_r}"
            );
        }
    }

    // ── exposure equivalence with raster/adjust.rs exposure model ─────────────

    #[test]
    fn exposure_matches_adjust_in_encoded_space() {
        // raster exposure works in sRGB byte space: linearize → ×2^stops →
        // re-encode. Our grade exposure is the linear multiply; wrapping it in
        // enc/dec must reproduce the raster byte for a mid gray at +1 stop.
        let mut img = RasterImage::filled(1, 1, [100, 100, 100, 255]);
        adjust::exposure(&mut img, 1.0, None);
        let want = img.pixel(0, 0)[0];
        let lin = dec(100.0 / 255.0);
        let graded = apply_exposure([lin, lin, lin], 1.0);
        let got = (enc(graded[0]) * 255.0).round() as u8;
        assert!(
            (got as i32 - want as i32).abs() <= 1,
            "got={got} want={want}"
        );
    }

    // ── qualifier gate closed form (07 §3.7) ──────────────────────────────────

    #[test]
    fn qualifier_gate_closed_form() {
        // A gray pixel (sat 0, lum 0.5). A qualifier keyed on sat [0.4, 0.6]
        // with no softness excludes it (sat gate closed) → gate 0 → no change.
        let q = ResolvedHslQualifier {
            hue: [0.0, 1.0],
            sat: [0.4, 0.6],
            lum: [0.0, 1.0],
            softness: 0.0,
            correction: ResolvedCdl {
                slope: [2.0; 3],
                offset: [0.0; 3],
                power: [1.0; 3],
                sat: 1.0,
            },
        };
        let gray = [0.5, 0.5, 0.5];
        assert!((qualifier_gate(&q, gray)).abs() < 1e-6, "gray excluded");
        let out = apply_op(&ResolvedGradePayload::HslQualifier(q), gray, 1.0);
        for c in 0..3 {
            assert!((out[c] - gray[c]).abs() < 1e-6, "unchanged {out:?}");
        }
    }

    #[test]
    fn qualifier_gate_open_applies_correction() {
        // A saturated pixel inside a wide-open qualifier → gate 1 → full CDL.
        let q = ResolvedHslQualifier {
            hue: [0.0, 1.0],
            sat: [0.0, 1.0],
            lum: [0.0, 1.0],
            softness: 0.0,
            correction: ResolvedCdl {
                slope: [1.0; 3],
                offset: [0.1, 0.1, 0.1],
                power: [1.0; 3],
                sat: 1.0,
            },
        };
        let px = [0.6, 0.2, 0.3];
        let gate = qualifier_gate(&q, px);
        assert!((gate - 1.0).abs() < 1e-6, "wide open gate {gate}");
        let out = apply_op(&ResolvedGradePayload::HslQualifier(q), px, 1.0);
        let direct = apply_cdl(px, &q.correction);
        for c in 0..3 {
            assert!((out[c] - direct[c]).abs() < 1e-6, "full corr {out:?}");
        }
    }

    // ── power window mask (07 §4.1) ────────────────────────────────────────────

    #[test]
    fn power_window_ellipse_weight() {
        let m = ResolvedMask {
            rectangle: false,
            center: [0.5, 0.5],
            size: [0.25, 0.25],
            rotation: 0.0,
            softness: 0.0,
            invert: false,
        };
        assert!((m.weight(0.5, 0.5) - 1.0).abs() < 1e-6, "centre inside");
        assert!(m.weight(0.95, 0.5) < 1e-6, "far outside");
    }

    #[test]
    fn power_window_invert_flips() {
        let m = ResolvedMask {
            rectangle: false,
            center: [0.5, 0.5],
            size: [0.25, 0.25],
            rotation: 0.0,
            softness: 0.0,
            invert: true,
        };
        assert!(m.weight(0.5, 0.5) < 1e-6, "centre now excluded");
        assert!((m.weight(0.95, 0.5) - 1.0).abs() < 1e-6, "outside included");
    }

    #[test]
    fn masked_exposure_only_affects_inside() {
        // A 2×1 image; a tight window over the left pixel only.
        let mut px = vec![0.2, 0.2, 0.2, 1.0, 0.2, 0.2, 0.2, 1.0];
        let op = ResolvedGradeOp {
            payload: ResolvedGradePayload::Exposure { stops: 1.0 },
            mask: Some(ResolvedMask {
                rectangle: true,
                center: [0.25, 0.5],
                size: [0.2, 0.9],
                rotation: 0.0,
                softness: 0.0,
                invert: false,
            }),
        };
        apply_grade_cpu(&mut px, 2, 1, &[op]);
        assert!((px[0] - 0.4).abs() < 1e-4, "left doubled {}", px[0]);
        assert!((px[4] - 0.2).abs() < 1e-4, "right untouched {}", px[4]);
    }

    // ── lut3d op (07 §3.8) ─────────────────────────────────────────────────────

    #[test]
    fn lut3d_identity_intensity_full_is_noop() {
        let l = ResolvedLut3d {
            table: Arc::new(Lut3d::identity(2)),
            intensity: 1.0,
            tetrahedral: false,
        };
        for v in [0.1f32, 0.4, 0.7] {
            let out = apply_lut3d([v, v, v], &l);
            for c in 0..3 {
                assert!((out[c] - v).abs() < 1e-3, "v={v} out={out:?}");
            }
        }
    }

    #[test]
    fn lut3d_intensity_zero_is_noop_even_for_nonidentity() {
        // An inverting LUT at intensity 0 leaves the input untouched.
        let mut table = Lut3d::identity(2);
        for e in table.data.iter_mut() {
            for v in e.iter_mut() {
                *v = 1.0 - *v;
            }
        }
        let l = ResolvedLut3d {
            table: Arc::new(table),
            intensity: 0.0,
            tetrahedral: false,
        };
        let out = apply_lut3d([0.3, 0.6, 0.2], &l);
        assert!((out[0] - 0.3).abs() < 1e-3, "{out:?}");
        assert!((out[1] - 0.6).abs() < 1e-3, "{out:?}");
        assert!((out[2] - 0.2).abs() < 1e-3, "{out:?}");
    }
}
