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

/// Decoded YUV planes ready for GPU upload (03 §3.1). Each plane is 8-bit,
/// row-major, tightly packed (`bytes_per_row == plane_width`).
pub enum YuvPlanes<'a> {
    /// 4:2:0 — full-res luma, half-res (both dims) Cb/Cr, no alpha.
    Yuv420 {
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
}

impl YuvPlanes<'_> {
    fn dims(&self) -> (u32, u32) {
        match *self {
            YuvPlanes::Yuv420 { width, height, .. } | YuvPlanes::Yuva444 { width, height, .. } => {
                (width, height)
            }
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
    _pad: u32,
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
struct Params { matrix_id: u32, range_id: u32, has_alpha: u32, _pad: u32 }
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

    // 1. Range expansion (0..255 code domain), chroma centred on 0.
    var yp: f32;
    var cb: f32;
    var cr: f32;
    if (params.range_id == 0u) {
        yp = (y_raw  * 255.0 - 16.0) / 219.0;
        cb = (cb_raw * 255.0 - 16.0) / 224.0 - 0.5;
        cr = (cr_raw * 255.0 - 16.0) / 224.0 - 0.5;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum YuvUploadLayout {
    Yuv420 { width: u32, height: u32 },
    Yuva444 { width: u32, height: u32 },
}

impl YuvUploadLayout {
    fn from_planes(planes: &YuvPlanes, width: u32, height: u32) -> Self {
        match planes {
            YuvPlanes::Yuv420 { .. } => Self::Yuv420 { width, height },
            YuvPlanes::Yuva444 { .. } => Self::Yuva444 { width, height },
        }
    }

    fn has_alpha(self) -> u32 {
        match self {
            Self::Yuv420 { .. } => 0,
            Self::Yuva444 { .. } => 1,
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
            | YuvUploadLayout::Yuva444 { width, height } => (width, height),
        };
        let (cw, ch) = match layout {
            YuvUploadLayout::Yuv420 { .. } => (w.div_ceil(2), h.div_ceil(2)),
            YuvUploadLayout::Yuva444 { .. } => (w, h),
        };
        let y = r8_texture(device, w, h, "yuv_plane_y");
        let cb = r8_texture(device, cw, ch, "yuv_plane_cb");
        let cr = r8_texture(device, cw, ch, "yuv_plane_cr");
        // The non-alpha path still binds this texture, but never samples it.
        let a = match layout {
            YuvUploadLayout::Yuv420 { .. } => r8_texture(device, 1, 1, "yuv_plane_a_dummy"),
            YuvUploadLayout::Yuva444 { .. } => r8_texture(device, w, h, "yuv_plane_a"),
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
            last_used,
        }
    }
}

/// Persistent YUV→working conversion resources for one wgpu device.
pub struct YuvConverter {
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
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
        let (w, h) = planes.dims();
        let (w, h) = (w.max(1), h.max(1));
        let (out_w, out_h) = (output_size.0.max(w), output_size.1.max(h));

        let layout = YuvUploadLayout::from_planes(planes, w, h);
        let has_alpha = layout.has_alpha();

        let params = YuvParams {
            matrix_id: match colorimetry.matrix {
                Matrix::Bt709 => 0,
                Matrix::Bt601 => 1,
            },
            range_id: match colorimetry.range {
                Range::Limited => 0,
                Range::Full => 1,
            },
            has_alpha,
            _pad: 0,
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
            YuvPlanes::Yuva444 { y, cb, cr, a, .. } => {
                write_r8_plane(queue, &scratch.y, w, h, y);
                write_r8_plane(queue, &scratch.cb, w, h, cb);
                write_r8_plane(queue, &scratch.cr, w, h, cr);
                write_r8_plane(queue, &scratch.a, w, h, a);
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
            pass.set_pipeline(&self.pipeline);
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
        // So the present shader deliberately carries NONE of the sRGB OETF
        // constants (that transform lives at the format boundary, not here).
        for c in [
            color::SRGB_OETF_THRESHOLD,
            color::SRGB_SLOPE,
            color::SRGB_ALPHA,
            color::SRGB_BETA,
            color::SRGB_GAMMA_INV,
        ] {
            assert!(
                !contains_token(PRESENT_SHADER, c),
                "present shader must NOT apply the sRGB OETF (constant {c:?} found) — \
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
}
