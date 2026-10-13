//! Typed native channel signals. Scalar signals are opaque grayscale storage,
//! not coverage mattes: signed/HDR RGB survives split/combine. Alpha is bounded
//! only when constructing a premultiplied image. Scalar storage is bounded to
//! the finite FP16 range; unassociated values outside that range are clipped.
use super::ir::Channel;
use wgpu::util::DeviceExt;

pub(crate) fn extract_pixel(pixel: [f32; 4], channel: Option<Channel>) -> [f32; 4] {
    let rgb = if pixel[3] > 0.0 {
        [
            pixel[0] / pixel[3],
            pixel[1] / pixel[3],
            pixel[2] / pixel[3],
        ]
    } else {
        [0.; 3]
    };
    let value = match channel {
        Some(Channel::R) => rgb[0],
        Some(Channel::G) => rgb[1],
        Some(Channel::B) => rgb[2],
        Some(Channel::A) => pixel[3],
        None => rgb[0] * 0.27222872 + rgb[1] * 0.67408174 + rgb[2] * 0.053689517,
    };
    let value = value.clamp(-65504.0, 65504.0);
    [value, value, value, 1.]
}
pub(crate) fn combine_pixel(values: [f32; 4]) -> [f32; 4] {
    let alpha = values[3].clamp(0., 1.);
    [
        values[0] * alpha,
        values[1] * alpha,
        values[2] * alpha,
        alpha,
    ]
}

const SHADER: &str = r#"
@group(0) @binding(0) var first:texture_2d<f32>;
@group(0) @binding(1) var second:texture_2d<f32>;
@group(0) @binding(2) var third:texture_2d<f32>;
@group(0) @binding(3) var fourth:texture_2d<f32>;
struct Params {info:vec4<u32>}
@group(0) @binding(4) var<uniform> u:Params;
@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32>{
 var p=array<vec2<f32>,6>(vec2(-1.,-1.),vec2(1.,-1.),vec2(1.,1.),vec2(-1.,-1.),vec2(1.,1.),vec2(-1.,1.));return vec4(p[i],0.,1.);
}
@fragment fn fs(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32>{
 if p.x>=f32(u.info.x) || p.y>=f32(u.info.y){return vec4(0.);}
 let xy=vec2<i32>(p.xy); let source=textureLoad(first,xy,0);
 if u.info.z==6u {
  let a=clamp(textureLoad(fourth,xy,0).r,0.,1.);
  return vec4(source.r*a,textureLoad(second,xy,0).r*a,textureLoad(third,xy,0).r*a,a);
 }
 if u.info.z==5u {return vec4(vec3(source.r),1.);}
 var rgb=vec3(0.);if source.a>0.0 {rgb=source.rgb/source.a;}
 var value=source.a;
 if u.info.z<3u {value=rgb[u.info.z];}
 if u.info.z==4u {value=dot(rgb,vec3(0.27222872,0.67408174,0.053689517));}
 return vec4(vec3(clamp(value,-65504.0,65504.0)),1.);
}
"#;

pub(crate) struct ChannelPass {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}
impl ChannelPass {
    pub fn new(device: &wgpu::Device) -> Self {
        let mut entries: Vec<_> = (0..4)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            })
            .collect();
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 4,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("native channel signals"),
            entries: &entries,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native channel signals"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("native channel signals"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        Self { layout, pipeline }
    }
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        inputs: [&wgpu::Texture; 4],
        target: &wgpu::Texture,
        canvas: (u32, u32),
        mode: u32,
    ) {
        let views = inputs.map(|t| t.create_view(&Default::default()));
        let target_view = target.create_view(&Default::default());
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("native channel params"),
            contents: bytemuck::cast_slice(&[canvas.0, canvas.1, mode, 0u32]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut entries: Vec<_> = views
            .iter()
            .enumerate()
            .map(|(i, v)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: wgpu::BindingResource::TextureView(v),
            })
            .collect();
        entries.push(wgpu::BindGroupEntry {
            binding: 4,
            resource: params.as_entire_binding(),
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &entries,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("native channel signals"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
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
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..6, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scalar_signals_are_not_bounded_mattes_and_only_image_alpha_is_clamped() {
        assert_eq!(
            extract_pixel([4., -2., 8., 0.], Some(Channel::R)),
            [0., 0., 0., 1.]
        );
        assert_eq!(
            extract_pixel([-0.5, 2., 0., 0.5], Some(Channel::R)),
            [-1., -1., -1., 1.]
        );
        assert_eq!(
            extract_pixel([0., 0., 0., 0.25], Some(Channel::A)),
            [0.25, 0.25, 0.25, 1.]
        );
        assert_eq!(combine_pixel([-1., 3., 0.5, 2.]), [-1., 3., 0.5, 1.]);
        assert_eq!(combine_pixel([-1., 3., 0.5, -0.25]), [-0., 0., 0., 0.]);
        assert_eq!(
            extract_pixel([65504., 0., 0., 0.5], Some(Channel::R)),
            [65504., 65504., 65504., 1.]
        );
        assert_eq!(
            extract_pixel([1e-7, 0., 0., 1e-7], Some(Channel::R)),
            [1., 1., 1., 1.]
        );
        let y = extract_pixel([-1., 0., 0., 1.], None);
        assert!(y[0] < 0.);
        assert_eq!(y[3], 1.);
    }
}
