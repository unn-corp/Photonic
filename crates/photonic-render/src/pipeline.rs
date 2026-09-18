use bytemuck::{Pod, Zeroable};
use photonic_core::layer::BlendMode;

/// The colour encoding of the document fill/blend render target — the single
/// source of truth for "what space does fixed-function blending run in".
///
/// It is an **sRGB** format on purpose: with an sRGB attachment the GPU blend
/// unit decodes each channel to linear, blends, then re-encodes, so separable
/// modes (Multiply/Screen) and partial-alpha src-over composite in linear
/// space. Both the headless export path (`headless::FORMAT`) and the windowed
/// document pass (`PhotonicRenderer::scene_format`) target this encoding, which
/// is what makes on-canvas rendering and exported output pixel-identical
/// (issue #145).
pub const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The four separable blend modes implementable with fixed-function GPU blending.
///
/// Backdrop-read modes (Overlay, SoftLight, ColorDodge, ColorBurn) and the
/// non-separable HSL modes (Hue, Saturation, Color, Luminosity) cannot be
/// expressed as a `wgpu::BlendState` and require an offscreen-composite shader
/// pass — they are a documented follow-up (issue #17) and currently fall back to
/// normal alpha blending.
pub const SEPARABLE_BLEND_MODES: [BlendMode; 4] = [
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Darken,
    BlendMode::Lighten,
];

/// Returns the fixed-function `BlendState` implementing `mode`, or `None` when
/// the mode is not expressible as fixed-function blending (Normal, or a
/// backdrop-read / HSL mode that needs a shader pass).
///
/// The colour-channel mappings are exact for opaque fills (alpha = 1) over an
/// opaque backdrop, which is the common case. Partial-alpha blended fills are
/// approximated; full Porter-Duff alpha compositing is a documented follow-up.
pub fn separable_blend_state(mode: BlendMode) -> Option<wgpu::BlendState> {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation, BlendState};

    // Preserve the backdrop's alpha so a blended shape never punches a hole in
    // the coverage of whatever is underneath it.
    let keep_dst_alpha = BlendComponent {
        src_factor: BlendFactor::Zero,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    };
    let color = match mode {
        // Cs * Cb
        BlendMode::Multiply => BlendComponent {
            src_factor: BlendFactor::Dst,
            dst_factor: BlendFactor::Zero,
            operation: BlendOperation::Add,
        },
        // Cs + Cb - Cs*Cb  ==  Cs*(1 - Cb) + Cb
        BlendMode::Screen => BlendComponent {
            src_factor: BlendFactor::OneMinusDst,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        },
        // min(Cs, Cb) — factors are ignored for Min/Max operations.
        BlendMode::Darken => BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Min,
        },
        // max(Cs, Cb)
        BlendMode::Lighten => BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Max,
        },
        _ => return None,
    };
    Some(BlendState {
        color,
        alpha: keep_dst_alpha,
    })
}

/// A contiguous run of indices that share one blend mode, drawn in a single
/// `draw_indexed` call with the matching pipeline.
#[derive(Clone)]
pub(crate) struct DrawSegment {
    pub mode: BlendMode,
    /// Offset into the index buffer.
    pub start: u32,
    /// Number of indices in the run.
    pub count: u32,
}

/// Coalesce per-node `(mode, start, end)` index ranges (in draw order) into
/// contiguous draw runs, merging adjacent runs that share a mode. The common
/// all-`Normal` scene collapses to a single segment.
pub(crate) fn coalesce_segments(raw: Vec<(BlendMode, u32, u32)>) -> Vec<DrawSegment> {
    let mut segments: Vec<DrawSegment> = Vec::new();
    for (mode, start, end) in raw {
        if end == start {
            continue;
        }
        if let Some(last) = segments.last_mut() {
            if last.mode == mode && last.start + last.count == start {
                last.count += end - start;
                continue;
            }
        }
        segments.push(DrawSegment {
            mode,
            start,
            count: end - start,
        });
    }
    segments
}

