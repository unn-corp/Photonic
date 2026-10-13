//! Display-linear caption compositing transfer. This is not an ACES scene transform.
use crate::pipeline::WORKING_FORMAT;
const SHADER: &str = r#"@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var nearest_sampler: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_quad(@builtin(vertex_index) index: u32) -> VOut {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0)
    );
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 0.0)
    );
    var out: VOut;
    out.pos = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

fn transfer(v: f32, video: bool, encode: bool) -> f32 {
    if (video) {
        if (encode) { if (v < 0.018) { return v * 4.5; } return 1.099 * pow(v, 0.45) - 0.099; }
        if (v < 0.081) { return v / 4.5; } return pow((v + 0.099) / 1.099, 1.0 / 0.45);
    }
    if (encode) { if (v <= 0.0031308) { return v * 12.92; } return 1.055 * pow(v, 1.0 / 2.4) - 0.055; }
    if (v <= 0.04045) { return v / 12.92; } return pow((v + 0.055) / 1.055, 2.4);
}
fn convert(in: VOut, video: bool, encode: bool) -> vec4<f32> {
    let p = textureLoad(source, vec2<i32>(in.pos.xy), 0);
    if (p.a <= 0.0) { return vec4<f32>(0.0); }
    let c = p.rgb / p.a;
    return vec4<f32>(vec3<f32>(transfer(c.r,video,encode), transfer(c.g,video,encode), transfer(c.b,video,encode)) * p.a,p.a);
}
@fragment fn fs_srgb_encode(in: VOut) -> @location(0) vec4<f32> { return convert(in,false,true); }
@fragment fn fs_srgb_decode(in: VOut) -> @location(0) vec4<f32> { return convert(in,false,false); }
@fragment fn fs_video_encode(in: VOut) -> @location(0) vec4<f32> { return convert(in,true,true); }
@fragment fn fs_video_decode(in: VOut) -> @location(0) vec4<f32> { return convert(in,true,false); }
"#;
pub struct NativeCaptionTransferPass {
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipelines: [wgpu::RenderPipeline; 4],
}
impl NativeCaptionTransferPass {
    pub fn new(device: &wgpu::Device) -> Self {
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("native_caption_transfer_bind_layout"),
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("native_caption_transfer_pipeline_layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native_caption_transfer_shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let build = |entry| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("native_caption_transfer_pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_quad",
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: entry,
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
        };
        let pipelines = [
            build("fs_srgb_decode"),
            build("fs_srgb_encode"),
            build("fs_video_decode"),
            build("fs_video_encode"),
        ];
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("native_caption_transfer_nearest"),
            ..Default::default()
        });
        Self {
            bind_layout,
            sampler,
            pipelines,
        }
    }

    /// Render into a caller-owned RGBA16F target for graph texture pooling.
    pub fn apply_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        video: bool,
        encode: bool,
    ) -> Result<(), &'static str> {
        if input.size() != output.size()
            || input.format() != WORKING_FORMAT
            || output.format() != WORKING_FORMAT
        {
            return Err("Caption transfer target must match source size and use RGBA16F");
        }
        let source_view = input.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("native_caption_transfer_bind"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("native_caption_transfer_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
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
            pass.set_pipeline(&self.pipelines[2 * video as usize + encode as usize]);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..6, 0..1);
        }
        queue.submit([encoder.finish()]);
        Ok(())
    }
}
