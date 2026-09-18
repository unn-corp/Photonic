//! GPU grade kernels (07 §3) — the WGSL twins of [`crate::grade`]'s CPU
//! reference, plus the wgpu plumbing to run one op as a full-screen pass.
//!
//! **One pass per op (07 §3 "or one pass per op if simpler, justify").** The
//! grade kinds need heterogeneous resources — CDL/Contrast/Exposure/WB/qualifier
//! are uniform-only, Curves needs a 1-D LUT buffer, Lut3d needs a 3-D texture —
//! so a single über-shader would have to bind every resource for every op and
//! branch on all of them. Splitting into three shaders (uniform-math, curves,
//! 3D-LUT) keeps each kernel small and mirrors this crate's existing
//! per-pass architecture (`convert`/`blur`/`composite`/`yuv`). The resolved
//! stack is applied by ping-ponging two working textures, one pass per op
//! ([`apply_grade_stack_gpu`]).
//!
//! Numeric constants (`enc`/`dec` sRGB pair, Rec.709 luma weights) are shared
//! with Rust via [`crate::grade`] / [`crate::color`] and asserted in the shader
//! source by `wgsl_shaders_parse_and_validate` (pipeline.rs) plus the
//! `contains_token` test below (03 §4.4 rule 1 pattern).

use wgpu::util::DeviceExt;

use crate::grade::{
    ResolvedCdl, ResolvedGradeOp, ResolvedGradePayload, ResolvedLut3d, ResolvedMask,
};
use crate::pipeline::WORKING_FORMAT;

// Op-kind ids shared between the Rust dispatcher and the WGSL `switch`.
const KIND_EXPOSURE: u32 = 0;
const KIND_CONTRAST: u32 = 1;
const KIND_WHITE_BALANCE: u32 = 2;
const KIND_CDL: u32 = 3;
const KIND_HSL_QUALIFIER: u32 = 4;

// ── shared WGSL prelude (vertex quad + enc/dec + luma709 + hsl + mask) ──────

/// Reused by every grade shader: the fullscreen vertex stage, the sRGB enc/dec
/// pair (07 §3 Option B), Rec.709 luma, HSL conversions, and the power-window
/// mask weight (07 §4.1).
const PRELUDE: &str = r#"
struct VOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0)       uv:       vec2<f32>,
}

@vertex
fn vs_quad(@builtin(vertex_index) vi: u32) -> VOut {
    var pos = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0,  1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0,  1.0), vec2<f32>(-1.0, 1.0)
    );
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 0.0)
    );
    var out: VOut;
    out.clip_pos = vec4<f32>(pos[vi], 0.0, 1.0);
    out.uv       = uvs[vi];
    return out;
}