/// Issue one `draw_indexed` per blend-mode run, selecting the matching blend
/// pipeline and falling back to `fill_pipeline` for modes without one. Runs are
/// in draw order, so fixed-function blends composite against the correct
/// backdrop. With no segments (e.g. first frame), draws all indices once with
/// `fill_pipeline`, preserving the original single-draw behaviour.
pub(crate) fn draw_segments<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    segments: &[DrawSegment],
    blend_pipelines: &'a [(BlendMode, wgpu::RenderPipeline)],
    fill_pipeline: &'a wgpu::RenderPipeline,
    index_count: u32,
) {
    if segments.is_empty() {
        pass.set_pipeline(fill_pipeline);
        pass.draw_indexed(0..index_count, 0, 0..1);
        return;
    }
    for seg in segments {
        let pipeline = blend_pipelines
            .iter()
            .find(|(m, _)| *m == seg.mode)
            .map(|(_, p)| p)
            .unwrap_or(fill_pipeline);
        pass.set_pipeline(pipeline);
        pass.draw_indexed(seg.start..seg.start + seg.count, 0, 0..1);
    }
}

/// A single vertex: 2D position + RGBA colour.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 2],
    pub color: [f32; 4],
}

impl Vertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x2,
            },
            wgpu::VertexAttribute {
                offset: 8,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x4,
            },
        ],
    };
}

/// Camera uniform buffer: column-major 4×4 matrix mapping document coords → NDC.
///
/// For a viewport with `pan_x, pan_y` offset and `zoom` scale on a `width × height` screen:
///   ndc_x = 2 * (doc_x * zoom + pan_x) / width  - 1
///   ndc_y = 1 - 2 * (doc_y * zoom + pan_y) / height
///
/// The matrix (column-major):
///   col 0: [2z/w,    0,    0, 0]
///   col 1: [0,    -2z/h,   0, 0]
///   col 2: [0,       0,    1, 0]
///   col 3: [2px/w-1, 1-2py/h, 0, 1]
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct CameraUniform {
    pub view_proj: [[f32; 4]; 4],
}

impl CameraUniform {
    pub fn from_viewport(pan_x: f64, pan_y: f64, zoom: f64, width: u32, height: u32) -> Self {
        let z = zoom as f32;
        let w = width as f32;
        let h = height as f32;
        let px = pan_x as f32;
        let py = pan_y as f32;
        Self {
            view_proj: [
                [2.0 * z / w, 0.0, 0.0, 0.0],                       // col 0
                [0.0, -2.0 * z / h, 0.0, 0.0],                      // col 1
                [0.0, 0.0, 1.0, 0.0],                               // col 2
                [2.0 * px / w - 1.0, 1.0 - 2.0 * py / h, 0.0, 1.0], // col 3
            ],
        }
    }
}

// ─── WGSL shader ─────────────────────────────────────────────────────────────

const FILL_SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexIn {
    @location(0) position: vec2<f32>,
    @location(1) color:    vec4<f32>,
}

struct VertexOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0)       color:    vec4<f32>,
}

