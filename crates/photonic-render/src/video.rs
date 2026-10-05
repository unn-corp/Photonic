//! Video texture path (03-render-color-pipeline.md §3): YUV plane upload +
//! YUV→working conversion, and the `EngineFrame`→screen present pass (§5).
//!
//! The YUV conversion is the GPU twin of [`crate::color::yuv_to_working`] — they
//! share the constants in [`crate::color`] (§4.4 rule 1), and
//! `wgsl_yuv_constants_match_rust` asserts the shader source contains each one.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::color::{Colorimetry, Matrix, Range};
use crate::pipeline::WORKING_FORMAT;

/// Decoded YUV planes ready for GPU upload (03 §3.1). Rows are tightly packed.
pub enum YuvPlanes<'a> {
    /// 4:2:0 — full-res luma, half-res (both dims) Cb/Cr, no alpha.
    Yuv420 {
        width: u32,
        height: u32,
        y: &'a [u8],
        cb: &'a [u8],
        cr: &'a [u8],
    },
    /// 8-bit 4:2:2 — half-width, full-height Cb/Cr.
    Yuv422 {
        width: u32,
        height: u32,
        y: &'a [u8],
        cb: &'a [u8],
        cr: &'a [u8],
    },
    /// 8-bit 4:4:4 without alpha — three full-resolution planes.
    Yuv444 {
        width: u32,
        height: u32,
        y: &'a [u8],
        cb: &'a [u8],
        cr: &'a [u8],
    },
    /// 16-bit little-endian 4:2:0 source codes. Chroma planes use ceil(width/2)
    /// by ceil(height/2), retaining native sample siting for reconstruction.
    Yuv420P16 {
        width: u32,
        height: u32,
        y: &'a [u8],
        cb: &'a [u8],
        cr: &'a [u8],
    },
    /// 16-bit little-endian 4:2:2 source codes. Chroma is half-width and
    /// full-height, with horizontal sample siting preserved for reconstruction.
    Yuv422P16 {
        width: u32,
        height: u32,
        y: &'a [u8],
        cb: &'a [u8],
        cr: &'a [u8],
    },
    /// 4:4:4 with alpha — four full-res planes (ProRes 4444 / VP9-alpha etc.).
    Yuva444 {
        width: u32,
        height: u32,
        y: &'a [u8],
        cb: &'a [u8],
        cr: &'a [u8],
        a: &'a [u8],
    },
    /// Full-resolution 16-bit little-endian YUV, optionally with alpha.
    /// High-bit-depth source decode uses this without an 8-bit intermediate.
    Yuv444P16 {
        width: u32,
        height: u32,
        y: &'a [u8],
        cb: &'a [u8],
        cr: &'a [u8],
        a: Option<&'a [u8]>,
    },
}

impl YuvPlanes<'_> {
    fn dims(&self) -> (u32, u32) {
        match *self {
            YuvPlanes::Yuv420 { width, height, .. }
            | YuvPlanes::Yuv422 { width, height, .. }
            | YuvPlanes::Yuv444 { width, height, .. }
            | YuvPlanes::Yuv420P16 { width, height, .. }
            | YuvPlanes::Yuv422P16 { width, height, .. }
            | YuvPlanes::Yuva444 { width, height, .. }
            | YuvPlanes::Yuv444P16 { width, height, .. } => (width, height),
        }
    }
}

/// Uniform selecting matrix + range + alpha presence for the YUV shader.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct YuvParams {
    matrix_id: u32, // 0 = BT.709, 1 = BT.601
    range_id: u32,  // 0 = limited, 1 = full
    has_alpha: u32, // 0 / 1
    depth_id: u32,  // 0 = 8-bit, 1 = 16-bit normalized input
    chroma_dx: f32, // native subsampled offset in luma-sample spacings
    chroma_dy: f32,
    reference_white_nits: f32,
    hlg_peak_nits: f32,
}

/// YUV→linear-premultiplied-`Rgba16Float` conversion pass (03 §3.2/§3.3). The
/// numeric literals here are the [`crate::color`] constants; keep the two in
/// sync (a CI test enforces it).
pub const YUV_CONVERT_SHADER: &str = r#"
@group(0) @binding(0) var t_y:  texture_2d<f32>;
@group(0) @binding(1) var t_cb: texture_2d<f32>;
@group(0) @binding(2) var t_cr: texture_2d<f32>;
@group(0) @binding(3) var t_a:  texture_2d<f32>;
@group(0) @binding(4) var samp_n: sampler;  // nearest: luma, alpha
@group(0) @binding(5) var samp_l: sampler;  // linear: chroma upsample
struct Params {
    matrix_id: u32, range_id: u32, has_alpha: u32, depth_id: u32,
    chroma_dx: f32, chroma_dy: f32, reference_white_nits: f32, hlg_peak_nits: f32,
}
@group(0) @binding(6) var<uniform> params: Params;

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

// ITU-R BT.709 EOTF: video signal → scene-linear (exact, not the sRGB curve).
fn bt709_eotf(e: f32) -> f32 {
    if (e < 0.081) { return e / 4.5; }
    return pow((e + 0.099) / 1.099, 1.0 / 0.45);
}

@fragment
fn fs_yuv(in: VOut) -> @location(0) vec4<f32> {
    let y_raw  = textureSample(t_y,  samp_n, in.uv).r;
    let cb_raw = textureSample(t_cb, samp_l, in.uv).r;
    let cr_raw = textureSample(t_cr, samp_l, in.uv).r;
    var a = 1.0;
    if (params.has_alpha == 1u) { a = textureSample(t_a, samp_n, in.uv).r; }

    // 1. Range expansion in the actual input code domain. FFmpeg's 16-bit
    // planar output scales legal 8-bit levels by 256.
    var yp: f32;
    var cb: f32;
    var cr: f32;
    if (params.range_id == 0u) {
        if (params.depth_id == 1u) {
            yp = (y_raw * 65535.0 - 4096.0) / 56064.0;
            cb = (cb_raw * 65535.0 - 4096.0) / 57344.0 - 0.5;
            cr = (cr_raw * 65535.0 - 4096.0) / 57344.0 - 0.5;
        } else {
            yp = (y_raw * 255.0 - 16.0) / 219.0;
            cb = (cb_raw * 255.0 - 16.0) / 224.0 - 0.5;
            cr = (cr_raw * 255.0 - 16.0) / 224.0 - 0.5;
        }
    } else {
        yp = y_raw;
        cb = cb_raw - 0.5;
        cr = cr_raw - 0.5;
    }

    // 2. YUV→RGB matrix (video-signal domain, still gamma-encoded).
    var r: f32;
    var g: f32;
    var b: f32;
    if (params.matrix_id == 0u) {
        r = yp + 1.5748 * cr;
        g = yp - 0.1873 * cb - 0.4681 * cr;
        b = yp + 1.8556 * cb;
    } else {
        r = yp + 1.402 * cr;
        g = yp - 0.344136 * cb - 0.714136 * cr;
        b = yp + 1.772 * cb;
    }

    // 3. BT.709 EOTF → scene-linear.  4. Straight alpha, then premultiply.
    let lin = vec3<f32>(bt709_eotf(r), bt709_eotf(g), bt709_eotf(b));
    return vec4<f32>(lin * a, a);
}

