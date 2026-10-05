// SPDX-License-Identifier: Apache-2.0
// Copyright Contributors to the ACES Project.
//! Isolated GPU parity implementation of the scalar ACES 2-style SDR output.
//! Managed preview/export remain gated until image-level qualification.

use super::Aces2SdrOutput;
use photonic_render::pipeline::WORKING_FORMAT;
use wgpu::util::DeviceExt;

pub struct NativeAces2SdrPass {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    video_pipeline: wgpu::RenderPipeline,
    params: wgpu::Buffer,
}

impl NativeAces2SdrPass {
    pub fn new(device: &wgpu::Device) -> Result<Self, &'static str> {
        let output = Aces2SdrOutput::shared()?;
        let rows = output.gpu_rows();
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("native_aces2_sdr_params"),
            contents: bytemuck::cast_slice(&rows),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("native_aces2_sdr_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("native_aces2_sdr_pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native_aces2_sdr_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu.wgsl").into()),
        });
        let make_pipeline = |entry_point| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("native_aces2_sdr_pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vertex",
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point,
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
        let pipeline = make_pipeline("fragment");
        let video_pipeline = make_pipeline("video_fragment");
        Ok(Self {
            layout,
            pipeline,
            video_pipeline,
            params,
        })
    }

    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
    ) -> wgpu::Texture {
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("native_aces2_sdr_output"),
            size: input.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.apply_into(device, queue, input, &output);
        output
    }

    /// Render into a caller-owned target so a frame graph can reuse pooled
    /// textures. The source and target must be distinct and have equal extents;
    /// the target must be an RGBA16F render attachment.
    pub fn apply_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
    ) {
        let mut encoder = device.create_command_encoder(&Default::default());
        self.encode_into(device, &mut encoder, input, output);
        queue.submit([encoder.finish()]);
    }

    pub fn apply_video_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
    ) {
        let mut encoder = device.create_command_encoder(&Default::default());
        self.encode_with_pipeline(device, &mut encoder, input, output, &self.video_pipeline);
        queue.submit([encoder.finish()]);
    }

    /// Encode the transform into an existing command buffer. This lets the
    /// managed frame graph batch the output transform with adjacent passes.
    pub fn encode_into(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
    ) {
        self.encode_with_pipeline(device, encoder, input, output, &self.pipeline);
    }

    fn encode_with_pipeline(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        pipeline: &wgpu::RenderPipeline,
    ) {
        assert_eq!(input.size(), output.size(), "ACES 2 output size mismatch");
        assert_eq!(
            output.format(),
            WORKING_FORMAT,
            "ACES 2 output format mismatch"
        );
        let input_view = input.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("native_aces2_sdr_bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("native_aces2_sdr_pass"),
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}