@vertex
fn vs_main(v: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip_pos = camera.view_proj * vec4<f32>(v.position, 0.0, 1.0);
    out.color    = v.color;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

// ─── Pipeline factory ─────────────────────────────────────────────────────────

pub fn create_camera_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("camera_bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

pub fn create_fill_pipeline(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
    sample_count: u32,
) -> wgpu::RenderPipeline {
    create_fill_pipeline_with_blend(
        device,
        surface_format,
        camera_bgl,
        sample_count,
        wgpu::BlendState::ALPHA_BLENDING,
    )
}

/// Like [`create_fill_pipeline`] but with a caller-supplied `blend` state, used
/// to build one pipeline variant per separable blend mode.
pub fn create_fill_pipeline_with_blend(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
    sample_count: u32,
    blend: wgpu::BlendState,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fill_shader"),
        source: wgpu::ShaderSource::Wgsl(FILL_SHADER.into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("fill_layout"),
        bind_group_layouts: &[camera_bgl],
        push_constant_ranges: &[],
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("fill_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[Vertex::LAYOUT],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None, // 2D — no culling
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview: None,
        cache: None,
    })
}

// ─── Gaussian blur pipeline ────────────────────────────────────────────────────

pub const BLUR_SHADER: &str = r#"
struct BlurParams {
    sigma:      f32,
    horizontal: u32,
    _pad:       vec2<f32>,
}

@group(0) @binding(0) var t_in:  texture_2d<f32>;
@group(0) @binding(1) var s_in:  sampler;
@group(0) @binding(2) var<uniform> params: BlurParams;

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

@fragment
fn fs_blur(in: VOut) -> @location(0) vec4<f32> {
    let dim   = vec2<f32>(textureDimensions(t_in));
    let sigma = params.sigma;
    if sigma < 0.5 {
        return textureSample(t_in, s_in, in.uv);
    }
    let radius = min(i32(ceil(sigma * 3.0)), 128);
    var acc      = vec4<f32>(0.0);
    var w_total  = 0.0;
    let step_px  = select(vec2<f32>(0.0, 1.0 / dim.y),
                          vec2<f32>(1.0 / dim.x, 0.0),
                          params.horizontal != 0u);
    for (var i = -radius; i <= radius; i++) {
        let fi = f32(i);
        let w  = exp(-fi * fi / (2.0 * sigma * sigma));
        acc     += textureSample(t_in, s_in, in.uv + step_px * fi) * w;
        w_total += w;
    }
    return acc / w_total;
}
"#;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BlurParams {
    pub sigma: f32,
    pub horizontal: u32,
    pub _pad: [f32; 2],
}

pub fn create_blur_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("blur_bgl"),
        entries: &[
            // texture
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
            // sampler
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            // uniform BlurParams
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
        ],
    })
}

/// How a blur/composite pass blends its output into the target.
#[derive(Copy, Clone, Debug)]
pub enum BlurBlend {
    /// Premultiplied-alpha "over" — the glow halo default.
    Premultiplied,
    /// Additive (`src*srcAlpha + dst`) — brighten without erasing.
    Additive,
    /// Straight-alpha "over" — correct for compositing straight-color layers
    /// (the live-effects layer, whose textures hold non-premultiplied color).
    StraightAlpha,
}

pub fn create_blur_pipeline(
    device: &wgpu::Device,
    output_format: wgpu::TextureFormat,
    blur_bgl: &wgpu::BindGroupLayout,
    additive: bool,
) -> wgpu::RenderPipeline {
    create_blur_pipeline_with_blend(
        device,
        output_format,
        blur_bgl,
        if additive {
            BlurBlend::Additive
        } else {
            BlurBlend::Premultiplied
        },
    )
}