// Native managed-color source reference. This is a separate entry point so
// Legacy SDR keeps its established shader arithmetic and golden output.
fn native_bt709_decode(e: f32) -> f32 {
    let magnitude = abs(e);
    if (magnitude <= 0.081) { return e / 4.5; }
    return sign(e) * pow((magnitude + 0.099) / 1.099, 1.0 / 0.45);
}

fn native_bt2020_decode(e: f32) -> f32 {
    let alpha = 1.09929682680944;
    let beta = 0.018053968510807;
    let magnitude = abs(e);
    if (magnitude <= 4.5 * beta) { return e / 4.5; }
    return sign(e) * pow((magnitude + alpha - 1.0) / alpha, 1.0 / 0.45);
}

// HLG inverse OETF with an odd signed extension; nominal [0,1] follows BT.2100.
// System gamma calibrates neutral reference white only; no display OOTF is applied here.
fn native_hlg_decode(code: f32) -> f32 {
    let v = abs(code);
    var scene = v * v / 3.0;
    if (v > 0.5) { scene = (exp((v - 0.55991073) / 0.17883277) + 0.28466892) / 12.0; }
    let gamma = 1.2 + 0.42 * log2(params.hlg_peak_nits / 1000.0) / log2(10.0);
    let scale = pow(params.reference_white_nits / params.hlg_peak_nits, -1.0 / gamma);
    return sign(code) * scene * scale;
}

// ITU-R BT.2100-3 PQ EOTF. PQ RGB code excursions are clipped to [0,1].
fn native_pq_decode(e: f32) -> f32 {
    let n = pow(clamp(e, 0.0, 1.0), 32.0 / 2523.0);
    let luminance = 10000.0 * pow(max(n - 3424.0 / 4096.0, 0.0) / (2413.0 / 128.0 - 2392.0 / 128.0 * n), 16384.0 / 2610.0);
    return luminance / params.reference_white_nits;
}

// High-depth native upload stores each u16 code losslessly as little-endian
// low/high bytes in RG8Unorm. Rounded byte reconstruction precedes arithmetic.
fn native_sample_code(sample: vec4<f32>) -> f32 {
    if (params.depth_id == 1u) {
        return round(sample.r * 255.0) + 256.0 * round(sample.g * 255.0);
    }
    return sample.r * 255.0;
}

// Packed low/high bytes cannot be linearly filtered independently: carry
// across byte boundaries would corrupt the interpolated 16-bit code. Decode
// four neighboring texels first, then interpolate the reconstructed codes.
fn native_chroma_code_16(tex: texture_2d<f32>, uv: vec2<f32>) -> f32 {
    let dims = vec2<i32>(textureDimensions(tex));
    let pixel = uv * vec2<f32>(dims) - vec2<f32>(0.5);
    let lower = vec2<i32>(floor(pixel));
    let weight = fract(pixel);
    let lo = vec2<i32>(0);
    let hi = dims - vec2<i32>(1);
    let c00 = native_sample_code(textureLoad(tex, clamp(lower, lo, hi), 0));
    let c10 = native_sample_code(textureLoad(tex, clamp(lower + vec2<i32>(1, 0), lo, hi), 0));
    let c01 = native_sample_code(textureLoad(tex, clamp(lower + vec2<i32>(0, 1), lo, hi), 0));
    let c11 = native_sample_code(textureLoad(tex, clamp(lower + vec2<i32>(1, 1), lo, hi), 0));
    return mix(mix(c00, c10, weight.x), mix(c01, c11, weight.x), weight.y);
}

@fragment
fn fs_native_yuv(in: VOut) -> @location(0) vec4<f32> {
    let code_max = select(255.0, 65535.0, params.depth_id == 1u);
    let code_scale = select(1.0, 256.0, params.depth_id == 1u);
    let y_code = native_sample_code(textureSample(t_y, samp_n, in.uv));
    var chroma_uv = in.uv;
    if (any(textureDimensions(t_cb) != textureDimensions(t_y))) {
        let chroma_size = vec2<f32>(textureDimensions(t_cb));
        let luma_size = textureDimensions(t_y);
        let subsampling = vec2<f32>(
            select(1.0, 2.0, textureDimensions(t_cb).x != luma_size.x),
            select(1.0, 2.0, textureDimensions(t_cb).y != luma_size.y)
        );
        chroma_uv = (in.clip_pos.xy + vec2<f32>(params.chroma_dx, params.chroma_dy)) / (subsampling * chroma_size);
    }
    var cb_code: f32;
    var cr_code: f32;
    if (params.depth_id == 1u) {
        cb_code = native_chroma_code_16(t_cb, chroma_uv);
        cr_code = native_chroma_code_16(t_cr, chroma_uv);
    } else {
        cb_code = native_sample_code(textureSample(t_cb, samp_l, chroma_uv));
        cr_code = native_sample_code(textureSample(t_cr, samp_l, chroma_uv));
    }
    var y: f32;
    var cb: f32;
    var cr: f32;
    if (params.range_id == 0u) {
        y = (y_code - 16.0 * code_scale) / (219.0 * code_scale);
        cb = (cb_code - 128.0 * code_scale) / (224.0 * code_scale);
        cr = (cr_code - 128.0 * code_scale) / (224.0 * code_scale);
    } else {
        y = y_code / code_max;
        let center = select(128.0, 32768.0, params.depth_id == 1u);
        cb = (cb_code - center) / code_max;
        cr = (cr_code - center) / code_max;
    }
    var linear: vec3<f32>;
    var ap1: vec3<f32>;
    if (params.matrix_id >= 2u) {
        let r = y + 1.4746 * cr;
        let b = y + 1.8814 * cb;
        let g = (y - 0.2627 * r - 0.0593 * b) / 0.6780;
        linear = vec3<f32>(native_bt2020_decode(r), native_bt2020_decode(g), native_bt2020_decode(b));
        if (params.matrix_id == 3u) {
            linear = vec3<f32>(native_pq_decode(r), native_pq_decode(g), native_pq_decode(b));
        }
        if (params.matrix_id == 4u) { linear = vec3<f32>(native_hlg_decode(r), native_hlg_decode(g), native_hlg_decode(b)); }
        ap1 = vec3<f32>(
            dot(linear, vec3<f32>(0.9748949779, 0.0195991086, 0.0055059134)),
            dot(linear, vec3<f32>(0.0021795628, 0.9955354689, 0.0022849683)),
            dot(linear, vec3<f32>(0.0047972397, 0.0245320166, 0.9706707437))
        );
    } else {
        let r = y + 1.5748 * cr;
        let b = y + 1.8556 * cb;
        let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
        linear = vec3<f32>(native_bt709_decode(r), native_bt709_decode(g), native_bt709_decode(b));
        ap1 = vec3<f32>(
            dot(linear, vec3<f32>(0.6130974024, 0.3395231462, 0.0473794514)),
            dot(linear, vec3<f32>(0.0701937225, 0.9163538791, 0.0134523985)),
            dot(linear, vec3<f32>(0.0206155929, 0.1095697729, 0.8698146342))
        );
    }
    var a = 1.0;
    if (params.has_alpha == 1u) {
        a = native_sample_code(textureSample(t_a, samp_n, in.uv)) / code_max;
    }
    return vec4<f32>(clamp(ap1 * a, vec3<f32>(-65504.0), vec3<f32>(65504.0)), a);
}
"#;

