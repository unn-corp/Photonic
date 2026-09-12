//! wgpu evaluator (02 §2 "Evaluation").
//!
//! A topological walk of the [`FrameGraph`], one render pass per op, results
//! cached in [`NodeCache`] by [`ContentHash`] (02 §5). Working textures are
//! `Rgba16Float`, premultiplied, linear-light Rec.709 (D-09); `DecodeVideo`
//! reuses [`photonic_render::video::convert_yuv_planes_to_working`] for the
//! YUV→working upload so the GPU and CPU reference agree on the color math
//! (03 §4.4).
//!
//! ## P3 op coverage
//! - `SolidColor` — a uniform fill.
//! - `DecodeVideo` / `DecodeStill` / `RasterVector` — resolved by a
//!   [`GpuFrameSource`] (decode rings + the headless vector renderer at the
//!   session layer; never called for the solid-color paths tests exercise).
//! - `Merge` — a premultiplied `over` composite honouring all 26 blend modes
//!   (K-0.3a / 03 §2.4), the WGSL twin of `graph::ops::merge_pixel` + the CPU
//!   `blend_rgb` reference. `Normal` keeps the exact fast-path expression so the
//!   golden corpus does not shift; every other mode unpremultiplies, blends, and
//!   re-composites per the W3C model.
//! - `Grade` — the resolved grade stack, run through
//!   [`photonic_render::apply_grade_stack_gpu`] (07 §3), byte-for-byte the WGSL
//!   twin of `eval_cpu`'s `apply_grade_cpu` (GPU/CPU parity, 03 §4.4).
//! - `Effect{Invert}` — a real invert pass (08 §3). The other `Effect` kinds
//!   (Blur/Sharpen/Glow/ChromaKey/LumaKey/MaskShapeGen) stay blit-passthrough
//!   until their `ResolvedParams` payload finalizes (P5/P7).
//! - `Transform2D` — inverse-affine nearest/bilinear sampling matching the CPU
//!   reference's pixel-center and edge-clamp semantics.
//! - `Crop` / `Resize` / `Output` and the remaining passthrough ops — a texture
//!   blit.

use std::sync::Arc;

use photonic_core::layer::BlendMode;
use photonic_core::timeline::EffectKind;
use wgpu::util::DeviceExt;

use crate::contract::{AssetId, Tick, VectorRef, VectorStateKey};
use crate::graph::cache::{CacheStats, NodeCache};
use crate::graph::ir::{FitMode, FrameGraph, IrOp, TextureDesc, WipeDirection};
use crate::pool::DEFAULT_BUDGET_BYTES;

const WORKING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Uniform for the 26-mode merge pass: the blend-mode id and the layer opacity.
/// Same field layout as [`photonic_render::pipeline::CompositeParams`] so the two
/// composite paths stay in lockstep.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct MergeParams {
    mode: u32,
    opacity: f32,
    _pad: [f32; 2],
}

/// Stable numeric id for a blend mode, fed to the merge shader's `switch`.
///
/// This mirrors `photonic_render::pipeline::blend_mode_index` exactly (declaration
/// order, HSL modes ≥ 12, Photoshop extras 16..=25). The canonical mapping is
/// `pub(crate)` in `photonic-render`; once it is promoted to `pub` and re-exported
/// this local copy should be replaced by that re-export rather than kept forked.
/// The exhaustive `match` (no wildcard) makes any future `BlendMode` variant a
/// compile error here until it is given an id, so the fork cannot silently drift.
fn merge_mode_index(mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 1,
        BlendMode::Screen => 2,
        BlendMode::Overlay => 3,
        BlendMode::Darken => 4,
        BlendMode::Lighten => 5,
        BlendMode::ColorDodge => 6,
        BlendMode::ColorBurn => 7,
        BlendMode::HardLight => 8,
        BlendMode::SoftLight => 9,
        BlendMode::Difference => 10,
        BlendMode::Exclusion => 11,
        BlendMode::Hue => 12,
        BlendMode::Saturation => 13,
        BlendMode::Color => 14,
        BlendMode::Luminosity => 15,
        BlendMode::LinearDodge => 16,
        BlendMode::LinearBurn => 17,
        BlendMode::Subtract => 18,
        BlendMode::Divide => 19,
        BlendMode::VividLight => 20,
        BlendMode::LinearLight => 21,
        BlendMode::PinLight => 22,
        BlendMode::HardMix => 23,
        BlendMode::DarkerColor => 24,
        BlendMode::LighterColor => 25,
    }
}

/// The merge fragment shader body (26 blend modes). Working textures are
/// `Rgba16Float`, **premultiplied**, linear — so unlike `COMPOSITE_SHADER` (which
/// samples straight-alpha sRGB) this unpremultiplies before blending and keeps the
/// premultiplied source-over math of `graph::ops::merge_pixel` byte-for-byte:
/// `Normal` uses the exact fast-path expression (golden-corpus invariant); every
/// other mode blends `B(cb, cs)` then `Cs' = (1-αb)·Cs + αb·B`, and composites
/// `out = αs·Cs' + (1-αs)·bot`. The `blend_channel`/HSL helpers are the WGSL twins
/// of `photonic_core::raster::blend` (and identical to `COMPOSITE_SHADER`'s).
const MERGE_FS: &str = r#"
@group(0) @binding(0) var t_top: texture_2d<f32>;
@group(0) @binding(1) var t_bot: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
struct M { mode: u32, opacity: f32, pad: vec2<f32> }
@group(0) @binding(3) var<uniform> m: M;

fn screen1(cb: f32, cs: f32) -> f32 { return cb + cs - cb * cs; }
fn hard_light1(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) { return cb * (2.0 * cs); }
    return screen1(cb, 2.0 * cs - 1.0);
}
fn soft_light1(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) { return cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb); }
    var d: f32;
    if (cb <= 0.25) { d = ((16.0 * cb - 12.0) * cb + 4.0) * cb; } else { d = sqrt(cb); }
    return cb + (2.0 * cs - 1.0) * (d - cb);
}
fn color_dodge1(cb: f32, cs: f32) -> f32 {
    if (cb == 0.0) { return 0.0; }
    if (cs >= 1.0) { return 1.0; }
    return min(cb / (1.0 - cs), 1.0);
}
fn color_burn1(cb: f32, cs: f32) -> f32 {
    if (cb >= 1.0) { return 1.0; }
    if (cs <= 0.0) { return 0.0; }
    return 1.0 - min((1.0 - cb) / cs, 1.0);
}
fn vivid_light1(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) { return color_burn1(cb, min(2.0 * cs, 1.0)); }
    return color_dodge1(cb, min(2.0 * (cs - 0.5), 1.0));
}
fn blend_channel(mode: u32, cb: f32, cs: f32) -> f32 {
    switch (mode) {
        case 1u:  { return cb * cs; }
        case 2u:  { return screen1(cb, cs); }
        case 3u:  { return hard_light1(cs, cb); }
        case 4u:  { return min(cb, cs); }
        case 5u:  { return max(cb, cs); }
        case 6u:  { return color_dodge1(cb, cs); }
        case 7u:  { return color_burn1(cb, cs); }
        case 8u:  { return hard_light1(cb, cs); }
        case 9u:  { return soft_light1(cb, cs); }
        case 10u: { return abs(cb - cs); }
        case 11u: { return cb + cs - 2.0 * cb * cs; }
        case 16u: { return min(cb + cs, 1.0); }
        case 17u: { return max(cb + cs - 1.0, 0.0); }
        case 18u: { return max(cb - cs, 0.0); }
        case 19u: { if (cs <= 0.0) { return 1.0; } return min(cb / cs, 1.0); }
        case 20u: { return vivid_light1(cb, cs); }
        case 21u: { return clamp(cb + 2.0 * cs - 1.0, 0.0, 1.0); }
        case 22u: {
            if (cs <= 0.5) { return min(cb, 2.0 * cs); }
            return max(cb, 2.0 * cs - 1.0);
        }
        case 23u: {
            if (vivid_light1(cb, cs) < 0.5) { return 0.0; }
            return 1.0;
        }
        default:  { return cs; }
    }
}
fn lum(c: vec3<f32>) -> f32 { return 0.3 * c.r + 0.59 * c.g + 0.11 * c.b; }
fn clip_color(c: vec3<f32>) -> vec3<f32> {
    let l = lum(c);
    let n = min(min(c.r, c.g), c.b);
    let x = max(max(c.r, c.g), c.b);
    var o = c;
    if (n < 0.0) { o = vec3<f32>(l) + (o - vec3<f32>(l)) * (l / max(l - n, 1e-6)); }
    if (x > 1.0) { o = vec3<f32>(l) + (o - vec3<f32>(l)) * ((1.0 - l) / max(x - l, 1e-6)); }
    return o;
}
fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    let d = l - lum(c);
    return clip_color(c + vec3<f32>(d));
}
fn sat(c: vec3<f32>) -> f32 { return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b); }
fn set_sat(c: vec3<f32>, sval: f32) -> vec3<f32> {
    let cmin = min(min(c.r, c.g), c.b);
    let cmax = max(max(c.r, c.g), c.b);
    let rng = cmax - cmin;
    if (rng <= 0.0) { return vec3<f32>(0.0); }
    return (c - vec3<f32>(cmin)) * (sval / rng);
}
@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let top = textureSample(t_top, s, i.uv);
    let bot = textureSample(t_bot, s, i.uv);
    // Normal: exact fast-path expression, byte-for-byte the pre-K-0.3a shader so
    // the golden corpus does not shift.
    if (m.mode == 0u) {
        let t = top * m.opacity;
        return t + bot * (1.0 - t.a);
    }
    let a_s = top.a * m.opacity;
    let a_b = bot.a;
    var cs = vec3<f32>(0.0);
    if (top.a > 1e-6) { cs = top.rgb / top.a; }
    var cb = vec3<f32>(0.0);
    if (bot.a > 1e-6) { cb = bot.rgb / bot.a; }
    let mode = m.mode;
    var blended: vec3<f32>;
    if (mode == 12u)      { blended = set_lum(set_sat(cs, sat(cb)), lum(cb)); }
    else if (mode == 13u) { blended = set_lum(set_sat(cb, sat(cs)), lum(cb)); }
    else if (mode == 14u) { blended = set_lum(cs, lum(cb)); }
    else if (mode == 15u) { blended = set_lum(cb, lum(cs)); }
    else if (mode == 24u) { if (lum(cb) <= lum(cs)) { blended = cb; } else { blended = cs; } }
    else if (mode == 25u) { if (lum(cb) >= lum(cs)) { blended = cb; } else { blended = cs; } }
    else {
        blended = vec3<f32>(
            blend_channel(mode, cb.r, cs.r),
            blend_channel(mode, cb.g, cs.g),
            blend_channel(mode, cb.b, cs.b),
        );
    }
    let cs_prime = (1.0 - a_b) * cs + a_b * blended;
    let out_rgb = a_s * cs_prime + (1.0 - a_s) * bot.rgb;
    let out_a = a_s + a_b * (1.0 - a_s);
    return vec4<f32>(out_rgb, out_a);
}
"#;

/// `WipeMix` fragment shader (08 §2.0b) — the WGSL twin of `graph::ops::wipe`.
/// Reveal (`1` = incoming, `0` = outgoing) is a `smoothstep` edge at the eased
/// factor's remapped position `edge`, with a `hw` half-width band. The edge
/// position is derived from `@builtin(position)` and the LOGICAL canvas dims
/// (`dims.xy`) — matching the CPU pixel-center sweep coord — while the two layers
/// are sampled at the quad uv (same-pixel, like `MERGE_FS`). Premultiplied linear
/// is closed under lerp, so the blend is a plain `mix`.
const WIPE_FS: &str = r#"
@group(0) @binding(0) var t_in: texture_2d<f32>;
@group(0) @binding(1) var t_out: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
struct W { params: vec4<f32>, dims: vec4<f32> }
@group(0) @binding(3) var<uniform> u: W;
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
  let xn = i.pos.x / u.dims.x;
  let yn = i.pos.y / u.dims.y;
  let dir = i32(u.params.x + 0.5);
  var p = xn;
  if (dir == 1) { p = 1.0 - xn; }
  else if (dir == 2) { p = yn; }
  else if (dir == 3) { p = 1.0 - yn; }
  let edge = u.params.y;
  let hw = u.params.z;
  let reveal = 1.0 - smoothstep(edge - hw, edge + hw, p);
  let inc = textureSample(t_in, s, i.uv);
  let outg = textureSample(t_out, s, i.uv);
  return mix(outg, inc, reveal);
}
"#;

/// `LumaWipeMix` fragment shader (26 K-B7) — WGSL twin of `graph::ops::luma_wipe`.
/// Analytical map (kind 0..4) + soft_mix(t, m, soft); premultiplied lerp.
const LUMA_WIPE_FS: &str = r#"
@group(0) @binding(0) var t_in: texture_2d<f32>;
@group(0) @binding(1) var t_out: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
struct W { params: vec4<f32>, dims: vec4<f32> }
@group(0) @binding(3) var<uniform> u: W;
fn soft_mix(t: f32, m: f32, soft: f32) -> f32 {
  if (soft < 1e-6) {
    if (t >= m) { return 1.0; }
    return 0.0;
  }
  let lo = m - soft;
  let hi = m + soft;
  if (t <= lo) { return 0.0; }
  if (t >= hi) { return 1.0; }
  let x = clamp((t - lo) / (hi - lo), 0.0, 1.0);
  return x * x * (3.0 - 2.0 * x);
}
fn luma_map(kind: i32, uv: vec2<f32>, invert: bool) -> f32 {
  let u = clamp(uv.x, 0.0, 1.0);
  let v = clamp(uv.y, 0.0, 1.0);
  var t = 0.0;
  if (kind == 0) { t = u; }
  else if (kind == 1) { t = v; }
  else if (kind == 2) {
    let dx = u - 0.5;
    let dy = v - 0.5;
    let r = sqrt(dx * dx + dy * dy);
    t = clamp(r / 0.70710678118, 0.0, 1.0);
  } else if (kind == 3) {
    t = clamp(2.0 * abs(u - 0.5), 0.0, 1.0);
  } else {
    let dx = u - 0.5;
    let dy = v - 0.5;
    let a = atan2(dy, dx);
    var tt = (-a + 3.14159265359) / (2.0 * 3.14159265359);
    if (tt >= 1.0) { tt = 0.0; }
    t = tt;
  }
  t = clamp(t, 0.0, 1.0);
  if (invert) { return 1.0 - t; }
  return t;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
  let kind = i32(u.params.x + 0.5);
  let soft = u.params.y;
  let inv = u.params.z > 0.5;
  let t = u.params.w;
  let uv = vec2<f32>(i.pos.x / u.dims.x, i.pos.y / u.dims.y);
  let m = luma_map(kind, uv, inv);
  let reveal = soft_mix(t, m, soft);
  let inc = textureSample(t_in, s, i.uv);
  let outg = textureSample(t_out, s, i.uv);
  return mix(outg, inc, reveal);
}
"#;

/// `PushMix` fragment shader (08 §2.0b) — the WGSL twin of `graph::ops::push`.
/// Both layers translate along the direction axis by `t`; each output pixel picks
/// the incoming or outgoing layer and bilinear-samples it at the translated
/// pixel-center via `textureLoad` clamped to the source's LOGICAL dims (`dims.zw`)
/// — byte-for-byte the transform pass's edge-clamp/pixel-center semantics.
const PUSH_FS: &str = r#"
@group(0) @binding(0) var t_in: texture_2d<f32>;
@group(0) @binding(1) var t_out: texture_2d<f32>;
struct P { params: vec4<f32>, dims: vec4<f32> }
@group(0) @binding(2) var<uniform> u: P;
fn at_in(p: vec2<i32>) -> vec4<f32> {
  let hi = vec2<i32>(u.dims.zw) - vec2<i32>(1);
  return textureLoad(t_in, clamp(p, vec2<i32>(0), hi), 0);
}
fn at_out(p: vec2<i32>) -> vec4<f32> {
  let hi = vec2<i32>(u.dims.zw) - vec2<i32>(1);
  return textureLoad(t_out, clamp(p, vec2<i32>(0), hi), 0);
}
fn bilin_in(pf: vec2<f32>) -> vec4<f32> {
  let q = pf - vec2<f32>(0.5);
  let p0f = floor(q); let p0 = vec2<i32>(p0f); let f = q - p0f;
  return mix(mix(at_in(p0), at_in(p0 + vec2<i32>(1, 0)), f.x),
             mix(at_in(p0 + vec2<i32>(0, 1)), at_in(p0 + vec2<i32>(1, 1)), f.x), f.y);
}
fn bilin_out(pf: vec2<f32>) -> vec4<f32> {
  let q = pf - vec2<f32>(0.5);
  let p0f = floor(q); let p0 = vec2<i32>(p0f); let f = q - p0f;
  return mix(mix(at_out(p0), at_out(p0 + vec2<i32>(1, 0)), f.x),
             mix(at_out(p0 + vec2<i32>(0, 1)), at_out(p0 + vec2<i32>(1, 1)), f.x), f.y);
}
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  if (pos.x >= u.dims.x || pos.y >= u.dims.y) { return vec4<f32>(0.0); }
  let dir = i32(u.params.x + 0.5);
  let t = u.params.y;
  let horizontal = (dir == 0 || dir == 1);
  let forward = (dir == 0 || dir == 2);
  var uc: f32; var npx: f32;
  if (horizontal) { uc = pos.x / u.dims.x; npx = u.dims.x; }
  else { uc = pos.y / u.dims.y; npx = u.dims.y; }
  var uu = uc;
  if (!forward) { uu = 1.0 - uc; }
  var use_in = false; var src_u = 0.0;
  if (uu >= t) { use_in = false; src_u = uu - t; }
  else { use_in = true; src_u = uu - t + 1.0; }
  var along = src_u;
  if (!forward) { along = 1.0 - src_u; }
  var pf: vec2<f32>;
  if (horizontal) { pf = vec2<f32>(along * npx, pos.y); }
  else { pf = vec2<f32>(pos.x, along * npx); }
  if (use_in) { return bilin_in(pf); }
  return bilin_out(pf);
}
"#;

/// Shared wgpu device/queue handle (02 §1: "shares wgpu Device/Queue with
/// renderer"). Cheap to clone (two `Arc`s).
#[derive(Clone)]
pub struct GpuContext {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}

impl GpuContext {
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        GpuContext { device, queue }
    }

    pub fn device(&self) -> &Arc<wgpu::Device> {
        &self.device
    }

    pub fn queue(&self) -> &Arc<wgpu::Queue> {
        &self.queue
    }

    /// Request a headless GPU context (own adapter/device) for CLI/headless/MCP
    /// and tests. `None` when no adapter is available (CI without a GPU).
    pub fn request_blocking() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default(), None)).ok()?;
        Some(GpuContext::new(Arc::new(device), Arc::new(queue)))
    }
}

/// Resolves source ops (`DecodeVideo`/`DecodeStill`/`RasterVector`) to working
/// textures. The session implements it over decode rings + the headless vector
/// renderer; tests that use only `SolidColor`/`Merge` never trigger it. A `None`
/// return means "not available yet" — dependent results remain incomplete.
pub trait GpuFrameSource {
    /// Source context absent from the graph, including vector document updates
    /// and approximate samples. Change it whenever identical source ops can
    /// resolve to different pixels. Zero retains the default exact namespace.
    fn cache_namespace(&self) -> u64 {
        0
    }

    fn video_texture(
        &mut self,
        gpu: &GpuContext,
        asset: AssetId,
        src_time: Tick,
        proxy: bool,
    ) -> Option<GpuFrame>;

    /// `w`/`h` are the **logical** size the still is wanted at — the canvas, in
    /// picture pixels, never a pool bucket size. The provider may return a
    /// smaller frame (it must not upscale a small still); the evaluator
    /// normalizes whatever comes back to the canvas. Mirrors the CPU
    /// reference's [`FrameProvider::decode_still`](crate::graph::eval_cpu::FrameProvider::decode_still),
    /// which has carried the canvas hint from the start; see 26 K-C8 / 32 §9 for
    /// why the cache behind it must key on this size.
    fn still_texture(
        &mut self,
        gpu: &GpuContext,
        asset: AssetId,
        w: u32,
        h: u32,
    ) -> Option<GpuFrame>;

    fn vector_texture(
        &mut self,
        gpu: &GpuContext,
        vref: VectorRef,
        key: VectorStateKey,
        w: u32,
        h: u32,
    ) -> Option<GpuFrame>;
}

/// A source texture plus its exact logical dimensions. The physical texture
/// may be pool-bucket padded; sampling must never infer content bounds from it.
#[derive(Clone)]
pub struct GpuFrame {
    pub texture: Arc<wgpu::Texture>,
    pub width: u32,
    pub height: u32,
}

impl GpuFrame {
    pub fn new(texture: Arc<wgpu::Texture>, width: u32, height: u32) -> Self {
        Self {
            texture,
            width: width.max(1),
            height: height.max(1),
        }
    }
}

/// A source that resolves nothing (transparent everywhere) — for tests /
/// solid-color graphs with no media.
pub struct NullFrameSource;

impl GpuFrameSource for NullFrameSource {
    fn video_texture(&mut self, _: &GpuContext, _: AssetId, _: Tick, _: bool) -> Option<GpuFrame> {
        None
    }
    fn still_texture(&mut self, _: &GpuContext, _: AssetId, _: u32, _: u32) -> Option<GpuFrame> {
        None
    }
    fn vector_texture(
        &mut self,
        _: &GpuContext,
        _: VectorRef,
        _: VectorStateKey,
        _: u32,
        _: u32,
    ) -> Option<GpuFrame> {
        None
    }
}

/// The wgpu evaluator: pipelines + the node-result cache.
pub struct Evaluator {
    gpu: GpuContext,
    passes: Passes,
    cache: NodeCache,
    /// Glyphon caption text compositor (06 §5.3) — burns `CaptionOverlay` glyph
    /// runs over the working texture. Owns its glyphon state behind a `Mutex` so
    /// it composites through `&self` like the rest of `render_op`.
    caption: photonic_render::caption::CaptionCompositor,
    /// The content hash of the last presented output, pinned in the cache.
    pinned_output: Option<crate::graph::ir::ContentHash>,
    /// The content hash of the last published scope tap (K-E2), pinned for the
    /// same reason the output is: the GUI/MCP holds that `Arc<Texture>` past the
    /// end of the frame, and an unpinned intermediate is a legal LRU recycle
    /// target — the next frame would then render another node into the texture
    /// the scopes are still measuring.
    pinned_tap: Option<crate::graph::ir::ContentHash>,
    source_namespace: u64,
}

impl Evaluator {
    pub fn new(gpu: GpuContext) -> Self {
        Self::with_budget(gpu, DEFAULT_BUDGET_BYTES)
    }

    pub fn with_budget(gpu: GpuContext, budget_bytes: u64) -> Self {
        let passes = Passes::new(gpu.device());
        let cache = NodeCache::new(gpu.device().clone(), budget_bytes);
        let caption = photonic_render::caption::CaptionCompositor::new(
            gpu.device(),
            gpu.queue(),
            WORKING_FORMAT,
        );
        Evaluator {
            gpu,
            passes,
            cache,
            caption,
            pinned_output: None,
            pinned_tap: None,
            source_namespace: 0,
        }
    }