/// Like [`create_blur_pipeline`] but with an explicit blend mode.
pub fn create_blur_pipeline_with_blend(
    device: &wgpu::Device,
    output_format: wgpu::TextureFormat,
    blur_bgl: &wgpu::BindGroupLayout,
    blend_mode: BlurBlend,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blur_shader"),
        source: wgpu::ShaderSource::Wgsl(BLUR_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("blur_layout"),
        bind_group_layouts: &[blur_bgl],
        push_constant_ranges: &[],
    });
    let blend = Some(match blend_mode {
        BlurBlend::Additive => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        },
        BlurBlend::Premultiplied => wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        BlurBlend::StraightAlpha => wgpu::BlendState::ALPHA_BLENDING,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blur_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_quad",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_blur",
            targets: &[Some(wgpu::ColorTargetState {
                format: output_format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(), // sample_count = 1
        multiview: None,
        cache: None,
    })
}

// ─── Layer composite (offscreen isolation, all blend modes) ─────────────────────

/// True when `mode` cannot be expressed as fixed-function GPU blending and so
/// needs the offscreen COMPOSITE_SHADER isolation pass (03 §2.4): every mode
/// except `Normal` and the four [`SEPARABLE_BLEND_MODES`]. This is the 22 modes
/// (backdrop-read separable + HSL non-separable) that previously fell back to a
/// normal-alpha on-canvas approximation (issue #17).
pub(crate) fn mode_needs_isolation(mode: BlendMode) -> bool {
    mode != BlendMode::Normal && !SEPARABLE_BLEND_MODES.contains(&mode)
}

/// True when any draw segment uses a blend mode that needs the isolation pass.
/// The isolation compositing path is entered only in this case, so documents
/// that are entirely `Normal`/separable keep the untouched fixed-function draw
/// (and thus remain byte-identical — the golden-corpus contract, 03 §2.6).
pub(crate) fn segments_need_isolation(segments: &[DrawSegment]) -> bool {
    segments.iter().any(|s| mode_needs_isolation(s.mode))
}

/// Stable numeric id for a blend mode, passed to the composite shader. Matches
/// the `BlendMode` declaration order; indices `>= 12` are the non-separable HSL
/// modes. Explicit (not `as u32`) so the mapping can't drift if the enum is
/// reordered.
pub(crate) fn blend_mode_index(mode: BlendMode) -> u32 {
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
        // Photoshop extras: separable 16..=23, non-separable (whole-colour) 24..=25.
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

/// Uniform for the composite pass: which blend mode, and the layer's opacity.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CompositeParams {
    pub mode: u32,
    pub opacity: f32,
    pub _pad: [f32; 2],
}

/// Full-screen composite of an isolated **source** layer texture over a
/// **backdrop** texture, honouring every blend mode with a backdrop read — the
/// GPU port of the CPU `blend_rgb` + `composite_pixel` straight-alpha math. Both
/// inputs are sRGB textures, so sampling decodes to linear and the target
/// re-encodes: blending runs in linear space, matching fixed-function GPU blends
/// (#145). The shader writes the finished pixel, so its pipeline uses no
/// fixed-function blend (Replace).
pub const COMPOSITE_SHADER: &str = r#"
@group(0) @binding(0) var t_backdrop: texture_2d<f32>;
@group(0) @binding(1) var t_src:      texture_2d<f32>;
@group(0) @binding(2) var samp:       sampler;
struct Params { mode: u32, opacity: f32, _pad: vec2<f32> }
@group(0) @binding(3) var<uniform> params: Params;

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
// Non-recursive Vivid Light (WGSL forbids recursion, so it can't call blend_channel).
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
        case 16u: { return min(cb + cs, 1.0); }                 // Linear Dodge
        case 17u: { return max(cb + cs - 1.0, 0.0); }           // Linear Burn
        case 18u: { return max(cb - cs, 0.0); }                 // Subtract
        case 19u: { if (cs <= 0.0) { return 1.0; } return min(cb / cs, 1.0); } // Divide
        case 20u: { return vivid_light1(cb, cs); }              // Vivid Light
        case 21u: { return clamp(cb + 2.0 * cs - 1.0, 0.0, 1.0); } // Linear Light
        case 22u: {                                             // Pin Light
            if (cs <= 0.5) { return min(cb, 2.0 * cs); }
            return max(cb, 2.0 * cs - 1.0);
        }
        case 23u: {                                             // Hard Mix
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
fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let cmin = min(min(c.r, c.g), c.b);
    let cmax = max(max(c.r, c.g), c.b);
    let rng = cmax - cmin;
    if (rng <= 0.0) { return vec3<f32>(0.0); }
    // (c - min) * s / range maps min->0, max->s, mid->proportional.
    return (c - vec3<f32>(cmin)) * (s / rng);
}

@fragment
fn fs_composite(in: VOut) -> @location(0) vec4<f32> {
    let bd  = textureSample(t_backdrop, samp, in.uv);
    let src = textureSample(t_src, samp, in.uv);
    let cb  = bd.rgb;
    let cs  = src.rgb;
    let ba  = bd.a;
    let sa  = src.a * params.opacity;
    let mode = params.mode;

    var blended: vec3<f32>;
    if (mode == 12u)      { blended = set_lum(set_sat(cs, sat(cb)), lum(cb)); } // Hue
    else if (mode == 13u) { blended = set_lum(set_sat(cb, sat(cs)), lum(cb)); } // Saturation
    else if (mode == 14u) { blended = set_lum(cs, lum(cb)); }                   // Color
    else if (mode == 15u) { blended = set_lum(cb, lum(cs)); }                   // Luminosity
    else if (mode == 24u) {                                                     // Darker Color
        if (lum(cb) <= lum(cs)) { blended = cb; } else { blended = cs; }
    }
    else if (mode == 25u) {                                                     // Lighter Color
        if (lum(cb) >= lum(cs)) { blended = cb; } else { blended = cs; }
    }
    else {
        blended = vec3<f32>(
            blend_channel(mode, cb.r, cs.r),
            blend_channel(mode, cb.g, cs.g),
            blend_channel(mode, cb.b, cs.b),
        );
    }

    // Cs' = (1 - ab)·Cs + ab·B(Cb,Cs), then straight-alpha source-over.
    let mixed = (1.0 - ba) * cs + ba * blended;
    let oa = sa + ba * (1.0 - sa);
    var orgb = vec3<f32>(0.0);
    if (oa > 0.0) { orgb = (mixed * sa + cb * ba * (1.0 - sa)) / oa; }
    return vec4<f32>(orgb, oa);
}
"#;

/// Bind group layout for the composite pass: backdrop texture, source texture,
/// sampler, and the [`CompositeParams`] uniform.
pub fn create_composite_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let tex = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("composite_bgl"),
        entries: &[
            tex(0),
            tex(1),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

/// The composite pipeline. Output is [`SCENE_FORMAT`]; the shader emits the
/// finished pixel so fixed-function blending is disabled (Replace), and the pass
/// is single-sampled (the isolated layer texture is already resolved).
pub fn create_composite_pipeline(
    device: &wgpu::Device,
    output_format: wgpu::TextureFormat,
    composite_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("composite_shader"),
        source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("composite_layout"),
        bind_group_layouts: &[composite_bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("composite_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_quad",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_composite",
            targets: &[Some(wgpu::ColorTargetState {
                format: output_format,
                blend: None, // shader composites against the sampled backdrop
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

// ─── Scene → surface present (blit) pass (03 §4.5.4, audit A-1) ─────────────────

/// Full-screen present shader for the A-1 blit (03 §4.5.4): samples the sRGB
/// offscreen scene target (the sampler hardware-decodes it to linear) and writes
/// the sRGB-encoded pixel into a **non-sRGB** presentation surface — so the
/// gamma encode is applied *explicitly* here (`srgb_oetf`), because the target
/// view is not `*Srgb` and gives no free hardware encode. The `srgb_oetf`
/// function mirrors `crate::color::srgb_oetf` byte-for-byte (breakpoint 0.0031308,
/// slope 12.92, α 1.055, β 0.055, 1/γ = 1/2.4). Alpha is passed through: the
/// document scene is opaque-over-BG and premultiplied source-over, so no
/// unpremultiply is needed on this path.
pub const BLIT_SHADER: &str = r#"
@group(0) @binding(0) var t_scene: texture_2d<f32>;
@group(0) @binding(1) var samp:    sampler;

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

fn srgb_oetf(c: f32) -> f32 {
    if (c <= 0.0031308) { return 12.92 * c; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

@fragment
fn fs_blit(in: VOut) -> @location(0) vec4<f32> {
    // Hardware sRGB-decode on sample → linear scene colour.
    let lin = textureSample(t_scene, samp, in.uv);
    return vec4<f32>(srgb_oetf(lin.r), srgb_oetf(lin.g), srgb_oetf(lin.b), lin.a);
}
"#;

/// Bind group layout for the blit pass: scene texture + sampler.
pub fn create_blit_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("blit_bgl"),
        entries: &[
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
        ],
    })
}

/// The present (blit) pipeline. `output_format` is the non-sRGB presentation
/// surface format; the shader writes the finished pixel, so no fixed-function
/// blend and a single sample.
pub fn create_blit_pipeline(
    device: &wgpu::Device,
    output_format: wgpu::TextureFormat,
    blit_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blit_shader"),
        source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("blit_layout"),
        bind_group_layouts: &[blit_bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blit_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_quad",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_blit",
            targets: &[Some(wgpu::ColorTargetState {
                format: output_format,
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

// ─── Asset → working-texture conversion (03 §2.5 Tier B / §4.2) ─────────────────

/// The working-graph texture format: linear-light, premultiplied `Rgba16Float`
/// (D-09). Every `IrOp` output and the Tier B vector-to-working conversion
/// target this format.
pub const WORKING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Convert an sRGB-gamma, straight-alpha RGBA8 layer into linear-light,
/// premultiplied `Rgba16Float` — the "asset → working" boundary (§4.2 rows 2-3).
/// The source is sampled as **raw** bytes (a non-sRGB view / an `Rgba8Unorm`
/// upload), so the sRGB EOTF is applied explicitly here rather than by the
/// hardware; this keeps Tier A (CPU-readback upload) and Tier B (GPU-to-GPU)
/// numerically identical — they run the same math on the same bytes. Uses the
/// exact sRGB EOTF from `raster/adjust.rs:72` (threshold 0.04045).
pub const CONVERT_SHADER: &str = r#"
@group(0) @binding(0) var t_src: texture_2d<f32>;
@group(0) @binding(1) var samp:  sampler;

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

fn srgb_eotf(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@fragment
fn fs_convert(in: VOut) -> @location(0) vec4<f32> {
    let raw = textureSample(t_src, samp, in.uv);
    let lin = vec3<f32>(srgb_eotf(raw.r), srgb_eotf(raw.g), srgb_eotf(raw.b));
    let a = raw.a;
    return vec4<f32>(lin * a, a); // premultiplied, linear
}
"#;

/// Bind group layout for the conversion pass: source texture + sampler.
pub fn create_convert_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("convert_bgl"),
        entries: &[
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
        ],
    })
}

/// The asset→working conversion pipeline. Output is [`WORKING_FORMAT`]; the
/// shader writes the finished premultiplied pixel, so no fixed-function blend.
pub fn create_convert_pipeline(
    device: &wgpu::Device,
    convert_bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("convert_shader"),
        source: wgpu::ShaderSource::Wgsl(CONVERT_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("convert_layout"),
        bind_group_layouts: &[convert_bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("convert_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_quad",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_convert",
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

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::{BlendFactor, BlendOperation};

    #[test]
    fn scene_format_is_srgb_for_linear_blending() {
        // The document blend target must be sRGB-encoded so fixed-function
        // blending decodes to linear, blends, and re-encodes (issue #145).
        assert!(
            SCENE_FORMAT.is_srgb(),
            "SCENE_FORMAT must be an sRGB format so blending runs in linear space",
        );
    }

    #[test]
    fn windowed_scene_format_derivation_is_srgb() {
        // The windowed renderer derives its scene format from the swapchain
        // surface format via `add_srgb_suffix()`. For every non-sRGB surface
        // format we accept, that derivation must yield a distinct sRGB format
        // (otherwise the document pass would silently blend in non-sRGB space).
        for surface in [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ] {
            let scene = surface.add_srgb_suffix();
            assert!(
                scene.is_srgb(),
                "{surface:?}.add_srgb_suffix() must be sRGB"
            );
            assert_ne!(
                scene, surface,
                "{surface:?} must map to a distinct sRGB view format",
            );
        }
    }

    #[test]
    fn separable_modes_have_blend_states() {
        for mode in SEPARABLE_BLEND_MODES {
            assert!(
                separable_blend_state(mode).is_some(),
                "{mode:?} should map to a fixed-function blend state",
            );
        }
    }

    #[test]
    fn non_separable_modes_fall_back() {
        // Normal and shader-only modes have no fixed-function blend state.
        for mode in [
            BlendMode::Normal,
            BlendMode::Overlay,
            BlendMode::ColorDodge,
            BlendMode::Hue,
            BlendMode::Luminosity,
        ] {
            assert!(separable_blend_state(mode).is_none(), "{mode:?}");
        }
    }

    #[test]
    fn multiply_is_src_times_dst() {
        let bs = separable_blend_state(BlendMode::Multiply).unwrap();
        assert_eq!(bs.color.src_factor, BlendFactor::Dst);
        assert_eq!(bs.color.dst_factor, BlendFactor::Zero);
        assert_eq!(bs.color.operation, BlendOperation::Add);
    }

    #[test]
    fn darken_and_lighten_use_min_max() {
        assert_eq!(
            separable_blend_state(BlendMode::Darken)
                .unwrap()
                .color
                .operation,
            BlendOperation::Min,
        );
        assert_eq!(
            separable_blend_state(BlendMode::Lighten)
                .unwrap()
                .color
                .operation,
            BlendOperation::Max,
        );
    }

    #[test]
    fn alpha_channel_preserves_backdrop() {
        // Every separable mode must keep the backdrop's alpha (src*0 + dst*1).
        for mode in SEPARABLE_BLEND_MODES {
            let bs = separable_blend_state(mode).unwrap();
            assert_eq!(bs.alpha.src_factor, BlendFactor::Zero, "{mode:?}");
            assert_eq!(bs.alpha.dst_factor, BlendFactor::One, "{mode:?}");
        }
    }

    /// The composite shader's blend-mode ids are unique, dense 0..=15, and the
    /// non-separable HSL modes are exactly the ids `>= 12` the shader branches on.
    #[test]
    fn blend_mode_index_is_dense_and_hsl_is_high() {
        use photonic_core::layer::BlendMode::*;
        let all = [
            Normal, Multiply, Screen, Overlay, Darken, Lighten, ColorDodge, ColorBurn, HardLight,
            SoftLight, Difference, Exclusion, Hue, Saturation, Color, Luminosity,
        ];
        let mut seen = std::collections::HashSet::new();
        for m in all {
            assert!(
                seen.insert(blend_mode_index(m)),
                "duplicate index for {m:?}"
            );
        }
        assert_eq!(seen, (0u32..=15).collect());
        for m in [Hue, Saturation, Color, Luminosity] {
            assert!(blend_mode_index(m) >= 12, "{m:?} must be a high (HSL) id");
        }
        for m in [Normal, Overlay, SoftLight, ColorDodge, Exclusion] {
            assert!(blend_mode_index(m) < 12, "{m:?} must be a separable id");
        }
    }

    /// Validate every inline WGSL shader parses AND type-checks via naga — a full
    /// compile of the composite/blend math with no GPU device required.
    #[test]
    fn wgsl_shaders_parse_and_validate() {
        // Grade + scope shaders splice a shared prelude at build time, so
        // validate the expanded form the pipeline actually compiles.
        let grade_math = crate::grade_gpu::expanded_math_shader();
        let grade_curves = crate::grade_gpu::expanded_curves_shader();
        let grade_lut3d = crate::grade_gpu::expanded_lut3d_shader();
        let scope_hist = crate::scopes::expanded_histogram_shader();
        let scope_wave = crate::scopes::expanded_waveform_shader();
        let scope_vector = crate::scopes::expanded_vectorscope_shader();
        for (name, src) in [
            ("fill", FILL_SHADER),
            ("blur", BLUR_SHADER),
            ("composite", COMPOSITE_SHADER),
            ("blit", BLIT_SHADER),
            ("convert", CONVERT_SHADER),
            ("yuv_convert", crate::video::YUV_CONVERT_SHADER),
            ("present", crate::video::PRESENT_SHADER),
            ("grade_math", grade_math.as_str()),
            ("grade_curves", grade_curves.as_str()),
            ("grade_lut3d", grade_lut3d.as_str()),
            ("scope_histogram", scope_hist.as_str()),
            ("scope_waveform", scope_wave.as_str()),
            ("scope_vectorscope", scope_vector.as_str()),
        ] {
            let module = naga::front::wgsl::parse_str(src)
                .unwrap_or_else(|e| panic!("{name} shader failed to parse: {e:?}"));
            let mut validator = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            );
            validator
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name} shader failed to validate: {e:?}"));
        }
    }
}