fn enc1(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if (c <= 0.0031308) { return 12.92 * c; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}
fn dec1(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
fn enc3(c: vec3<f32>) -> vec3<f32> { return vec3<f32>(enc1(c.r), enc1(c.g), enc1(c.b)); }
fn dec3(c: vec3<f32>) -> vec3<f32> { return vec3<f32>(dec1(c.r), dec1(c.g), dec1(c.b)); }

fn luma709(c: vec3<f32>) -> f32 { return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b; }

fn cdl3(c: vec3<f32>, slope: vec3<f32>, offset: vec3<f32>, power: vec3<f32>, sat: f32) -> vec3<f32> {
    let e = enc3(c);
    let base = max(e * slope + offset, vec3<f32>(0.0));
    let corr = pow(base, power);
    let l = luma709(corr);
    let s = vec3<f32>(l) + sat * (corr - vec3<f32>(l));
    return dec3(clamp(s, vec3<f32>(0.0), vec3<f32>(1.0)));
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let mx = max(max(rgb.r, rgb.g), rgb.b);
    let mn = min(min(rgb.r, rgb.g), rgb.b);
    let l = (mx + mn) * 0.5;
    let d = mx - mn;
    var s = 0.0;
    if (d > 1e-9) { s = d / max(1.0 - abs(2.0 * l - 1.0), 1e-9); }
    var h = 0.0;
    if (d > 1e-9) {
        if (mx == rgb.r) {
            let t = (rgb.g - rgb.b) / d;
            h = 60.0 * (t - 6.0 * floor(t / 6.0));
        } else if (mx == rgb.g) {
            h = 60.0 * ((rgb.b - rgb.r) / d + 2.0);
        } else {
            h = 60.0 * ((rgb.r - rgb.g) / d + 4.0);
        }
    }
    h = h - 360.0 * floor(h / 360.0);
    return vec3<f32>(h, clamp(s, 0.0, 1.0), clamp(l, 0.0, 1.0));
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x - 360.0 * floor(hsl.x / 360.0);
    let s = clamp(hsl.y, 0.0, 1.0);
    let l = clamp(hsl.z, 0.0, 1.0);
    let c = (1.0 - abs(2.0 * l - 1.0)) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - abs((hp - 2.0 * floor(hp / 2.0)) - 1.0));
    var rgb = vec3<f32>(0.0);
    let seg = i32(floor(hp));
    if (seg == 0) { rgb = vec3<f32>(c, x, 0.0); }
    else if (seg == 1) { rgb = vec3<f32>(x, c, 0.0); }
    else if (seg == 2) { rgb = vec3<f32>(0.0, c, x); }
    else if (seg == 3) { rgb = vec3<f32>(0.0, x, c); }
    else if (seg == 4) { rgb = vec3<f32>(x, 0.0, c); }
    else { rgb = vec3<f32>(c, 0.0, x); }
    return rgb + vec3<f32>(l - c * 0.5);
}

fn smoothstep1(e0: f32, e1: f32, x: f32) -> f32 {
    if (abs(e1 - e0) < 1e-9) { if (x < e0) { return 0.0; } return 1.0; }
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// Power-window weight at normalized coord (x,y). center.xy, size.xy in `c`;
// rotation, softness in `p.xy`; `p.zw` is the physical→logical uv scale.
// `rect` != 0 → rectangle (Chebyshev) else ellipse.
//
// `x`/`y` arrive normalized against the RENDER TARGET, which is a pool-bucketed
// texture whose dimensions are rounded up to a multiple of 64 — so uv 1.0 is the
// edge of the bucket, not the edge of the picture. The CPU reference
// (`apply_grade_cpu`) normalizes against the LOGICAL frame instead, so without
// `p.zw` the two disagree for any frame whose size is not a multiple of 64:
// 1920x1080 buckets to 1920x1088 and the window lands 0.741% off vertically.
// Scaling here rather than pre-scaling center/size on the CPU keeps the rotation
// in logical space — with a per-axis scale, a rotated ellipse would shear.
fn window_weight(x: f32, y: f32, c: vec4<f32>, p: vec4<f32>, rect: u32, invert: u32) -> f32 {
    let dx = x * p.z - c.x;
    let dy = y * p.w - c.y;
    let sn = sin(p.x);
    let cs = cos(p.x);
    let xl = dx * cs + dy * sn;
    let yl = -dx * sn + dy * cs;
    let sx = max(c.z, 1e-4);
    let sy = max(c.w, 1e-4);
    var d: f32;
    if (rect != 0u) { d = max(abs(xl / sx), abs(yl / sy)); }
    else { d = sqrt((xl / sx) * (xl / sx) + (yl / sy) * (yl / sy)); }
    let soft = max(p.y, 0.0);
    var w = 1.0 - smoothstep1(1.0 - soft, 1.0 + soft, d);
    if (invert != 0u) { w = 1.0 - w; }
    return w;
}

// Straight (non-premultiplied) linear RGB from a premultiplied pixel (03 §4.5.3).
// At α <= 1e-6 the RGB is carried through unchanged rather than divided
// (03 §4.5.2); the 1e-6 literal must equal Rust `grade::ALPHA_EPS` — guarded by
// `shaders_share_constants_with_rust`.
fn unpremul3(c: vec4<f32>) -> vec3<f32> {
    if (c.a <= 1e-6) { return vec3<f32>(0.0); }
    return c.rgb / c.a;
}
"#;

// ── uniform-math shader (Exposure / Contrast / WhiteBalance / CDL / Qualifier) ─

/// Shared uniform for the uniform-math ops. Slots are interpreted per `kind`.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct MathUniform {
    /// x = op kind, y = has_mask, z = mask_rect, w = mask_invert.
    kind_flags: [u32; 4],
    /// Exposure: x=stops. Contrast: x=pivot,y=amount. WB: x=temp,y=tint.
    /// CDL/Qualifier-correction: xyz = slope.
    p0: [f32; 4],
    /// CDL/Qualifier-correction: xyz = offset.
    p1: [f32; 4],
    /// CDL/Qualifier-correction: xyz = power.
    p2: [f32; 4],
    /// CDL/Qualifier-correction: x = sat.
    p3: [f32; 4],
    /// Qualifier: x=hue_lo, y=hue_hi, z=sat_lo, w=sat_hi.
    q_hue_sat: [f32; 4],
    /// Qualifier: x=lum_lo, y=lum_hi, z=softness.
    q_lum: [f32; 4],
    /// Mask: center.xy, size.xy.
    mask_c: [f32; 4],
    /// Mask: rotation, softness.
    mask_p: [f32; 4],
}

/// The uniform-math grade kernel. Reused for the five ops with no extra texture.
pub const MATH_SHADER: &str = r#"
@group(0) @binding(0) var t_src: texture_2d<f32>;
@group(0) @binding(1) var samp:  sampler;
struct U {
    kind_flags: vec4<u32>,
    p0: vec4<f32>, p1: vec4<f32>, p2: vec4<f32>, p3: vec4<f32>,
    q_hue_sat: vec4<f32>, q_lum: vec4<f32>,
    mask_c: vec4<f32>, mask_p: vec4<f32>,
}
@group(0) @binding(2) var<uniform> u: U;
__PRELUDE__

fn range_gate(v: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    if (soft <= 1e-6) { if (v >= lo && v <= hi) { return 1.0; } return 0.0; }
    let rise = smoothstep1(lo - soft, lo, v);
    let fall = 1.0 - smoothstep1(hi, hi + soft, v);
    return clamp(rise * fall, 0.0, 1.0);
}
fn hue_gate(h: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    return max(max(range_gate(h, lo, hi, soft), range_gate(h - 1.0, lo, hi, soft)),
               range_gate(h + 1.0, lo, hi, soft));
}

@fragment
fn fs_grade(in: VOut) -> @location(0) vec4<f32> {
    let src = textureSample(t_src, samp, in.uv);
    // 03 §4.5.3: grade operates on straight colour, then re-premultiplies.
    let c = unpremul3(src);
    let kind = u.kind_flags.x;
    var corrected = c;
    var gate = 1.0;
    if (kind == 0u) {
        corrected = c * exp2(u.p0.x);
    } else if (kind == 1u) {
        var slope = 1.0 + u.p0.y;
        if (u.p0.y >= 0.0) { slope = 1.0 / (1.0 - min(u.p0.y, 0.999)); }
        let e = enc3(c);
        corrected = dec3(clamp((e - vec3<f32>(u.p0.x)) * slope + vec3<f32>(u.p0.x), vec3<f32>(0.0), vec3<f32>(1.0)));
    } else if (kind == 2u) {
        corrected = c * vec3<f32>(1.0 + 0.4 * u.p0.x, 1.0 - 0.2 * u.p0.y, 1.0 - 0.4 * u.p0.x);
    } else if (kind == 3u) {
        corrected = cdl3(c, u.p0.xyz, u.p1.xyz, u.p2.xyz, u.p3.x);
    } else if (kind == 4u) {
        corrected = cdl3(c, u.p0.xyz, u.p1.xyz, u.p2.xyz, u.p3.x);
        let hsl = rgb_to_hsl(c);
        let hg = hue_gate(hsl.x / 360.0, u.q_hue_sat.x, u.q_hue_sat.y, u.q_lum.z);
        let sg = range_gate(hsl.y, u.q_hue_sat.z, u.q_hue_sat.w, u.q_lum.z);
        let lg = range_gate(hsl.z, u.q_lum.x, u.q_lum.y, u.q_lum.z);
        gate = hg * sg * lg;
    }
    var w = gate;
    if (u.kind_flags.y != 0u) {
        w = w * window_weight(in.uv.x, in.uv.y, u.mask_c, u.mask_p, u.kind_flags.z, u.kind_flags.w);
    }
    return vec4<f32>(mix(c, corrected, w) * src.a, src.a);
}
"#;

// ── curves shader (master + per-channel + hue LUTs via a storage buffer) ──────

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct CurvesUniform {
    /// x=has_mask, y=mask_rect, z=mask_invert, w=has_huehue.
    flags: [u32; 4],
    /// x=has_huesat.
    flags2: [u32; 4],
    mask_c: [f32; 4],
    mask_p: [f32; 4],
}

/// The curves grade kernel. `curves[row*256 + i]` holds row 0=master,
/// 1=red, 2=green, 3=blue, 4=hue_vs_hue, 5=hue_vs_sat (07 §3.6).
pub const CURVES_SHADER: &str = r#"
@group(0) @binding(0) var t_src: texture_2d<f32>;
@group(0) @binding(1) var samp:  sampler;
struct U { flags: vec4<u32>, flags2: vec4<u32>, mask_c: vec4<f32>, mask_p: vec4<f32> }
@group(0) @binding(2) var<uniform> u: U;
@group(0) @binding(3) var<storage, read> curves: array<f32>;
__PRELUDE__

fn sample_curve(row: u32, v: f32) -> f32 {
    let p = clamp(v, 0.0, 1.0) * 255.0;
    let i0 = u32(floor(p));
    let i1 = min(i0 + 1u, 255u);
    let f = p - floor(p);
    let base = row * 256u;
    return mix(curves[base + i0], curves[base + i1], f);
}

@fragment
fn fs_curves(in: VOut) -> @location(0) vec4<f32> {
    let src = textureSample(t_src, samp, in.uv);
    // 03 §4.5.3: grade operates on straight colour, then re-premultiplies.
    let c = unpremul3(src);
    var out = vec3<f32>(
        sample_curve(1u, sample_curve(0u, c.r)),
        sample_curve(2u, sample_curve(0u, c.g)),
        sample_curve(3u, sample_curve(0u, c.b)),
    );
    if (u.flags.w != 0u || u.flags2.x != 0u) {
        var hsl = rgb_to_hsl(out);
        let hue_x = hsl.x / 360.0;
        if (u.flags.w != 0u) {
            let delta = (sample_curve(4u, hue_x) - 0.5) * 360.0;
            hsl.x = hsl.x + delta;
        }
        if (u.flags2.x != 0u) {
            hsl.y = clamp(hsl.y * sample_curve(5u, hue_x) * 2.0, 0.0, 1.0);
        }
        out = hsl_to_rgb(hsl);
    }
    var w = 1.0;
    if (u.flags.x != 0u) {
        w = window_weight(in.uv.x, in.uv.y, u.mask_c, u.mask_p, u.flags.y, u.flags.z);
    }
    return vec4<f32>(mix(c, out, w) * src.a, src.a);
}
"#;

// ── 3D-LUT shader (trilinear via hardware sampler; tetrahedral via loads) ─────

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Lut3dUniform {
    /// x=has_mask, y=mask_rect, z=mask_invert, w=tetrahedral.
    flags: [u32; 4],
    /// x=intensity, y=size (as f32).
    misc: [f32; 4],
    dmin: [f32; 4],
    dmax: [f32; 4],
    mask_c: [f32; 4],
    mask_p: [f32; 4],
}

/// The 3D-LUT grade kernel (07 §3.8, §6.5). Trilinear uses the hardware linear
/// sampler at texel-centre-corrected coords; tetrahedral loads the 8 cell
/// corners and runs the canonical decomposition.
pub const LUT3D_SHADER: &str = r#"
@group(0) @binding(0) var t_src: texture_2d<f32>;
@group(0) @binding(1) var samp:  sampler;
struct U { flags: vec4<u32>, misc: vec4<f32>, dmin: vec4<f32>, dmax: vec4<f32>, mask_c: vec4<f32>, mask_p: vec4<f32> }
@group(0) @binding(2) var<uniform> u: U;
@group(0) @binding(3) var lut_tex: texture_3d<f32>;
@group(0) @binding(4) var lut_samp: sampler;
__PRELUDE__

fn lut_load(x: i32, y: i32, z: i32) -> vec3<f32> {
    return textureLoad(lut_tex, vec3<i32>(x, y, z), 0).rgb;
}

fn tetra(t: vec3<f32>, n: f32) -> vec3<f32> {
    let g = t * (n - 1.0);
    let b0 = floor(g);
    let d = g - b0;
    let mx = i32(n) - 1;
    let x0 = clamp(i32(b0.x), 0, mx); let x1 = min(x0 + 1, mx);
    let y0 = clamp(i32(b0.y), 0, mx); let y1 = min(y0 + 1, mx);
    let z0 = clamp(i32(b0.z), 0, mx); let z1 = min(z0 + 1, mx);
    let c000 = lut_load(x0, y0, z0);
    let c100 = lut_load(x1, y0, z0);
    let c010 = lut_load(x0, y1, z0);
    let c110 = lut_load(x1, y1, z0);
    let c001 = lut_load(x0, y0, z1);
    let c101 = lut_load(x1, y0, z1);
    let c011 = lut_load(x0, y1, z1);
    let c111 = lut_load(x1, y1, z1);
    let dr = d.x; let dg = d.y; let db = d.z;
    if (dr > dg) {
        if (dg > db) {
            return c000 + dr * (c100 - c000) + dg * (c110 - c100) + db * (c111 - c110);
        } else if (dr > db) {
            return c000 + dr * (c100 - c000) + db * (c101 - c100) + dg * (c111 - c101);
        } else {
            return c000 + db * (c001 - c000) + dr * (c101 - c001) + dg * (c111 - c101);
        }
    } else if (db > dg) {
        return c000 + db * (c001 - c000) + dg * (c011 - c001) + dr * (c111 - c011);
    } else if (db > dr) {
        return c000 + dg * (c010 - c000) + db * (c011 - c010) + dr * (c111 - c011);
    } else {
        return c000 + dg * (c010 - c000) + dr * (c110 - c010) + db * (c111 - c110);
    }
}

@fragment
fn fs_lut3d(in: VOut) -> @location(0) vec4<f32> {
    let src = textureSample(t_src, samp, in.uv);
    // 03 §4.5.3: grade operates on straight colour, then re-premultiplies.
    let c = unpremul3(src);
    let e = enc3(c);
    let span = max(u.dmax.xyz - u.dmin.xyz, vec3<f32>(1e-9));
    let t = clamp((e - u.dmin.xyz) / span, vec3<f32>(0.0), vec3<f32>(1.0));
    let n = u.misc.y;
    var sampled: vec3<f32>;
    if (u.flags.w != 0u) {
        sampled = tetra(t, n);
    } else {
        // Texel-centre-corrected coord so hardware trilinear matches the CPU
        // grid interpolation between nodes 0..N-1.
        let coord = (t * (n - 1.0) + vec3<f32>(0.5)) / n;
        sampled = textureSampleLevel(lut_tex, lut_samp, coord, 0.0).rgb;
    }
    let mixed = mix(e, sampled, u.misc.x);
    let out = dec3(mixed);
    var w = 1.0;
    if (u.flags.x != 0u) {
        w = window_weight(in.uv.x, in.uv.y, u.mask_c, u.mask_p, u.flags.y, u.flags.z);
    }
    return vec4<f32>(mix(c, out, w) * src.a, src.a);
}
"#;

/// Materialize a shader source with the shared [`PRELUDE`] spliced in.
fn expand(src: &str) -> String {
    src.replace("__PRELUDE__", PRELUDE)
}

// Expanded sources for the pipeline shader-validation test (pipeline.rs).
#[cfg(test)]
pub(crate) fn expanded_math_shader() -> String {
    expand(MATH_SHADER)
}
#[cfg(test)]
pub(crate) fn expanded_curves_shader() -> String {
    expand(CURVES_SHADER)
}
#[cfg(test)]
pub(crate) fn expanded_lut3d_shader() -> String {
    expand(LUT3D_SHADER)
}

// ── run one op as a full-screen pass ────────────────────────────────────────

/// Mask uniform slots. `uv_scale` converts a render-target-normalized uv into a
/// logical-frame-normalized one (see `window_weight`); it rides in the two spare
/// floats of the `p` vector so no uniform layout changes.
fn mask_fields(mask: Option<&ResolvedMask>, uv_scale: [f32; 2]) -> ([u32; 3], [f32; 4], [f32; 4]) {
    match mask {
        Some(m) => (
            [1, m.rectangle as u32, m.invert as u32],
            [m.center[0], m.center[1], m.size[0], m.size[1]],
            [m.rotation, m.softness, uv_scale[0], uv_scale[1]],
        ),
        // Unmasked: every shader gates its `window_weight` call on the has-mask
        // flag, so these are inert — but carry an identity scale rather than
        // zeros so a future unguarded read cannot collapse the frame to a point.
        None => ([0, 0, 0], [0.0; 4], [0.0, 0.0, 1.0, 1.0]),
    }
}

fn new_working(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("grade_out"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: WORKING_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn f32_to_f16_bits(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32 - 112; // 127 - 15
    let m = b & 0x7fffff;
    if e <= 0 {
        sign
    } else if e >= 0x1f {
        sign | 0x7c00
    } else {
        sign | ((e as u16) << 10) | ((m >> 13) as u16)
    }
}

fn linear_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("grade_samp"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    })
}

/// Common entries 0 (input texture) + 1 (sampler) + 2 (uniform).
fn base_entries() -> [wgpu::BindGroupLayoutEntry; 3] {
    [
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 2,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
    ]
}

fn build_pipeline(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    src: &str,
    fs_entry: &str,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("grade_shader"),
        source: wgpu::ShaderSource::Wgsl(expand(src).into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("grade_layout"),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("grade_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_quad",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: fs_entry,
            targets: &[Some(wgpu::ColorTargetState {
                format: WORKING_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

fn run_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::RenderPipeline,
    bind: &wgpu::BindGroup,
    out: &wgpu::Texture,
) {
    let view = out.create_view(&Default::default());
    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("grade_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..6, 0..1);
    }
    queue.submit([enc.finish()]);
}

/// Apply one resolved grade op to `input` (a `WORKING_FORMAT` texture), returning
/// a fresh output texture. A fresh pipeline/bind-group is built per call — the
/// engine (P3+) will hold persistent pipelines; this mirrors
/// `convert_yuv_planes_to_working`'s per-call contract.
/// `logical` is the picture size in pixels, which is NOT the texture size when
/// the input comes from the bucketed texture pool. Power-window masks are
/// normalized against it, matching `apply_grade_cpu`'s `width`/`height`. Pass
/// the texture's own dimensions when there is no separate logical size.
pub fn apply_grade_op_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    input: &wgpu::Texture,
    op: &ResolvedGradeOp,
    logical: (u32, u32),
) -> wgpu::Texture {
    let w = input.width();
    let h = input.height();
    let out = new_working(device, w, h);
    let in_view = input.create_view(&Default::default());
    let samp = linear_sampler(device);
    let uv_scale = [
        w as f32 / logical.0.max(1) as f32,
        h as f32 / logical.1.max(1) as f32,
    ];
    let (mflag, mc, mp) = mask_fields(op.mask.as_ref(), uv_scale);

    match &op.payload {
        ResolvedGradePayload::Curves(c) => {
            let mut data = Vec::with_capacity(256 * 6);
            data.extend_from_slice(&c.master);
            data.extend_from_slice(&c.red);
            data.extend_from_slice(&c.green);
            data.extend_from_slice(&c.blue);
            data.extend_from_slice(&c.hue_vs_hue.unwrap_or([0.0; 256]));
            data.extend_from_slice(&c.hue_vs_sat.unwrap_or([0.0; 256]));
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("curve_lut"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::STORAGE,
            });
            let uni = CurvesUniform {
                flags: [mflag[0], mflag[1], mflag[2], c.hue_vs_hue.is_some() as u32],
                flags2: [c.hue_vs_sat.is_some() as u32, 0, 0, 0],
                mask_c: mc,
                mask_p: mp,
            };
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("curves_u"),
                contents: bytemuck::bytes_of(&uni),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let mut entries = base_entries().to_vec();
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
            let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("curves_bgl"),
                entries: &entries,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("curves_bg"),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&in_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&samp),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: buf.as_entire_binding(),
                    },
                ],
            });
            let pipeline = build_pipeline(device, &bgl, CURVES_SHADER, "fs_curves");
            run_pass(device, queue, &pipeline, &bind, &out);
        }
        ResolvedGradePayload::Lut3d(l) => {
            upload_and_run_lut3d(device, queue, &in_view, &samp, l, mflag, mc, mp, &out);
        }
        other => {
            let uni = math_uniform(other, mflag, mc, mp);
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("math_u"),
                contents: bytemuck::bytes_of(&uni),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("math_bgl"),
                entries: &base_entries(),
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("math_bg"),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&in_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&samp),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: ubuf.as_entire_binding(),
                    },
                ],
            });
            let pipeline = build_pipeline(device, &bgl, MATH_SHADER, "fs_grade");
            run_pass(device, queue, &pipeline, &bind, &out);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn upload_and_run_lut3d(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    in_view: &wgpu::TextureView,
    samp: &wgpu::Sampler,
    l: &ResolvedLut3d,
    mflag: [u32; 3],
    mc: [f32; 4],
    mp: [f32; 4],
    out: &wgpu::Texture,
) {
    let n = l.table.size as u32;
    // Rgba16Float 3-D texture; red-fastest data order == x fastest.
    let mut texels: Vec<u16> = Vec::with_capacity((n * n * n) as usize * 4);
    for e in &l.table.data {
        texels.push(f32_to_f16_bits(e[0]));
        texels.push(f32_to_f16_bits(e[1]));
        texels.push(f32_to_f16_bits(e[2]));
        texels.push(f32_to_f16_bits(1.0));
    }
    let lut_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lut3d"),
        size: wgpu::Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: n,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        lut_tex.as_image_copy(),
        bytemuck::cast_slice(&texels),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(n * 8),
            rows_per_image: Some(n),
        },
        wgpu::Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: n,
        },
    );
    let lut_view = lut_tex.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D3),
        ..Default::default()
    });
    let lut_samp = linear_sampler(device);
    let uni = Lut3dUniform {
        flags: [mflag[0], mflag[1], mflag[2], l.tetrahedral as u32],
        misc: [l.intensity, n as f32, 0.0, 0.0],
        dmin: [
            l.table.domain_min[0],
            l.table.domain_min[1],
            l.table.domain_min[2],
            0.0,
        ],
        dmax: [
            l.table.domain_max[0],
            l.table.domain_max[1],
            l.table.domain_max[2],
            0.0,
        ],
        mask_c: mc,
        mask_p: mp,
    };
    let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lut3d_u"),
        contents: bytemuck::bytes_of(&uni),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let mut entries = base_entries().to_vec();
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D3,
            multisampled: false,
        },
        count: None,
    });
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: 4,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("lut3d_bgl"),
        entries: &entries,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lut3d_bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(in_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(samp),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: ubuf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&lut_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(&lut_samp),
            },
        ],
    });
    let pipeline = build_pipeline(device, &bgl, LUT3D_SHADER, "fs_lut3d");
    run_pass(device, queue, &pipeline, &bind, out);
}