/// `EngineFrame`→screen present pass (03 §5): sample the linear premultiplied
/// working texture, unpremultiply, sRGB-OETF encode, write the non-sRGB surface
/// (the shader does the encode itself since the surface is not an sRGB format).
pub const PRESENT_SHADER: &str = r#"
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

@fragment
fn fs_present(in: VOut) -> @location(0) vec4<f32> {
    let p = textureSample(t_src, samp, in.uv);
    let a = max(p.a, 1e-6);
    let straight = p.rgb / a;
    return vec4<f32>(straight, p.a);
}

fn srgb_decode(v: f32) -> f32 {
    let bounded = clamp(v, 0.0, 1.0);
    if (bounded <= 0.04045) { return bounded / 12.92; }
    return pow((bounded + 0.055) / 1.055, 2.4);
}

// The source already carries straight-display sRGB after unpremultiplication.
// Decode once before writing to an sRGB attachment, whose hardware OETF then
// restores the intended code values. The legacy linear path above is unchanged.
@fragment
fn fs_present_encoded(in: VOut) -> @location(0) vec4<f32> {
    let p = textureSample(t_src, samp, in.uv);
    let a = max(p.a, 1e-6);
    let encoded = p.rgb / a;
    return vec4<f32>(
        srgb_decode(encoded.r), srgb_decode(encoded.g), srgb_decode(encoded.b), p.a
    );
}

// K-B17 alpha view: show the alpha channel as luminance so keys are
// judicable against something other than black. Fully opaque so the
// checkerboard / compositor behind does not dim the matte.
@fragment
fn fs_present_alpha(in: VOut) -> @location(0) vec4<f32> {
    let p = textureSample(t_src, samp, in.uv);
    return vec4<f32>(p.a, p.a, p.a, 1.0);
}
"#;

/// How the program monitor presents the working-format engine frame (K-B17).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum PresentChannel {
    /// Colour (unpremultiply → sRGB target). Default.
    #[default]
    Color,
    /// Alpha as luminance, fully opaque.
    Alpha,
}