    pub fn gpu(&self) -> &GpuContext {
        &self.gpu
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    pub fn set_cache_budget_bytes(&mut self, bytes: u64) {
        self.cache.set_budget_bytes(bytes);
    }

    fn evaluation_hash(
        &self,
        hash: crate::graph::ir::ContentHash,
        w: u32,
        h: u32,
    ) -> crate::graph::ir::ContentHash {
        let mut hash = evaluation_hash(hash, w, h);
        if self.source_namespace != 0 {
            // Exact evaluation keeps its stable namespace; approximations must
            // never satisfy a later paused/export lookup for the same tick.
            hash.0 ^= (self.source_namespace as u128) << 64;
            hash.0 ^= 0x7363_7275_625f_6170_7072_6f78_696d_6174_u128;
        }
        hash
    }

    /// Evict cached results whose content hash matches `pred` (asset relink /
    /// proxy swap — `InvalidateRange`, 02 §5). Call between frames.
    pub fn invalidate_matching(&mut self, pred: impl Fn(crate::graph::ir::ContentHash) -> bool) {
        self.cache.invalidate_matching(pred);
    }

    /// Evaluate `graph` at canvas size `canvas`, returning the pinned output
    /// texture (or `None` for an empty graph). Sources are resolved via `source`.
    pub fn evaluate(
        &mut self,
        graph: &FrameGraph,
        canvas: (u32, u32),
        source: &mut dyn GpuFrameSource,
    ) -> Option<Arc<wgpu::Texture>> {
        self.evaluate_with_tap(graph, canvas, source, None).0
    }

    /// [`Evaluator::evaluate`] that also hands back one **intermediate** node's
    /// texture — the K-E2 scope tap (03 §3.6 / 07 §5).
    ///
    /// This is the whole reason a per-clip scope tap is not a second render:
    /// `tap` names a node of the same graph, and this evaluator already
    /// materialises every node in the graph on its way to the output, so the tap
    /// costs one `Arc` clone and one cache pin — **no extra evaluation**, no
    /// second compile, and nothing added to the per-frame budget of 02 §8. A
    /// `tap` outside the graph (stale id) yields `None` rather than panicking.
    /// The tap is returned as a [`GpuFrame`] and not a bare texture because a
    /// pooled texture is bucket-padded: a scope that measured the physical
    /// extent would fold the padding into every histogram. The logical size
    /// rides along so the readback crops correctly.
    pub fn evaluate_with_tap(
        &mut self,
        graph: &FrameGraph,
        canvas: (u32, u32),
        source: &mut dyn GpuFrameSource,
        tap: Option<crate::graph::ir::IrNodeId>,
    ) -> (Option<Arc<wgpu::Texture>>, Option<GpuFrame>) {
        let (output, tap) = self.evaluate_with_tap_frame(graph, canvas, source, tap);
        (output.map(|frame| frame.texture), tap)
    }

    /// [`Evaluator::evaluate_with_tap`] with the output's logical dimensions
    /// preserved alongside its texture. The session uses this for presentation
    /// because an asset peek can have a different logical size than the active
    /// sequence format even though both use pooled textures.
    pub fn evaluate_with_tap_frame(
        &mut self,
        graph: &FrameGraph,
        canvas: (u32, u32),
        source: &mut dyn GpuFrameSource,
        tap: Option<crate::graph::ir::IrNodeId>,
    ) -> (Option<GpuFrame>, Option<GpuFrame>) {
        self.source_namespace = source.cache_namespace();
        let (cw, ch) = (canvas.0.max(1), canvas.1.max(1));
        let mut results: Vec<Option<GpuFrame>> = (0..graph.nodes.len()).map(|_| None).collect();

        for (i, node) in graph.nodes.iter().enumerate() {
            let inputs: Vec<GpuFrame> = node
                .inputs
                .iter()
                .filter_map(|(id, _)| results[id.0 as usize].clone())
                .collect();

            let out = match &node.op {
                IrOp::DecodeVideo {
                    asset,
                    src_time,
                    proxy,
                } => match source.video_texture(&self.gpu, *asset, *src_time, *proxy) {
                    Some(frame) => {
                        Some(self.normalize_source_cached(node.content_hash, frame, cw, ch))
                    }
                    None => None,
                },
                // K-C8: the still is requested at the LOGICAL canvas size
                // (`cw`/`ch` — already preview-scaled by `preview_canvas`), not
                // at a pool bucket size. The provider caches on exactly that.
                IrOp::DecodeStill { asset } => {
                    match source.still_texture(&self.gpu, *asset, cw, ch) {
                        Some(frame) => {
                            Some(self.normalize_source_cached(node.content_hash, frame, cw, ch))
                        }
                        None => None,
                    }
                }
                IrOp::RasterVector {
                    vref,
                    doc_state,
                    w,
                    h,
                } => match source.vector_texture(&self.gpu, *vref, *doc_state, *w, *h) {
                    Some(frame) => {
                        Some(self.normalize_source_cached(node.content_hash, frame, cw, ch))
                    }
                    None => None,
                },
                _ if inputs.len() == node.inputs.len() => {
                    Some(self.render_cached(node, &inputs, cw, ch))
                }
                _ => None,
            };
            results[i] = out;
        }

        // All source nodes above are visited, including independent misses, so
        // they can request work concurrently. Keep the last complete program
        // and scope pins until the requested output is actually ready.
        if graph
            .output
            .is_some_and(|id| results[id.0 as usize].is_none())
        {
            return (None, None);
        }

        // The scope tap (K-E2): a node already computed above. Pinned like the
        // output so the consumer's `Arc` cannot alias a recycled pool texture.
        let tap_hit = tap
            .filter(|id| (id.0 as usize) < graph.nodes.len())
            .and_then(|id| {
                results[id.0 as usize].clone().map(|frame| {
                    (
                        self.evaluation_hash(graph.nodes[id.0 as usize].content_hash, cw, ch),
                        frame,
                    )
                })
            });
        match &tap_hit {
            Some((hash, _)) => {
                if let Some(prev) = self.pinned_tap.replace(*hash) {
                    if prev != *hash {
                        self.cache.unpin(prev);
                    }
                }
                self.cache.pin(*hash);
            }
            None => {
                if let Some(prev) = self.pinned_tap.take() {
                    self.cache.unpin(prev);
                }
            }
        }
        let tap_tex = tap_hit.map(|(_, frame)| frame);

        let Some(out_node) = graph.output else {
            return (None, tap_tex);
        };
        let out_hash = self.evaluation_hash(graph.nodes[out_node.0 as usize].content_hash, cw, ch);
        // Pin the displayed output; unpin the previous one (03 §3.4 exception 1).
        if let Some(prev) = self.pinned_output.replace(out_hash) {
            if prev != out_hash {
                self.cache.unpin(prev);
            }
        }
        self.cache.pin(out_hash);
        let out_frame = results[out_node.0 as usize].clone();
        (out_frame, tap_tex)
    }

    /// Render a computed (non-source) op into a cached texture, or return the
    /// cache hit unchanged.
    fn render_cached(
        &mut self,
        node: &crate::graph::ir::IrNode,
        inputs: &[GpuFrame],
        cw: u32,
        ch: u32,
    ) -> GpuFrame {
        let (w, h) = op_size(&node.op, cw, ch);
        let desc = TextureDesc {
            width: w,
            height: h,
        };
        let hash = self.evaluation_hash(node.content_hash, cw, ch);
        let (target, valid) = self.cache.lookup_or_alloc(hash, desc);
        if valid {
            return GpuFrame::new(target, w, h);
        }
        self.render_op(&node.op, inputs, &target, w, h);
        self.cache.mark_rendered(hash);
        GpuFrame::new(target, w, h)
    }

    fn normalize_source_cached(
        &mut self,
        hash: crate::graph::ir::ContentHash,
        source: GpuFrame,
        width: u32,
        height: u32,
    ) -> GpuFrame {
        if source.width == width && source.height == height {
            return source;
        }
        let hash = self.evaluation_hash(hash, width, height);
        let desc = TextureDesc { width, height };
        let (target, valid) = self.cache.lookup_or_alloc(hash, desc);
        if !valid {
            self.passes.transform(
                &self.gpu,
                &source.texture,
                &target,
                glam::Mat3::IDENTITY,
                crate::graph::ir::Sampling::Bilinear,
                width,
                height,
                source.width,
                source.height,
            );
            self.cache.mark_rendered(hash);
        }
        GpuFrame::new(target, width, height)
    }

    fn render_op(
        &self,
        op: &IrOp,
        inputs: &[GpuFrame],
        target: &wgpu::Texture,
        logical_w: u32,
        logical_h: u32,
    ) {
        match op {
            IrOp::SolidColor { color } => {
                self.passes
                    .fill(&self.gpu, target, [color.r, color.g, color.b, color.a]);
            }
            IrOp::Deinterlace {
                method,
                field_order,
            } => match inputs.first() {
                Some(src) => self.passes.deinterlace(
                    &self.gpu,
                    &src.texture,
                    target,
                    *method,
                    *field_order,
                    logical_w,
                    logical_h,
                ),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Merge { mode, opacity } => match (inputs.first(), inputs.get(1)) {
                (Some(top), Some(bottom)) => {
                    self.passes.merge(
                        &self.gpu,
                        &top.texture,
                        &bottom.texture,
                        *mode,
                        *opacity,
                        target,
                    );
                }
                (Some(only), None) | (None, Some(only)) => {
                    self.passes.blit(&self.gpu, &only.texture, target);
                }
                (None, None) => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            // Directional transitions (08 §2.0b): inputs [incoming, outgoing], the
            // WGSL twins of `ops::wipe` / `ops::push`.
            IrOp::WipeMix {
                direction,
                softness,
                t,
            } => match (inputs.first(), inputs.get(1)) {
                (Some(incoming), Some(outgoing)) => self.passes.wipe(
                    &self.gpu,
                    &incoming.texture,
                    &outgoing.texture,
                    *direction,
                    *softness,
                    *t,
                    target,
                    logical_w,
                    logical_h,
                ),
                (Some(only), None) | (None, Some(only)) => {
                    self.passes.blit(&self.gpu, &only.texture, target)
                }
                (None, None) => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::PushMix { direction, t } => match (inputs.first(), inputs.get(1)) {
                (Some(incoming), Some(outgoing)) => self.passes.push(
                    &self.gpu,
                    &incoming.texture,
                    &outgoing.texture,
                    *direction,
                    *t,
                    target,
                    logical_w,
                    logical_h,
                    incoming.width,
                    incoming.height,
                ),
                (Some(only), None) | (None, Some(only)) => {
                    self.passes.blit(&self.gpu, &only.texture, target)
                }
                (None, None) => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::LumaWipeMix {
                kind,
                softness,
                invert,
                t,
            } => match (inputs.first(), inputs.get(1)) {
                (Some(incoming), Some(outgoing)) => self.passes.luma_wipe(
                    &self.gpu,
                    &incoming.texture,
                    &outgoing.texture,
                    *kind,
                    *softness,
                    *invert,
                    *t,
                    target,
                    logical_w,
                    logical_h,
                ),
                (Some(only), None) | (None, Some(only)) => {
                    self.passes.blit(&self.gpu, &only.texture, target)
                }
                (None, None) => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            // Styled-text generator (G-12 title clips): start transparent, then
            // burn the resolved cue on top via the SAME glyphon compositor the
            // `CaptionOverlay` path uses (06 §5.3) — text over transparent, which
            // the clip's `Transform2D`/`Merge` then places in the frame. An absent
            // cue (empty string) stays transparent.
            IrOp::TextGen { block } => {
                self.passes.fill(&self.gpu, target, [0.0; 4]);
                if let Some(cue) = &block.cue {
                    self.caption.composite(
                        self.gpu.device(),
                        self.gpu.queue(),
                        target,
                        std::slice::from_ref(cue),
                        target.width(),
                        target.height(),
                    );
                }
            }
            // Caption overlay (06 §5.3): lay the input composite down, then burn
            // the resolved glyph runs on top via the glyphon pipeline. glyphon's
            // `ALPHA_BLENDING` + `Accurate` sRGB→linear conversion make this a
            // correct straight-alpha `over` onto the premultiplied linear target.
            IrOp::CaptionOverlay { cue_batch } => {
                match inputs.first() {
                    Some(src) => self.passes.blit(&self.gpu, &src.texture, target),
                    None => self.passes.fill(&self.gpu, target, [0.0; 4]),
                }
                if !cue_batch.cues.is_empty() {
                    self.caption.composite(
                        self.gpu.device(),
                        self.gpu.queue(),
                        target,
                        &cue_batch.cues,
                        target.width(),
                        target.height(),
                    );
                }
            }
            // Real effect kernels for all seven v1 kinds (08 §3 / K-0.2), the
            // WGSL twins of the `ops::*` kernels the CPU reference runs.
            // Unknown/forward-compat kinds fall through to the blit passthrough.
            IrOp::Effect {
                kind: EffectKind::Invert,
                ..
            } => match inputs.first() {
                Some(src) => self.passes.invert(&self.gpu, &src.texture, target),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Effect {
                kind: EffectKind::Deflicker,
                params,
            } => match inputs.first() {
                Some(src) => {
                    let amp = params.f32_or("params.band_amp", 0.0);
                    let band = (amp.abs() > 1e-6).then(|| crate::graph::rolling_bands::BandModel {
                        cycles: params.f32_or("params.band_cycles", 0.0).max(0.0) as u32,
                        amplitude: amp,
                        phase: params.f32_or("params.band_phase", 0.0),
                        confidence: 1.0,
                    });
                    self.passes.deflicker(
                        &self.gpu,
                        &src.texture,
                        target,
                        [
                            params.f32_or("params.gain_r", 1.0),
                            params.f32_or("params.gain_g", 1.0),
                            params.f32_or("params.gain_b", 1.0),
                        ],
                        band,
                        logical_h,
                    )
                }
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Effect {
                kind: EffectKind::Blur,
                params,
            } => match inputs.first() {
                Some(src) => self.passes.gaussian_blur(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.radius", 0.0),
                    logical_w,
                    logical_h,
                ),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Effect {
                kind: EffectKind::Sharpen,
                params,
            } => match inputs.first() {
                Some(src) => self.passes.sharpen(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.amount", 0.0),
                    params.f32_or("params.radius", 0.0),
                    logical_w,
                    logical_h,
                ),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Effect {
                kind: EffectKind::Glow,
                params,
            } => match inputs.first() {
                Some(src) => {
                    let tint = params.color_or("params.tint", photonic_core::Color::WHITE);
                    self.passes.glow(
                        &self.gpu,
                        &src.texture,
                        target,
                        params.f32_or("params.radius", 0.0),
                        params.f32_or("params.threshold", 0.0),
                        params.f32_or("params.intensity", 0.0),
                        [
                            crate::graph::ops::srgb_to_linear(tint.r),
                            crate::graph::ops::srgb_to_linear(tint.g),
                            crate::graph::ops::srgb_to_linear(tint.b),
                        ],
                        logical_w,
                        logical_h,
                    );
                }
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Effect {
                kind: EffectKind::LumaKey,
                params,
            } => match inputs.first() {
                Some(src) => self.passes.luma_key(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.threshold", 0.0),
                    params.f32_or("params.softness", 0.0),
                    params.bool_or("params.invert", false),
                ),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Effect {
                kind: EffectKind::ChromaKey,
                params,
            } => match inputs.first() {
                Some(src) => {
                    let key = params.color_or("params.key_color", photonic_core::Color::BLACK);
                    self.passes.chroma_key(
                        &self.gpu,
                        &src.texture,
                        target,
                        [
                            crate::graph::ops::srgb_to_linear(key.r),
                            crate::graph::ops::srgb_to_linear(key.g),
                            crate::graph::ops::srgb_to_linear(key.b),
                        ],
                        params.f32_or("params.tolerance", 0.0),
                        params.f32_or("params.edge_softness", 0.0),
                        params.f32_or("params.spill_suppress", 0.0),
                    );
                }
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            // K-B16: bridged raster ids lower as Unknown(tag). GPU ports for the
            // Tier-1 neighbourhood/point kernels; remaining bridge ids blit
            // (CPU is the oracle until their WGSL twins land).
            IrOp::Effect {
                kind: EffectKind::Unknown(tag),
                params,
            } => match (tag.as_str(), inputs.first()) {
                ("blur.box" | "blur.gaussian", Some(src)) => self.passes.gaussian_blur(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.radius", 1.0),
                    logical_w,
                    logical_h,
                ),
                ("sharpen.unsharp_raster", Some(src)) => self.passes.sharpen(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.amount", 1.0),
                    params.f32_or("params.radius", 1.0),
                    logical_w,
                    logical_h,
                ),
                ("filter.high_pass", Some(src)) => {
                    let blurred = self.passes.temp_texture(&self.gpu, target);
                    self.passes.gaussian_blur(
                        &self.gpu,
                        &src.texture,
                        &blurred,
                        params.f32_or("params.radius", 2.0),
                        logical_w,
                        logical_h,
                    );
                    self.gpu.device().poll(wgpu::Maintain::Wait);
                    self.passes
                        .high_pass_combine(&self.gpu, &src.texture, &blurred, target);
                }
                ("stylize.emboss", Some(src)) => {
                    self.passes
                        .emboss(&self.gpu, &src.texture, target, logical_w, logical_h)
                }
                ("stylize.find_edges", Some(src)) => {
                    self.passes
                        .find_edges(&self.gpu, &src.texture, target, logical_w, logical_h)
                }
                ("filter.median", Some(src)) => self.passes.median(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.radius", 1.0),
                    logical_w,
                    logical_h,
                ),
                // Point ops — WGSL twins of Transfer/Linear catalogue kernels.
                ("color.levels", Some(src)) => self.passes.levels(
                    &self.gpu,
                    &src.texture,
                    target,
                    [
                        params.f32_or("params.in_black", 0.0),
                        params.f32_or("params.in_white", 1.0),
                        params.f32_or("params.gamma", 1.0),
                        params.f32_or("params.out_black", 0.0),
                        params.f32_or("params.out_white", 1.0),
                    ],
                ),
                ("color.posterize", Some(src)) => self.passes.posterize(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.levels", 4.0),
                ),
                ("color.threshold", Some(src)) => self.passes.threshold(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.level", 0.5),
                ),
                ("color.desaturate" | "color.invert_raster", Some(src)) => {
                    if tag.as_str() == "color.invert_raster" {
                        self.passes.invert(&self.gpu, &src.texture, target);
                    } else {
                        self.passes.desaturate(&self.gpu, &src.texture, target);
                    }
                }
                ("stylize.vignette", Some(src)) => self.passes.vignette(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.amount", -0.5),
                    params.f32_or("params.feather", 0.5),
                    logical_w,
                    logical_h,
                ),
                ("stylize.mosaic", Some(src)) => self.passes.mosaic(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.block", 8.0),
                    logical_w,
                    logical_h,
                ),
                ("blur.motion", Some(src)) => self.passes.motion_blur(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.angle", 0.0),
                    params.f32_or("params.distance", 8.0),
                    logical_w,
                    logical_h,
                ),
                ("color.hue_saturation", Some(src)) => self.passes.hue_saturation(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.hue", 0.0),
                    params.f32_or("params.saturation", 0.0),
                    params.f32_or("params.lightness", 0.0),
                ),
                ("color.vibrance", Some(src)) => self.passes.vibrance(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.amount", 0.0),
                ),
                ("color.channel_mixer", Some(src)) => self.passes.channel_mixer(
                    &self.gpu,
                    &src.texture,
                    target,
                    [
                        params.f32_or("params.rr", 1.0),
                        params.f32_or("params.rg", 0.0),
                        params.f32_or("params.rb", 0.0),
                        params.f32_or("params.gr", 0.0),
                        params.f32_or("params.gg", 1.0),
                        params.f32_or("params.gb", 0.0),
                        params.f32_or("params.br", 0.0),
                        params.f32_or("params.bg", 0.0),
                        params.f32_or("params.bb", 1.0),
                    ],
                ),
                ("color.curves", Some(src)) => {
                    let contrast = params.f32_or("params.contrast", 0.0);
                    let mut knots = [
                        [
                            params.f32_or("params.p0x", 0.0),
                            params.f32_or("params.p0y", 0.0),
                        ],
                        [
                            params.f32_or("params.p1x", 0.25),
                            params.f32_or("params.p1y", 0.25),
                        ],
                        [
                            params.f32_or("params.p2x", 0.5),
                            params.f32_or("params.p2y", 0.5),
                        ],
                        [
                            params.f32_or("params.p3x", 0.75),
                            params.f32_or("params.p3y", 0.75),
                        ],
                        [
                            params.f32_or("params.p4x", 1.0),
                            params.f32_or("params.p4y", 1.0),
                        ],
                    ];
                    if contrast.abs() > 1e-6 {
                        knots[2][1] = (0.5 + contrast.clamp(-1.0, 1.0) * 0.25).clamp(0.05, 0.95);
                    }
                    self.passes
                        .curves_lut(&self.gpu, &src.texture, target, &knots);
                }
                ("blur.surface", Some(src)) => self.passes.surface_blur(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.radius", 2.0),
                    params.f32_or("params.threshold", 0.25),
                    logical_w,
                    logical_h,
                ),
                ("blur.lens", Some(src)) => self.passes.lens_blur(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.radius", 4.0),
                    logical_w,
                    logical_h,
                ),
                ("color.black_and_white", Some(src)) => self.passes.black_and_white(
                    &self.gpu,
                    &src.texture,
                    target,
                    [
                        params.f32_or("params.wr", 0.299),
                        params.f32_or("params.wg", 0.587),
                        params.f32_or("params.wb", 0.114),
                    ],
                ),
                ("sharpen.smart", Some(src)) => self.passes.smart_sharpen(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.amount", 1.0),
                    params.f32_or("params.radius", 1.0),
                    params.f32_or("params.threshold", 0.0),
                    logical_w,
                    logical_h,
                ),
                ("geo.pinch" | "geo.spherize", Some(src)) => self.passes.pinch(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.amount", 0.0),
                    tag.as_str() == "geo.spherize",
                    logical_w,
                    logical_h,
                ),
                ("geo.ripple", Some(src)) => self.passes.ripple(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.amplitude", 4.0),
                    params.f32_or("params.wavelength", 16.0),
                    logical_w,
                    logical_h,
                ),
                ("geo.perspective", Some(src)) => self.passes.perspective(
                    &self.gpu,
                    &src.texture,
                    target,
                    [
                        params.f32_or("params.tl_x", 0.0),
                        params.f32_or("params.tl_y", 0.0),
                        params.f32_or("params.tr_x", 1.0),
                        params.f32_or("params.tr_y", 0.0),
                        params.f32_or("params.br_x", 1.0),
                        params.f32_or("params.br_y", 1.0),
                        params.f32_or("params.bl_x", 0.0),
                        params.f32_or("params.bl_y", 1.0),
                    ],
                    logical_w,
                    logical_h,
                ),
                ("stylize.grain", Some(src)) => self.passes.grain(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.amount", 0.1),
                    params.f32_or("params.monochrome", 1.0) > 0.5,
                    params.f32_or("params.seed", 1.0),
                    logical_w,
                    logical_h,
                ),
                ("stylize.chromatic_aberration", Some(src)) => self.passes.chromatic_aberration(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.amount", 2.0),
                    logical_w,
                    logical_h,
                ),
                ("util.unpremultiply", Some(src)) => {
                    self.passes.unpremultiply(&self.gpu, &src.texture, target)
                }
                ("util.alpha_view", Some(src)) => self.passes.alpha_view(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.mode", 0.0),
                ),
                ("util.drop_shadow", Some(src)) => self.passes.drop_shadow(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.x", 4.0),
                    params.f32_or("params.y", 4.0),
                    params.f32_or("params.radius", 3.0),
                    [
                        params.f32_or("params.r", 0.0),
                        params.f32_or("params.g", 0.0),
                        params.f32_or("params.b", 0.0),
                    ],
                    params.f32_or("params.opacity", 0.5),
                    logical_w,
                    logical_h,
                ),
                ("util.outline", Some(src)) => self.passes.outline(
                    &self.gpu,
                    &src.texture,
                    target,
                    params.f32_or("params.thickness", 2.0),
                    [
                        params.f32_or("params.r", 1.0),
                        params.f32_or("params.g", 1.0),
                        params.f32_or("params.b", 1.0),
                    ],
                    params.f32_or("params.opacity", 1.0),
                    logical_w,
                    logical_h,
                ),
                (_, Some(src)) => self.passes.blit(&self.gpu, &src.texture, target),
                (_, None) => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Effect {
                kind: EffectKind::MaskShapeGen,
                params,
            } => self.passes.mask_shape(
                &self.gpu,
                target,
                [
                    params.f32_or("params.center_x", 0.5),
                    params.f32_or("params.center_y", 0.5),
                ],
                [
                    params.f32_or("params.size_x", 0.5),
                    params.f32_or("params.size_y", 0.5),
                ],
                params.f32_or("params.rotation", 0.0),
                params.f32_or("params.feather", 0.0),
                logical_w,
                logical_h,
            ),
            IrOp::Transform2D { mat, sampling } => match inputs.first() {
                Some(src) => self.passes.transform(
                    &self.gpu,
                    &src.texture,
                    target,
                    *mat,
                    *sampling,
                    logical_w,
                    logical_h,
                    src.width,
                    src.height,
                ),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::StabilizeWarp { warp, sampling } => match inputs.first() {
                Some(src) => self.passes.stabilize(
                    &self.gpu,
                    &src.texture,
                    warp,
                    *sampling,
                    logical_w,
                    logical_h,
                    src.width,
                    src.height,
                    target,
                ),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Resize { fit, .. } => match inputs.first() {
                Some(src) => self.passes.resize(
                    &self.gpu,
                    &src.texture,
                    target,
                    *fit,
                    logical_w,
                    logical_h,
                    src.width,
                    src.height,
                ),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            IrOp::Crop {
                left,
                top,
                right,
                bottom,
            } => match inputs.first() {
                Some(src) => self.passes.crop(
                    &self.gpu,
                    &src.texture,
                    target,
                    logical_w,
                    logical_h,
                    src.width,
                    src.height,
                    *left,
                    *top,
                    *right,
                    *bottom,
                ),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            // The terminal output is also the boundary between a logical
            // frame and its bucket-padded backing texture. Resample only the
            // logical source extent so padding never becomes visible in the
            // output's logical pixels.
            IrOp::Output { .. } => match inputs.first() {
                Some(src) => self.passes.resize(
                    &self.gpu,
                    &src.texture,
                    target,
                    FitMode::Stretch,
                    logical_w,
                    logical_h,
                    src.width,
                    src.height,
                ),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            // Real grade kernel: run the resolved stack through the WGSL twins of
            // `eval_cpu`'s `apply_grade_cpu`, then blit the result into the pooled
            // cache target (07 §3, GPU/CPU parity 03 §4.4). An empty stack falls
            // through to the passthrough blit below.
            IrOp::Grade { ops } if !ops.is_empty() => match inputs.first() {
                Some(src) => {
                    // `src.texture` is pool-bucketed (dims rounded up to 64), so
                    // the logical frame size has to be passed explicitly or a
                    // power-window mask lands on the bucket edge instead of the
                    // picture edge — the same logical-vs-physical split
                    // `Transform2D` above already threads.
                    let graded = photonic_render::apply_grade_stack_gpu(
                        self.gpu.device(),
                        self.gpu.queue(),
                        &src.texture,
                        ops,
                        (logical_w, logical_h),
                    );
                    self.passes.blit(&self.gpu, &graded, target);
                }
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
            // Blit passthrough for empty Grade and marker filter/color ops.
            _ => match inputs.first() {
                Some(src) => self.passes.blit(&self.gpu, &src.texture, target),
                None => self.passes.fill(&self.gpu, target, [0.0; 4]),
            },
        }
    }
}

/// The processing canvas affects every dependency, including outputs with an
/// explicit size. A full-size output made from Draft inputs must never satisfy
/// a later Full evaluation of the same compiled graph.
fn evaluation_hash(
    hash: crate::graph::ir::ContentHash,
    width: u32,
    height: u32,
) -> crate::graph::ir::ContentHash {
    let mut key = [0u8; 24];
    key[..16].copy_from_slice(&hash.0.to_le_bytes());
    key[16..20].copy_from_slice(&width.to_le_bytes());
    key[20..].copy_from_slice(&height.to_le_bytes());
    // Keep the top byte clear, matching compiled hashes and reserving 0xFE
    // for transparent fillers.
    crate::graph::ir::ContentHash(xxhash_rust::xxh3::xxh3_128(&key) & ((1u128 << 120) - 1))
}

/// The output size of a computed op.
fn op_size(op: &IrOp, cw: u32, ch: u32) -> (u32, u32) {
    match op {
        IrOp::Resize { w, h, .. } | IrOp::Output { w, h } => (*w, *h),
        _ => (cw, ch),
    }
}

// ── Render pipelines ──────────────────────────────────────────────────────────

struct Passes {
    fill_pipeline: wgpu::RenderPipeline,
    fill_bgl: wgpu::BindGroupLayout,
    blit_pipeline: wgpu::RenderPipeline,
    blit_bgl: wgpu::BindGroupLayout,
    transform_pipeline: wgpu::RenderPipeline,
    transform_bgl: wgpu::BindGroupLayout,
    crop_pipeline: wgpu::RenderPipeline,
    resize_pipeline: wgpu::RenderPipeline,
    /// D-12 stabilization warp (22 §6.4); WGSL twin of `ops::stabilize_warp`.
    stabilize_pipeline: wgpu::RenderPipeline,
    stabilize_bgl: wgpu::BindGroupLayout,
    /// `Effect{Invert}` (08 §3): shares the blit bind-group layout (tex + sampler).
    invert_pipeline: wgpu::RenderPipeline,
    deflicker_pipeline: wgpu::RenderPipeline,
    /// K-G6 deinterlace: textureLoad + method/order/dims uniform (reuses transform_bgl).
    deinterlace_pipeline: wgpu::RenderPipeline,
    /// `Effect{LumaKey}`/`Effect{ChromaKey}` (08 §3): a filter BGL (tex + sampler
    /// + a params uniform), the WGSL twins of `ops::luma_key`/`ops::chroma_key`.
    filter_bgl: wgpu::BindGroupLayout,
    luma_key_pipeline: wgpu::RenderPipeline,
    chroma_key_pipeline: wgpu::RenderPipeline,
    /// Separable Gaussian blur (K-0.2): tex + sampler + `BlurParams` uniform.
    /// Shared by `Effect{Blur}` and the blur half of Sharpen/Glow. Algorithm
    /// matches `photonic_render::pipeline::BLUR_SHADER` / `ops::blur`.
    blur_bgl: wgpu::BindGroupLayout,
    blur_pipeline: wgpu::RenderPipeline,
    /// Unsharp combine (K-0.2): two textures (src, blurred) + amount uniform.
    sharpen_bgl: wgpu::BindGroupLayout,
    sharpen_pipeline: wgpu::RenderPipeline,
    /// High-pass combine (K-B16): `src - blurred` on RGB (reuses sharpen BGL).
    high_pass_pipeline: wgpu::RenderPipeline,
    /// Emboss / find-edges / median neighbourhood BGL (tex + logical-dim uniform).
    /// Reuses the blur BGL layout (textureLoad, no sampler).
    emboss_pipeline: wgpu::RenderPipeline,
    find_edges_pipeline: wgpu::RenderPipeline,
    median_pipeline: wgpu::RenderPipeline,
    /// K-B16 point / stylize twins (levels, posterize, threshold, desaturate,
    /// vignette, mosaic). Reuse filter_bgl (tex+sampler+uniform) or blur_bgl.
    levels_pipeline: wgpu::RenderPipeline,
    posterize_pipeline: wgpu::RenderPipeline,
    threshold_pipeline: wgpu::RenderPipeline,
    desaturate_pipeline: wgpu::RenderPipeline,
    vignette_pipeline: wgpu::RenderPipeline,
    mosaic_pipeline: wgpu::RenderPipeline,
    motion_blur_pipeline: wgpu::RenderPipeline,
    hue_sat_pipeline: wgpu::RenderPipeline,
    vibrance_pipeline: wgpu::RenderPipeline,
    channel_mixer_pipeline: wgpu::RenderPipeline,
    curves_lut_pipeline: wgpu::RenderPipeline,
    black_and_white_pipeline: wgpu::RenderPipeline,
    surface_blur_pipeline: wgpu::RenderPipeline,
    lens_blur_pipeline: wgpu::RenderPipeline,
    smart_sharpen_pipeline: wgpu::RenderPipeline,
    pinch_pipeline: wgpu::RenderPipeline,
    ripple_pipeline: wgpu::RenderPipeline,
    perspective_pipeline: wgpu::RenderPipeline,
    grain_pipeline: wgpu::RenderPipeline,
    ca_pipeline: wgpu::RenderPipeline,
    unpremultiply_pipeline: wgpu::RenderPipeline,
    alpha_view_pipeline: wgpu::RenderPipeline,
    outline_pipeline: wgpu::RenderPipeline,
    drop_shadow_pipeline: wgpu::RenderPipeline,
    util2_bgl: wgpu::BindGroupLayout,
    /// Glow extract (K-0.2): keep pixels whose straight luma ≥ threshold.
    glow_extract_pipeline: wgpu::RenderPipeline,
    /// Glow composite (K-0.2): screen-add tinted glow over source.
    glow_comp_bgl: wgpu::BindGroupLayout,
    glow_comp_pipeline: wgpu::RenderPipeline,
    /// `Effect{MaskShapeGen}` (08 §3): a 0-input generator, uniform-only BGL.
    mask_bgl: wgpu::BindGroupLayout,
    mask_shape_pipeline: wgpu::RenderPipeline,
    merge_pipeline: wgpu::RenderPipeline,
    merge_bgl: wgpu::BindGroupLayout,
    /// `WipeMix` (08 §2.0b): binary directional smoothstep wipe. Reuses the merge
    /// BGL (two textures + sampler + a params uniform). WGSL twin of `ops::wipe`.
    wipe_pipeline: wgpu::RenderPipeline,
    /// `LumaWipeMix` (26 K-B7): analytical map wipe. Reuses the merge BGL.
    luma_wipe_pipeline: wgpu::RenderPipeline,
    /// `PushMix` (08 §2.0b): binary directional slide. Its own BGL (two textures +
    /// a params uniform; textureLoad, no sampler). WGSL twin of `ops::push`.
    push_pipeline: wgpu::RenderPipeline,
    push_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// Stable numeric id for a wipe/push direction, fed to the wipe/push shaders (and
/// matching the `ops::sweep_coord` / `ops::push` axis+orientation branch order).
fn wipe_direction_index(dir: WipeDirection) -> f32 {
    match dir {
        WipeDirection::LeftToRight => 0.0,
        WipeDirection::RightToLeft => 1.0,
        WipeDirection::TopToBottom => 2.0,
        WipeDirection::BottomToTop => 3.0,
    }
}

const QUAD_VS: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
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
    var o: VOut;
    o.pos = vec4<f32>(pos[vi], 0.0, 1.0);
    o.uv = uvs[vi];
    return o;
}
"#;

impl Passes {
    fn new(device: &wgpu::Device) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("eval_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Fill: uniform premultiplied color.
        let fill_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fill_bgl"),
            entries: &[uniform_entry(0)],
        });
        let fill_src = format!(
            "{QUAD_VS}\nstruct Fill {{ color: vec4<f32> }}\n@group(0) @binding(0) var<uniform> f: Fill;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{ return f.color; }}\n"
        );
        let fill_pipeline = make_pipeline(device, &fill_bgl, &fill_src, "fs");

        // Blit: sample one source.
        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit_bgl"),
            entries: &[tex_entry(0), sampler_entry(1)],
        });
        let blit_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{ return textureSample(t, s, i.uv); }}\n"
        );
        let blit_pipeline = make_pipeline(device, &blit_bgl, &blit_src, "fs");

        // Transform2D: explicit textureLoad sampling avoids sampler-dependent
        // coordinate and padding behavior. The uniform stores three inverse
        // affine columns, sampling mode, and logical canvas/source dimensions.
        let transform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transform_bgl"),
            entries: &[tex_entry(0), uniform_entry(1)],
        });
        let transform_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct T {{ c0: vec4<f32>, c1: vec4<f32>, c2: vec4<f32>, info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> u: T;\nfn at(p: vec2<i32>) -> vec4<f32> {{\n  let hi = vec2<i32>(u.info.zw) - vec2<i32>(1);\n  return textureLoad(t, clamp(p, vec2<i32>(0), hi), 0);\n}}\nfn nearest(p: vec2<f32>) -> vec4<f32> {{ return at(vec2<i32>(floor(p))); }}\nfn bilinear(p: vec2<f32>) -> vec4<f32> {{\n  let q = p - vec2<f32>(0.5);\n  let p0f = floor(q);\n  let p0 = vec2<i32>(p0f);\n  let f = q - p0f;\n  let p00 = at(p0);\n  let p10 = at(p0 + vec2<i32>(1, 0));\n  let p01 = at(p0 + vec2<i32>(0, 1));\n  let p11 = at(p0 + vec2<i32>(1, 1));\n  return mix(mix(p00, p10, f.x), mix(p01, p11, f.x), f.y);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  if (pos.x >= u.info.x || pos.y >= u.info.y) {{ return vec4<f32>(0.0); }}\n  let canvas_src = u.c0.xy * pos.x + u.c1.xy * pos.y + u.c2.xy;\n  let src = canvas_src * u.info.zw / u.info.xy;\n  if (u.c0.w > 0.5) {{ return nearest(src); }}\n  return bilinear(src);\n}}\n"
        );
        let transform_pipeline = make_pipeline(device, &transform_bgl, &transform_src, "fs");

        // Crop: remove normalized margins while retaining the logical canvas.
        // Keeping this as a textureLoad pass makes source dimensions explicit
        // and prevents pooled padding from becoming visible at the crop edge.
        let crop_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n\
struct C {{ info: vec4<f32>, edges: vec4<f32> }}\n\
@group(0) @binding(1) var<uniform> u: C;\n\
fn at(p: vec2<i32>) -> vec4<f32> {{\n\
  let hi = vec2<i32>(u.info.zw) - vec2<i32>(1);\n\
  return textureLoad(t, clamp(p, vec2<i32>(0), hi), 0);\n\
}}\n\
fn bilinear(p: vec2<f32>) -> vec4<f32> {{\n\
  let q = p - vec2<f32>(0.5);\n\
  let p0f = floor(q);\n\
  let p0 = vec2<i32>(p0f);\n\
  let f = q - p0f;\n\
  let p00 = at(p0);\n\
  let p10 = at(p0 + vec2<i32>(1, 0));\n\
  let p01 = at(p0 + vec2<i32>(0, 1));\n\
  let p11 = at(p0 + vec2<i32>(1, 1));\n\
  return mix(mix(p00, p10, f.x), mix(p01, p11, f.x), f.y);\n\
}}\n\
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n\
  let out_size = u.info.xy;\n\
  if (pos.x >= out_size.x || pos.y >= out_size.y) {{ return vec4<f32>(0.0); }}\n\
  let left = u.edges.x * out_size.x;\n\
  let top = u.edges.y * out_size.y;\n\
  let right = out_size.x - u.edges.z * out_size.x;\n\
  let bottom = out_size.y - u.edges.w * out_size.y;\n\
  if (right <= left || bottom <= top || pos.x < left || pos.x >= right ||\n\
      pos.y < top || pos.y >= bottom) {{ return vec4<f32>(0.0); }}\n\
  return bilinear(pos.xy * u.info.zw / out_size);\n\
}}\n"
        );
        let crop_pipeline = make_pipeline(device, &transform_bgl, &crop_src, "fs");

        // Resize: preserve the source aspect ratio for Fit/Fill and leave Fit's
        // letterbox/pillarbox bars transparent. The pass uses logical dimensions
        // explicitly because pooled target textures are rounded up to 64px.
        let resize_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n\
struct R {{ info: vec4<f32>, params: vec4<f32> }}\n\
@group(0) @binding(1) var<uniform> u: R;\n\
fn at(p: vec2<i32>) -> vec4<f32> {{\n\
  let hi = vec2<i32>(u.info.zw) - vec2<i32>(1);\n\
  return textureLoad(t, clamp(p, vec2<i32>(0), hi), 0);\n\
}}\n\
fn bilinear(p: vec2<f32>) -> vec4<f32> {{\n\
  let q = p - vec2<f32>(0.5);\n\
  let p0f = floor(q);\n\
  let p0 = vec2<i32>(p0f);\n\
  let f = q - p0f;\n\
  let p00 = at(p0);\n\
  let p10 = at(p0 + vec2<i32>(1, 0));\n\
  let p01 = at(p0 + vec2<i32>(0, 1));\n\
  let p11 = at(p0 + vec2<i32>(1, 1));\n\
  return mix(mix(p00, p10, f.x), mix(p01, p11, f.x), f.y);\n\
}}\n\
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n\
  let out_size = u.info.xy;\n\
  let source_size = u.info.zw;\n\
  if (pos.x >= out_size.x || pos.y >= out_size.y) {{ return vec4<f32>(0.0); }}\n\
  var scale = out_size / source_size;\n\
  var offset = vec2<f32>(0.0);\n\
  if (u.params.x < 0.5) {{\n\
    let uniform_scale = min(scale.x, scale.y);\n\
    scale = vec2<f32>(uniform_scale);\n\
    let content = source_size * scale;\n\
    offset = (out_size - content) * 0.5;\n\
    if (pos.x < offset.x || pos.x >= offset.x + content.x ||\n\
        pos.y < offset.y || pos.y >= offset.y + content.y) {{\n\
      return vec4<f32>(0.0);\n\
    }}\n\
  }} else if (u.params.x < 1.5) {{\n\
    let uniform_scale = max(scale.x, scale.y);\n\
    scale = vec2<f32>(uniform_scale);\n\
    let content = source_size * scale;\n\
    offset = (out_size - content) * 0.5;\n\
  }}\n\
  return bilinear((pos.xy - offset) / scale);\n\
}}\n"
        );
        let resize_pipeline = make_pipeline(device, &transform_bgl, &resize_src, "fs");

        // D-12 StabilizeWarp: the WGSL twin of `ops::stabilize_warp`. Same
        // five steps in the same order, same fixed Newton iteration count, and
        // `textureLoad` with explicit clamping for the same reason the
        // Transform2D pass uses it — sampler-dependent edge and padding
        // behaviour would make CPU/GPU parity a function of the driver.
        //
        // Uniform layout mirrors the CPU struct:
        //   r0 = rot[0..3],           r1 = rot[3..6],        r2 = rot[6..9]
        //   k  = k1..k4
        //   intr = [fx, fy, cx, cy]
        //   info = [logical_w, logical_h, source_w, source_h]
        //   flags = [zoom, fisheye, transparent_edges, sampling]
        let stabilize_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stabilize_bgl"),
            entries: &[tex_entry(0), uniform_entry(1)],
        });
        let stabilize_src = format!(
            "{QUAD_VS}\n\
@group(0) @binding(0) var t: texture_2d<f32>;\n\
struct S {{ r0: vec4<f32>, r1: vec4<f32>, r2: vec4<f32>, k: vec4<f32>, intr: vec4<f32>, info: vec4<f32>, flags: vec4<f32> }}\n\
@group(0) @binding(1) var<uniform> u: S;\n\
fn at(p: vec2<i32>) -> vec4<f32> {{\n\
  let hi = vec2<i32>(u.info.zw) - vec2<i32>(1);\n\
  return textureLoad(t, clamp(p, vec2<i32>(0), hi), 0);\n\
}}\n\
fn bilinear(p: vec2<f32>) -> vec4<f32> {{\n\
  let q = p - vec2<f32>(0.5);\n\
  let p0f = floor(q);\n\
  let p0 = vec2<i32>(p0f);\n\
  let f = q - p0f;\n\
  let p00 = at(p0);\n\
  let p10 = at(p0 + vec2<i32>(1, 0));\n\
  let p01 = at(p0 + vec2<i32>(0, 1));\n\
  let p11 = at(p0 + vec2<i32>(1, 1));\n\
  return mix(mix(p00, p10, f.x), mix(p01, p11, f.x), f.y);\n\
}}\n\
fn distort_theta(th: f32) -> f32 {{\n\
  let t2 = th * th; let t4 = t2 * t2; let t6 = t4 * t2; let t8 = t4 * t4;\n\
  return th * (1.0 + u.k.x * t2 + u.k.y * t4 + u.k.z * t6 + u.k.w * t8);\n\
}}\n\
fn undistort_theta(td: f32) -> f32 {{\n\
  var th = td;\n\
  for (var i = 0u; i < {iters}u; i = i + 1u) {{\n\
    let t2 = th * th; let t4 = t2 * t2; let t6 = t4 * t2; let t8 = t4 * t4;\n\
    let f = th * (1.0 + u.k.x * t2 + u.k.y * t4 + u.k.z * t6 + u.k.w * t8) - td;\n\
    let df = 1.0 + 3.0 * u.k.x * t2 + 5.0 * u.k.y * t4 + 7.0 * u.k.z * t6 + 9.0 * u.k.w * t8;\n\
    if (abs(df) < 1e-12) {{ break; }}\n\
    th = th - f / df;\n\
  }}\n\
  return th;\n\
}}\n\
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n\
  if (pos.x >= u.info.x || pos.y >= u.info.y) {{ return vec4<f32>(0.0); }}\n\
  let w = u.info.z; let h = u.info.w;\n\
  let fx = u.intr.x * w; let fy = u.intr.y * h; let cx = u.intr.z * w; let cy = u.intr.w * h;\n\
  let zoom = u.flags.x; let is_fish = u.flags.y > 0.5; let transp = u.flags.z > 0.5;\n\
  let nearest = u.flags.w > 0.5;\n\
  // 1 - undo the crop zoom about the frame centre.\n\
  let o = vec2<f32>(w, h) * 0.5 + (pos.xy - vec2<f32>(w, h) * 0.5) / zoom;\n\
  // 2 - unproject to a ray in the virtual camera.\n\
  let uu = (o.x - cx) / fx;\n\
  let vv = (o.y - cy) / fy;\n\
  var ray: vec3<f32>;\n\
  if (is_fish) {{\n\
    let td = sqrt(uu * uu + vv * vv);\n\
    if (td < 1e-12) {{ ray = vec3<f32>(0.0, 0.0, 1.0); }}\n\
    else {{ let th = undistort_theta(td); let s = sin(th);\n\
           ray = vec3<f32>(s * uu / td, s * vv / td, cos(th)); }}\n\
  }} else {{\n\
    let n = sqrt(uu * uu + vv * vv + 1.0);\n\
    ray = vec3<f32>(uu / n, vv / n, 1.0 / n);\n\
  }}\n\
  // 3 - rotate into the real camera's frame (row-major).\n\
  let sx3 = dot(u.r0.xyz, ray);\n\
  let sy3 = dot(u.r1.xyz, ray);\n\
  let sz3 = dot(u.r2.xyz, ray);\n\
  // 4 - project back through the same lens.\n\
  var sp: vec2<f32>;\n\
  var valid = true;\n\
  if (is_fish) {{\n\
    let rr = sqrt(sx3 * sx3 + sy3 * sy3);\n\
    if (rr < 1e-12) {{ sp = vec2<f32>(cx, cy); }}\n\
    else {{ let th = atan2(rr, sz3); let td = distort_theta(th); let sc = td / rr;\n\
           sp = vec2<f32>(fx * sx3 * sc + cx, fy * sy3 * sc + cy); }}\n\
  }} else {{\n\
    if (sz3 <= 1e-12) {{ sp = vec2<f32>(0.0); valid = false; }}\n\
    else {{ sp = vec2<f32>(fx * (sx3 / sz3) + cx, fy * (sy3 / sz3) + cy); }}\n\
  }}\n\
  // 5 - sample, or leave the hole showing.\n\
  let outside = !valid || sp.x < 0.0 || sp.y < 0.0 || sp.x > w - 1.0 || sp.y > h - 1.0;\n\
  if (!valid) {{ return vec4<f32>(0.0); }}\n\
  if (outside && transp) {{ return vec4<f32>(0.0); }}\n\
  if (nearest) {{ return at(vec2<i32>(floor(sp))); }}\n\
  return bilinear(sp);\n\
}}\n",
            iters = crate::graph::stabilize::lens::UNDISTORT_ITERATIONS,
        );
        let stabilize_pipeline = make_pipeline(device, &stabilize_bgl, &stabilize_src, "fs");

        // Invert (08 §3): invert straight (unpremult) color, keep alpha, then
        // re-premultiply — the WGSL twin of `ops::invert`. Reuses the blit BGL.
        let invert_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var straight = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ straight = clamp(c.rgb / c.a, vec3<f32>(0.0), vec3<f32>(1.0)); }}\n  let inv = (vec3<f32>(1.0) - straight) * c.a;\n  return vec4<f32>(inv, c.a);\n}}\n"
        );
        let invert_pipeline = make_pipeline(device, &blit_bgl, &invert_src, "fs");

        // K-G6 deinterlace: WGSL twin of `ops::deinterlace` spatial methods.
        // Uniform `info = [method, field_order, logical_w, logical_h]` —
        // method 0=OneField, 1=LinearBlend, 2=YadifSpatial; field_order 0=TFF, 1=BFF.
        // Reuses transform_bgl (tex + uniform).
        let deinterlace_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn at(x: i32, y: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(x, y), vec2<i32>(0), hi), 0);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  if (pos.x >= p.info.z || pos.y >= p.info.w) {{ return vec4<f32>(0.0); }}\n  let x = i32(pos.x);\n  let y = i32(pos.y);\n  let method = i32(p.info.x + 0.5);\n  let keep_even = p.info.y < 0.5;\n  let even = (y % 2) == 0;\n  if (method == 1) {{\n    let a = at(x, y - 1);\n    let b = at(x, y);\n    let c = at(x, y + 1);\n    return (a + b * 2.0 + c) * 0.25;\n  }}\n  if (even == keep_even) {{ return at(x, y); }}\n  if (method == 2) {{\n    let yp = y - 1; let yn = y + 1;\n    let above = at(x, yp); let below = at(x, yn);\n    let diag_a = (at(x - 1, yp) + at(x + 1, yn)) * 0.5;\n    let diag_b = (at(x + 1, yp) + at(x - 1, yn)) * 0.5;\n    let vert = (above + below) * 0.5;\n    // Per-channel median of three predictors (spatial edge adapt).\n    var out = vec4<f32>(0.0);\n    for (var c = 0; c < 4; c++) {{\n      let v0 = vert[c]; let v1 = diag_a[c]; let v2 = diag_b[c];\n      let mn = min(v0, min(v1, v2));\n      let mx = max(v0, max(v1, v2));\n      out[c] = v0 + v1 + v2 - mn - mx;\n    }}\n    return out;\n  }}\n  return (at(x, y - 1) + at(x, y + 1)) * 0.5;\n}}\n"
        );
        let deinterlace_pipeline = make_pipeline(device, &transform_bgl, &deinterlace_src, "fs");

        // Filter BGL (tex + sampler + params uniform) — shared by LumaKey/ChromaKey.
        let filter_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("filter_bgl"),
            entries: &[tex_entry(0), sampler_entry(1), uniform_entry(2)],
        });

        // LumaKey (08 §3): scale α by a luma-driven keep factor. `u = [threshold,
        // hi, invert, pad]` (hi = threshold + max(softness, eps), floored in Rust so
        // the GPU and CPU smoothstep bands match). WGSL twin of `ops::luma_key`.
        let luma_key_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct LK {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: LK;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var straight = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ straight = c.rgb / c.a; }}\n  let luma = 0.2126 * straight.r + 0.7152 * straight.g + 0.0722 * straight.b;\n  var keep = smoothstep(u.p.x, u.p.y, luma);\n  if (u.p.z > 0.5) {{ keep = 1.0 - keep; }}\n  return c * keep;\n}}\n"
        );
        let luma_key_pipeline = make_pipeline(device, &filter_bgl, &luma_key_src, "fs");

        // ChromaKey (08 §3): key out colours near `key`, feather the matte edge,
        // suppress spill on the dominant key channel. `key = [r,g,b,tolerance]`,
        // `aux = [hi, spill, dom, pad]`. WGSL twin of `ops::chroma_key`.
        let chroma_key_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct CK {{ key: vec4<f32>, aux: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: CK;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var straight = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ straight = c.rgb / c.a; }}\n  let dist = length(straight - u.key.xyz);\n  let keep = smoothstep(u.key.w, u.aux.x, dist);\n  var col = straight;\n  let spill = u.aux.y;\n  let dom = i32(u.aux.z + 0.5);\n  if (dom == 0) {{ let m = 0.5 * (col.g + col.b); if (col.r > m) {{ col.r = col.r + (m - col.r) * spill; }} }}\n  else if (dom == 1) {{ let m = 0.5 * (col.r + col.b); if (col.g > m) {{ col.g = col.g + (m - col.g) * spill; }} }}\n  else {{ let m = 0.5 * (col.r + col.g); if (col.b > m) {{ col.b = col.b + (m - col.b) * spill; }} }}\n  let a = c.a * keep;\n  return vec4<f32>(col * a, a);\n}}\n"
        );
        let chroma_key_pipeline = make_pipeline(device, &filter_bgl, &chroma_key_src, "fs");

        // Deflicker (`color.deflicker`): per-channel gain applied to sRGB-ENCODED
        // straight colour, with a highlight rolloff so a brightening gain cannot
        // push near-white over. WGSL twin of `deflicker::apply_gain` — the
        // transfer-function breakpoints are byte-for-byte the Rust constants so
        // the two agree. `u = [gain_r, gain_g, gain_b, knee]`. Reuses filter_bgl.
        let deflicker_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct DF {{ g: vec4<f32>, b: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: DF;\nfn lin2srgb(c: f32) -> f32 {{\n  if (c <= 0.0031308) {{ return c * 12.92; }}\n  return 1.055 * pow(c, 1.0 / 2.4) - 0.055;\n}}\nfn srgb2lin(c: f32) -> f32 {{\n  if (c <= 0.04045) {{ return c / 12.92; }}\n  return pow((c + 0.055) / 1.055, 2.4);\n}}\nfn rolled(g: f32, enc: f32, knee: f32) -> f32 {{\n  if (g <= 1.0 || enc <= knee) {{ return g; }}\n  let tt = clamp((enc - knee) / (1.0 - knee), 0.0, 1.0);\n  return 1.0 + (g - 1.0) * (1.0 - tt * tt);\n}}\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  if (c.a <= 1e-6) {{ return c; }}\n  // Exact pass-through when nothing was measured, matching the CPU kernel.\n  // Without this an inert Deflicker still round-trips sRGB and clamps to\n  // [0,1], which silently hard-clips super-white values the rgba16f working\n  // space legitimately carries.\n  if (abs(u.b.y) <= 1e-6 && all(abs(u.g.rgb - vec3<f32>(1.0)) <= vec3<f32>(1e-6))) {{ return c; }}\n  var lin = c.rgb / c.a;\n  if (abs(u.b.y) > 1e-6 && u.b.w > 0.5) {{\n    let kk = 6.2831853 * u.b.x / u.b.w;\n    let bb = u.b.y * cos(kk * (i.pos.y - 0.5) + u.b.z);\n    lin = lin * clamp(1.0 / (1.0 + bb), 0.5, 2.0);\n  }}\n  let straight = clamp(lin, vec3<f32>(0.0), vec3<f32>(1.0));\n  let knee = u.g.w;\n  var out = vec3<f32>(0.0);\n  for (var k = 0; k < 3; k++) {{\n    let enc = lin2srgb(straight[k]);\n    let gain = rolled(u.g[k], enc, knee);\n    out[k] = srgb2lin(clamp(enc * gain, 0.0, 1.0));\n  }}\n  return vec4<f32>(out * c.a, c.a);\n}}\n"
        );
        let deflicker_pipeline = make_pipeline(device, &filter_bgl, &deflicker_src, "fs");

        // Separable Gaussian blur (K-0.2) — WGSL twin of `ops::blur`. Uses
        // `textureLoad` + LOGICAL dims (not physical pool-bucket size) so the
        // kernel steps match the CPU's clamp-to-edge pixel sampling exactly.
        // Uniform: `info = [sigma, horizontal, logical_w, logical_h]`.
        let blur_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur_bgl"),
            entries: &[tex_entry(0), uniform_entry(1)],
        });
        let blur_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct BlurP {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: BlurP;\nfn at(px: i32, py: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(px, py), vec2<i32>(0), hi), 0);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let sigma = p.info.x;\n  let x = i32(pos.x); let y = i32(pos.y);\n  if (sigma < 0.5) {{ return at(x, y); }}\n  let radius = min(i32(ceil(sigma * 3.0)), 128);\n  var acc = vec4<f32>(0.0);\n  var w_total = 0.0;\n  let horiz = p.info.y > 0.5;\n  for (var k = -radius; k <= radius; k++) {{\n    let fi = f32(k);\n    let w = exp(-fi * fi / (2.0 * sigma * sigma));\n    var sx = x; var sy = y;\n    if (horiz) {{ sx = x + k; }} else {{ sy = y + k; }}\n    acc += at(sx, sy) * w;\n    w_total += w;\n  }}\n  return acc / w_total;\n}}\n"
        );
        let blur_pipeline = make_pipeline(device, &blur_bgl, &blur_src, "fs");

        // Unsharp combine: `src + amount * (src - blurred)` (K-0.2).
        let sharpen_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sharpen_bgl"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                sampler_entry(2),
                uniform_entry(3),
            ],
        });
        let sharpen_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t_src: texture_2d<f32>;\n@group(0) @binding(1) var t_blur: texture_2d<f32>;\n@group(0) @binding(2) var s: sampler;\nstruct S {{ amount: vec4<f32> }}\n@group(0) @binding(3) var<uniform> u: S;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let src = textureSample(t_src, s, i.uv);\n  let bl = textureSample(t_blur, s, i.uv);\n  return src + u.amount.x * (src - bl);\n}}\n"
        );
        let sharpen_pipeline = make_pipeline(device, &sharpen_bgl, &sharpen_src, "fs");

        // High-pass: src − blurred on RGB, keep src alpha (K-B16).
        let high_pass_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t_src: texture_2d<f32>;\n@group(0) @binding(1) var t_blur: texture_2d<f32>;\n@group(0) @binding(2) var s: sampler;\nstruct S {{ amount: vec4<f32> }}\n@group(0) @binding(3) var<uniform> u: S;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let src = textureSample(t_src, s, i.uv);\n  let bl = textureSample(t_blur, s, i.uv);\n  return vec4<f32>(src.rgb - bl.rgb, src.a);\n}}\n"
        );
        let high_pass_pipeline = make_pipeline(device, &sharpen_bgl, &high_pass_src, "fs");

        // Emboss (K-B16): 3×3 directional luma gradient + mid-gray bias.
        // Uniform `info = [pad, pad, logical_w, logical_h]` (blur BGL layout).
        let emboss_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn at(px: i32, py: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(px, py), vec2<i32>(0), hi), 0);\n}}\nfn straight_luma(c: vec4<f32>) -> f32 {{\n  var s = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ s = c.rgb / c.a; }}\n  return 0.2126 * s.r + 0.7152 * s.g + 0.0722 * s.b;\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let x = i32(pos.x); let y = i32(pos.y);\n  let a = at(x, y).a;\n  var acc = 0.0;\n  // Kernel [[-1,-1,0],[-1,0,1],[0,1,1]] — sum-zero so flat → 0 before bias.\n  acc += straight_luma(at(x-1, y-1)) * -1.0;\n  acc += straight_luma(at(x  , y-1)) * -1.0;\n  acc += straight_luma(at(x-1, y  )) * -1.0;\n  acc += straight_luma(at(x+1, y  )) *  1.0;\n  acc += straight_luma(at(x  , y+1)) *  1.0;\n  acc += straight_luma(at(x+1, y+1)) *  1.0;\n  let g = clamp(acc + 0.5, 0.0, 1.0);\n  return vec4<f32>(g * a, g * a, g * a, a);\n}}\n"
        );
        let emboss_pipeline = make_pipeline(device, &blur_bgl, &emboss_src, "fs");

        // Find edges (K-B16): Sobel magnitude on straight luma → gray.
        let find_edges_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn at(px: i32, py: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(px, py), vec2<i32>(0), hi), 0);\n}}\nfn straight_luma(c: vec4<f32>) -> f32 {{\n  var s = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ s = c.rgb / c.a; }}\n  return 0.2126 * s.r + 0.7152 * s.g + 0.0722 * s.b;\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let x = i32(pos.x); let y = i32(pos.y);\n  let a = at(x, y).a;\n  var gx = 0.0; var gy = 0.0;\n  gx += straight_luma(at(x-1, y-1)) * -1.0; gy += straight_luma(at(x-1, y-1)) * -1.0;\n  gy += straight_luma(at(x  , y-1)) * -2.0;\n  gx += straight_luma(at(x+1, y-1)) *  1.0; gy += straight_luma(at(x+1, y-1)) * -1.0;\n  gx += straight_luma(at(x-1, y  )) * -2.0;\n  gx += straight_luma(at(x+1, y  )) *  2.0;\n  gx += straight_luma(at(x-1, y+1)) * -1.0; gy += straight_luma(at(x-1, y+1)) *  1.0;\n  gy += straight_luma(at(x  , y+1)) *  2.0;\n  gx += straight_luma(at(x+1, y+1)) *  1.0; gy += straight_luma(at(x+1, y+1)) *  1.0;\n  let mag = clamp(sqrt(gx * gx + gy * gy), 0.0, 1.0);\n  return vec4<f32>(mag * a, mag * a, mag * a, a);\n}}\n"
        );
        let find_edges_pipeline = make_pipeline(device, &blur_bgl, &find_edges_src, "fs");

        // Median (K-B16): per-channel 3×3 median (radius clamped to 1 on GPU).
        // Bubble-sort 9 samples — fixed window, no dynamic indexing hazards.
        let median_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn at(px: i32, py: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(px, py), vec2<i32>(0), hi), 0);\n}}\nfn median9(vals: array<f32, 9>) -> f32 {{\n  var v = vals;\n  for (var a = 0u; a < 8u; a++) {{\n    for (var b = 0u; b < 8u - a; b++) {{\n      if (v[b] > v[b + 1u]) {{\n        let tmp = v[b];\n        v[b] = v[b + 1u];\n        v[b + 1u] = tmp;\n      }}\n    }}\n  }}\n  return v[4];\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let x = i32(pos.x); let y = i32(pos.y);\n  let r = i32(max(p.info.x, 0.0) + 0.5);\n  if (r < 1) {{ return at(x, y); }}\n  var rr = array<f32, 9>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);\n  var gg = array<f32, 9>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);\n  var bb = array<f32, 9>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);\n  var aa = array<f32, 9>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);\n  var i = 0u;\n  for (var dy = -1; dy <= 1; dy++) {{\n    for (var dx = -1; dx <= 1; dx++) {{\n      let c = at(x + dx, y + dy);\n      rr[i] = c.r; gg[i] = c.g; bb[i] = c.b; aa[i] = c.a;\n      i = i + 1u;\n    }}\n  }}\n  return vec4<f32>(median9(rr), median9(gg), median9(bb), median9(aa));\n}}\n"
        );
        let median_pipeline = make_pipeline(device, &blur_bgl, &median_src, "fs");

        // Levels (Transfer): unpremult → encode-approx via straight → levels → premult.
        // Working buffer is already linear; we apply levels on straight linear
        // as a transfer-domain approximation matching the CPU Transfer path's
        // intent (identity at defaults).
        let levels_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct L {{ p0: vec4<f32>, p1: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: L;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var rgb = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ rgb = c.rgb / c.a; }}\n  let ib = u.p0.x; let iw = u.p0.y; let gamma = max(u.p0.z, 0.01);\n  let ob = u.p0.w; let ow = u.p1.x;\n  let span = max(iw - ib, 1e-4);\n  var outc = vec3<f32>(0.0);\n  for (var k = 0; k < 3; k++) {{\n    var v = clamp((rgb[k] - ib) / span, 0.0, 1.0);\n    v = pow(v, 1.0 / gamma);\n    outc[k] = clamp(ob + v * (ow - ob), 0.0, 1.0);\n  }}\n  return vec4<f32>(outc * c.a, c.a);\n}}\n"
        );
        let levels_pipeline = make_pipeline(device, &filter_bgl, &levels_src, "fs");

        let posterize_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var rgb = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ rgb = c.rgb / c.a; }}\n  let steps = max(u.p.x - 1.0, 1.0);\n  let outc = round(rgb * steps) / steps;\n  return vec4<f32>(outc * c.a, c.a);\n}}\n"
        );
        let posterize_pipeline = make_pipeline(device, &filter_bgl, &posterize_src, "fs");

        let threshold_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var rgb = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ rgb = c.rgb / c.a; }}\n  let luma = 0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b;\n  let v = select(0.0, 1.0, luma >= u.p.x);\n  return vec4<f32>(v * c.a, v * c.a, v * c.a, c.a);\n}}\n"
        );
        let threshold_pipeline = make_pipeline(device, &filter_bgl, &threshold_src, "fs");

        let desaturate_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var rgb = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ rgb = c.rgb / c.a; }}\n  let mx = max(rgb.r, max(rgb.g, rgb.b));\n  let mn = min(rgb.r, min(rgb.g, rgb.b));\n  let g = 0.5 * (mx + mn);\n  return vec4<f32>(g * c.a, g * c.a, g * c.a, c.a);\n}}\n"
        );
        let desaturate_pipeline = make_pipeline(device, &filter_bgl, &desaturate_src, "fs");

        // Vignette: info = [amount, feather, logical_w, logical_h]
        let vignette_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn at(px: i32, py: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(px, py), vec2<i32>(0), hi), 0);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let x = i32(pos.x); let y = i32(pos.y);\n  let c = at(x, y);\n  let cx = (lw - 1.0) * 0.5; let cy = (lh - 1.0) * 0.5;\n  let maxd = max(sqrt(cx * cx + cy * cy), 1e-4);\n  let dx = f32(x) - cx; let dy = f32(y) - cy;\n  let d = sqrt(dx * dx + dy * dy) / maxd;\n  let inner = clamp(1.0 - p.info.y, 0.0, 1.0);\n  let span = max(1.0 - inner, 1e-4);\n  let t = clamp((d - inner) / span, 0.0, 1.0);\n  let vig = t * t * (3.0 - 2.0 * t);\n  let factor = max(1.0 + p.info.x * vig, 0.0);\n  return vec4<f32>(c.rgb * factor, c.a);\n}}\n"
        );
        let vignette_pipeline = make_pipeline(device, &blur_bgl, &vignette_src, "fs");

        // Mosaic: info = [block, pad, logical_w, logical_h]
        let mosaic_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn at(px: i32, py: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(px, py), vec2<i32>(0), hi), 0);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let b = max(i32(p.info.x + 0.5), 1);\n  let x = i32(pos.x); let y = i32(pos.y);\n  let bx = (x / b) * b; let by = (y / b) * b;\n  let x1 = min(bx + b, i32(lw)); let y1 = min(by + b, i32(lh));\n  var acc = vec4<f32>(0.0);\n  var n = 0.0;\n  for (var yy = by; yy < y1; yy++) {{\n    for (var xx = bx; xx < x1; xx++) {{\n      acc += at(xx, yy);\n      n += 1.0;\n    }}\n  }}\n  return acc / max(n, 1.0);\n}}\n"
        );
        let mosaic_pipeline = make_pipeline(device, &blur_bgl, &mosaic_src, "fs");

        // Motion blur: info = [angle_rad, distance, logical_w, logical_h]
        let motion_blur_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn at(px: i32, py: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(px, py), vec2<i32>(0), hi), 0);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let dist = max(i32(p.info.y + 0.5), 0);\n  let x = i32(pos.x); let y = i32(pos.y);\n  if (dist < 1) {{ return at(x, y); }}\n  let n = dist;\n  let half = f32(n - 1) * 0.5;\n  let dx = cos(p.info.x); let dy = sin(p.info.x);\n  var acc = vec4<f32>(0.0);\n  for (var i = 0; i < n; i++) {{\n    let t = f32(i) - half;\n    let sx = i32(round(f32(x) + dx * t));\n    let sy = i32(round(f32(y) + dy * t));\n    acc += at(sx, sy);\n  }}\n  return acc / f32(n);\n}}\n"
        );
        let motion_blur_pipeline = make_pipeline(device, &blur_bgl, &motion_blur_src, "fs");

        // Hue/saturation: p = [hue_deg, sat, lightness, pad]
        let hue_sat_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\nfn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {{\n  let mx = max(c.r, max(c.g, c.b)); let mn = min(c.r, min(c.g, c.b));\n  let l = 0.5 * (mx + mn); let d = mx - mn;\n  var h = 0.0; var sat = 0.0;\n  if (d > 1e-6) {{\n    sat = select(d / (2.0 - mx - mn), d / (mx + mn), l > 0.5);\n    if (mx == c.r) {{ h = (c.g - c.b) / d + select(0.0, 6.0, c.g < c.b); }}\n    else if (mx == c.g) {{ h = (c.b - c.r) / d + 2.0; }}\n    else {{ h = (c.r - c.g) / d + 4.0; }}\n    h = h / 6.0;\n  }}\n  return vec3<f32>(h * 360.0, sat, l);\n}}\nfn hue2rgb(p: f32, q: f32, t_in: f32) -> f32 {{\n  var t = t_in;\n  if (t < 0.0) {{ t += 1.0; }}\n  if (t > 1.0) {{ t -= 1.0; }}\n  if (t < 1.0/6.0) {{ return p + (q - p) * 6.0 * t; }}\n  if (t < 0.5) {{ return q; }}\n  if (t < 2.0/3.0) {{ return p + (q - p) * (2.0/3.0 - t) * 6.0; }}\n  return p;\n}}\nfn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {{\n  let h = hsl.x / 360.0; let s = hsl.y; let l = hsl.z;\n  if (s < 1e-6) {{ return vec3<f32>(l, l, l); }}\n  let q = select(l * (1.0 + s), l + s - l * s, l < 0.5);\n  let p = 2.0 * l - q;\n  return vec3<f32>(hue2rgb(p, q, h + 1.0/3.0), hue2rgb(p, q, h), hue2rgb(p, q, h - 1.0/3.0));\n}}\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var rgb = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ rgb = c.rgb / c.a; }}\n  var hsl = rgb_to_hsl(rgb);\n  hsl.x = (hsl.x + u.p.x) % 360.0; if (hsl.x < 0.0) {{ hsl.x += 360.0; }}\n  hsl.y = clamp(hsl.y * (1.0 + clamp(u.p.y, -1.0, 1.0)), 0.0, 1.0);\n  let light = clamp(u.p.z, -1.0, 1.0);\n  if (light >= 0.0) {{ hsl.z = hsl.z + (1.0 - hsl.z) * light; }} else {{ hsl.z = hsl.z * (1.0 + light); }}\n  let outc = hsl_to_rgb(hsl);\n  return vec4<f32>(outc * c.a, c.a);\n}}\n"
        );
        let hue_sat_pipeline = make_pipeline(device, &filter_bgl, &hue_sat_src, "fs");

        let vibrance_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\nfn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {{\n  let mx = max(c.r, max(c.g, c.b)); let mn = min(c.r, min(c.g, c.b));\n  let l = 0.5 * (mx + mn); let d = mx - mn;\n  var h = 0.0; var sat = 0.0;\n  if (d > 1e-6) {{\n    sat = select(d / (2.0 - mx - mn), d / (mx + mn), l > 0.5);\n    if (mx == c.r) {{ h = (c.g - c.b) / d + select(0.0, 6.0, c.g < c.b); }}\n    else if (mx == c.g) {{ h = (c.b - c.r) / d + 2.0; }}\n    else {{ h = (c.r - c.g) / d + 4.0; }}\n    h = h / 6.0;\n  }}\n  return vec3<f32>(h * 360.0, sat, l);\n}}\nfn hue2rgb(p: f32, q: f32, t_in: f32) -> f32 {{\n  var t = t_in;\n  if (t < 0.0) {{ t += 1.0; }}\n  if (t > 1.0) {{ t -= 1.0; }}\n  if (t < 1.0/6.0) {{ return p + (q - p) * 6.0 * t; }}\n  if (t < 0.5) {{ return q; }}\n  if (t < 2.0/3.0) {{ return p + (q - p) * (2.0/3.0 - t) * 6.0; }}\n  return p;\n}}\nfn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {{\n  let h = hsl.x / 360.0; let s = hsl.y; let l = hsl.z;\n  if (s < 1e-6) {{ return vec3<f32>(l, l, l); }}\n  let q = select(l * (1.0 + s), l + s - l * s, l < 0.5);\n  let p = 2.0 * l - q;\n  return vec3<f32>(hue2rgb(p, q, h + 1.0/3.0), hue2rgb(p, q, h), hue2rgb(p, q, h - 1.0/3.0));\n}}\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var rgb = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ rgb = c.rgb / c.a; }}\n  var hsl = rgb_to_hsl(rgb);\n  let amt = clamp(u.p.x, -1.0, 1.0);\n  hsl.y = clamp(hsl.y + amt * (1.0 - hsl.y), 0.0, 1.0);\n  let outc = hsl_to_rgb(hsl);\n  return vec4<f32>(outc * c.a, c.a);\n}}\n"
        );
        let vibrance_pipeline = make_pipeline(device, &filter_bgl, &vibrance_src, "fs");

        // Channel mixer: p0 = [rr,rg,rb,gr], p1 = [gg,gb,br,bg], p2 = [bb, pad...]
        let channel_mixer_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p0: vec4<f32>, p1: vec4<f32>, p2: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var rgb = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ rgb = c.rgb / c.a; }}\n  let outc = vec3<f32>(\n    clamp(u.p0.x * rgb.r + u.p0.y * rgb.g + u.p0.z * rgb.b, 0.0, 1.0),\n    clamp(u.p0.w * rgb.r + u.p1.x * rgb.g + u.p1.y * rgb.b, 0.0, 1.0),\n    clamp(u.p1.z * rgb.r + u.p1.w * rgb.g + u.p2.x * rgb.b, 0.0, 1.0)\n  );\n  return vec4<f32>(outc * c.a, c.a);\n}}\n"
        );
        let channel_mixer_pipeline = make_pipeline(device, &filter_bgl, &channel_mixer_src, "fs");

        // Multi-point RGB curve: 5 knots packed as p0=[x0,y0,x1,y1], p1=[x2,y2,x3,y3], p2=[x4,y4,0,0]
        let curves_lut_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p0: vec4<f32>, p1: vec4<f32>, p2: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\nfn eval_curve(x_in: f32) -> f32 {{\n  var xs = array<f32, 5>(u.p0.x, u.p0.z, u.p1.x, u.p1.z, u.p2.x);\n  var ys = array<f32, 5>(u.p0.y, u.p0.w, u.p1.y, u.p1.w, u.p2.y);\n  let x = clamp(x_in, 0.0, 1.0);\n  // Piecewise linear through sorted knots (caller sorts).\n  if (x <= xs[0]) {{ return ys[0]; }}\n  for (var i = 0; i < 4; i++) {{\n    if (x <= xs[i + 1]) {{\n      let span = max(xs[i + 1] - xs[i], 1e-6);\n      let t = (x - xs[i]) / span;\n      return mix(ys[i], ys[i + 1], t);\n    }}\n  }}\n  return ys[4];\n}}\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var rgb = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ rgb = c.rgb / c.a; }}\n  let outc = vec3<f32>(eval_curve(rgb.r), eval_curve(rgb.g), eval_curve(rgb.b));\n  return vec4<f32>(outc * c.a, c.a);\n}}\n"
        );
        let curves_lut_pipeline = make_pipeline(device, &filter_bgl, &curves_lut_src, "fs");

        // Surface (bilateral-ish) blur: info = [radius, threshold, lw, lh]
        let surface_blur_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn at(px: i32, py: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(px, py), vec2<i32>(0), hi), 0);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let r = min(i32(p.info.x + 0.5), 8);\n  let thr = max(p.info.y, 1e-4);\n  let x = i32(pos.x); let y = i32(pos.y);\n  let center = at(x, y);\n  if (r < 1) {{ return center; }}\n  var acc = vec4<f32>(0.0);\n  var wsum = 0.0;\n  for (var j = -r; j <= r; j++) {{\n    for (var i = -r; i <= r; i++) {{\n      let s = at(x + i, y + j);\n      let d = length(s.rgb - center.rgb);\n      let wr = exp(-d * d / (2.0 * thr * thr));\n      let ws = exp(-f32(i * i + j * j) / (2.0 * f32(r * r)));\n      let w = wr * ws;\n      acc += s * w;\n      wsum += w;\n    }}\n  }}\n  return acc / max(wsum, 1e-6);\n}}\n"
        );
        let surface_blur_pipeline = make_pipeline(device, &blur_bgl, &surface_blur_src, "fs");

        // Lens (disc) blur: info = [radius, 0, lw, lh]
        let lens_blur_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn at(px: i32, py: i32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  return textureLoad(t, clamp(vec2<i32>(px, py), vec2<i32>(0), hi), 0);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let radius = clamp(p.info.x, 0.0, 12.0);\n  let x = i32(pos.x); let y = i32(pos.y);\n  if (radius < 0.5) {{ return at(x, y); }}\n  let r = i32(ceil(radius));\n  let r2 = radius * radius;\n  var acc = vec4<f32>(0.0);\n  var n = 0.0;\n  for (var j = -r; j <= r; j++) {{\n    for (var i = -r; i <= r; i++) {{\n      if (f32(i * i + j * j) <= r2) {{\n        acc += at(x + i, y + j);\n        n += 1.0;\n      }}\n    }}\n  }}\n  return acc / max(n, 1.0);\n}}\n"
        );
        let lens_blur_pipeline = make_pipeline(device, &blur_bgl, &lens_blur_src, "fs");

        // Smart sharpen combine: src + amount * edge * (src - blur), edge gated by threshold.
        // tex0=src, tex1=blur, uniform amount=[amount, threshold, 0, 0]
        let smart_sharpen_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t_src: texture_2d<f32>;\n@group(0) @binding(1) var t_blur: texture_2d<f32>;\n@group(0) @binding(2) var s: sampler;\nstruct S {{ amount: vec4<f32> }}\n@group(0) @binding(3) var<uniform> u: S;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let src = textureSample(t_src, s, i.uv);\n  let bl = textureSample(t_blur, s, i.uv);\n  let diff = src - bl;\n  let mag = length(diff.rgb);\n  let thr = u.amount.y / 255.0;\n  var edge = 0.0;\n  if (mag > thr) {{ edge = clamp((mag - thr) / (32.0 / 255.0), 0.0, 1.0); }}\n  return src + u.amount.x * edge * diff;\n}}\n"
        );
        let smart_sharpen_pipeline = make_pipeline(device, &sharpen_bgl, &smart_sharpen_src, "fs");

        let black_and_white_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var rgb = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ rgb = c.rgb / c.a; }}\n  var w = u.p.xyz;\n  let sum = w.x + w.y + w.z;\n  if (abs(sum) < 1e-6) {{ w = vec3<f32>(0.333333, 0.333333, 0.333333); }} else {{ w = w / sum; }}\n  let g = clamp(dot(rgb, w), 0.0, 1.0);\n  return vec4<f32>(g * c.a, g * c.a, g * c.a, c.a);\n}}\n"
        );
        let black_and_white_pipeline =
            make_pipeline(device, &filter_bgl, &black_and_white_src, "fs");

        // Pinch / spherize: info = [amount, is_spherize, logical_w, logical_h]
        let pinch_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn atf(xf: f32, yf: f32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  let x0 = i32(floor(xf)); let y0 = i32(floor(yf));\n  let fx = xf - f32(x0); let fy = yf - f32(y0);\n  let c00 = textureLoad(t, clamp(vec2<i32>(x0, y0), vec2<i32>(0), hi), 0);\n  let c10 = textureLoad(t, clamp(vec2<i32>(x0+1, y0), vec2<i32>(0), hi), 0);\n  let c01 = textureLoad(t, clamp(vec2<i32>(x0, y0+1), vec2<i32>(0), hi), 0);\n  let c11 = textureLoad(t, clamp(vec2<i32>(x0+1, y0+1), vec2<i32>(0), hi), 0);\n  return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let amount = clamp(p.info.x, -1.0, 1.0);\n  if (abs(amount) < 1e-6) {{\n    return textureLoad(t, vec2<i32>(i32(pos.x), i32(pos.y)), 0);\n  }}\n  let cx = lw * 0.5; let cy = lh * 0.5;\n  let radius = min(lw, lh) * 0.5;\n  let fx = pos.x; let fy = pos.y;\n  let relx = fx - cx; let rely = fy - cy;\n  let r = sqrt(relx * relx + rely * rely);\n  var sxf = fx; var syf = fy;\n  if (r > 0.0 && r < radius) {{\n    let nd = r / radius;\n    if (p.info.y > 0.5) {{\n      // spherize\n      let half_pi = 1.5707963;\n      var curved = nd;\n      if (amount >= 0.0) {{\n        let a = asin(nd) / half_pi;\n        curved = nd + (a - nd) * amount;\n      }} else {{\n        let a = sin(nd * half_pi);\n        curved = nd + (a - nd) * (-amount);\n      }}\n      let scale = curved / nd;\n      sxf = cx + relx * scale; syf = cy + rely * scale;\n    }} else {{\n      // pinch\n      let ff = 1.0 - nd;\n      let scale = 1.0 + amount * ff * ff;\n      sxf = cx + relx * scale; syf = cy + rely * scale;\n    }}\n  }}\n  return atf(sxf, syf);\n}}\n"
        );
        let pinch_pipeline = make_pipeline(device, &blur_bgl, &pinch_src, "fs");

        // Ripple: info = [amplitude, wavelength, logical_w, logical_h]
        let ripple_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn atf(xf: f32, yf: f32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  let x0 = i32(floor(xf)); let y0 = i32(floor(yf));\n  let fx = xf - f32(x0); let fy = yf - f32(y0);\n  let c00 = textureLoad(t, clamp(vec2<i32>(x0, y0), vec2<i32>(0), hi), 0);\n  let c10 = textureLoad(t, clamp(vec2<i32>(x0+1, y0), vec2<i32>(0), hi), 0);\n  let c01 = textureLoad(t, clamp(vec2<i32>(x0, y0+1), vec2<i32>(0), hi), 0);\n  let c11 = textureLoad(t, clamp(vec2<i32>(x0+1, y0+1), vec2<i32>(0), hi), 0);\n  return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let amp = p.info.x; let wl = max(p.info.y, 1.0);\n  let k = 6.2831853 / wl;\n  let sxf = pos.x + amp * sin(k * pos.y);\n  let syf = pos.y + amp * sin(k * pos.x);\n  return atf(sxf, syf);\n}}\n"
        );
        let ripple_pipeline = make_pipeline(device, &blur_bgl, &ripple_src, "fs");

        // Perspective (bilinear approx of inverse mapping via 4 normalized corners).
        // c0=[tlx,tly,trx,try], c1=[brx,bry,blx,bly], c2=[lw,lh,0,0]
        // Inverse: map dest UV → source via bilinear interp of corners is forward;
        // for inverse we use bilinear patch: sample at inverse of bilinear is hard,
        // so use simple projective approx: treat as bilinear UV warp of source.
        let perspective_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ c0: vec4<f32>, c1: vec4<f32>, c2: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn atf(xf: f32, yf: f32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.c2.x), i32(p.c2.y)) - vec2<i32>(1);\n  if (xf < 0.0 || yf < 0.0 || xf >= p.c2.x || yf >= p.c2.y) {{ return vec4<f32>(0.0); }}\n  let x0 = i32(floor(xf)); let y0 = i32(floor(yf));\n  let fx = xf - f32(x0); let fy = yf - f32(y0);\n  let c00 = textureLoad(t, clamp(vec2<i32>(x0, y0), vec2<i32>(0), hi), 0);\n  let c10 = textureLoad(t, clamp(vec2<i32>(x0+1, y0), vec2<i32>(0), hi), 0);\n  let c01 = textureLoad(t, clamp(vec2<i32>(x0, y0+1), vec2<i32>(0), hi), 0);\n  let c11 = textureLoad(t, clamp(vec2<i32>(x0+1, y0+1), vec2<i32>(0), hi), 0);\n  return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.c2.x; let lh = p.c2.y;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  // Normalized dest UV.\n  let u = pos.x / max(lw - 1.0, 1.0); let v = pos.y / max(lh - 1.0, 1.0);\n  // Inverse bilinear: find source UV by interpolating corner origins (identity src corners).\n  // We map dest → source by treating dst corners as where unit square lands, solve approx:\n  // source pos ≈ bilinear of unit-square corners at inverse of dest bilinear (Newton-free):\n  // For identity defaults, corners match unit square → passthrough.\n  let tl = p.c0.xy; let tr = p.c0.zw; let br = p.c1.xy; let bl = p.c1.zw;\n  // Forward bilinear from source unit square s,t to dest:\n  // dest = mix(mix(tl,tr,s), mix(bl,br,s), t). Invert with simple grid search (2 Newton steps).\n  var s = u; var t = v;\n  for (var iter = 0; iter < 4; iter++) {{\n    let top = mix(tl, tr, s); let bot = mix(bl, br, s);\n    let pxy = mix(top, bot, t);\n    let want = vec2<f32>(u, v);\n    let err = pxy - want;\n    // Jacobian via finite differences.\n    let eps = 1e-3;\n    let tops = mix(tl, tr, s + eps); let bots = mix(bl, br, s + eps);\n    let dsd = (mix(tops, bots, t) - pxy) / eps;\n    let topt = mix(tl, tr, s); let bott = mix(bl, br, s);\n    // d/dt of mix(top,bot,t) = bot-top\n    let dtd = bot - top;\n    // Solve J * ds = -err (2x2).\n    let det = dsd.x * dtd.y - dsd.y * dtd.x;\n    if (abs(det) > 1e-8) {{\n      let inv = 1.0 / det;\n      let ds = inv * (-err.x * dtd.y + err.y * dtd.x);\n      let dt = inv * (-dsd.x * err.y + dsd.y * err.x);\n      s = clamp(s + ds, 0.0, 1.0);\n      t = clamp(t + dt, 0.0, 1.0);\n    }}\n  }}\n  return atf(s * (lw - 1.0), t * (lh - 1.0));\n}}\n"
        );
        let perspective_pipeline = make_pipeline(device, &blur_bgl, &perspective_src, "fs");

        // Grain: deterministic hash noise. info = [amount, mono, seed, 0], dims in second half via p
        // Use filter_bgl: p = [amount, mono, seed, 0]
        let grain_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\nfn hash21(p: vec2<f32>) -> f32 {{\n  var p3 = fract(vec3<f32>(p.xyx) * 0.1031 + u.p.z * 0.017);\n  p3 += dot(p3, p3.yzx + 33.33);\n  return fract((p3.x + p3.y) * p3.z) * 2.0 - 1.0;\n}}\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  let amt = clamp(u.p.x, 0.0, 1.0);\n  if (amt < 1e-6) {{ return c; }}\n  let px = i.uv * 1024.0;\n  if (u.p.y > 0.5) {{\n    let n = hash21(px) * amt;\n    return vec4<f32>(c.rgb + vec3<f32>(n, n, n) * c.a, c.a);\n  }}\n  let nr = hash21(px) * amt;\n  let ng = hash21(px + vec2<f32>(17.0, 0.0)) * amt;\n  let nb = hash21(px + vec2<f32>(0.0, 31.0)) * amt;\n  return vec4<f32>(c.r + nr * c.a, c.g + ng * c.a, c.b + nb * c.a, c.a);\n}}\n"
        );
        let grain_pipeline = make_pipeline(device, &filter_bgl, &grain_src, "fs");

        // Chromatic aberration: info = [amount_px, 0, logical_w, logical_h]
        let ca_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn atf(xf: f32, yf: f32) -> vec4<f32> {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  let x0 = i32(floor(xf)); let y0 = i32(floor(yf));\n  let fx = xf - f32(x0); let fy = yf - f32(y0);\n  let c00 = textureLoad(t, clamp(vec2<i32>(x0, y0), vec2<i32>(0), hi), 0);\n  let c10 = textureLoad(t, clamp(vec2<i32>(x0+1, y0), vec2<i32>(0), hi), 0);\n  let c01 = textureLoad(t, clamp(vec2<i32>(x0, y0+1), vec2<i32>(0), hi), 0);\n  let c11 = textureLoad(t, clamp(vec2<i32>(x0+1, y0+1), vec2<i32>(0), hi), 0);\n  return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let amount = p.info.x;\n  let cx = (lw - 1.0) * 0.5; let cy = (lh - 1.0) * 0.5;\n  let dx = pos.x - cx; let dy = pos.y - cy;\n  let dist = sqrt(dx * dx + dy * dy);\n  let maxd = max(sqrt(cx * cx + cy * cy), 1e-4);\n  var ux = 0.0; var uy = 0.0;\n  if (dist > 1e-4) {{ ux = dx / dist; uy = dy / dist; }}\n  let off = amount * (dist / maxd);\n  let center = atf(pos.x, pos.y);\n  let red = atf(pos.x + ux * off, pos.y + uy * off);\n  let blue = atf(pos.x - ux * off, pos.y - uy * off);\n  return vec4<f32>(red.r, center.g, blue.b, center.a);\n}}\n"
        );
        let ca_pipeline = make_pipeline(device, &blur_bgl, &ca_src, "fs");

        let unpremultiply_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  if (c.a > 1e-6) {{ return vec4<f32>(c.rgb / c.a, c.a); }}\n  return vec4<f32>(0.0);\n}}\n"
        );
        let unpremultiply_pipeline = make_pipeline(device, &filter_bgl, &unpremultiply_src, "fs");

        // Alpha view: mode in p.x — 0 alpha-as-luma, 1 premul, 2 straight
        let alpha_view_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct P {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: P;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  let mode = i32(u.p.x + 0.5);\n  if (mode == 1) {{ return c; }}\n  if (mode == 2) {{\n    if (c.a > 1e-6) {{ return vec4<f32>(c.rgb / c.a, 1.0); }}\n    return vec4<f32>(0.0, 0.0, 0.0, 1.0);\n  }}\n  return vec4<f32>(c.a, c.a, c.a, 1.0);\n}}\n"
        );
        let alpha_view_pipeline = make_pipeline(device, &filter_bgl, &alpha_view_src, "fs");

        // Outline: util2_bgl = tex + 32-byte uniform (info + color).
        // info = [thickness, opacity, logical_w, logical_h], col = [r, g, b, pad]
        //
        // WGSL twin of `raster_bridge::util_outline` / proposal 208 §4.1:
        // exterior Euclidean distance to alpha≥0.5 coverage (same isosurface as
        // the CPU Jump-Flood field for exterior points), solid stroke band out
        // to `thickness`, Hermite smoothstep AA of half-width 1.0 at the outer
        // rim, source-over-outline composite. Replaces the old linear-ramp box
        // search that diverged from the CPU oracle on corners and band profile.
        let util2_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("util2_bgl"),
            entries: &[tex_entry(0), uniform_entry(1)],
        });
        let outline_src = format!(
            r#"{QUAD_VS}
@group(0) @binding(0) var t: texture_2d<f32>;
struct P {{ info: vec4<f32>, col: vec4<f32> }}
@group(0) @binding(1) var<uniform> p: P;
fn alpha_at(x: i32, y: i32, hi: vec2<i32>) -> f32 {{
  return textureLoad(t, clamp(vec2<i32>(x, y), vec2<i32>(0), hi), 0).a;
}}
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{
  let lw = p.info.z; let lh = p.info.w;
  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}
  let x = i32(pos.x); let y = i32(pos.y);
  let hi = vec2<i32>(i32(lw), i32(lh)) - vec2<i32>(1);
  let center = textureLoad(t, clamp(vec2<i32>(x, y), vec2<i32>(0), hi), 0);
  let thick = max(p.info.x, 0.0);
  let opac = clamp(p.info.y, 0.0, 1.0);
  // AA half-width matches OUTLINE_AA_PX on the CPU path (1.0 px).
  let aa = 1.0;
  // Search radius covers solid band + AA rim (CPU JFA is unbounded; for the
  // outline band only distances ≤ thickness+aa matter).
  let tpx = i32(ceil(thick + aa));
  var min_d = 1e9;
  let inside = center.a >= 0.5;
  if (!inside && tpx > 0) {{
    for (var j = -tpx; j <= tpx; j++) {{
      for (var i = -tpx; i <= tpx; i++) {{
        if (alpha_at(x + i, y + j, hi) >= 0.5) {{
          min_d = min(min_d, sqrt(f32(i * i + j * j)));
        }}
      }}
    }}
  }}
  // Exterior signed distance ≈ +min_d; deep exterior with no seed → far.
  var dist = min_d;
  if (inside) {{
    // Interior contributes full band under the source; source composites over.
    dist = -1.0;
  }}
  // Solid from isosurface out to thickness, Hermite AA at outer rim:
  // t = ((dist - (thickness - aa)) / (2*aa)).clamp(0,1); band = 1 - smoothstep(t)
  let t = clamp((dist - (thick - aa)) / (2.0 * aa), 0.0, 1.0);
  let band = 1.0 - t * t * (3.0 - 2.0 * t);
  let ea = band * opac;
  let outline = p.col.xyz * ea;
  // Premultiplied over: source over outline (outline behind content).
  return vec4<f32>(
    center.rgb + outline * (1.0 - center.a),
    center.a + ea * (1.0 - center.a)
  );
}}
"#
        );
        let outline_pipeline = make_pipeline(device, &util2_bgl, &outline_src, "fs");

        // Drop shadow: info = [ox, oy, lw, lh], col = [r, g, b, opacity]
        // Soft shadow via multi-tap of source alpha at offset.
        let drop_shadow_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\nstruct P {{ info: vec4<f32>, col: vec4<f32> }}\n@group(0) @binding(1) var<uniform> p: P;\nfn alpha_at(xf: f32, yf: f32) -> f32 {{\n  let hi = vec2<i32>(i32(p.info.z), i32(p.info.w)) - vec2<i32>(1);\n  let x0 = i32(floor(xf)); let y0 = i32(floor(yf));\n  return textureLoad(t, clamp(vec2<i32>(x0, y0), vec2<i32>(0), hi), 0).a;\n}}\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let lw = p.info.z; let lh = p.info.w;\n  if (pos.x >= lw || pos.y >= lh) {{ return vec4<f32>(0.0); }}\n  let x = i32(pos.x); let y = i32(pos.y);\n  let hi = vec2<i32>(i32(lw), i32(lh)) - vec2<i32>(1);\n  let center = textureLoad(t, clamp(vec2<i32>(x, y), vec2<i32>(0), hi), 0);\n  let ox = p.info.x; let oy = p.info.y;\n  // 5-tap soft shadow of alpha at offset.\n  var sa = 0.0;\n  sa += alpha_at(pos.x - ox, pos.y - oy);\n  sa += alpha_at(pos.x - ox - 1.0, pos.y - oy) * 0.5;\n  sa += alpha_at(pos.x - ox + 1.0, pos.y - oy) * 0.5;\n  sa += alpha_at(pos.x - ox, pos.y - oy - 1.0) * 0.5;\n  sa += alpha_at(pos.x - ox, pos.y - oy + 1.0) * 0.5;\n  sa = clamp(sa / 3.0, 0.0, 1.0) * clamp(p.col.w, 0.0, 1.0);\n  let shadow = vec4<f32>(p.col.xyz * sa, sa);\n  return vec4<f32>(center.rgb + shadow.rgb * (1.0 - center.a), center.a + shadow.a * (1.0 - center.a));\n}}\n"
        );
        let drop_shadow_pipeline = make_pipeline(device, &util2_bgl, &drop_shadow_src, "fs");

        // Glow extract: zero out pixels whose straight luma is below threshold.
        // Reuses filter_bgl (tex + sampler + params).
        let glow_extract_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t: texture_2d<f32>;\n@group(0) @binding(1) var s: sampler;\nstruct G {{ p: vec4<f32> }}\n@group(0) @binding(2) var<uniform> u: G;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let c = textureSample(t, s, i.uv);\n  var straight = vec3<f32>(0.0);\n  if (c.a > 1e-6) {{ straight = c.rgb / c.a; }}\n  let luma = 0.2126 * straight.r + 0.7152 * straight.g + 0.0722 * straight.b;\n  let keep = select(0.0, 1.0, luma >= u.p.x);\n  return c * keep;\n}}\n"
        );
        let glow_extract_pipeline = make_pipeline(device, &filter_bgl, &glow_extract_src, "fs");

        // Glow composite: screen-add tinted glow over source.
        let glow_comp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glow_comp_bgl"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                sampler_entry(2),
                uniform_entry(3),
            ],
        });
        let glow_comp_src = format!(
            "{QUAD_VS}\n@group(0) @binding(0) var t_src: texture_2d<f32>;\n@group(0) @binding(1) var t_glow: texture_2d<f32>;\n@group(0) @binding(2) var s: sampler;\nstruct GC {{ tint: vec4<f32>, intensity: vec4<f32> }}\n@group(0) @binding(3) var<uniform> u: GC;\n@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {{\n  let src = textureSample(t_src, s, i.uv);\n  let g = textureSample(t_glow, s, i.uv);\n  let gr = g.r * u.tint.x * u.intensity.x;\n  let gg = g.g * u.tint.y * u.intensity.x;\n  let gb = g.b * u.tint.z * u.intensity.x;\n  let ga = clamp(g.a * u.intensity.x, 0.0, 1.0);\n  return vec4<f32>(\n    src.r + gr * max(1.0 - src.r, 0.0),\n    src.g + gg * max(1.0 - src.g, 0.0),\n    src.b + gb * max(1.0 - src.b, 0.0),\n    src.a + ga * (1.0 - src.a)\n  );\n}}\n"
        );
        let glow_comp_pipeline = make_pipeline(device, &glow_comp_bgl, &glow_comp_src, "fs");

        // MaskShapeGen (08 §3): a uniform-only ellipse-matte generator (no input).
        // `center = [cx, cy, sx, sy]`, `aux = [cos(-rot), sin(-rot), inner, pad]`
        // (inner = 1 − max(feather, eps)). WGSL twin of `ops::mask_shape`; uv is
        // the top-left-origin canvas coordinate (matches the CPU pixel-center uv).
        let mask_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask_bgl"),
            entries: &[uniform_entry(0)],
        });
        // Working textures are pool-bucketed (dims rounded up to 64), so the
        // logical canvas occupies the top-left region. A generator must derive its
        // uv from `@builtin(position)` and the LOGICAL dims (`dims.xy`) — exactly
        // as the transform pass does — not from the quad's bucket-spanning uv, or
        // the ellipse would be placed against the 64px bucket, not the canvas.
        let mask_shape_src = format!(
            "{QUAD_VS}\nstruct MS {{ center: vec4<f32>, aux: vec4<f32>, dims: vec4<f32> }}\n@group(0) @binding(0) var<uniform> u: MS;\n@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {{\n  let uv = vec2<f32>(pos.x / u.dims.x, pos.y / u.dims.y);\n  let d = uv - u.center.xy;\n  let rd = vec2<f32>(d.x * u.aux.x - d.y * u.aux.y, d.x * u.aux.y + d.y * u.aux.x);\n  var ex = 1e18; var ey = 1e18;\n  if (abs(u.center.z) > 1e-6) {{ ex = rd.x / u.center.z; }}\n  if (abs(u.center.w) > 1e-6) {{ ey = rd.y / u.center.w; }}\n  let r = sqrt(ex * ex + ey * ey);\n  let a = 1.0 - smoothstep(u.aux.z, 1.0, r);\n  return vec4<f32>(a, a, a, a);\n}}\n"
        );
        let mask_shape_pipeline = make_pipeline(device, &mask_bgl, &mask_shape_src, "fs");

        // Merge: premultiplied `over`, all 26 blend modes (K-0.3a, 03 §2.4).
        let merge_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("merge_bgl"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                sampler_entry(2),
                uniform_entry(3),
            ],
        });
        let merge_src = format!("{QUAD_VS}\n{MERGE_FS}");
        let merge_pipeline = make_pipeline(device, &merge_bgl, &merge_src, "fs");

        // WipeMix (08 §2.0b): binary directional wipe, reusing the merge BGL
        // (two textures + sampler + a params uniform). WGSL twin of `ops::wipe`.
        let wipe_src = format!("{QUAD_VS}\n{WIPE_FS}");
        let wipe_pipeline = make_pipeline(device, &merge_bgl, &wipe_src, "fs");

        // LumaWipeMix (26 K-B7): analytical map wipe, same BGL as Wipe/Merge.
        let luma_wipe_src = format!("{QUAD_VS}\n{LUMA_WIPE_FS}");
        let luma_wipe_pipeline = make_pipeline(device, &merge_bgl, &luma_wipe_src, "fs");

        // PushMix (08 §2.0b): binary directional slide via textureLoad (no
        // sampler) — its own BGL (two textures + a params uniform). WGSL twin of
        // `ops::push`.
        let push_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("push_bgl"),
            entries: &[tex_entry(0), tex_entry(1), uniform_entry(2)],
        });
        let push_src = format!("{QUAD_VS}\n{PUSH_FS}");
        let push_pipeline = make_pipeline(device, &push_bgl, &push_src, "fs");

        Passes {
            fill_pipeline,
            fill_bgl,
            blit_pipeline,
            blit_bgl,
            transform_pipeline,
            transform_bgl,
            crop_pipeline,
            resize_pipeline,
            stabilize_pipeline,
            stabilize_bgl,
            invert_pipeline,
            deflicker_pipeline,
            deinterlace_pipeline,
            filter_bgl,
            luma_key_pipeline,
            chroma_key_pipeline,
            blur_bgl,
            blur_pipeline,
            sharpen_bgl,
            sharpen_pipeline,
            high_pass_pipeline,
            emboss_pipeline,
            find_edges_pipeline,
            median_pipeline,
            levels_pipeline,
            posterize_pipeline,
            threshold_pipeline,
            desaturate_pipeline,
            vignette_pipeline,
            mosaic_pipeline,
            motion_blur_pipeline,
            hue_sat_pipeline,
            vibrance_pipeline,
            channel_mixer_pipeline,
            curves_lut_pipeline,
            black_and_white_pipeline,
            surface_blur_pipeline,
            lens_blur_pipeline,
            smart_sharpen_pipeline,
            pinch_pipeline,
            ripple_pipeline,
            perspective_pipeline,
            grain_pipeline,
            ca_pipeline,
            unpremultiply_pipeline,
            alpha_view_pipeline,
            outline_pipeline,
            drop_shadow_pipeline,
            util2_bgl,
            glow_extract_pipeline,
            glow_comp_bgl,
            glow_comp_pipeline,
            mask_bgl,
            mask_shape_pipeline,
            merge_pipeline,
            merge_bgl,
            wipe_pipeline,
            luma_wipe_pipeline,
            push_pipeline,
            push_bgl,
            sampler,
        }
    }

    fn fill(&self, gpu: &GpuContext, target: &wgpu::Texture, color: [f32; 4]) {
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fill_color"),
                contents: bytemuck::cast_slice(&color),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fill_bg"),
            layout: &self.fill_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });
        self.run(gpu, &self.fill_pipeline, &bind, target);
    }

    fn blit(&self, gpu: &GpuContext, src: &wgpu::Texture, target: &wgpu::Texture) {
        let view = src.create_view(&Default::default());
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit_bg"),
            layout: &self.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.run(gpu, &self.blit_pipeline, &bind, target);
    }

    #[allow(clippy::too_many_arguments)]
    fn crop(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        logical_w: u32,
        logical_h: u32,
        source_w: u32,
        source_h: u32,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) {
        let uniform = [
            logical_w as f32,
            logical_h as f32,
            source_w as f32,
            source_h as f32,
            left.clamp(0.0, 1.0),
            top.clamp(0.0, 1.0),
            right.clamp(0.0, 1.0),
            bottom.clamp(0.0, 1.0),
        ];
        let buffer = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("crop_uniform"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let view = src.create_view(&Default::default());
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("crop_bg"),
            layout: &self.transform_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.crop_pipeline, &bind, target);
    }

    fn transform(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        mat: glam::Mat3,
        sampling: crate::graph::ir::Sampling,
        logical_w: u32,
        logical_h: u32,
        source_w: u32,
        source_h: u32,
    ) {
        if !crate::graph::ops::transform_matrix_is_valid(mat) {
            self.fill(gpu, target, [0.0; 4]);
            return;
        }
        let inverse = mat.inverse().to_cols_array();
        let uniform = [
            inverse[0],
            inverse[1],
            inverse[2],
            if matches!(sampling, crate::graph::ir::Sampling::Nearest) {
                1.0
            } else {
                0.0
            },
            inverse[3],
            inverse[4],
            inverse[5],
            0.0,
            inverse[6],
            inverse[7],
            inverse[8],
            0.0,
            logical_w as f32,
            logical_h as f32,
            source_w as f32,
            source_h as f32,
        ];
        let buffer = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("transform_uniform"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let view = src.create_view(&Default::default());
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transform_bg"),
            layout: &self.transform_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.transform_pipeline, &bind, target);
    }

    #[allow(clippy::too_many_arguments)]
    fn resize(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        fit: crate::graph::ir::FitMode,
        output_w: u32,
        output_h: u32,
        source_w: u32,
        source_h: u32,
    ) {
        let mode = match fit {
            crate::graph::ir::FitMode::Fit => 0.0,
            crate::graph::ir::FitMode::Fill => 1.0,
            crate::graph::ir::FitMode::Stretch => 2.0,
        };
        let uniform = [
            output_w as f32,
            output_h as f32,
            source_w as f32,
            source_h as f32,
            mode,
            0.0,
            0.0,
            0.0,
        ];
        let buffer = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("resize_uniform"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let view = src.create_view(&Default::default());
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resize_bg"),
            layout: &self.transform_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.resize_pipeline, &bind, target);
    }

    /// `StabilizeWarp` pass (D-12, 22 §6.4).
    ///
    /// One pass per frame, as 22 §6.6 requires, and the same pass serves proxy
    /// preview at proxy dimensions — the intrinsics are already scaled for the
    /// delivered size by [`StabilizationAnalysis::warp_at`].
    ///
    /// [`StabilizationAnalysis::warp_at`]: crate::graph::stabilize::StabilizationAnalysis::warp_at
    #[allow(clippy::too_many_arguments)]
    fn stabilize(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        warp: &crate::graph::ir::StabilizeWarp,
        sampling: crate::graph::ir::Sampling,
        logical_w: u32,
        logical_h: u32,
        source_w: u32,
        source_h: u32,
        target: &wgpu::Texture,
    ) {
        use crate::graph::ir::Sampling;
        let r = warp.rotation;
        let uniform: [f32; 28] = [
            // r0, r1, r2 — row-major rotation, one row per vec4 (w unused).
            r[0],
            r[1],
            r[2],
            0.0,
            r[3],
            r[4],
            r[5],
            0.0,
            r[6],
            r[7],
            r[8],
            0.0,
            // k1..k4
            warp.k[0],
            warp.k[1],
            warp.k[2],
            warp.k[3],
            // fx, fy, cx, cy
            warp.intrinsics[0],
            warp.intrinsics[1],
            warp.intrinsics[2],
            warp.intrinsics[3],
            // logical / source dimensions
            logical_w as f32,
            logical_h as f32,
            source_w as f32,
            source_h as f32,
            // zoom, fisheye, transparent_edges, sampling
            warp.zoom,
            if warp.fisheye { 1.0 } else { 0.0 },
            if warp.transparent_edges { 1.0 } else { 0.0 },
            match sampling {
                Sampling::Nearest => 1.0,
                Sampling::Bilinear => 0.0,
            },
        ];
        let buffer = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("stabilize_uniform"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let view = src.create_view(&Default::default());
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stabilize_bg"),
            layout: &self.stabilize_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.stabilize_pipeline, &bind, target);
    }

    /// `Effect{Invert}` pass — same bind group as `blit` (tex + sampler), the
    /// invert pipeline instead.
    fn invert(&self, gpu: &GpuContext, src: &wgpu::Texture, target: &wgpu::Texture) {
        let view = src.create_view(&Default::default());
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("invert_bg"),
            layout: &self.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.run(gpu, &self.invert_pipeline, &bind, target);
    }

    /// K-G6 deinterlace — WGSL twin of `ops::deinterlace`.
    fn deinterlace(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        method: crate::graph::ir::DeinterlaceMethod,
        field_order: crate::graph::ir::FieldOrder,
        logical_w: u32,
        logical_h: u32,
    ) {
        use crate::graph::ir::{DeinterlaceMethod, FieldOrder};
        let method_f = match method {
            DeinterlaceMethod::OneField => 0.0,
            DeinterlaceMethod::LinearBlend => 1.0,
            DeinterlaceMethod::YadifSpatial => 2.0,
        };
        let order_f = match field_order {
            FieldOrder::TopFirst => 0.0,
            FieldOrder::BottomFirst => 1.0,
        };
        let view = src.create_view(&Default::default());
        let ubuf = gpu.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("deinterlace_u"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let info = [method_f, order_f, logical_w as f32, logical_h as f32];
        gpu.queue()
            .write_buffer(&ubuf, 0, bytemuck::cast_slice(&info));
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("deinterlace_bg"),
            layout: &self.transform_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: ubuf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.deinterlace_pipeline, &bind, target);
    }

    /// Scratch texture matching `like`'s size for multi-pass effects (blur H/V).
    fn temp_like(gpu: &GpuContext, like: &wgpu::Texture) -> wgpu::Texture {
        Self::alloc_temp(gpu, like)
    }

    fn alloc_temp(gpu: &GpuContext, like: &wgpu::Texture) -> wgpu::Texture {
        gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("eval_temp"),
            size: like.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    fn temp_texture(&self, gpu: &GpuContext, like: &wgpu::Texture) -> wgpu::Texture {
        Self::alloc_temp(gpu, like)
    }

    /// Neighbourhood pass shared by emboss / find_edges / median (blur BGL).
    fn neighbourhood(
        &self,
        gpu: &GpuContext,
        pipeline: &wgpu::RenderPipeline,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        radius: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let uniform = [radius, 0.0, logical_w as f32, logical_h as f32];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("neighbourhood_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neighbourhood_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, pipeline, &bind, target);
    }

    /// K-B16 emboss — 3×3 directional luma gradient + mid-gray bias.
    fn emboss(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        logical_w: u32,
        logical_h: u32,
    ) {
        self.neighbourhood(
            gpu,
            &self.emboss_pipeline,
            src,
            target,
            0.0,
            logical_w,
            logical_h,
        );
    }

    /// K-B16 find-edges — Sobel magnitude on straight luma.
    fn find_edges(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        logical_w: u32,
        logical_h: u32,
    ) {
        self.neighbourhood(
            gpu,
            &self.find_edges_pipeline,
            src,
            target,
            0.0,
            logical_w,
            logical_h,
        );
    }

    /// K-B16 median — per-channel 3×3 median (radius ≥ 1; larger still 3×3).
    fn median(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        radius: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let r = if radius.is_finite() {
            radius.max(0.0)
        } else {
            0.0
        };
        if r < 0.5 {
            self.blit(gpu, src, target);
            return;
        }
        self.neighbourhood(
            gpu,
            &self.median_pipeline,
            src,
            target,
            1.0,
            logical_w,
            logical_h,
        );
    }

    /// Levels — `p = [in_black, in_white, gamma, out_black]` + out_white in p1.x.
    fn levels(&self, gpu: &GpuContext, src: &wgpu::Texture, target: &wgpu::Texture, p: [f32; 5]) {
        let uniform = [p[0], p[1], p[2], p[3], p[4], 0.0, 0.0, 0.0];
        self.run_filter(gpu, &self.levels_pipeline, src, target, &uniform);
    }

    fn posterize(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        levels: f32,
    ) {
        let n = if levels.is_finite() {
            levels.clamp(2.0, 255.0)
        } else {
            4.0
        };
        self.run_filter(
            gpu,
            &self.posterize_pipeline,
            src,
            target,
            &[n, 0.0, 0.0, 0.0],
        );
    }

    fn threshold(&self, gpu: &GpuContext, src: &wgpu::Texture, target: &wgpu::Texture, level: f32) {
        let t = if level.is_finite() {
            level.clamp(0.0, 1.0)
        } else {
            0.5
        };
        self.run_filter(
            gpu,
            &self.threshold_pipeline,
            src,
            target,
            &[t, 0.0, 0.0, 0.0],
        );
    }

    fn desaturate(&self, gpu: &GpuContext, src: &wgpu::Texture, target: &wgpu::Texture) {
        self.run_filter(gpu, &self.desaturate_pipeline, src, target, &[0.0; 4]);
    }

    fn vignette(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        amount: f32,
        feather: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let amount = if amount.is_finite() {
            amount.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let feather = if feather.is_finite() {
            feather.clamp(0.0, 1.0)
        } else {
            0.5
        };
        let uniform = [amount, feather, logical_w as f32, logical_h as f32];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vignette_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vignette_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.vignette_pipeline, &bind, target);
    }

    fn mosaic(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        block: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let b = if block.is_finite() {
            block.max(1.0)
        } else {
            8.0
        };
        self.neighbourhood(
            gpu,
            &self.mosaic_pipeline,
            src,
            target,
            b,
            logical_w,
            logical_h,
        );
    }

    fn motion_blur(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        angle_deg: f32,
        distance: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let angle = if angle_deg.is_finite() {
            angle_deg.to_radians()
        } else {
            0.0
        };
        let dist = if distance.is_finite() {
            distance.max(0.0).min(128.0)
        } else {
            0.0
        };
        let uniform = [angle, dist, logical_w as f32, logical_h as f32];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("motion_blur_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("motion_blur_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.motion_blur_pipeline, &bind, target);
    }

    fn hue_saturation(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        hue: f32,
        sat: f32,
        lightness: f32,
    ) {
        self.run_filter(
            gpu,
            &self.hue_sat_pipeline,
            src,
            target,
            &[hue, sat, lightness, 0.0],
        );
    }

    fn vibrance(&self, gpu: &GpuContext, src: &wgpu::Texture, target: &wgpu::Texture, amount: f32) {
        let a = if amount.is_finite() {
            amount.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.run_filter(
            gpu,
            &self.vibrance_pipeline,
            src,
            target,
            &[a, 0.0, 0.0, 0.0],
        );
    }

    fn channel_mixer(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        m: [f32; 9],
    ) {
        let uniform = [
            m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], 0.0, 0.0, 0.0,
        ];
        self.run_filter(gpu, &self.channel_mixer_pipeline, src, target, &uniform);
    }

    /// Multi-point RGB curve through up to 5 knots (piecewise linear).
    fn curves_lut(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        knots: &[[f32; 2]; 5],
    ) {
        // Sort by x.
        let mut pts = *knots;
        pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
        // Ensure endpoints span 0..1.
        pts[0][0] = pts[0][0].clamp(0.0, 1.0);
        pts[4][0] = pts[4][0].clamp(0.0, 1.0);
        let uniform = [
            pts[0][0], pts[0][1], pts[1][0], pts[1][1], pts[2][0], pts[2][1], pts[3][0], pts[3][1],
            pts[4][0], pts[4][1], 0.0, 0.0,
        ];
        self.run_filter(gpu, &self.curves_lut_pipeline, src, target, &uniform);
    }

    fn surface_blur(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        radius: f32,
        threshold: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let r = if radius.is_finite() {
            radius.max(0.0).min(8.0)
        } else {
            0.0
        };
        let thr = if threshold.is_finite() {
            threshold.clamp(0.0, 1.0).max(1e-4)
        } else {
            0.25
        };
        let uniform = [r, thr, logical_w as f32, logical_h as f32];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("surface_blur_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("surface_blur_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.surface_blur_pipeline, &bind, target);
    }

    fn lens_blur(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        radius: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let r = if radius.is_finite() {
            radius.max(0.0).min(12.0)
        } else {
            0.0
        };
        self.neighbourhood(
            gpu,
            &self.lens_blur_pipeline,
            src,
            target,
            r,
            logical_w,
            logical_h,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn smart_sharpen(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        amount: f32,
        radius: f32,
        threshold: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let amount = if amount.is_finite() {
            amount.max(0.0)
        } else {
            0.0
        };
        if amount == 0.0 {
            self.blit(gpu, src, target);
            return;
        }
        let radius = if radius.is_finite() {
            radius.max(0.0)
        } else {
            1.0
        };
        let threshold = if threshold.is_finite() {
            threshold.clamp(0.0, 255.0)
        } else {
            0.0
        };
        let blurred = Self::temp_like(gpu, target);
        self.gaussian_blur(gpu, src, &blurred, radius, logical_w, logical_h);
        gpu.device().poll(wgpu::Maintain::Wait);
        let sv = src.create_view(&Default::default());
        let bv = blurred.create_view(&Default::default());
        let uniform = [amount, threshold, 0.0, 0.0];
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("smart_sharpen_u"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("smart_sharpen_bg"),
            layout: &self.sharpen_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&sv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&bv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.smart_sharpen_pipeline, &bind, target);
    }

    fn black_and_white(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        w: [f32; 3],
    ) {
        self.run_filter(
            gpu,
            &self.black_and_white_pipeline,
            src,
            target,
            &[w[0], w[1], w[2], 0.0],
        );
    }

    fn pinch(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        amount: f32,
        spherize: bool,
        logical_w: u32,
        logical_h: u32,
    ) {
        let amount = if amount.is_finite() {
            amount.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let uniform = [
            amount,
            if spherize { 1.0 } else { 0.0 },
            logical_w as f32,
            logical_h as f32,
        ];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pinch_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pinch_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.pinch_pipeline, &bind, target);
    }

    fn ripple(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        amplitude: f32,
        wavelength: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let amp = if amplitude.is_finite() {
            amplitude
        } else {
            0.0
        };
        let wl = if wavelength.is_finite() {
            wavelength.max(1.0)
        } else {
            16.0
        };
        let uniform = [amp, wl, logical_w as f32, logical_h as f32];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ripple_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ripple_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.ripple_pipeline, &bind, target);
    }

    fn perspective(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        corners: [f32; 8],
        logical_w: u32,
        logical_h: u32,
    ) {
        let uniform = [
            corners[0],
            corners[1],
            corners[2],
            corners[3],
            corners[4],
            corners[5],
            corners[6],
            corners[7],
            logical_w as f32,
            logical_h as f32,
            0.0,
            0.0,
        ];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("perspective_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("perspective_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        // perspective uniform is 48 bytes; blur_bgl only has min_binding_size None so OK.
        self.run(gpu, &self.perspective_pipeline, &bind, target);
    }

    fn grain(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        amount: f32,
        mono: bool,
        seed: f32,
        _logical_w: u32,
        _logical_h: u32,
    ) {
        let amount = if amount.is_finite() {
            amount.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.run_filter(
            gpu,
            &self.grain_pipeline,
            src,
            target,
            &[amount, if mono { 1.0 } else { 0.0 }, seed, 0.0],
        );
    }

    fn chromatic_aberration(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        amount: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let amount = if amount.is_finite() { amount } else { 0.0 };
        let uniform = [amount, 0.0, logical_w as f32, logical_h as f32];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ca_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ca_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.ca_pipeline, &bind, target);
    }

    fn unpremultiply(&self, gpu: &GpuContext, src: &wgpu::Texture, target: &wgpu::Texture) {
        self.run_filter(gpu, &self.unpremultiply_pipeline, src, target, &[0.0; 4]);
    }

    fn alpha_view(&self, gpu: &GpuContext, src: &wgpu::Texture, target: &wgpu::Texture, mode: f32) {
        self.run_filter(
            gpu,
            &self.alpha_view_pipeline,
            src,
            target,
            &[mode, 0.0, 0.0, 0.0],
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn drop_shadow(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        ox: f32,
        oy: f32,
        _radius: f32,
        color: [f32; 3],
        opacity: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let uniform = [
            ox,
            oy,
            logical_w as f32,
            logical_h as f32,
            color[0],
            color[1],
            color[2],
            if opacity.is_finite() {
                opacity.clamp(0.0, 1.0)
            } else {
                0.5
            },
        ];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("drop_shadow_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("drop_shadow_bg"),
            layout: &self.util2_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.drop_shadow_pipeline, &bind, target);
    }

    #[allow(clippy::too_many_arguments)]
    fn outline(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        thickness: f32,
        color: [f32; 3],
        opacity: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let thick = if thickness.is_finite() {
            thickness.max(0.0)
        } else {
            0.0
        };
        let opac = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let uniform = [
            thick,
            opac,
            logical_w as f32,
            logical_h as f32,
            color[0],
            color[1],
            color[2],
            0.0,
        ];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("outline_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("outline_bg"),
            layout: &self.util2_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.outline_pipeline, &bind, target);
    }

    /// High-pass combine: `src - blurred` on RGB, keep src alpha (K-B16).
    fn high_pass_combine(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        blurred: &wgpu::Texture,
        target: &wgpu::Texture,
    ) {
        let sv = src.create_view(&Default::default());
        let bv = blurred.create_view(&Default::default());
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("high_pass_bg"),
            layout: &self.sharpen_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&sv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&bv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: gpu
                        .device()
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("high_pass_u"),
                            // amount = -1 ⇒ src + (-1)*(src-blur) wait — we want src-blur.
                            // sharpen is src + amount*(src-blur). amount=1 ⇒ 2src-blur.
                            // So use a dedicated small shader via amount=-0 and...
                            // Use amount=1 on (src, 2*blur-src)? Simpler: pass amount
                            // as a special: we'll use amount = -1 with inverted meaning
                            // in a one-off uniform: store 1.0 and use high_pass_pipeline.
                            contents: bytemuck::cast_slice(&[1.0f32, 0.0, 0.0, 0.0]),
                            usage: wgpu::BufferUsages::UNIFORM,
                        })
                        .as_entire_binding(),
                },
            ],
        });
        // Reuse sharpen BGL layout but high_pass_pipeline: out = src - blur.
        self.run(gpu, &self.high_pass_pipeline, &bind, target);
    }

    /// One axis of the separable Gaussian (`horizontal` = H pass).
    fn blur_axis(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        sigma: f32,
        horizontal: bool,
        logical_w: u32,
        logical_h: u32,
    ) {
        let uniform = [
            sigma,
            if horizontal { 1.0 } else { 0.0 },
            logical_w as f32,
            logical_h as f32,
        ];
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur_params"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.blur_pipeline, &bind, target);
    }

    /// `Effect{Blur}` — multi-iteration dual-pass separable Gaussian (proposal
    /// 209 / `ops::blur_plan`). `sigma < 0.5` is a blit (matches `ops::blur` /
    /// the shader early-out). Each axis is a separate submit; we `poll(Wait)`
    /// between them so the V pass samples a finished H target.
    fn gaussian_blur(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        sigma: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let plan = crate::graph::ops::blur_plan(sigma);
        if plan.iterations == 0 {
            self.blit(gpu, src, target);
            return;
        }
        // GPU path: multi-iter with unit-step kernels (shader has no step
        // uniform). Large-σ quality comes from σ_pass = σ/√n stacking.
        let h_tmp = Self::temp_like(gpu, target);
        let ping = Self::temp_like(gpu, target);
        let pong = Self::temp_like(gpu, target);
        for i in 0..plan.iterations {
            let is_first = i == 0;
            let is_last = i + 1 == plan.iterations;
            // Even iters write ping (or target on last); odd write pong (or target).
            let from: &wgpu::Texture = if is_first {
                src
            } else if i % 2 == 1 {
                &ping
            } else {
                &pong
            };
            let to: &wgpu::Texture = if is_last {
                target
            } else if i % 2 == 0 {
                &ping
            } else {
                &pong
            };
            self.blur_axis(
                gpu,
                from,
                &h_tmp,
                plan.sigma_pass,
                true,
                logical_w,
                logical_h,
            );
            gpu.device().poll(wgpu::Maintain::Wait);
            self.blur_axis(
                gpu,
                &h_tmp,
                to,
                plan.sigma_pass,
                false,
                logical_w,
                logical_h,
            );
            if !is_last {
                gpu.device().poll(wgpu::Maintain::Wait);
            }
        }
    }

    /// `Effect{Sharpen}` — unsharp mask via blur intermediate + combine pass.
    #[allow(clippy::too_many_arguments)]
    fn sharpen(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        amount: f32,
        radius: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let amount = if amount.is_finite() { amount } else { 0.0 };
        if amount == 0.0 {
            self.blit(gpu, src, target);
            return;
        }
        let blurred = Self::temp_like(gpu, target);
        self.gaussian_blur(gpu, src, &blurred, radius, logical_w, logical_h);
        gpu.device().poll(wgpu::Maintain::Wait);
        let sv = src.create_view(&Default::default());
        let bv = blurred.create_view(&Default::default());
        let uniform = [amount, 0.0, 0.0, 0.0];
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sharpen_uniform"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sharpen_bg"),
            layout: &self.sharpen_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&sv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&bv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.sharpen_pipeline, &bind, target);
    }

    /// `Effect{Glow}` — extract brights → blur → tinted screen-add over source.
    #[allow(clippy::too_many_arguments)]
    fn glow(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        radius: f32,
        threshold: f32,
        intensity: f32,
        tint_linear: [f32; 3],
        logical_w: u32,
        logical_h: u32,
    ) {
        let intensity = if intensity.is_finite() {
            intensity.max(0.0)
        } else {
            0.0
        };
        if intensity == 0.0 {
            self.blit(gpu, src, target);
            return;
        }
        let threshold = if threshold.is_finite() {
            threshold.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let extract = Self::temp_like(gpu, target);
        let uniform = [threshold, 0.0, 0.0, 0.0];
        self.run_filter(gpu, &self.glow_extract_pipeline, src, &extract, &uniform);
        gpu.device().poll(wgpu::Maintain::Wait);
        let blurred = Self::temp_like(gpu, target);
        self.gaussian_blur(gpu, &extract, &blurred, radius, logical_w, logical_h);
        gpu.device().poll(wgpu::Maintain::Wait);
        let sv = src.create_view(&Default::default());
        let gv = blurred.create_view(&Default::default());
        let u = [
            tint_linear[0],
            tint_linear[1],
            tint_linear[2],
            0.0,
            intensity,
            0.0,
            0.0,
            0.0,
        ];
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("glow_comp_uniform"),
                contents: bytemuck::cast_slice(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glow_comp_bg"),
            layout: &self.glow_comp_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&sv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&gv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.glow_comp_pipeline, &bind, target);
    }

    /// `Effect{LumaKey}` pass — tex + sampler + a `[threshold, hi, invert, pad]`
    /// uniform. `hi` is pre-floored in Rust so the shader's smoothstep band and
    /// `ops::luma_key`'s agree exactly (parity).
    fn luma_key(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        threshold: f32,
        softness: f32,
        invert: bool,
    ) {
        let hi = threshold + softness.max(crate::graph::ops::KEY_BAND_EPS);
        let uniform = [threshold, hi, if invert { 1.0 } else { 0.0 }, 0.0];
        self.run_filter(gpu, &self.luma_key_pipeline, src, target, &uniform);
    }

    /// `Effect{Deflicker}` pass — tex + sampler + `[gain_r, gain_g, gain_b,
    /// knee]` and `[band_cycles, band_amp, band_phase, rows]` uniforms.
    ///
    /// `rows` must be the **logical** height, not the physical pool-bucket
    /// height, or the band frequency is scaled wrong; and the shader subtracts
    /// the half-pixel from `@builtin(position)` so its row index matches the
    /// CPU's integer `y`. Both are asserted by the GPU/CPU band parity test. The gain is resolved by the compile pass from a cached
    /// whole-range measurement; this pass only applies it.
    fn deflicker(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        gain: [f32; 3],
        band: Option<crate::graph::rolling_bands::BandModel>,
        rows: u32,
    ) {
        let uniform = [
            gain[0],
            gain[1],
            gain[2],
            crate::graph::deflicker::HIGHLIGHT_KNEE,
            band.map(|b| b.cycles as f32).unwrap_or(0.0),
            band.map(|b| b.amplitude).unwrap_or(0.0),
            band.map(|b| b.phase).unwrap_or(0.0),
            rows as f32,
        ];
        self.run_filter(gpu, &self.deflicker_pipeline, src, target, &uniform);
    }

    /// `Effect{ChromaKey}` pass — tex + sampler + a `[key.rgb, tolerance | hi,
    /// spill, dom, pad]` uniform. `key` is the sRGB→linear key colour, `dom` the
    /// dominant key channel; both are computed in Rust so the GPU matches
    /// `ops::chroma_key` exactly.
    fn chroma_key(
        &self,
        gpu: &GpuContext,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        key_linear: [f32; 3],
        tolerance: f32,
        edge_softness: f32,
        spill_suppress: f32,
    ) {
        let hi = tolerance + edge_softness.max(crate::graph::ops::KEY_BAND_EPS);
        let dom = if key_linear[0] >= key_linear[1] && key_linear[0] >= key_linear[2] {
            0.0
        } else if key_linear[1] >= key_linear[2] {
            1.0
        } else {
            2.0
        };
        let uniform = [
            key_linear[0],
            key_linear[1],
            key_linear[2],
            tolerance,
            hi,
            spill_suppress,
            dom,
            0.0,
        ];
        self.run_filter(gpu, &self.chroma_key_pipeline, src, target, &uniform);
    }

    /// Shared driver for the tex+sampler+uniform filter passes (LumaKey/ChromaKey).
    fn run_filter(
        &self,
        gpu: &GpuContext,
        pipeline: &wgpu::RenderPipeline,
        src: &wgpu::Texture,
        target: &wgpu::Texture,
        uniform: &[f32],
    ) {
        let view = src.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("filter_uniform"),
                contents: bytemuck::cast_slice(uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("filter_bg"),
            layout: &self.filter_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, pipeline, &bind, target);
    }

    /// `Effect{MaskShapeGen}` pass — a uniform-only ellipse-matte generator (no
    /// input texture), the WGSL twin of `ops::mask_shape`.
    #[allow(clippy::too_many_arguments)]
    fn mask_shape(
        &self,
        gpu: &GpuContext,
        target: &wgpu::Texture,
        center: [f32; 2],
        size: [f32; 2],
        rotation: f32,
        feather: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        let (cos_r, sin_r) = ((-rotation).cos(), (-rotation).sin());
        let inner = 1.0 - feather.clamp(0.0, 1.0).max(crate::graph::ops::KEY_BAND_EPS);
        let uniform = [
            center[0],
            center[1],
            size[0],
            size[1],
            cos_r,
            sin_r,
            inner,
            0.0,
            logical_w as f32,
            logical_h as f32,
            0.0,
            0.0,
        ];
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mask_uniform"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask_bg"),
            layout: &self.mask_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });
        self.run(gpu, &self.mask_shape_pipeline, &bind, target);
    }

    fn merge(
        &self,
        gpu: &GpuContext,
        top: &wgpu::Texture,
        bottom: &wgpu::Texture,
        mode: BlendMode,
        opacity: f32,
        target: &wgpu::Texture,
    ) {
        let tv = top.create_view(&Default::default());
        let bv = bottom.create_view(&Default::default());
        // Clamp opacity to match the CPU reference `graph::ops::merge` exactly.
        let uni = MergeParams {
            mode: merge_mode_index(mode),
            opacity: opacity.clamp(0.0, 1.0),
            _pad: [0.0; 2],
        };
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("merge_uni"),
                contents: bytemuck::bytes_of(&uni),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("merge_bg"),
            layout: &self.merge_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&bv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.merge_pipeline, &bind, target);
    }

    /// `WipeMix` pass (08 §2.0b): the WGSL twin of `ops::wipe`. `edge`/`hw` are
    /// computed in Rust identically to the CPU kernel so the smoothstep bands
    /// agree; the sweep coord is derived in-shader from the logical dims.
    #[allow(clippy::too_many_arguments)]
    fn wipe(
        &self,
        gpu: &GpuContext,
        incoming: &wgpu::Texture,
        outgoing: &wgpu::Texture,
        dir: WipeDirection,
        softness: f32,
        t: f32,
        target: &wgpu::Texture,
        logical_w: u32,
        logical_h: u32,
    ) {
        let s = softness.max(0.0);
        let edge = -s + t * (1.0 + 2.0 * s);
        let hw = s.max(crate::graph::ops::KEY_BAND_EPS);
        let uniform = [
            wipe_direction_index(dir),
            edge,
            hw,
            0.0,
            logical_w as f32,
            logical_h as f32,
            0.0,
            0.0,
        ];
        let iv = incoming.create_view(&Default::default());
        let ov = outgoing.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("wipe_uniform"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wipe_bg"),
            layout: &self.merge_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&iv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&ov),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.wipe_pipeline, &bind, target);
    }

    /// `LumaWipeMix` pass (26 K-B7): WGSL twin of `ops::luma_wipe`.
    #[allow(clippy::too_many_arguments)]
    fn luma_wipe(
        &self,
        gpu: &GpuContext,
        incoming: &wgpu::Texture,
        outgoing: &wgpu::Texture,
        kind: crate::graph::luma_wipe::LumaWipeKind,
        softness: f32,
        invert: bool,
        t: f32,
        target: &wgpu::Texture,
        logical_w: u32,
        logical_h: u32,
    ) {
        let uniform = [
            kind as u8 as f32,
            softness.clamp(0.0, 0.5),
            if invert { 1.0 } else { 0.0 },
            t,
            logical_w as f32,
            logical_h as f32,
            0.0,
            0.0,
        ];
        let iv = incoming.create_view(&Default::default());
        let ov = outgoing.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("luma_wipe_uniform"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luma_wipe_bg"),
            layout: &self.merge_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&iv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&ov),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.luma_wipe_pipeline, &bind, target);
    }

    /// `PushMix` pass (08 §2.0b): the WGSL twin of `ops::push`. `dims.zw` is the
    /// source's logical size (edge-clamp bound); `dims.xy` the logical canvas.
    #[allow(clippy::too_many_arguments)]
    fn push(
        &self,
        gpu: &GpuContext,
        incoming: &wgpu::Texture,
        outgoing: &wgpu::Texture,
        dir: WipeDirection,
        t: f32,
        target: &wgpu::Texture,
        logical_w: u32,
        logical_h: u32,
        source_w: u32,
        source_h: u32,
    ) {
        let uniform = [
            wipe_direction_index(dir),
            t,
            0.0,
            0.0,
            logical_w as f32,
            logical_h as f32,
            source_w as f32,
            source_h as f32,
        ];
        let iv = incoming.create_view(&Default::default());
        let ov = outgoing.create_view(&Default::default());
        let buf = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("push_uniform"),
                contents: bytemuck::cast_slice(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("push_bg"),
            layout: &self.push_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&iv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&ov),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        self.run(gpu, &self.push_pipeline, &bind, target);
    }

    fn run(
        &self,
        gpu: &GpuContext,
        pipeline: &wgpu::RenderPipeline,
        bind: &wgpu::BindGroup,
        target: &wgpu::Texture,
    ) {
        let view = target.create_view(&Default::default());
        let mut enc = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("eval_pass_enc"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("eval_pass"),
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
        gpu.queue().submit([enc.finish()]);
    }
}