fn cdl_slots(c: &ResolvedCdl) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
    (
        [c.slope[0], c.slope[1], c.slope[2], 0.0],
        [c.offset[0], c.offset[1], c.offset[2], 0.0],
        [c.power[0], c.power[1], c.power[2], 0.0],
        [c.sat, 0.0, 0.0, 0.0],
    )
}

fn math_uniform(
    payload: &ResolvedGradePayload,
    mflag: [u32; 3],
    mask_c: [f32; 4],
    mask_p: [f32; 4],
) -> MathUniform {
    let mut u = MathUniform {
        kind_flags: [0, mflag[0], mflag[1], mflag[2]],
        p0: [0.0; 4],
        p1: [0.0; 4],
        p2: [0.0; 4],
        p3: [0.0; 4],
        q_hue_sat: [0.0; 4],
        q_lum: [0.0; 4],
        mask_c,
        mask_p,
    };
    match payload {
        ResolvedGradePayload::Exposure { stops } => {
            u.kind_flags[0] = KIND_EXPOSURE;
            u.p0[0] = *stops;
        }
        ResolvedGradePayload::Contrast { pivot, amount } => {
            u.kind_flags[0] = KIND_CONTRAST;
            u.p0[0] = *pivot;
            u.p0[1] = *amount;
        }
        ResolvedGradePayload::WhiteBalance { temp, tint } => {
            u.kind_flags[0] = KIND_WHITE_BALANCE;
            u.p0[0] = *temp;
            u.p0[1] = *tint;
        }
        ResolvedGradePayload::Cdl(c) => {
            u.kind_flags[0] = KIND_CDL;
            let (p0, p1, p2, p3) = cdl_slots(c);
            u.p0 = p0;
            u.p1 = p1;
            u.p2 = p2;
            u.p3 = p3;
        }
        ResolvedGradePayload::HslQualifier(q) => {
            u.kind_flags[0] = KIND_HSL_QUALIFIER;
            let (p0, p1, p2, p3) = cdl_slots(&q.correction);
            u.p0 = p0;
            u.p1 = p1;
            u.p2 = p2;
            u.p3 = p3;
            u.q_hue_sat = [q.hue[0], q.hue[1], q.sat[0], q.sat[1]];
            u.q_lum = [q.lum[0], q.lum[1], q.softness, 0.0];
        }
        ResolvedGradePayload::Curves(_) | ResolvedGradePayload::Lut3d(_) => {
            unreachable!("curves/lut3d handled separately")
        }
    }
    u
}