fn plane_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
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
    let samp = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuv_bgl"),
        entries: &[
            tex(0),
            tex(1),
            tex(2),
            tex(3),
            samp(4),
            samp(5),
            wgpu::BindGroupLayoutEntry {
                binding: 6,
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

fn fullscreen_pipeline(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    shader_src: &str,
    fs_entry: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("video_shader"),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("video_layout"),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("video_pipeline"),
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
                format,
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

fn r8_texture(device: &wgpu::Device, w: u32, h: u32, label: &'static str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn r16_texture(device: &wgpu::Device, w: u32, h: u32, label: &'static str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn rg8_texture(device: &wgpu::Device, w: u32, h: u32, label: &'static str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rg8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write_r8_plane(queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, h: u32, data: &[u8]) {
    queue.write_texture(
        tex.as_image_copy(),
        data,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(w),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

fn write_r16_plane(
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
    data: &[u8],
    staging: &mut Vec<u8>,
) {
    assert_eq!(data.len(), w as usize * h as usize * 2);
    staging.clear();
    staging.reserve(data.len());
    for sample in data.chunks_exact(2) {
        let code = u16::from_le_bytes([sample[0], sample[1]]);
        let bits = unit_f32_to_f16_bits(code as f32 / 65535.0);
        staging.extend_from_slice(&bits.to_le_bytes());
    }
    queue.write_texture(
        tex.as_image_copy(),
        staging,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(w * 2),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

fn write_rg8_packed_u16_plane(
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
    data: &[u8],
) {
    assert_eq!(data.len(), w as usize * h as usize * 2);
    queue.write_texture(
        tex.as_image_copy(),
        data,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(w * 2),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

/// Round a finite unit-interval f32 to IEEE binary16 (ties to even). The input
/// is a normalized u16 code, so NaN, infinity and negative cases cannot occur.
fn unit_f32_to_f16_bits(value: f32) -> u16 {
    debug_assert!(value.is_finite() && (0.0..=1.0).contains(&value));
    let bits = value.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    if exponent >= 1 {
        let rounded = mantissa + 0xfff + ((mantissa >> 13) & 1);
        (((exponent as u32) << 10) + (rounded >> 13)) as u16
    } else if exponent >= -10 {
        let significand = mantissa | 0x80_0000;
        let shift = (14 - exponent) as u32;
        let retained = significand >> shift;
        let remainder = significand & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        (retained + u32::from(remainder > halfway || (remainder == halfway && retained & 1 != 0)))
            as u16
    } else {
        0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum YuvUploadLayout {
    Yuv420 {
        width: u32,
        height: u32,
    },
    Yuv422 {
        width: u32,
        height: u32,
    },
    Yuv444 {
        width: u32,
        height: u32,
    },
    Yuv420P16 {
        width: u32,
        height: u32,
        native: bool,
    },
    Yuv422P16 {
        width: u32,
        height: u32,
        native: bool,
    },
    Yuva444 {
        width: u32,
        height: u32,
    },
    Yuv444P16 {
        width: u32,
        height: u32,
        alpha: bool,
        native: bool,
    },
}

impl YuvUploadLayout {
    fn from_planes(planes: &YuvPlanes, width: u32, height: u32, native: bool) -> Self {
        match planes {
            YuvPlanes::Yuv420 { .. } => Self::Yuv420 { width, height },
            YuvPlanes::Yuv422 { .. } => Self::Yuv422 { width, height },
            YuvPlanes::Yuv444 { .. } => Self::Yuv444 { width, height },
            YuvPlanes::Yuv420P16 { .. } => Self::Yuv420P16 {
                width,
                height,
                native,
            },
            YuvPlanes::Yuv422P16 { .. } => Self::Yuv422P16 {
                width,
                height,
                native,
            },
            YuvPlanes::Yuva444 { .. } => Self::Yuva444 { width, height },
            YuvPlanes::Yuv444P16 { a, .. } => Self::Yuv444P16 {
                width,
                height,
                alpha: a.is_some(),
                native,
            },
        }
    }

    fn has_alpha(self) -> u32 {
        match self {
            Self::Yuv420 { .. } => 0,
            Self::Yuv422 { .. } => 0,
            Self::Yuv444 { .. } => 0,
            Self::Yuv420P16 { .. } => 0,
            Self::Yuv422P16 { .. } => 0,
            Self::Yuva444 { .. } => 1,
            Self::Yuv444P16 { alpha, .. } => u32::from(alpha),
        }
    }
}

struct YuvUploadScratch {
    y: wgpu::Texture,
    cb: wgpu::Texture,
    cr: wgpu::Texture,
    a: wgpu::Texture,
    params: wgpu::Buffer,
    bind: wgpu::BindGroup,
    upload_bytes: Vec<u8>,
    last_used: u64,
}

struct YuvUploadScratchCache {
    entries: HashMap<YuvUploadLayout, YuvUploadScratch>,
    use_counter: u64,
}

/// Decoded clips commonly keep one source layout for a long run. Four layouts
/// cover normal A/B editing without letting a project with mixed source sizes
/// retain unbounded GPU upload surfaces.
const YUV_UPLOAD_SCRATCH_CAP: usize = 4;

impl YuvUploadScratchCache {
    fn get_or_create<'a>(
        &'a mut self,
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        nearest: &wgpu::Sampler,
        linear: &wgpu::Sampler,
        layout: YuvUploadLayout,
    ) -> &'a mut YuvUploadScratch {
        self.use_counter = self.use_counter.wrapping_add(1);
        let stamp = self.use_counter;
        if !self.entries.contains_key(&layout) {
            if self.entries.len() >= YUV_UPLOAD_SCRATCH_CAP {
                if let Some(victim) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, scratch)| scratch.last_used)
                    .map(|(layout, _)| *layout)
                {
                    self.entries.remove(&victim);
                }
            }
            self.entries.insert(
                layout,
                YuvUploadScratch::new(device, bgl, nearest, linear, layout, stamp),
            );
        }
        let scratch = self
            .entries
            .get_mut(&layout)
            .expect("scratch inserted or already present");
        scratch.last_used = stamp;
        scratch
    }
}

impl YuvUploadScratch {
    fn new(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        nearest: &wgpu::Sampler,
        linear: &wgpu::Sampler,
        layout: YuvUploadLayout,
        last_used: u64,
    ) -> Self {
        let (w, h) = match layout {
            YuvUploadLayout::Yuv420 { width, height }
            | YuvUploadLayout::Yuv422 { width, height }
            | YuvUploadLayout::Yuv444 { width, height }
            | YuvUploadLayout::Yuv420P16 { width, height, .. }
            | YuvUploadLayout::Yuv422P16 { width, height, .. }
            | YuvUploadLayout::Yuva444 { width, height }
            | YuvUploadLayout::Yuv444P16 { width, height, .. } => (width, height),
        };
        let (cw, ch) = match layout {
            YuvUploadLayout::Yuv420 { .. } | YuvUploadLayout::Yuv420P16 { .. } => {
                (w.div_ceil(2), h.div_ceil(2))
            }
            YuvUploadLayout::Yuv422 { .. } | YuvUploadLayout::Yuv422P16 { .. } => {
                (w.div_ceil(2), h)
            }
            YuvUploadLayout::Yuv444 { .. }
            | YuvUploadLayout::Yuva444 { .. }
            | YuvUploadLayout::Yuv444P16 { .. } => (w, h),
        };
        let high_depth = matches!(
            layout,
            YuvUploadLayout::Yuv444P16 { .. }
                | YuvUploadLayout::Yuv420P16 { .. }
                | YuvUploadLayout::Yuv422P16 { .. }
        );
        let native_high_depth = matches!(
            layout,
            YuvUploadLayout::Yuv444P16 { native: true, .. }
                | YuvUploadLayout::Yuv420P16 { native: true, .. }
                | YuvUploadLayout::Yuv422P16 { native: true, .. }
        );
        let make = |width, height, label| {
            if native_high_depth {
                rg8_texture(device, width, height, label)
            } else if high_depth {
                r16_texture(device, width, height, label)
            } else {
                r8_texture(device, width, height, label)
            }
        };
        let y = make(w, h, "yuv_plane_y");
        let cb = make(cw, ch, "yuv_plane_cb");
        let cr = make(cw, ch, "yuv_plane_cr");
        // The non-alpha path still binds this texture, but never samples it.
        let a = match layout {
            YuvUploadLayout::Yuv420 { .. } => r8_texture(device, 1, 1, "yuv_plane_a_dummy"),
            YuvUploadLayout::Yuv422 { .. } => r8_texture(device, 1, 1, "yuv_plane_a_dummy"),
            YuvUploadLayout::Yuv444 { .. } => r8_texture(device, 1, 1, "yuv_plane_a_dummy"),
            YuvUploadLayout::Yuv420P16 { .. } => make(1, 1, "yuv_plane_a_dummy"),
            YuvUploadLayout::Yuv422P16 { .. } => make(1, 1, "yuv_plane_a_dummy"),
            YuvUploadLayout::Yuva444 { .. } => r8_texture(device, w, h, "yuv_plane_a"),
            YuvUploadLayout::Yuv444P16 { alpha, .. } => {
                if alpha {
                    make(w, h, "yuv_plane_a")
                } else {
                    make(1, 1, "yuv_plane_a_dummy")
                }
            }
        };
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuv_params"),
            size: std::mem::size_of::<YuvParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let v = |t: &wgpu::Texture| t.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv_bg"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&v(&y)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&v(&cb)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&v(&cr)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&v(&a)),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(nearest),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(linear),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: params.as_entire_binding(),
                },
            ],
        });
        Self {
            y,
            cb,
            cr,
            a,
            params,
            bind,
            upload_bytes: Vec::new(),
            last_used,
        }
    }
}

/// Persistent YUV→working conversion resources for one wgpu device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeYuvInput {
    Bt2100HlgScene {
        reference_white_nits: u32,
        display_peak_nits: u32,
    },
    Bt2100PqDisplay {
        reference_white_nits: u32,
    },
    Bt709Scene,
    Bt2020Scene,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeChromaLocation {
    Left,
    Center,
    TopLeft,
    Top,
    BottomLeft,
    Bottom,
}

impl NativeChromaLocation {
    fn luma_offset(self) -> (f32, f32) {
        match self {
            Self::Left => (0.0, 0.5),
            Self::Center => (0.5, 0.5),
            Self::TopLeft => (0.0, 0.0),
            Self::Top => (0.5, 0.0),
            Self::BottomLeft => (0.0, 1.0),
            Self::Bottom => (0.5, 1.0),
        }
    }
}

pub struct YuvConverter {
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    native_pipeline: wgpu::RenderPipeline,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
    /// Reuses upload-side resources across cache misses. The mutex makes the
    /// write + submission sequence atomic if a caller shares a converter; each
    /// conversion submits before another can overwrite its source planes.
    scratch: Mutex<YuvUploadScratchCache>,
}

impl YuvConverter {
    pub fn new(device: &wgpu::Device) -> Self {
        let bgl = plane_bgl(device);
        let pipeline =
            fullscreen_pipeline(device, &bgl, YUV_CONVERT_SHADER, "fs_yuv", WORKING_FORMAT);
        let native_pipeline = fullscreen_pipeline(
            device,
            &bgl,
            YUV_CONVERT_SHADER,
            "fs_native_yuv",
            WORKING_FORMAT,
        );
        let nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuv_nearest"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuv_linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            bgl,
            pipeline,
            native_pipeline,
            nearest,
            linear,
            scratch: Mutex::new(YuvUploadScratchCache {
                entries: HashMap::new(),
                use_counter: 0,
            }),
        }
    }

    /// Upload YUV planes and convert them to a linear, premultiplied
    /// `Rgba16Float` working texture.
    pub fn convert(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        planes: &YuvPlanes,
        colorimetry: Colorimetry,
    ) -> wgpu::Texture {
        let (w, h) = planes.dims();
        let (w, h) = (w.max(1), h.max(1));
        self.convert_to_size(device, queue, planes, colorimetry, (w, h))
    }

    /// Like [`Self::convert`], but writes the logical source image into the
    /// upper-left region of a larger working texture. Preview graph textures
    /// use 64px pool buckets; rendering directly into that bucket avoids a
    /// second full-frame working texture plus a GPU copy for every cache miss.
    ///
    /// `output_size` must cover the source dimensions. The remaining texture
    /// area is transparent and is outside the logical frame region.
    pub fn convert_to_size(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        planes: &YuvPlanes,
        colorimetry: Colorimetry,
        output_size: (u32, u32),
    ) -> wgpu::Texture {
        self.convert_to_size_impl(device, queue, planes, colorimetry, output_size, None)
    }

    /// Isolated source-transform reference for the native managed pipeline.
    /// This does not enable native sequence rendering or export.
    pub fn convert_native(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        planes: &YuvPlanes,
        input: NativeYuvInput,
        range: Range,
        chroma_location: NativeChromaLocation,
    ) -> wgpu::Texture {
        let (w, h) = planes.dims();
        self.convert_native_to_size(
            device,
            queue,
            planes,
            input,
            range,
            chroma_location,
            (w.max(1), h.max(1)),
        )
    }

    /// Native source conversion into a padded graph pool bucket.
    pub fn convert_native_to_size(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        planes: &YuvPlanes,
        input: NativeYuvInput,
        range: Range,
        chroma_location: NativeChromaLocation,
        output_size: (u32, u32),
    ) -> wgpu::Texture {
        if let NativeYuvInput::Bt2100PqDisplay {
            reference_white_nits,
        } = input
        {
            assert!(
                (1..=10000).contains(&reference_white_nits),
                "invalid PQ reference white"
            );
        }
        if let NativeYuvInput::Bt2100HlgScene {
            reference_white_nits,
            display_peak_nits,
        } = input
        {
            assert!(
                (400..=2000).contains(&display_peak_nits)
                    && (1..=display_peak_nits).contains(&reference_white_nits),
                "invalid HLG normalization"
            );
        }
        self.convert_to_size_impl(
            device,
            queue,
            planes,
            Colorimetry {
                matrix: Matrix::Bt709,
                range,
            },
            output_size,
            Some((input, chroma_location)),
        )
    }

    fn convert_to_size_impl(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        planes: &YuvPlanes,
        colorimetry: Colorimetry,
        output_size: (u32, u32),
        native: Option<(NativeYuvInput, NativeChromaLocation)>,
    ) -> wgpu::Texture {
        let (w, h) = planes.dims();
        let (w, h) = (w.max(1), h.max(1));
        let (out_w, out_h) = (output_size.0.max(w), output_size.1.max(h));

        let layout = YuvUploadLayout::from_planes(planes, w, h, native.is_some());
        let has_alpha = layout.has_alpha();

        let params = YuvParams {
            matrix_id: match native {
                Some((NativeYuvInput::Bt709Scene, _)) => 0,
                Some((NativeYuvInput::Bt2020Scene, _)) => 2,
                Some((NativeYuvInput::Bt2100PqDisplay { .. }, _)) => 3,
                Some((NativeYuvInput::Bt2100HlgScene { .. }, _)) => 4,
                None => match colorimetry.matrix {
                    Matrix::Bt709 => 0,
                    Matrix::Bt601 => 1,
                },
            },
            range_id: match colorimetry.range {
                Range::Limited => 0,
                Range::Full => 1,
            },
            has_alpha,
            depth_id: u32::from(matches!(
                layout,
                YuvUploadLayout::Yuv444P16 { .. }
                    | YuvUploadLayout::Yuv420P16 { .. }
                    | YuvUploadLayout::Yuv422P16 { .. }
            )),
            chroma_dx: if matches!(
                planes,
                YuvPlanes::Yuv420 { .. }
                    | YuvPlanes::Yuv422 { .. }
                    | YuvPlanes::Yuv420P16 { .. }
                    | YuvPlanes::Yuv422P16 { .. }
            ) {
                native.map_or(0.0, |(_, location)| 0.5 - location.luma_offset().0)
            } else {
                0.0
            },
            chroma_dy: if matches!(
                planes,
                YuvPlanes::Yuv420 { .. } | YuvPlanes::Yuv420P16 { .. }
            ) {
                native.map_or(0.0, |(_, location)| 0.5 - location.luma_offset().1)
            } else {
                0.0
            },
            reference_white_nits: match native {
                Some((
                    NativeYuvInput::Bt2100PqDisplay {
                        reference_white_nits,
                    },
                    _,
                ))
                | Some((
                    NativeYuvInput::Bt2100HlgScene {
                        reference_white_nits,
                        ..
                    },
                    _,
                )) => reference_white_nits as f32,
                _ => 1.0,
            },
            hlg_peak_nits: match native {
                Some((
                    NativeYuvInput::Bt2100HlgScene {
                        display_peak_nits, ..
                    },
                    _,
                )) => display_peak_nits as f32,
                _ => 1000.0,
            },
        };
        // Queue writes and the conversion submission under one scratch lock.
        // This preserves the queue order when callers share a converter: a
        // later frame cannot overwrite the reused upload textures before this
        // frame's render pass has been submitted.
        let mut scratch_cache = self.scratch.lock().unwrap_or_else(|e| e.into_inner());
        let scratch =
            scratch_cache.get_or_create(device, &self.bgl, &self.nearest, &self.linear, layout);
        match *planes {
            YuvPlanes::Yuv420 { y, cb, cr, .. } => {
                let cw = w.div_ceil(2);
                let ch = h.div_ceil(2);
                write_r8_plane(queue, &scratch.y, w, h, y);
                write_r8_plane(queue, &scratch.cb, cw, ch, cb);
                write_r8_plane(queue, &scratch.cr, cw, ch, cr);
            }
            YuvPlanes::Yuv422 { y, cb, cr, .. } => {
                let cw = w.div_ceil(2);
                write_r8_plane(queue, &scratch.y, w, h, y);
                write_r8_plane(queue, &scratch.cb, cw, h, cb);
                write_r8_plane(queue, &scratch.cr, cw, h, cr);
            }
            YuvPlanes::Yuv444 { y, cb, cr, .. } => {
                write_r8_plane(queue, &scratch.y, w, h, y);
                write_r8_plane(queue, &scratch.cb, w, h, cb);
                write_r8_plane(queue, &scratch.cr, w, h, cr);
            }
            YuvPlanes::Yuv420P16 { y, cb, cr, .. } => {
                let cw = w.div_ceil(2);
                let ch = h.div_ceil(2);
                if native.is_some() {
                    write_rg8_packed_u16_plane(queue, &scratch.y, w, h, y);
                    write_rg8_packed_u16_plane(queue, &scratch.cb, cw, ch, cb);
                    write_rg8_packed_u16_plane(queue, &scratch.cr, cw, ch, cr);
                } else {
                    write_r16_plane(queue, &scratch.y, w, h, y, &mut scratch.upload_bytes);
                    write_r16_plane(queue, &scratch.cb, cw, ch, cb, &mut scratch.upload_bytes);
                    write_r16_plane(queue, &scratch.cr, cw, ch, cr, &mut scratch.upload_bytes);
                }
            }
            YuvPlanes::Yuv422P16 { y, cb, cr, .. } => {
                let cw = w.div_ceil(2);
                if native.is_some() {
                    write_rg8_packed_u16_plane(queue, &scratch.y, w, h, y);
                    write_rg8_packed_u16_plane(queue, &scratch.cb, cw, h, cb);
                    write_rg8_packed_u16_plane(queue, &scratch.cr, cw, h, cr);
                } else {
                    write_r16_plane(queue, &scratch.y, w, h, y, &mut scratch.upload_bytes);
                    write_r16_plane(queue, &scratch.cb, cw, h, cb, &mut scratch.upload_bytes);
                    write_r16_plane(queue, &scratch.cr, cw, h, cr, &mut scratch.upload_bytes);
                }
            }
            YuvPlanes::Yuva444 { y, cb, cr, a, .. } => {
                write_r8_plane(queue, &scratch.y, w, h, y);
                write_r8_plane(queue, &scratch.cb, w, h, cb);
                write_r8_plane(queue, &scratch.cr, w, h, cr);
                write_r8_plane(queue, &scratch.a, w, h, a);
            }
            YuvPlanes::Yuv444P16 { y, cb, cr, a, .. } => {
                let write = |texture: &wgpu::Texture, data: &[u8], staging: &mut Vec<u8>| {
                    if native.is_some() {
                        write_rg8_packed_u16_plane(queue, texture, w, h, data);
                    } else {
                        write_r16_plane(queue, texture, w, h, data, staging);
                    }
                };
                write(&scratch.y, y, &mut scratch.upload_bytes);
                write(&scratch.cb, cb, &mut scratch.upload_bytes);
                write(&scratch.cr, cr, &mut scratch.upload_bytes);
                if let Some(alpha) = a {
                    write(&scratch.a, alpha, &mut scratch.upload_bytes);
                }
            }
        }
        queue.write_buffer(&scratch.params, 0, bytemuck::bytes_of(&params));

        let out = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuv_working"),
            size: wgpu::Extent3d {
                width: out_w,
                height: out_h,
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
        });
        let out_view = out.create_view(&Default::default());
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yuv_convert_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out_view,
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
            pass.set_pipeline(if native.is_some() {
                &self.native_pipeline
            } else {
                &self.pipeline
            });
            pass.set_bind_group(0, &scratch.bind, &[]);
            // The source planes use logical-size UVs. Restrict rasterization to
            // that logical rectangle so a padded pool bucket does not stretch
            // the image into its transparent margin.
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            pass.draw(0..6, 0..1);
        }
        queue.submit([enc.finish()]);
        drop(scratch_cache);
        out
    }

    #[cfg(test)]
    fn scratch_layout_count(&self) -> usize {
        self.scratch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }
}

/// One-shot YUV conversion convenience wrapper for tests and one-off callers.
pub fn convert_yuv_planes_to_working(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    planes: &YuvPlanes,
    colorimetry: Colorimetry,
) -> wgpu::Texture {
    YuvConverter::new(device).convert(device, queue, planes, colorimetry)
}

/// Presents a working-format (`Rgba16Float`, linear, premultiplied) texture to a
/// sRGB target, per 03 §5 — the normative `EngineFrame`→screen handoff 04
/// conforms to. The pipeline writes linear values; hardware encodes to the
/// sRGB target and egui decodes it when sampling. Holds the pipeline + sampler
/// for one target format; call
/// [`Self::present_engine_frame`] per displayed frame.
pub struct VideoPresenter {
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    pipeline_encoded: wgpu::RenderPipeline,
    /// K-B17 alpha-as-luminance present path (same BGL/sampler as colour).
    pipeline_alpha: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

impl VideoPresenter {
    /// Build the present pipeline for `target_format` (the negotiated
    /// `Rgba8UnormSrgb` / `Bgra8UnormSrgb` target format).
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present_bgl"),
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
        });
        let pipeline =
            fullscreen_pipeline(device, &bgl, PRESENT_SHADER, "fs_present", target_format);
        let pipeline_encoded = fullscreen_pipeline(
            device,
            &bgl,
            PRESENT_SHADER,
            "fs_present_encoded",
            target_format,
        );
        let pipeline_alpha = fullscreen_pipeline(
            device,
            &bgl,
            PRESENT_SHADER,
            "fs_present_alpha",
            target_format,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("present_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            bgl,
            pipeline,
            pipeline_encoded,
            pipeline_alpha,
            sampler,
        }
    }

    /// Record the present pass: sample `source` (working format) → unpremultiply
    /// → write linear values to an sRGB `target` (03 §5).
    ///
    /// `channel` selects colour (default) or alpha-as-luminance (K-B17).
    pub fn present_engine_frame(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        self.present_engine_frame_channel(device, encoder, source, target, PresentChannel::Color);
    }

    /// Like [`present_engine_frame`], with an explicit present channel (K-B17).
    pub fn present_engine_frame_channel(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        channel: PresentChannel,
    ) {
        self.present_with_encoding(device, encoder, source, target, channel, false);
    }

    /// Present a premultiplied sRGB-encoded managed output without applying a
    /// second display OETF. Alpha inspection uses the same path as Legacy SDR.
    pub fn present_encoded_engine_frame_channel(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        channel: PresentChannel,
    ) {
        self.present_with_encoding(device, encoder, source, target, channel, true);
    }

    fn present_with_encoding(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        channel: PresentChannel,
        encoded: bool,
    ) {
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present_bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("present_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
        let pipeline = match channel {
            PresentChannel::Color if encoded => &self.pipeline_encoded,
            PresentChannel::Color => &self.pipeline,
            PresentChannel::Alpha => &self.pipeline_alpha,
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color;
    use crate::renderer::align256;

    /// Whether `value` appears in `src` as a **complete numeric token** — the
    /// literal (Debug-formatted, so it carries a decimal point) bounded on both
    /// sides by a non-`[0-9.]` character. Rejects substring false positives
    /// (e.g. `16.0` inside `216.0`, or `0.5` inside `0.55`).
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

    /// 03 §4.4 rule 1 (hardened): every YUV/present shader numeric literal is a
    /// `crate::color` constant, matched as a whole token so drift on either side
    /// fails here. Covers the matrix coefficients, the full limited-range
    /// expansion constants, the BT.709 transfer, and the sRGB OETF.
    #[test]
    fn wgsl_yuv_constants_match_rust() {
        let yuv = [
            color::BT709_CR_R,
            color::BT709_CB_G,
            color::BT709_CR_G,
            color::BT709_CB_B,
            color::BT601_CR_R,
            color::BT601_CB_G,
            color::BT601_CR_G,
            color::BT601_CB_B,
            // Limited-range expansion: 16/219 luma, 16/224 chroma, code max, centre.
            color::LIMITED_LUMA_MIN,
            color::LIMITED_LUMA_SCALE,
            color::LIMITED_CHROMA_MIN,
            color::LIMITED_CHROMA_SCALE,
            color::CODE_MAX,
            color::CHROMA_CENTRE,
            // BT.709 EOTF.
            color::BT709_EOTF_THRESHOLD,
            color::BT709_SLOPE,
            color::BT709_ALPHA,
            color::BT709_BETA,
            color::BT709_GAMMA,
        ];
        for c in yuv {
            assert!(
                contains_token(YUV_CONVERT_SHADER, c),
                "YUV shader missing constant token {c:?}"
            );
        }
        // The present shader is a straight-alpha *linear* passthrough: it
        // unpremultiplies and writes linear values, letting the sRGB present
        // target (and egui's window pass) apply the OETF once in hardware —
        // applying the sRGB OETF in the shader too would double-encode gamma.
        // The legacy linear entry point deliberately carries none of those
        // constants. The separate encoded-input entry point uses the inverse
        // transfer to avoid a second OETF at the attachment boundary.
        let linear_present = PRESENT_SHADER.split("fn srgb_decode").next().unwrap();
        for c in [
            color::SRGB_OETF_THRESHOLD,
            color::SRGB_SLOPE,
            color::SRGB_ALPHA,
            color::SRGB_BETA,
            color::SRGB_GAMMA_INV,
        ] {
            assert!(
                !contains_token(linear_present, c),
                "linear present entry point must NOT apply the sRGB OETF (constant {c:?} found) — \
                 gamma is encoded by the sRGB target, not the shader"
            );
        }
        // The matcher must reject substrings, not just accept whole tokens.
        assert!(!contains_token("value = 216.0;", 16.0));
        assert!(!contains_token("value = 0.55;", 0.5));
        assert!(contains_token("value = 16.0;", 16.0));
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

    fn f32_to_f16(v: f32) -> u16 {
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

    fn readback(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tex: &wgpu::Texture,
        w: u32,
        h: u32,
        bytes_per_px: u32,
    ) -> Vec<u8> {
        let bpr = align256(w * bytes_per_px);
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rb"),
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
        let mut out = Vec::with_capacity((w * h * bytes_per_px) as usize);
        for row in 0..h {
            let start = (row * bpr) as usize;
            out.extend_from_slice(&raw[start..start + (w * bytes_per_px) as usize]);
        }
        drop(raw);
        staging.unmap();
        out
    }

    /// 03 §6: YUV→linear GPU output matches the CPU reference (identical
    /// constants + operation order, §4.4) within 1e-3 linear. Constant planes so
    /// chroma upsampling is a no-op and every pixel has the same expected value.
    #[test]
    fn yuv_gpu_matches_cpu_reference() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping YUV GPU/CPU parity test");
            return;
        };
        let (w, h) = (4u32, 4u32);
        let (yv, cbv, crv, av) = (128u8, 100u8, 150u8, 200u8);
        let planes = YuvPlanes::Yuva444 {
            width: w,
            height: h,
            y: &vec![yv; (w * h) as usize],
            cb: &vec![cbv; (w * h) as usize],
            cr: &vec![crv; (w * h) as usize],
            a: &vec![av; (w * h) as usize],
        };
        let cm = Colorimetry::BT709_LIMITED;
        let tex = convert_yuv_planes_to_working(&device, &queue, &planes, cm);
        let bytes = readback(&device, &queue, &tex, w, h, 8);

        let want = color::yuv_to_working(
            yv as f32 / 255.0,
            cbv as f32 / 255.0,
            crv as f32 / 255.0,
            av as f32 / 255.0,
            cm,
        );
        for px in bytes.chunks_exact(8) {
            let got = [
                f16_to_f32(u16::from_le_bytes([px[0], px[1]])),
                f16_to_f32(u16::from_le_bytes([px[2], px[3]])),
                f16_to_f32(u16::from_le_bytes([px[4], px[5]])),
                f16_to_f32(u16::from_le_bytes([px[6], px[7]])),
            ];
            for i in 0..4 {
                assert!(
                    (got[i] - want[i]).abs() < 1e-3,
                    "channel {i}: gpu {:.5} vs cpu {:.5}",
                    got[i],
                    want[i],
                );
            }
        }
    }

    #[test]
    fn sixteen_bit_yuv_upload_preserves_more_than_eight_bit_levels() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping 16-bit YUV upload test");
            return;
        };
        let width = 512u32;
        let mut y = Vec::with_capacity(width as usize * 2);
        let mut chroma = Vec::with_capacity(width as usize * 2);
        for i in 0..width {
            let code = 4096 + ((56064u32 * i) / (width - 1)) as u16;
            y.extend_from_slice(&code.to_le_bytes());
            chroma.extend_from_slice(&32768u16.to_le_bytes());
        }
        let planes = YuvPlanes::Yuv444P16 {
            width,
            height: 1,
            y: &y,
            cb: &chroma,
            cr: &chroma,
            a: None,
        };
        let texture =
            convert_yuv_planes_to_working(&device, &queue, &planes, Colorimetry::BT709_LIMITED);
        let pixels = readback(&device, &queue, &texture, width, 1, 8);
        let red: Vec<u16> = pixels
            .chunks_exact(8)
            .map(|pixel| u16::from_le_bytes([pixel[0], pixel[1]]))
            .collect();
        assert!(f16_to_f32(red[0]).abs() < 0.01);
        assert!((f16_to_f32(red[511]) - 1.0).abs() < 0.01);
        assert!(
            red.iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 256
        );
        let alpha = 32768u16.to_le_bytes().repeat(width as usize);
        let with_alpha = YuvPlanes::Yuv444P16 {
            width,
            height: 1,
            y: &y,
            cb: &chroma,
            cr: &chroma,
            a: Some(&alpha),
        };
        let texture =
            convert_yuv_planes_to_working(&device, &queue, &with_alpha, Colorimetry::BT709_LIMITED);
        let pixels = readback(&device, &queue, &texture, width, 1, 8);
        let last = &pixels[511 * 8..512 * 8];
        let rgb = f16_to_f32(u16::from_le_bytes([last[0], last[1]]));
        let a = f16_to_f32(u16::from_le_bytes([last[6], last[7]]));
        assert!((a - 0.5).abs() < 0.01);
        assert!((rgb - 0.5).abs() < 0.01);
    }

    #[test]
    fn sixteen_bit_420_upload_preserves_luma_precision_and_legacy_color() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping 16-bit 4:2:0 upload test");
            return;
        };
        let (width, height) = (512u32, 2u32);
        let mut y = Vec::with_capacity((width * height * 2) as usize);
        for _row in 0..height {
            for x in 0..width {
                let code = 4096 + ((56064u32 * x) / (width - 1)) as u16;
                y.extend_from_slice(&code.to_le_bytes());
            }
        }
        let chroma_420 = 32768u16.to_le_bytes().repeat((width / 2) as usize);
        let chroma_444 = 32768u16.to_le_bytes().repeat((width * height) as usize);
        let subsampled = YuvPlanes::Yuv420P16 {
            width,
            height,
            y: &y,
            cb: &chroma_420,
            cr: &chroma_420,
        };
        let full = YuvPlanes::Yuv444P16 {
            width,
            height,
            y: &y,
            cb: &chroma_444,
            cr: &chroma_444,
            a: None,
        };
        let converter = YuvConverter::new(&device);
        let color = Colorimetry::BT709_LIMITED;
        let got = converter.convert(&device, &queue, &subsampled, color);
        let want = converter.convert(&device, &queue, &full, color);
        let got = readback(&device, &queue, &got, width, height, 8);
        let want = readback(&device, &queue, &want, width, height, 8);
        assert_eq!(got, want, "constant-chroma 4:2:0 and 4:4:4 should agree");
        assert!(
            got.chunks_exact(8)
                .take(width as usize)
                .map(|pixel| u16::from_le_bytes([pixel[0], pixel[1]]))
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 256
        );
    }

    #[test]
    fn normalized_u16_to_half_is_monotone_with_exact_endpoints() {
        assert_eq!(unit_f32_to_f16_bits(0.0), 0);
        assert_eq!(unit_f32_to_f16_bits(0.5), 0x3800);
        assert_eq!(unit_f32_to_f16_bits(1.0), 0x3c00);
        let mut previous = 0;
        for code in 0..=u16::MAX {
            let bits = unit_f32_to_f16_bits(code as f32 / 65535.0);
            assert!(bits >= previous, "half conversion reversed at code {code}");
            previous = bits;
        }
    }

    /// A source frame may be rendered directly into a larger evaluator pool
    /// bucket. Its logical pixels must stay unchanged rather than stretching to
    /// fill the padded extent, and the unused margin must remain transparent.
    #[test]
    fn padded_yuv_output_keeps_logical_extent() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping padded YUV output test");
            return;
        };
        let (w, h) = (2u32, 2u32);
        let planes = YuvPlanes::Yuv420 {
            width: w,
            height: h,
            y: &[16, 235, 16, 235],
            cb: &[128],
            cr: &[128],
        };
        let tex = YuvConverter::new(&device).convert_to_size(
            &device,
            &queue,
            &planes,
            Colorimetry::BT709_LIMITED,
            (4, 4),
        );
        assert_eq!((tex.width(), tex.height()), (4, 4));

        let bytes = readback(&device, &queue, &tex, 4, 4, 8);
        let at = |x: usize, y: usize| &bytes[(y * 4 + x) * 8..(y * 4 + x + 1) * 8];
        let logical_white = f16_to_f32(u16::from_le_bytes([at(1, 0)[0], at(1, 0)[1]]));
        assert!(
            logical_white > 0.9,
            "logical YUV pixels must not be scaled away"
        );
        let padding = at(3, 3);
        assert!(
            padding.iter().all(|&byte| byte == 0),
            "padded margin must be transparent"
        );
    }

    /// Reusing a layout's upload textures must preserve every submitted frame:
    /// the second plane write cannot leak back into the first conversion. This
    /// is both the scratch-cache regression test and the queue-ordering guard.
    #[test]
    fn yuv_upload_scratch_reuses_layout_without_overwriting_prior_output() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping YUV scratch reuse test");
            return;
        };
        let converter = YuvConverter::new(&device);
        let black = YuvPlanes::Yuv420 {
            width: 2,
            height: 2,
            y: &[16; 4],
            cb: &[128],
            cr: &[128],
        };
        let white = YuvPlanes::Yuv420 {
            width: 2,
            height: 2,
            y: &[235; 4],
            cb: &[128],
            cr: &[128],
        };
        let first = converter.convert(&device, &queue, &black, Colorimetry::BT709_LIMITED);
        let second = converter.convert(&device, &queue, &white, Colorimetry::BT709_LIMITED);

        assert_eq!(converter.scratch_layout_count(), 1);
        let first_px = readback(&device, &queue, &first, 2, 2, 8);
        let second_px = readback(&device, &queue, &second, 2, 2, 8);
        let first_luma = f16_to_f32(u16::from_le_bytes([first_px[0], first_px[1]]));
        let second_luma = f16_to_f32(u16::from_le_bytes([second_px[0], second_px[1]]));
        assert!(first_luma < 0.01, "first conversion must remain black");
        assert!(second_luma > 0.9, "second conversion must be white");
    }

    /// 03 §6 / §5: the present pass round-trips a known linear value to the
    /// expected sRGB byte within 1 LSB. Alpha 1 so premultiplied == straight.
    #[test]
    fn present_round_trips_linear_to_srgb() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping present round-trip test");
            return;
        };
        let (w, h) = (2u32, 2u32);
        // Working texture: linear 0.5 grey, premultiplied (alpha 1).
        let working = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("present_src"),
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
        let px = [
            f32_to_f16(0.5),
            f32_to_f16(0.5),
            f32_to_f16(0.5),
            f32_to_f16(1.0),
        ];
        let mut data = Vec::new();
        for _ in 0..(w * h) {
            for c in px {
                data.extend_from_slice(&c.to_le_bytes());
            }
        }
        queue.write_texture(
            working.as_image_copy(),
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

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("present_target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let presenter = VideoPresenter::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let mut enc = device.create_command_encoder(&Default::default());
        presenter.present_engine_frame(
            &device,
            &mut enc,
            &working.create_view(&Default::default()),
            &target.create_view(&Default::default()),
        );
        queue.submit([enc.finish()]);

        let bytes = readback(&device, &queue, &target, w, h, 4);
        let want = (color::srgb_oetf(0.5) * 255.0).round() as i32;
        for px in bytes.chunks_exact(4) {
            for &ch in &px[0..3] {
                assert!(
                    (ch as i32 - want).abs() <= 1,
                    "present grey: got {ch}, want {want} (±1 LSB)"
                );
            }
            assert_eq!(px[3], 255, "alpha must be opaque");
        }
    }

    #[test]
    fn present_encoded_sdr_does_not_apply_a_second_oetf() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping encoded present test");
            return;
        };
        let (w, h) = (2_u32, 2_u32);
        let encoded = color::srgb_oetf(0.5);
        let alpha = 0.5_f32;
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("encoded_present_source"),
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
        let mut data = Vec::new();
        for _ in 0..(w * h) {
            for component in [encoded * alpha, encoded * alpha, encoded * alpha, alpha] {
                data.extend_from_slice(&f32_to_f16(component).to_le_bytes());
            }
        }
        queue.write_texture(
            source.as_image_copy(),
            &data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 8),
                rows_per_image: Some(h),
            },
            source.size(),
        );
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("encoded_present_target"),
            size: source.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let presenter = VideoPresenter::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let mut encoder = device.create_command_encoder(&Default::default());
        presenter.present_encoded_engine_frame_channel(
            &device,
            &mut encoder,
            &source.create_view(&Default::default()),
            &target.create_view(&Default::default()),
            PresentChannel::Color,
        );
        queue.submit([encoder.finish()]);
        let bytes = readback(&device, &queue, &target, w, h, 4);
        let want = (encoded * 255.0).round() as i32;
        for pixel in bytes.chunks_exact(4) {
            for channel in &pixel[..3] {
                assert!((i32::from(*channel) - want).abs() <= 2, "{pixel:?}");
            }
            assert!((i32::from(pixel[3]) - 128).abs() <= 1);
        }
    }
}