fn make_pipeline(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    src: &str,
    fs_entry: &str,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("eval_shader"),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("eval_layout"),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("eval_pipeline"),
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

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

// ── Readback (tests + the session's sampled-pixel probe) ──────────────────────

/// Read a `Rgba16Float` texture back to CPU as `[r, g, b, a]` f32 pixels
/// (row-major, `w*h`). Used by tests and the engine's sampled-pixel status.
pub fn read_texture_rgba16f(
    gpu: &GpuContext,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Vec<[f32; 4]> {
    let bpp = 8u32; // Rgba16Float
    let unaligned = w * bpp;
    let bpr = align256(unaligned);
    let staging = gpu.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device().create_command_encoder(&Default::default());
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
    gpu.queue().submit([enc.finish()]);
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device().poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let raw = slice.get_mapped_range();
    let mut out = Vec::with_capacity((w * h) as usize);
    for row in 0..h {
        let base = (row * bpr) as usize;
        for x in 0..w {
            let o = base + (x * bpp) as usize;
            out.push([
                f16_to_f32(u16::from_le_bytes([raw[o], raw[o + 1]])),
                f16_to_f32(u16::from_le_bytes([raw[o + 2], raw[o + 3]])),
                f16_to_f32(u16::from_le_bytes([raw[o + 4], raw[o + 5]])),
                f16_to_f32(u16::from_le_bytes([raw[o + 6], raw[o + 7]])),
            ]);
        }
    }
    drop(raw);
    staging.unmap();
    out
}

fn align256(n: u32) -> u32 {
    (n + 255) & !255
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::compile::{compile, Quality};
    use crate::graph::eval_cpu::FrameProvider;
    use crate::graph::ir::{ContentHash, IrNode, IrNodeId, OutPort, Sampling};
    use crate::graph::ops::Image;
    use photonic_core::timeline::{
        CaptionCue, CaptionStyle, CaptionTrack, CaptionWord, Clip, ClipSource, FrameRate,
        KaraokeMode, KaraokeStyle, Sequence, SequenceId, TextClipContent, TimelineProject, Track,
        TrackKind,
    };
    use photonic_core::Color;

    struct PatternSource {
        frame: GpuFrame,
    }

    struct PatternCpuSource {
        image: Image,
    }

    impl FrameProvider for PatternCpuSource {
        fn decode_video(&mut self, _: AssetId, _: Tick, _: bool, _: u32, _: u32) -> Image {
            self.image.clone()
        }
        fn decode_still(&mut self, _: AssetId, _: u32, _: u32) -> Image {
            self.image.clone()
        }
        fn raster_vector(&mut self, _: VectorRef, _: VectorStateKey, _: u32, _: u32) -> Image {
            self.image.clone()
        }
    }

    impl GpuFrameSource for PatternSource {
        fn video_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            _: Tick,
            _: bool,
        ) -> Option<GpuFrame> {
            Some(self.frame.clone())
        }
        fn still_texture(
            &mut self,
            _: &GpuContext,
            _: AssetId,
            _: u32,
            _: u32,
        ) -> Option<GpuFrame> {
            Some(self.frame.clone())
        }
        fn vector_texture(
            &mut self,
            _: &GpuContext,
            _: VectorRef,
            _: VectorStateKey,
            _: u32,
            _: u32,
        ) -> Option<GpuFrame> {
            Some(self.frame.clone())
        }
    }

    fn patterned_image(width: u32, height: u32) -> Image {
        let mut image = Image::new(width, height);
        for y in 0..height {
            for x in 0..width {
                image.pixels[(y * width + x) as usize] = [
                    (x % 2) as f32,
                    (y % 2) as f32,
                    ((x + 2 * y) % 3 == 0) as u8 as f32,
                    1.0,
                ];
            }
        }
        image
    }

    fn upload_pattern(gpu: &GpuContext, image: &Image) -> Arc<wgpu::Texture> {
        let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("transform_test_pattern"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut half = Vec::with_capacity(image.pixels.len() * 4);
        for pixel in &image.pixels {
            for channel in pixel {
                half.push(if *channel == 0.0 { 0u16 } else { 0x3c00u16 });
            }
        }
        gpu.queue().write_texture(
            texture.as_image_copy(),
            bytemuck::cast_slice(&half),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 8),
                rows_per_image: Some(image.height),
            },
            wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
        );
        Arc::new(texture)
    }

    fn upload_padded_solid(
        gpu: &GpuContext,
        logical: (u32, u32),
        physical: (u32, u32),
        content: [u16; 4],
        padding: [u16; 4],
    ) -> Arc<wgpu::Texture> {
        let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("logical_padding_test"),
            size: wgpu::Extent3d {
                width: physical.0,
                height: physical.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut half = Vec::with_capacity((physical.0 * physical.1 * 4) as usize);
        for y in 0..physical.1 {
            for _x in 0..physical.0 {
                let pixel = if y < logical.1 { content } else { padding };
                half.extend_from_slice(&pixel);
            }
        }
        gpu.queue().write_texture(
            texture.as_image_copy(),
            bytemuck::cast_slice(&half),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(physical.0 * 8),
                rows_per_image: Some(physical.1),
            },
            wgpu::Extent3d {
                width: physical.0,
                height: physical.1,
                depth_or_array_layers: 1,
            },
        );
        Arc::new(texture)
    }

    #[test]
    fn changing_processing_canvas_preserves_full_detail_in_cached_outputs() {
        let Some(gpu) = GpuContext::request_blocking() else {
            assert!(
                std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
                "GPU required"
            );
            eprintln!("no GPU adapter; skipping processing-canvas regression");
            return;
        };
        // An explicit output size stays unchanged between Draft and Full. The
        // one-pixel pattern exposes accidental reuse of a downsampled input.
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            let pattern = patterned_image(w, h);
            let mut source = PatternSource {
                frame: GpuFrame::new(upload_pattern(&gpu, &pattern), w, h),
            };
            let graph = FrameGraph {
                nodes: vec![
                    IrNode {
                        op: IrOp::DecodeStill {
                            asset: AssetId::new(),
                        },
                        inputs: vec![],
                        content_hash: ContentHash(710),
                    },
                    IrNode {
                        op: IrOp::Output { w, h },
                        inputs: vec![(IrNodeId(0), OutPort::default())],
                        content_hash: ContentHash(711),
                    },
                ],
                output: Some(IrNodeId(1)),
            };
            let mut reused = Evaluator::new(gpu.clone());
            let draft = reused.evaluate(&graph, (960, 540), &mut source).unwrap();
            let draft_pixels = read_texture_rgba16f(&gpu, &draft, w, h);
            let full = reused.evaluate(&graph, (w, h), &mut source).unwrap();
            let full_pixels = read_texture_rgba16f(&gpu, &full, w, h);
            let mut fresh = Evaluator::new(gpu.clone());
            let reference = fresh.evaluate(&graph, (w, h), &mut source).unwrap();
            let reference_pixels = read_texture_rgba16f(&gpu, &reference, w, h);
            assert_eq!(
                full_pixels, reference_pixels,
                "reused Full differs at {w}x{h}"
            );
            assert!(
                full_pixels[201][0] - full_pixels[200][0] > 0.9,
                "Full lost detail at {w}x{h}"
            );
            assert!(
                (draft_pixels[201][0] - draft_pixels[200][0]).abs() < 0.1,
                "Draft fixture must lose one-pixel detail"
            );
        }
    }

    #[test]
    fn output_does_not_stretch_bucket_padding_into_logical_frame() {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("no GPU adapter; skipping logical-output padding policy");
            return;
        };
        let source_logical = (960, 540);
        let source_physical = (960, 576);
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::DecodeStill {
                        asset: AssetId::new(),
                    },
                    inputs: vec![],
                    content_hash: ContentHash(700),
                },
                IrNode {
                    op: IrOp::Output { w: 1920, h: 1080 },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(701),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        let frame = GpuFrame::new(
            upload_padded_solid(
                &gpu,
                source_logical,
                source_physical,
                [0x3c00, 0, 0, 0x3c00],
                [0, 0, 0x3c00, 0x3c00],
            ),
            source_logical.0,
            source_logical.1,
        );
        let mut source = PatternSource { frame };
        let mut evaluator = Evaluator::new(gpu.clone());
        let output = evaluator
            .evaluate(&graph, source_logical, &mut source)
            .expect("output frame");
        let actual = read_texture_rgba16f(&gpu, &output, 1920, 1080);
        for (index, pixel) in actual.iter().enumerate() {
            assert!(
                pixel[0] > 0.9,
                "logical pixel {index} lost content: {pixel:?}"
            );
            assert!(
                pixel[2] < 0.1,
                "padding leaked at logical pixel {index}: {pixel:?}"
            );
        }
    }

    #[test]
    fn crop_gpu_matches_cpu_reference() {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("no GPU adapter; skipping crop parity");
            return;
        };
        let image = patterned_image(8, 6);
        let crop = IrOp::Crop {
            left: 0.125,
            top: 0.25,
            right: 0.25,
            bottom: 0.125,
        };
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::DecodeStill {
                        asset: AssetId::new(),
                    },
                    inputs: vec![],
                    content_hash: ContentHash(702),
                },
                IrNode {
                    op: crop.clone(),
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(703),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        let mut cpu_source = PatternCpuSource {
            image: image.clone(),
        };
        let expected = crate::graph::eval_cpu::evaluate(&graph, (8, 6), &mut cpu_source);
        let mut gpu_source = PatternSource {
            frame: GpuFrame::new(upload_pattern(&gpu, &image), image.width, image.height),
        };
        let mut evaluator = Evaluator::new(gpu.clone());
        let output = evaluator
            .evaluate(&graph, (8, 6), &mut gpu_source)
            .expect("crop output");
        let actual = read_texture_rgba16f(&gpu, &output, 8, 6);
        for (pixel_index, (gpu_pixel, cpu_pixel)) in actual.iter().zip(&expected.pixels).enumerate()
        {
            for channel in 0..4 {
                assert!(
                    (gpu_pixel[channel] - cpu_pixel[channel]).abs() < 2e-3,
                    "pixel {pixel_index} channel {channel}: GPU {} vs CPU {}",
                    gpu_pixel[channel],
                    cpu_pixel[channel]
                );
            }
        }
    }

    #[test]
    fn transform2d_pattern_gpu_matches_cpu_for_bilinear_and_nearest() {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("no GPU adapter; skipping GPU transform parity");
            return;
        };
        let image = patterned_image(11, 7);
        let mat = glam::Mat3::from_translation(glam::Vec2::new(1.25, -0.75))
            * glam::Mat3::from_angle(0.23)
            * glam::Mat3::from_scale(glam::Vec2::new(1.2, 0.8));

        for (index, sampling) in [Sampling::Bilinear, Sampling::Nearest]
            .into_iter()
            .enumerate()
        {
            let graph = FrameGraph {
                nodes: vec![
                    IrNode {
                        op: IrOp::DecodeStill {
                            asset: AssetId::new(),
                        },
                        inputs: vec![],
                        content_hash: ContentHash(100 + index as u128),
                    },
                    IrNode {
                        op: IrOp::Transform2D { mat, sampling },
                        inputs: vec![(IrNodeId(0), OutPort::default())],
                        content_hash: ContentHash(200 + index as u128),
                    },
                ],
                output: Some(IrNodeId(1)),
            };
            let mut source = PatternSource {
                frame: GpuFrame::new(upload_pattern(&gpu, &image), image.width, image.height),
            };
            let mut evaluator = Evaluator::new(gpu.clone());
            let output = evaluator
                .evaluate(&graph, (image.width, image.height), &mut source)
                .expect("transform output");
            let actual = read_texture_rgba16f(&gpu, &output, image.width, image.height);
            let expected = crate::graph::ops::transform2d(&image, mat, sampling);
            for (pixel_index, (gpu_pixel, cpu_pixel)) in
                actual.iter().zip(&expected.pixels).enumerate()
            {
                for channel in 0..4 {
                    assert!(
                        (gpu_pixel[channel] - cpu_pixel[channel]).abs() < 2e-3,
                        "{sampling:?} pixel {pixel_index} channel {channel}: GPU {} vs CPU {}",
                        gpu_pixel[channel],
                        cpu_pixel[channel]
                    );
                }
            }
        }
    }

    #[test]
    fn transform2d_native_source_gpu_matches_canvas_normalized_cpu() {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("no GPU adapter; skipping native-source transform parity");
            return;
        };
        let image = patterned_image(7, 5);
        let (canvas_w, canvas_h) = (11, 9);
        let mat = glam::Mat3::from_translation(glam::Vec2::new(0.75, -0.5))
            * glam::Mat3::from_angle(0.17)
            * glam::Mat3::from_scale(glam::Vec2::new(1.1, 0.85));

        for (index, sampling) in [Sampling::Bilinear, Sampling::Nearest]
            .into_iter()
            .enumerate()
        {
            let graph = FrameGraph {
                nodes: vec![
                    IrNode {
                        op: IrOp::DecodeStill {
                            asset: AssetId::new(),
                        },
                        inputs: vec![],
                        content_hash: ContentHash(300 + index as u128),
                    },
                    IrNode {
                        op: IrOp::Transform2D { mat, sampling },
                        inputs: vec![(IrNodeId(0), OutPort::default())],
                        content_hash: ContentHash(400 + index as u128),
                    },
                    IrNode {
                        op: IrOp::Output {
                            w: canvas_w,
                            h: canvas_h,
                        },
                        inputs: vec![(IrNodeId(1), OutPort::default())],
                        content_hash: ContentHash(500 + index as u128),
                    },
                ],
                output: Some(IrNodeId(2)),
            };
            let mut cpu_source = PatternCpuSource {
                image: image.clone(),
            };
            let expected =
                crate::graph::eval_cpu::evaluate(&graph, (canvas_w, canvas_h), &mut cpu_source);
            let mut gpu_source = PatternSource {
                frame: GpuFrame::new(upload_pattern(&gpu, &image), image.width, image.height),
            };
            let mut evaluator = Evaluator::new(gpu.clone());
            let output = evaluator
                .evaluate(&graph, (canvas_w, canvas_h), &mut gpu_source)
                .expect("transform output");
            let actual = read_texture_rgba16f(&gpu, &output, canvas_w, canvas_h);
            for (pixel_index, (gpu_pixel, cpu_pixel)) in
                actual.iter().zip(&expected.pixels).enumerate()
            {
                for channel in 0..4 {
                    assert!(
                        (gpu_pixel[channel] - cpu_pixel[channel]).abs() < 2e-3,
                        "{sampling:?} pixel {pixel_index} channel {channel}: GPU {} vs CPU {}",
                        gpu_pixel[channel],
                        cpu_pixel[channel]
                    );
                }
            }
        }
    }

    #[test]
    fn singular_transform_gpu_is_transparent() {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("no GPU adapter; skipping singular transform policy");
            return;
        };
        let image = patterned_image(7, 5);
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::DecodeStill {
                        asset: AssetId::new(),
                    },
                    inputs: vec![],
                    content_hash: ContentHash(600),
                },
                IrNode {
                    op: IrOp::Transform2D {
                        mat: glam::Mat3::from_scale(glam::Vec2::new(0.0, 1.0)),
                        sampling: Sampling::Nearest,
                    },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(601),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        let mut source = PatternSource {
            frame: GpuFrame::new(upload_pattern(&gpu, &image), image.width, image.height),
        };
        let mut evaluator = Evaluator::new(gpu.clone());
        let output = evaluator
            .evaluate(&graph, (image.width, image.height), &mut source)
            .expect("transform output");
        let actual = read_texture_rgba16f(&gpu, &output, image.width, image.height);
        assert!(
            actual.iter().all(|pixel| *pixel == [0.0; 4]),
            "singular GPU transform must be transparent"
        );
    }

    #[test]
    fn solid_graph_gpu_matches_cpu_reference() {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("no GPU adapter — skipping GPU solid parity");
            return;
        };
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 8, 8);
        let seq_id = seq.id;
        let mut t = Track::new(TrackKind::Video, "V1");
        t.clips.push(Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 0.5,
                    g: 0.25,
                    b: 0.75,
                    a: 1.0,
                },
            },
            crate::contract::Tick(0),
            crate::contract::Tick::from_seconds(2),
        ));
        seq.video_tracks.push(t);
        project.insert_sequence(seq);

        let compiled = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(0),
            Quality::FULL,
            None,
        );
        let cpu = crate::graph::eval_cpu::evaluate(
            &compiled.graph,
            (8, 8),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );

        let mut eval = Evaluator::new(gpu.clone());
        let out = eval
            .evaluate(&compiled.graph, (8, 8), &mut NullFrameSource)
            .expect("output texture");
        let gpu_px = read_texture_rgba16f(&gpu, &out, 8, 8);

        for (g, c) in gpu_px.iter().zip(cpu.pixels.iter()) {
            for k in 0..4 {
                assert!(
                    (g[k] - c[k]).abs() < 1e-3,
                    "channel {k}: gpu {} vs cpu {}",
                    g[k],
                    c[k]
                );
            }
        }
    }

    /// Build a single-clip project (solid `color`) and let `decorate` attach a
    /// grade / effect before compiling. Returns the compiled graph.
    fn solid_project_graph(
        color: Color,
        decorate: impl FnOnce(&mut Clip),
    ) -> crate::graph::compile::CompiledFrame {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 8, 8);
        let seq_id = seq.id;
        let mut t = Track::new(TrackKind::Video, "V1");
        let mut clip = Clip::new(
            ClipSource::SolidColor { color },
            crate::contract::Tick(0),
            crate::contract::Tick::from_seconds(2),
        );
        decorate(&mut clip);
        t.clips.push(clip);
        seq.video_tracks.push(t);
        project.insert_sequence(seq);
        compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(0),
            Quality::FULL,
            None,
        )
    }

    fn assert_graph_gpu_matches_cpu(compiled: &crate::graph::compile::CompiledFrame, tol: f32) {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("no GPU adapter — skipping GPU parity");
            return;
        };
        let cpu = crate::graph::eval_cpu::evaluate(
            &compiled.graph,
            (8, 8),
            &mut crate::graph::eval_cpu::EmptyProvider,
        );
        let mut eval = Evaluator::new(gpu.clone());
        let out = eval
            .evaluate(&compiled.graph, (8, 8), &mut NullFrameSource)
            .expect("output texture");
        let gpu_px = read_texture_rgba16f(&gpu, &out, 8, 8);
        for (g, c) in gpu_px.iter().zip(cpu.pixels.iter()) {
            for k in 0..4 {
                assert!(
                    (g[k] - c[k]).abs() < tol,
                    "channel {k}: gpu {} vs cpu {}",
                    g[k],
                    c[k]
                );
            }
        }
    }

    /// Every `BlendMode`, so a new variant added without a shader case fails here.
    const ALL_BLEND_MODES: [BlendMode; 26] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
        BlendMode::LinearDodge,
        BlendMode::LinearBurn,
        BlendMode::Subtract,
        BlendMode::Divide,
        BlendMode::VividLight,
        BlendMode::LinearLight,
        BlendMode::PinLight,
        BlendMode::HardMix,
        BlendMode::DarkerColor,
        BlendMode::LighterColor,
    ];

    /// A two-`SolidColor` → `Merge` graph: `top` over `bottom` under `mode` at
    /// full opacity. Colours are premultiplied linear (the working space), so the
    /// CPU and GPU evaluators start from identical pixels.
    fn merge_graph(
        top: crate::graph::ir::LinearColor,
        bottom: crate::graph::ir::LinearColor,
        mode: BlendMode,
    ) -> crate::graph::compile::CompiledFrame {
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor { color: top },
                    inputs: vec![],
                    content_hash: ContentHash(700),
                },
                IrNode {
                    op: IrOp::SolidColor { color: bottom },
                    inputs: vec![],
                    content_hash: ContentHash(701),
                },
                IrNode {
                    op: IrOp::Merge { mode, opacity: 1.0 },
                    inputs: vec![
                        (IrNodeId(0), OutPort::default()),
                        (IrNodeId(1), OutPort::default()),
                    ],
                    content_hash: ContentHash(702),
                },
            ],
            output: Some(IrNodeId(2)),
        };
        crate::graph::compile::CompiledFrame {
            graph,
            diagnostics: vec![],
            ..Default::default()
        }
    }

    /// K-0.3a / E-9: the GPU Merge pass must agree with the CPU `blend_rgb`
    /// reference for every one of the 26 blend modes, over both an opaque and a
    /// transparent backdrop (a semi-transparent top exercises the `Cs'` mix and
    /// the source-over composite). Closes the CPU/GPU divergence the Normal-only
    /// shader left. Self-skips without a GPU adapter; run `--test-threads=1`.
    #[test]
    fn merge_gpu_matches_cpu_for_every_blend_mode() {
        use crate::graph::ir::LinearColor;
        // Semi-transparent top (straight 0.7,0.35,0.55 @ α0.5, premultiplied) over
        // an opaque and a transparent backdrop. Colours are chosen to sit clear of
        // every mode's discontinuities — in particular `HardMix`'s 0.5 threshold,
        // where a sub-ULP CPU/GPU difference would flip the output 0↔1 (an inherent
        // discontinuity of the mode, not a divergence in the composite math).
        let top = LinearColor {
            r: 0.35,
            g: 0.175,
            b: 0.275,
            a: 0.5,
        };
        let opaque = LinearColor {
            r: 0.25,
            g: 0.6,
            b: 0.4,
            a: 1.0,
        };
        let transparent = LinearColor {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
        for mode in ALL_BLEND_MODES {
            for bottom in [opaque, transparent] {
                assert_graph_gpu_matches_cpu(&merge_graph(top, bottom, mode), 1e-3);
            }
        }
    }

    /// GPU/CPU parity for the `Grade` op (07 §3, 03 §4.4): an Exposure grade over a
    /// solid must agree within 1e-3 between `apply_grade_stack_gpu` and
    /// `apply_grade_cpu`.
    #[test]
    fn graded_solid_gpu_matches_cpu_reference() {
        use photonic_core::timeline::grade::{Grade, GradeOp, GradeOpKind, GradeOpParams};
        let compiled = solid_project_graph(
            Color {
                r: 0.5,
                g: 0.4,
                b: 0.6,
                a: 1.0,
            },
            |clip| {
                let mut grade = Grade::new();
                grade.ops.push(GradeOp::new(
                    GradeOpKind::Exposure,
                    GradeOpParams::Exposure { stops: 0.7 },
                ));
                clip.grade = Some(grade);
            },
        );
        assert!(
            compiled
                .graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, IrOp::Grade { .. })),
            "a real Grade node is present"
        );
        assert_graph_gpu_matches_cpu(&compiled, 1e-3);
    }

    /// GPU/CPU parity for `Effect{Invert}` (08 §3).
    #[test]
    fn inverted_solid_gpu_matches_cpu_reference() {
        use photonic_core::timeline::{ClipEffect, EffectKind};
        let compiled = solid_project_graph(
            Color {
                r: 0.7,
                g: 0.2,
                b: 0.9,
                a: 1.0,
            },
            |clip| clip.effects.push(ClipEffect::new(EffectKind::Invert)),
        );
        assert_graph_gpu_matches_cpu(&compiled, 1e-3);
    }

    /// Build a clip effect of `kind` with the given `(path, PropValue)` params set
    /// on its base, for the per-effect parity tests (K-0.2 Step B).
    fn effect_with(
        kind: photonic_core::timeline::EffectKind,
        params: &[(&str, photonic_core::timeline::PropValue)],
    ) -> photonic_core::timeline::ClipEffect {
        let mut eff = photonic_core::timeline::ClipEffect::new(kind);
        for (path, value) in params {
            eff.params.base.set(*path, *value);
        }
        eff
    }

    /// GPU/CPU parity for `Effect{LumaKey}` (08 §3): the shader's α-keep smoothstep
    /// must match `ops::luma_key`. Threshold/softness sit so the solid's luma lands
    /// mid-band (a fractional keep, exercising the interpolation, not a 0/1 step).
    #[test]
    fn luma_key_solid_gpu_matches_cpu_reference() {
        use photonic_core::timeline::{EffectKind, PropValue};
        let compiled = solid_project_graph(
            Color {
                r: 0.5,
                g: 0.5,
                b: 0.5,
                a: 1.0,
            },
            |clip| {
                clip.effects.push(effect_with(
                    EffectKind::LumaKey,
                    &[
                        ("params.threshold", PropValue::Float(0.15)),
                        ("params.softness", PropValue::Float(0.2)),
                        ("params.invert", PropValue::Bool(false)),
                    ],
                ))
            },
        );
        assert_graph_gpu_matches_cpu(&compiled, 1e-3);
    }

    /// GPU/CPU parity for `Effect{Deflicker}`: the WGSL must reproduce
    /// `deflicker::apply_gain`'s encode → scale → decode round trip, including
    /// the highlight rolloff. Two gains are exercised — a mid-grey brightening
    /// (well below the knee, so a plain gain) and a near-white one (inside the
    /// knee, so the rolloff branch fires) — because a shader that ignored the
    /// rolloff would still pass the first case.
    ///
    /// The gain is injected the way the real pipeline injects it: through
    /// `compile_full` with a populated `GainTable`, not by hand-setting params.
    #[test]
    fn deflicker_solid_gpu_matches_cpu_reference() {
        use crate::graph::deflicker::{ClipGains, GainTable};
        use photonic_core::timeline::EffectKind;

        // Third case carries a rolling band: a uniform source becomes row-varying,
        // so this is what catches a row-orientation flip between the CPU's
        // top-down `y` index and the shader's fragment coordinate — a bug that
        // would otherwise show only as an inverted band on real footage.
        let cases: [(
            f32,
            [f32; 3],
            Option<crate::graph::rolling_bands::BandModel>,
        ); 3] = [
            (0.5, [1.2, 1.05, 0.9], None),
            (0.95, [1.4, 1.4, 1.4], None),
            (
                0.45,
                [1.0, 1.0, 1.0],
                Some(crate::graph::rolling_bands::BandModel {
                    cycles: 3,
                    amplitude: 0.12,
                    phase: 0.7,
                    confidence: 1.0,
                }),
            ),
        ];
        for (level, gain, band) in cases {
            let mut project = TimelineProject::new();
            let mut seq = Sequence::new("seq", FrameRate::FPS_30, 8, 8);
            let seq_id = seq.id;
            let mut t = Track::new(TrackKind::Video, "V1");
            let mut clip = Clip::new(
                ClipSource::SolidColor {
                    color: Color {
                        r: level,
                        g: level,
                        b: level,
                        a: 1.0,
                    },
                },
                crate::contract::Tick(0),
                crate::contract::Tick::from_seconds(2),
            );
            clip.effects.push(photonic_core::timeline::ClipEffect::new(
                EffectKind::Deflicker,
            ));
            let clip_id = clip.id;
            t.clips.push(clip);
            seq.video_tracks.push(t);
            project.insert_sequence(seq);

            let mut table = GainTable::new();
            table.insert(
                clip_id,
                ClipGains {
                    first: crate::contract::Tick(0),
                    step: 1,
                    gains: vec![gain],
                    // `dphase: 0` pins the phase so the parity comparison is
                    // against a fixed band, not a moving one.
                    band: band.map(|m| crate::graph::rolling_bands::BandTrack {
                        cycles: m.cycles,
                        amplitude: m.amplitude,
                        phase0: m.phase,
                        dphase: 0.0,
                        anchor_frame: 0,
                        mains_hz: 120.0,
                        confidence: m.confidence,
                    }),
                    frame_ticks: 1,
                },
            );
            let compiled = crate::graph::compile::compile_full(
                &project,
                seq_id,
                0,
                crate::contract::Tick(0),
                Quality::FULL,
                None,
                None,
                false,
                Some(&table),
                None,
            );
            // Guard the test itself: if the gain stopped being injected this
            // would silently become a parity check on a pass-through.
            assert!(
                compiled.graph.nodes.iter().any(|n| matches!(
                    &n.op,
                    IrOp::Effect { kind: EffectKind::Deflicker, params }
                        if params.get("params.gain_r").is_some()
                )),
                "gain must be injected for this parity test to mean anything"
            );
            if band.is_some() {
                assert!(
                    compiled.graph.nodes.iter().any(|n| matches!(
                        &n.op,
                        IrOp::Effect { kind: EffectKind::Deflicker, params }
                            if params.get("params.band_amp").is_some()
                    )),
                    "band must be injected for the band parity case to mean anything"
                );
            }
            assert_graph_gpu_matches_cpu(&compiled, 1e-3);
        }
    }

    /// GPU/CPU parity for `Effect{ChromaKey}` (08 §3): keep-smoothstep + dominant-
    /// channel spill suppression. The solid is greenish and the key is green, so
    /// the colour distance lands in the feather band and the spill branch fires.
    #[test]
    fn chroma_key_solid_gpu_matches_cpu_reference() {
        use photonic_core::timeline::{EffectKind, PropValue};
        let compiled = solid_project_graph(
            Color {
                r: 0.2,
                g: 0.7,
                b: 0.2,
                a: 1.0,
            },
            |clip| {
                clip.effects.push(effect_with(
                    EffectKind::ChromaKey,
                    &[
                        (
                            "params.key_color",
                            PropValue::Color(Color {
                                r: 0.0,
                                g: 1.0,
                                b: 0.0,
                                a: 1.0,
                            }),
                        ),
                        ("params.tolerance", PropValue::Float(0.3)),
                        ("params.edge_softness", PropValue::Float(0.4)),
                        ("params.spill_suppress", PropValue::Float(0.5)),
                    ],
                ))
            },
        );
        assert_graph_gpu_matches_cpu(&compiled, 1e-3);
    }

    /// GPU/CPU parity for `Effect{MaskShapeGen}` (08 §3): a 0-input ellipse-matte
    /// generator. A centered feathered ellipse over the 8×8 canvas exercises the
    /// interior (α=1), exterior (α=0), and the feathered edge band.
    #[test]
    fn mask_shape_solid_gpu_matches_cpu_reference() {
        use photonic_core::timeline::{EffectKind, PropValue};
        let compiled = solid_project_graph(
            Color {
                r: 0.4,
                g: 0.3,
                b: 0.6,
                a: 1.0,
            },
            |clip| {
                clip.effects.push(effect_with(
                    EffectKind::MaskShapeGen,
                    &[
                        ("params.center_x", PropValue::Float(0.5)),
                        ("params.center_y", PropValue::Float(0.5)),
                        ("params.size_x", PropValue::Float(0.5)),
                        ("params.size_y", PropValue::Float(0.5)),
                        ("params.rotation", PropValue::Float(0.0)),
                        ("params.feather", PropValue::Float(0.2)),
                    ],
                ))
            },
        );
        assert_graph_gpu_matches_cpu(&compiled, 1e-3);
    }

    /// GPU/CPU parity for `Effect{Blur}` (K-0.2): a uniform solid is a fixed
    /// point of the Gaussian, locking the dual-pass path. Tolerance is looser
    /// than 1e-3 because the multi-tap f16 accumulation drifts ~1.5e-3 on α≈1.
    #[test]
    fn blur_solid_gpu_matches_cpu_reference() {
        use photonic_core::timeline::{EffectKind, PropValue};
        let compiled = solid_project_graph(
            Color {
                r: 0.6,
                g: 0.3,
                b: 0.1,
                a: 1.0,
            },
            |clip| {
                clip.effects.push(effect_with(
                    EffectKind::Blur,
                    &[("params.radius", PropValue::Float(1.5))],
                ))
            },
        );
        assert_graph_gpu_matches_cpu(&compiled, 1e-3);
    }

    /// GPU/CPU parity for `Effect{Sharpen}` (K-0.2) on a uniform solid.
    #[test]
    fn sharpen_solid_gpu_matches_cpu_reference() {
        use photonic_core::timeline::{EffectKind, PropValue};
        let compiled = solid_project_graph(
            Color {
                r: 0.4,
                g: 0.5,
                b: 0.6,
                a: 1.0,
            },
            |clip| {
                clip.effects.push(effect_with(
                    EffectKind::Sharpen,
                    &[
                        ("params.amount", PropValue::Float(1.0)),
                        ("params.radius", PropValue::Float(1.0)),
                    ],
                ))
            },
        );
        assert_graph_gpu_matches_cpu(&compiled, 1e-3);
    }

    /// GPU/CPU parity for `Effect{Glow}` (K-0.2): threshold 0 keeps every pixel
    /// in the extract, so a bright solid blooms under intensity.
    #[test]
    fn glow_solid_gpu_matches_cpu_reference() {
        use photonic_core::timeline::{EffectKind, PropValue};
        let compiled = solid_project_graph(
            Color {
                r: 0.9,
                g: 0.85,
                b: 0.7,
                a: 1.0,
            },
            |clip| {
                clip.effects.push(effect_with(
                    EffectKind::Glow,
                    &[
                        ("params.radius", PropValue::Float(1.0)),
                        ("params.threshold", PropValue::Float(0.0)),
                        ("params.intensity", PropValue::Float(0.5)),
                        (
                            "params.tint",
                            PropValue::Color(Color {
                                r: 1.0,
                                g: 1.0,
                                b: 1.0,
                                a: 1.0,
                            }),
                        ),
                    ],
                ))
            },
        );
        assert_graph_gpu_matches_cpu(&compiled, 1e-3);
    }

    // ── Caption overlay compositing (06 §5.3) ─────────────────────────────────

    /// A 128×64 blue-background sequence with one caption track (cue `[0,200)`,
    /// two words "AB"/"CD"), optionally karaoke-highlighted.
    fn captioned_project(highlight: Option<KaraokeStyle>) -> (TimelineProject, SequenceId) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 128, 64);
        let seq_id = seq.id;
        let mut vt = Track::new(TrackKind::Video, "V1");
        vt.clips.push(Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.6,
                    a: 1.0,
                },
            },
            crate::contract::Tick(0),
            crate::contract::Tick(1000),
        ));
        seq.video_tracks.push(vt);

        let mut ct = CaptionTrack::new("Caps");
        ct.style = CaptionStyle {
            font_size: 22.0,
            position: [0.5, 0.25],
            highlight,
            ..CaptionStyle::default()
        };
        ct.cues.push(CaptionCue::new(
            crate::contract::Tick(0),
            crate::contract::Tick(200),
            vec![
                CaptionWord::new("AB", crate::contract::Tick(0), crate::contract::Tick(100)),
                CaptionWord::new("CD", crate::contract::Tick(100), crate::contract::Tick(200)),
            ],
        ));
        seq.caption_tracks.push(ct);
        project.insert_sequence(seq);
        (project, seq_id)
    }

    /// Count pixels differing in any channel by more than `tol`.
    fn count_diff(a: &[[f32; 4]], b: &[[f32; 4]], tol: f32) -> usize {
        a.iter()
            .zip(b)
            .filter(|(p, q)| (0..4).any(|k| (p[k] - q[k]).abs() > tol))
            .count()
    }

    /// AS-1 "burned caption": a covering cue burns glyphs over the frame (non-zero
    /// delta vs the caption-free background), and WordPop karaoke recolors the
    /// active word so a mid-word tick differs from a tick where the other word is
    /// active. Proves `CaptionOverlay` is no longer a passthrough stub.
    #[test]
    fn caption_overlay_burns_glyphs_and_karaoke_recolors() {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("no GPU adapter — skipping caption overlay render");
            return;
        };
        let (w, h) = (128u32, 64u32);
        let highlight = KaraokeStyle {
            mode: KaraokeMode::WordPop,
            active_color: Color {
                r: 1.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            }, // yellow
            inactive_color: Color {
                r: 0.5,
                g: 0.5,
                b: 0.5,
                a: 1.0,
            }, // grey
        };
        let (project, seq_id) = captioned_project(Some(highlight));
        let mut eval = Evaluator::new(gpu.clone());
        let render = |eval: &mut Evaluator, t: i64| {
            let c = compile(
                &project,
                seq_id,
                0,
                crate::contract::Tick(t),
                Quality::FULL,
                None,
            );
            let out = eval
                .evaluate(&c.graph, (w, h), &mut NullFrameSource)
                .expect("output texture");
            read_texture_rgba16f(&gpu, &out, w, h)
        };

        // t=500 is inside the background clip but past the cue → background only.
        let bg = render(&mut eval, 500);
        // t=50: "AB" active (yellow), "CD" inactive (grey).
        let f_before = render(&mut eval, 50);
        // t=150: swap — "AB" inactive, "CD" active.
        let f_mid = render(&mut eval, 150);

        let burned = count_diff(&f_before, &bg, 0.02);
        assert!(
            burned > 0,
            "caption glyphs must change pixels vs the caption-free background (got {burned})"
        );
        let karaoke = count_diff(&f_before, &f_mid, 0.02);
        assert!(
            karaoke > 0,
            "WordPop karaoke must change pixels between t=50 and t=150 (got {karaoke})"
        );
    }

    // ── Title / text clip rendering (G-12) ────────────────────────────────────

    /// A `ClipSource::Text` clip renders styled glyphs, reusing the caption
    /// glyphon path: the compiled graph lowers it to a `TextGen` carrying a
    /// populated cue, and the rendered frame has lit pixels carrying the fill
    /// colour. Proves the title is no longer a transparent placeholder.
    #[test]
    fn text_clip_renders_styled_glyphs() {
        let Some(gpu) = GpuContext::request_blocking() else {
            eprintln!("no GPU adapter — skipping text clip render");
            return;
        };
        let (w, h) = (128u32, 64u32);
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, w, h);
        let seq_id = seq.id;
        let mut vt = Track::new(TrackKind::Video, "V1");
        // Red fill, no stroke, so lit glyph pixels isolate the fill colour.
        let style = CaptionStyle {
            font_size: 40.0,
            fill: Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            stroke: None,
            position: [0.5, 0.4],
            ..CaptionStyle::default()
        };
        vt.clips.push(Clip::new(
            ClipSource::Text {
                content: TextClipContent {
                    text: "HELLO".to_string(),
                    style,
                },
            },
            crate::contract::Tick(0),
            crate::contract::Tick(1000),
        ));
        seq.video_tracks.push(vt);
        project.insert_sequence(seq);

        let compiled = compile(
            &project,
            seq_id,
            0,
            crate::contract::Tick(100),
            Quality::FULL,
            None,
        );
        assert!(
            compiled
                .graph
                .nodes
                .iter()
                .any(|n| matches!(&n.op, IrOp::TextGen { block } if block.cue.is_some())),
            "Text clip lowers to a TextGen carrying a resolved cue"
        );

        let mut eval = Evaluator::new(gpu.clone());
        let out = eval
            .evaluate(&compiled.graph, (w, h), &mut NullFrameSource)
            .expect("output texture");
        let px = read_texture_rgba16f(&gpu, &out, w, h);

        // Non-zero: glyph coverage lit some pixels over the transparent frame.
        let lit: Vec<[f32; 4]> = px.into_iter().filter(|p| p[3] > 0.05).collect();
        assert!(!lit.is_empty(), "text clip must produce lit glyph pixels");
        // Correctly coloured: premultiplied red (r≈coverage, g≈b≈0) — the fill,
        // not a stray white/black. Every lit pixel is red-dominant.
        let red_dominant = lit
            .iter()
            .filter(|p| p[0] > 0.05 && p[1] < 0.05 && p[2] < 0.05)
            .count();
        assert!(
            red_dominant > 0,
            "lit glyph pixels must carry the red fill ({} lit, {} red-dominant)",
            lit.len(),
            red_dominant
        );
    }

    // ── K-0.5 resolved LUT grade / K-0.4 directional transitions parity ───────

    /// A `SolidColor → Grade{Lut3d} → Output` graph carrying a resolved 2³ LUT
    /// that maps `c → 0.5·c` (a linear map, so trilinear reconstruction is exact
    /// on both the GPU sampler and the CPU reference).
    fn lut_grade_graph(
        color: crate::graph::ir::LinearColor,
    ) -> crate::graph::compile::CompiledFrame {
        use photonic_render::grade::{ResolvedGradeOp, ResolvedGradePayload, ResolvedLut3d};
        let mut table = photonic_render::Lut3d::identity(2);
        for sample in &mut table.data {
            for v in sample {
                *v *= 0.5;
            }
        }
        let op = ResolvedGradeOp {
            payload: ResolvedGradePayload::Lut3d(ResolvedLut3d {
                table: Arc::new(table),
                intensity: 1.0,
                tetrahedral: false,
            }),
            mask: None,
        };
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor { color },
                    inputs: vec![],
                    content_hash: ContentHash(820),
                },
                IrNode {
                    op: IrOp::Grade { ops: vec![op] },
                    inputs: vec![(IrNodeId(0), OutPort::default())],
                    content_hash: ContentHash(821),
                },
            ],
            output: Some(IrNodeId(1)),
        };
        crate::graph::compile::CompiledFrame {
            graph,
            diagnostics: vec![],
            ..Default::default()
        }
    }

    /// K-0.5: a resolved `Lut3d` grade evaluates identically on the GPU (3D-LUT
    /// sampler) and the CPU reference (03 §4.4).
    #[test]
    fn resolved_lut_grade_gpu_matches_cpu() {
        let compiled = lut_grade_graph(crate::graph::ir::LinearColor {
            r: 0.6,
            g: 0.4,
            b: 0.8,
            a: 1.0,
        });
        assert!(
            compiled
                .graph
                .nodes
                .iter()
                .any(|n| matches!(&n.op, IrOp::Grade { ops } if !ops.is_empty())),
            "a resolved LUT Grade node is present"
        );
        assert_graph_gpu_matches_cpu(&compiled, 1e-3);
    }

    /// A two-`SolidColor` → binary-transition (`WipeMix`/`PushMix`) → `Output`
    /// graph; inputs are `[incoming, outgoing]`.
    fn transition_graph(
        op: IrOp,
        incoming: crate::graph::ir::LinearColor,
        outgoing: crate::graph::ir::LinearColor,
    ) -> crate::graph::compile::CompiledFrame {
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::SolidColor { color: incoming },
                    inputs: vec![],
                    content_hash: ContentHash(830),
                },
                IrNode {
                    op: IrOp::SolidColor { color: outgoing },
                    inputs: vec![],
                    content_hash: ContentHash(831),
                },
                IrNode {
                    op,
                    inputs: vec![
                        (IrNodeId(0), OutPort::default()),
                        (IrNodeId(1), OutPort::default()),
                    ],
                    content_hash: ContentHash(832),
                },
            ],
            output: Some(IrNodeId(2)),
        };
        crate::graph::compile::CompiledFrame {
            graph,
            diagnostics: vec![],
            ..Default::default()
        }
    }

    const PARITY_DIRS: [WipeDirection; 4] = [
        WipeDirection::LeftToRight,
        WipeDirection::RightToLeft,
        WipeDirection::TopToBottom,
        WipeDirection::BottomToTop,
    ];

    /// K-0.4: the `WipeMix` GPU pass must match `ops::wipe` for every direction at
    /// t = 0.25/0.5/0.75 (a softened edge exercises the smoothstep band).
    #[test]
    fn wipe_gpu_matches_cpu_for_every_direction_and_t() {
        let inc = crate::graph::ir::LinearColor {
            r: 0.8,
            g: 0.2,
            b: 0.3,
            a: 1.0,
        };
        let outg = crate::graph::ir::LinearColor {
            r: 0.1,
            g: 0.5,
            b: 0.9,
            a: 1.0,
        };
        for dir in PARITY_DIRS {
            for t in [0.25f32, 0.5, 0.75] {
                let op = IrOp::WipeMix {
                    direction: dir,
                    softness: 0.1,
                    t,
                };
                assert_graph_gpu_matches_cpu(&transition_graph(op, inc, outg), 1e-3);
            }
        }
    }

    /// K-0.4: the `PushMix` GPU pass must match `ops::push` for every direction at
    /// t = 0.25/0.5/0.75.
    #[test]
    fn push_gpu_matches_cpu_for_every_direction_and_t() {
        let inc = crate::graph::ir::LinearColor {
            r: 0.8,
            g: 0.2,
            b: 0.3,
            a: 1.0,
        };
        let outg = crate::graph::ir::LinearColor {
            r: 0.1,
            g: 0.5,
            b: 0.9,
            a: 1.0,
        };
        for dir in PARITY_DIRS {
            for t in [0.25f32, 0.5, 0.75] {
                let op = IrOp::PushMix { direction: dir, t };
                assert_graph_gpu_matches_cpu(&transition_graph(op, inc, outg), 1e-3);
            }
        }
    }

    #[test]
    fn t005_incomplete_graph_drains_sources_without_poisoning_cache_or_retained_frames() {
        let Some(gpu) = GpuContext::request_blocking() else {
            assert!(
                std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
                "GPU required"
            );
            return;
        };
        struct Delayed {
            frame: GpuFrame,
            ready: bool,
            calls: usize,
            namespace: u64,
        }
        impl GpuFrameSource for Delayed {
            fn cache_namespace(&self) -> u64 {
                self.namespace
            }
            fn video_texture(
                &mut self,
                _: &GpuContext,
                _: AssetId,
                _: Tick,
                _: bool,
            ) -> Option<GpuFrame> {
                None
            }
            fn still_texture(
                &mut self,
                _: &GpuContext,
                _: AssetId,
                _: u32,
                _: u32,
            ) -> Option<GpuFrame> {
                self.calls += 1;
                self.ready.then(|| self.frame.clone())
            }
            fn vector_texture(
                &mut self,
                _: &GpuContext,
                _: VectorRef,
                _: VectorStateKey,
                _: u32,
                _: u32,
            ) -> Option<GpuFrame> {
                None
            }
        }
        let mut red = Image::new(8, 8);
        red.pixels.fill([1.0, 0.0, 0.0, 1.0]);
        let mut source = Delayed {
            frame: GpuFrame::new(upload_pattern(&gpu, &red), 8, 8),
            ready: false,
            calls: 0,
            namespace: 0,
        };
        let graph = FrameGraph {
            nodes: vec![
                IrNode {
                    op: IrOp::DecodeStill {
                        asset: AssetId::new(),
                    },
                    inputs: vec![],
                    content_hash: ContentHash(900),
                },
                IrNode {
                    op: IrOp::DecodeStill {
                        asset: AssetId::new(),
                    },
                    inputs: vec![],
                    content_hash: ContentHash(901),
                },
                IrNode {
                    op: IrOp::Merge {
                        mode: BlendMode::Normal,
                        opacity: 1.0,
                    },
                    inputs: vec![
                        (IrNodeId(0), OutPort::default()),
                        (IrNodeId(1), OutPort::default()),
                    ],
                    content_hash: ContentHash(902),
                },
                IrNode {
                    op: IrOp::Output { w: 8, h: 8 },
                    inputs: vec![(IrNodeId(2), OutPort::default())],
                    content_hash: ContentHash(903),
                },
            ],
            output: Some(IrNodeId(3)),
        };
        let mut evaluator = Evaluator::new(gpu.clone());
        assert!(evaluator.evaluate(&graph, (8, 8), &mut source).is_none());
        assert_eq!(source.calls, 2);
        assert_eq!(evaluator.cache_stats().resident_entries, 0);
        source.ready = true;
        let retained = evaluator.evaluate(&graph, (8, 8), &mut source).unwrap();
        let pixels = read_texture_rgba16f(&gpu, &retained, 8, 8);
        assert!(pixels[0][0] > 0.99);
        source.ready = false;
        assert!(evaluator.evaluate(&graph, (8, 8), &mut source).is_none());
        source.ready = true;
        let mut blue = Image::new(8, 8);
        blue.pixels.fill([0.0, 0.0, 1.0, 1.0]);
        source.frame = GpuFrame::new(upload_pattern(&gpu, &blue), 8, 8);
        source.namespace = 1;
        let approximate = evaluator.evaluate(&graph, (8, 8), &mut source).unwrap();
        assert!(read_texture_rgba16f(&gpu, &approximate, 8, 8)[0][2] > 0.99);
        source.namespace = 0;
        source.frame = GpuFrame::new(upload_pattern(&gpu, &red), 8, 8);
        let exact = evaluator.evaluate(&graph, (8, 8), &mut source).unwrap();
        assert_eq!(read_texture_rgba16f(&gpu, &exact, 8, 8), pixels);
        evaluator.invalidate_matching(|_| true);
        source.frame = GpuFrame::new(upload_pattern(&gpu, &blue), 8, 8);
        let replacement = evaluator.evaluate(&graph, (8, 8), &mut source).unwrap();
        assert!(read_texture_rgba16f(&gpu, &replacement, 8, 8)[0][2] > 0.99);
        assert_eq!(
            read_texture_rgba16f(&gpu, &retained, 8, 8),
            pixels,
            "retained Arc was overwritten during recycling"
        );
    }
}