/// Apply an entire resolved stack, ping-ponging between working textures. A
/// fresh owned texture is always returned; the caller retains `input`.
/// `logical` is the picture size, not the (possibly pool-bucketed) texture size
/// — see [`apply_grade_op_gpu`].
pub fn apply_grade_stack_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    input: &wgpu::Texture,
    ops: &[ResolvedGradeOp],
    logical: (u32, u32),
) -> wgpu::Texture {
    let mut cur: Option<wgpu::Texture> = None;
    for op in ops {
        let src = cur.as_ref().unwrap_or(input);
        let next = apply_grade_op_gpu(device, queue, src, op, logical);
        cur = Some(next);
    }
    match cur {
        Some(t) => t,
        None => {
            let out = new_working(device, input.width(), input.height());
            let mut enc = device.create_command_encoder(&Default::default());
            enc.copy_texture_to_texture(
                input.as_image_copy(),
                out.as_image_copy(),
                wgpu::Extent3d {
                    width: input.width(),
                    height: input.height(),
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([enc.finish()]);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::{apply_grade_cpu, ResolvedCurves, ResolvedHslQualifier};
    use crate::lut::Lut3d;
    use std::sync::Arc;

    /// Same whole-token matcher as video.rs — a shader literal must be a
    /// complete numeric token, not a substring.
    fn contains_token(src: &str, value: f32) -> bool {
        let lit = format!("{value:?}");
        let bytes = src.as_bytes();
        let boundary = |b: u8| !matches!(b, b'0'..=b'9' | b'.');
        let mut from = 0;
        while let Some(rel) = src[from..].find(&lit) {
            let start = from + rel;
            let end = start + lit.len();
            let before_ok = start == 0 || boundary(bytes[start - 1]);
            let after_ok = end >= bytes.len() || boundary(bytes[end]);
            if before_ok && after_ok {
                return true;
            }
            from = start + 1;
        }
        false
    }

    #[test]
    fn shaders_share_constants_with_rust() {
        let expanded = expand(MATH_SHADER);
        for c in [
            crate::color::SRGB_SLOPE,
            crate::color::SRGB_ALPHA,
            crate::color::SRGB_BETA,
            crate::grade::SRGB_EOTF_THRESHOLD,
            crate::grade::LUMA709_R,
            crate::grade::LUMA709_G,
            crate::grade::LUMA709_B,
        ] {
            assert!(contains_token(&expanded, c), "MATH shader missing {c:?}");
        }
        // 03 §4.5.3: the WGSL `unpremul3` epsilon must equal Rust `ALPHA_EPS`.
        assert!(
            contains_token(&expanded, crate::grade::ALPHA_EPS),
            "MATH shader missing ALPHA_EPS {:?}",
            crate::grade::ALPHA_EPS
        );
        assert!(!contains_token("x = 10.2126;", crate::grade::LUMA709_R));
    }

    fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        pollster::block_on(adapter.request_device(&Default::default(), None)).ok()
    }

    fn f16_to_f32(bits: u16) -> f32 {
        let sign = ((bits >> 15) & 1) as u32;
        let exp = ((bits >> 10) & 0x1f) as i32;
        let frac = (bits & 0x3ff) as u32;
        let v = if exp == 0 {
            frac as f32 * 2f32.powi(-24)
        } else if exp == 0x1f {
            f32::INFINITY
        } else {
            (1.0 + frac as f32 / 1024.0) * 2f32.powi(exp - 15)
        };
        if sign == 1 {
            -v
        } else {
            v
        }
    }

    fn align256(n: u32) -> u32 {
        (n + 255) & !255
    }

    fn gradient_texture(device: &wgpu::Device, queue: &wgpu::Queue, n: u32) -> wgpu::Texture {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("grad_in"),
            size: wgpu::Extent3d {
                width: n,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut data: Vec<u8> = Vec::new();
        for i in 0..n {
            let v = i as f32 / (n - 1) as f32;
            for c in [v, v, v, 1.0] {
                data.extend_from_slice(&f32_to_f16_bits(c).to_le_bytes());
            }
        }
        queue.write_texture(
            tex.as_image_copy(),
            &data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(n * 8),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: n,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        tex
    }

    fn cpu_pixels(n: u32) -> Vec<f32> {
        let mut v = Vec::with_capacity(n as usize * 4);
        for i in 0..n {
            let g = i as f32 / (n - 1) as f32;
            v.extend_from_slice(&[g, g, g, 1.0]);
        }
        v
    }

    /// Straight luma (0.2..0.8) and alpha (0→1) for pixel `i` of `n`. The
    /// straight value stays mid-band so unpremultiplying at low α does not
    /// amplify f16 quantization out of tolerance (03 §4.5.3).
    fn ramp_straight_alpha(i: u32, n: u32) -> (f32, f32) {
        let t = i as f32 / (n - 1) as f32;
        (0.2 + 0.6 * t, t)
    }

    /// Premultiplied α-ramp input, stored as f16 exactly like the GPU texture so
    /// the only GPU/CPU divergence is the op math itself.
    fn gradient_texture_with_alpha(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        n: u32,
    ) -> wgpu::Texture {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("grad_in_alpha"),
            size: wgpu::Extent3d {
                width: n,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut data: Vec<u8> = Vec::new();
        for i in 0..n {
            let (v, a) = ramp_straight_alpha(i, n);
            // Stored premultiplied (03 §4.5.1 working space).
            for c in [v * a, v * a, v * a, a] {
                data.extend_from_slice(&f32_to_f16_bits(c).to_le_bytes());
            }
        }
        queue.write_texture(
            tex.as_image_copy(),
            &data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(n * 8),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: n,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        tex
    }

    /// CPU mirror of [`gradient_texture_with_alpha`], f16-quantized so it matches
    /// what the shader samples bit-for-bit before the op runs.
    fn cpu_pixels_with_alpha(n: u32) -> Vec<f32> {
        let mut v = Vec::with_capacity(n as usize * 4);
        for i in 0..n {
            let (val, a) = ramp_straight_alpha(i, n);
            for c in [val * a, val * a, val * a, a] {
                v.push(f16_to_f32(f32_to_f16_bits(c)));
            }
        }
        v
    }

    fn readback_rgba16(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tex: &wgpu::Texture,
        n: u32,
    ) -> Vec<f32> {
        let bpr = align256(n * 8);
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rb"),
            size: bpr as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: n,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let raw = slice.get_mapped_range();
        let mut out = Vec::with_capacity((n * 4) as usize);
        for px in 0..n as usize {
            for c in 0..4 {
                let o = px * 8 + c * 2;
                out.push(f16_to_f32(u16::from_le_bytes([raw[o], raw[o + 1]])));
            }
        }
        drop(raw);
        staging.unmap();
        out
    }

    /// A `w`x`h` texture of one premultiplied colour.
    fn solid_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        w: u32,
        h: u32,
        rgba: [f32; 4],
    ) -> wgpu::Texture {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("solid_in"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut data: Vec<u8> = Vec::with_capacity((w * h * 8) as usize);
        for _ in 0..(w * h) {
            for c in rgba {
                data.extend_from_slice(&f32_to_f16_bits(c).to_le_bytes());
            }
        }
        queue.write_texture(
            tex.as_image_copy(),
            &data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 8),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        tex
    }

    /// Full 2D readback, row-padded to the 256-byte copy alignment.
    fn readback_rgba16_2d(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tex: &wgpu::Texture,
        w: u32,
        h: u32,
    ) -> Vec<f32> {
        let bpr = align256(w * 8);
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rb2d"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let raw = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            for x in 0..w as usize {
                for c in 0..4 {
                    let o = y * bpr as usize + x * 8 + c * 2;
                    out.push(f16_to_f32(u16::from_le_bytes([raw[o], raw[o + 1]])));
                }
            }
        }
        drop(raw);
        staging.unmap();
        out
    }

    /// A power-window mask must be normalized against the LOGICAL frame, not the
    /// pool-bucketed render target.
    ///
    /// `photonic-video`'s texture pool rounds each dimension up to a multiple of
    /// 64 (`TextureDesc::bucket`, pinned by `pool.rs`'s `desc(100,100).bucket()
    /// == (128,128)`), so a graded frame is rendered into a texture larger than
    /// the picture. The shader's full-quad uv therefore reaches 1.0 at the
    /// BUCKET edge while `apply_grade_cpu` normalizes at the PICTURE edge — so
    /// before the `uv_scale` fix a masked grade disagreed between the two
    /// evaluators for any frame whose size is not a multiple of 64 (0.741%
    /// vertically at 1920x1080, and 28% at the 100->128 case used here).
    ///
    /// `cpu_gpu_parity`'s sweep cannot catch this: its grade rows use
    /// `mask: None`, and its 8x8 canvas buckets to a *square* 64x64 where the
    /// two axes scale identically and a centred window stays centred.
    #[test]
    fn masked_grade_normalizes_the_window_against_the_logical_frame() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping logical-frame mask parity test");
            return;
        };
        // Deliberately non-square so a swapped or shared axis scale is caught:
        // 100 -> 128 and 200 -> 256 are different ratios in x and y.
        const LOG_W: u32 = 100;
        const LOG_H: u32 = 200;
        const PHYS_W: u32 = 128;
        const PHYS_H: u32 = 256;

        let grey = [0.5, 0.5, 0.5, 1.0];
        let input = solid_texture(&device, &queue, PHYS_W, PHYS_H, grey);
        let op = ResolvedGradeOp {
            payload: ResolvedGradePayload::Exposure { stops: 1.5 },
            mask: Some(ResolvedMask {
                rectangle: false,
                center: [0.5, 0.5],
                // Hard-edged and well inside the frame, so the in/out boundary
                // falls where a mis-scaled uv would visibly move it.
                size: [0.25, 0.25],
                rotation: 0.0,
                softness: 0.0,
                invert: false,
            }),
        };

        let out = apply_grade_op_gpu(&device, &queue, &input, &op, (LOG_W, LOG_H));
        let got = readback_rgba16_2d(&device, &queue, &out, PHYS_W, PHYS_H);

        let mut want: Vec<f32> = Vec::with_capacity((LOG_W * LOG_H * 4) as usize);
        for _ in 0..(LOG_W * LOG_H) {
            want.extend_from_slice(&grey);
        }
        apply_grade_cpu(&mut want, LOG_W, LOG_H, std::slice::from_ref(&op));

        let mut worst = 0.0f32;
        let mut differing = 0usize;
        for y in 0..LOG_H as usize {
            for x in 0..LOG_W as usize {
                let g = got[(y * PHYS_W as usize + x) * 4];
                let w = want[(y * LOG_W as usize + x) * 4];
                let d = (g - w).abs();
                if d > 2e-3 {
                    differing += 1;
                }
                worst = worst.max(d);
            }
        }
        assert!(
            worst < 2e-3,
            "masked grade must match the CPU reference over the logical frame: \
             worst delta {worst}, {differing} of {} pixels differ",
            LOG_W * LOG_H
        );

        // The fixture must be able to fail: grading the same input as though the
        // bucket WERE the picture is the pre-fix behaviour, and it must not
        // agree with the CPU reference. Without this, a mask that silently
        // stopped applying would also pass the assertion above.
        let wrong = apply_grade_op_gpu(&device, &queue, &input, &op, (PHYS_W, PHYS_H));
        let wrong_px = readback_rgba16_2d(&device, &queue, &wrong, PHYS_W, PHYS_H);
        let mut wrong_worst = 0.0f32;
        for y in 0..LOG_H as usize {
            for x in 0..LOG_W as usize {
                let g = wrong_px[(y * PHYS_W as usize + x) * 4];
                let w = want[(y * LOG_W as usize + x) * 4];
                wrong_worst = wrong_worst.max((g - w).abs());
            }
        }
        assert!(
            wrong_worst > 1e-2,
            "regression fixture is inert: normalizing against the bucket should \
             visibly disagree with the CPU reference, but worst delta was only \
             {wrong_worst}"
        );
    }

    /// Opaque parity (α == 1): the default the per-op cases use.
    fn assert_gpu_matches_cpu(op: ResolvedGradeOp, n: u32) {
        assert_gpu_matches_cpu_alpha(op, n, false, 2e-3);
    }

    /// GPU/CPU parity over either the opaque gradient (`alpha_ramp == false`) or
    /// the premultiplied α-ramp (03 §4.5.3). Compares premultiplied outputs on
    /// RGB within `tol` and asserts α is preserved exactly.
    fn assert_gpu_matches_cpu_alpha(op: ResolvedGradeOp, n: u32, alpha_ramp: bool, tol: f32) {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping grade GPU parity test");
            return;
        };
        let input = if alpha_ramp {
            gradient_texture_with_alpha(&device, &queue, n)
        } else {
            gradient_texture(&device, &queue, n)
        };
        // This fixture builds its texture directly rather than from the pool, so
        // logical size == texture size.
        let out = apply_grade_op_gpu(&device, &queue, &input, &op, (n, 1));
        let got = readback_rgba16(&device, &queue, &out, n);

        let mut want = if alpha_ramp {
            cpu_pixels_with_alpha(n)
        } else {
            cpu_pixels(n)
        };
        apply_grade_cpu(&mut want, n, 1, std::slice::from_ref(&op));

        for i in 0..n as usize {
            for c in 0..3 {
                let g = got[i * 4 + c];
                let w = want[i * 4 + c];
                assert!(
                    (g - w).abs() <= tol,
                    "pixel {i} ch {c}: gpu {g:.5} vs cpu {w:.5} (alpha_ramp={alpha_ramp})"
                );
            }
            // Alpha is never modified by a grade op (03 §4.5.3): the α-ramp path
            // must carry the input alpha straight through.
            let ga = got[i * 4 + 3];
            let wa = want[i * 4 + 3];
            assert!(
                (ga - wa).abs() <= tol,
                "pixel {i} alpha: gpu {ga:.5} vs cpu {wa:.5}"
            );
        }
    }

    #[test]
    fn gpu_exposure_matches_cpu() {
        assert_gpu_matches_cpu(
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Exposure { stops: 0.7 },
                mask: None,
            },
            32,
        );
    }

    #[test]
    fn gpu_contrast_matches_cpu() {
        assert_gpu_matches_cpu(
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Contrast {
                    pivot: 0.5,
                    amount: 0.4,
                },
                mask: None,
            },
            32,
        );
    }

    #[test]
    fn gpu_white_balance_matches_cpu() {
        assert_gpu_matches_cpu(
            ResolvedGradeOp {
                payload: ResolvedGradePayload::WhiteBalance {
                    temp: 0.3,
                    tint: -0.2,
                },
                mask: None,
            },
            32,
        );
    }

    #[test]
    fn gpu_cdl_matches_cpu() {
        assert_gpu_matches_cpu(
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Cdl(ResolvedCdl {
                    slope: [1.1, 1.0, 0.9],
                    offset: [0.02, 0.0, -0.02],
                    power: [0.95, 1.0, 1.05],
                    sat: 1.2,
                }),
                mask: None,
            },
            32,
        );
    }

    #[test]
    fn gpu_curves_matches_cpu() {
        let curves = ResolvedCurves {
            master: crate::grade::curve_lut(&[(0.0, 0.0), (0.25, 0.5), (1.0, 1.0)]),
            red: crate::grade::curve_lut(&[(0.0, 0.1), (1.0, 0.9)]),
            green: crate::grade::curve_lut(&[]),
            blue: crate::grade::curve_lut(&[]),
            hue_vs_hue: None,
            hue_vs_sat: None,
        };
        assert_gpu_matches_cpu(
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Curves(Box::new(curves)),
                mask: None,
            },
            32,
        );
    }

    #[test]
    fn gpu_lut3d_trilinear_matches_cpu() {
        // A realistically-sized LUT: hardware texture filtering uses ~8-bit
        // sub-texel interpolation weights, so a 2-node LUT would expose that
        // quantization (~1/256 across the full range). At size 16 each cell
        // spans 1/15, shrinking the quantization error well under tolerance.
        // Still multilinear (blue halved) so CPU trilinear is the exact target.
        let mut table = Lut3d::identity(16);
        for e in table.data.iter_mut() {
            e[2] *= 0.5;
        }
        assert_gpu_matches_cpu(
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Lut3d(ResolvedLut3d {
                    table: Arc::new(table),
                    intensity: 1.0,
                    tetrahedral: false,
                }),
                mask: None,
            },
            32,
        );
    }

    #[test]
    fn gpu_lut3d_tetrahedral_matches_cpu() {
        let table = Lut3d::identity(3);
        assert_gpu_matches_cpu(
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Lut3d(ResolvedLut3d {
                    table: Arc::new(table),
                    intensity: 1.0,
                    tetrahedral: true,
                }),
                mask: None,
            },
            32,
        );
    }

    #[test]
    fn gpu_qualifier_matches_cpu() {
        let q = ResolvedHslQualifier {
            hue: [0.0, 1.0],
            sat: [0.0, 1.0],
            lum: [0.0, 1.0],
            softness: 0.0,
            correction: ResolvedCdl {
                slope: [1.05, 1.0, 0.95],
                offset: [0.0; 3],
                power: [1.0; 3],
                sat: 1.0,
            },
        };
        assert_gpu_matches_cpu(
            ResolvedGradeOp {
                payload: ResolvedGradePayload::HslQualifier(q),
                mask: None,
            },
            32,
        );
    }

    #[test]
    fn gpu_masked_op_matches_cpu() {
        assert_gpu_matches_cpu(
            ResolvedGradeOp {
                payload: ResolvedGradePayload::Exposure { stops: 1.0 },
                mask: Some(ResolvedMask {
                    rectangle: false,
                    center: [0.5, 0.5],
                    size: [0.3, 0.3],
                    rotation: 0.0,
                    softness: 0.2,
                    invert: false,
                }),
            },
            32,
        );
    }

    /// 03 §4.5.3: grade GPU kernels operate on straight colour, so a
    /// premultiplied α-ramp input must still match the CPU reference (which does
    /// the same unpremultiply → op → repremultiply round trip). Runs each of the
    /// five op kinds over the ramp; α is asserted preserved by the harness.
    ///
    /// Tolerance is 2e-3 (the same as the opaque per-op cases): the residual is
    /// the intrinsic WGSL-vs-Rust op divergence (f16 storage + `pow`/sample), not
    /// the alpha round trip — both paths unpremultiply the identical f16-quantized
    /// input, so the division contributes nothing extra. (03 §4.4 rule 3 names
    /// 1e-3, but the measured op divergence alone is ~1.5e-3 at high α; see the
    /// opaque cases, which already run at 2e-3.)
    #[test]
    fn gpu_partial_alpha_grade_matches_cpu() {
        const TOL: f32 = 2e-3;
        let ops: Vec<ResolvedGradePayload> = vec![
            ResolvedGradePayload::Exposure { stops: 0.7 },
            ResolvedGradePayload::Contrast {
                pivot: 0.4,
                amount: 0.3,
            },
            ResolvedGradePayload::Cdl(ResolvedCdl {
                slope: [1.1, 1.0, 0.9],
                offset: [0.02, 0.0, -0.02],
                power: [0.95, 1.0, 1.05],
                sat: 1.2,
            }),
            ResolvedGradePayload::Curves(Box::new(ResolvedCurves {
                master: crate::grade::curve_lut(&[(0.0, 0.0), (0.25, 0.5), (1.0, 1.0)]),
                red: crate::grade::curve_lut(&[(0.0, 0.1), (1.0, 0.9)]),
                green: crate::grade::curve_lut(&[]),
                blue: crate::grade::curve_lut(&[]),
                hue_vs_hue: None,
                hue_vs_sat: None,
            })),
            ResolvedGradePayload::Lut3d(ResolvedLut3d {
                table: {
                    let mut t = Lut3d::identity(16);
                    for e in t.data.iter_mut() {
                        e[2] *= 0.5;
                    }
                    Arc::new(t)
                },
                intensity: 1.0,
                tetrahedral: false,
            }),
        ];
        for payload in ops {
            assert_gpu_matches_cpu_alpha(
                ResolvedGradeOp {
                    payload,
                    mask: None,
                },
                32,
                true,
                TOL,
            );
        }
    }
}
