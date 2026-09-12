//! Reusable GPU downscale and readback for agent inspection. Full/raw callers
//! retain native resolution; display thumbnails shrink before crossing the bus.
use photonic_video::GpuContext;
use std::sync::Arc;
use wgpu::util::DeviceExt;

const SHADER: &str = r#"
struct Dimensions { source: vec2<u32>, output: vec2<u32> };
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> dims: Dimensions;
@vertex fn vs(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
    return vec4<f32>(positions[vertex], 0.0, 1.0);
}
@fragment fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<u32>(position.xy);
    let begin = pixel * dims.source / dims.output;
    let end = min(((pixel + vec2<u32>(1u)) * dims.source + dims.output - vec2<u32>(1u)) / dims.output, dims.source);
    var sum = vec4<f32>(0.0);
    for (var y = begin.y; y < end.y; y = y + 1u) {
        for (var x = begin.x; x < end.x; x = x + 1u) { sum += textureLoad(source, vec2<i32>(i32(x),i32(y)), 0); }
    }
    return sum / f32((end.x-begin.x)*(end.y-begin.y));
}
"#;

struct Downscale {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}
impl Downscale {
    fn new(gpu: &GpuContext) -> Self {
        let device = gpu.device();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("inspection_downscale"),
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
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("inspection_downscale"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("inspection_downscale"),
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
        Self { pipeline, layout }
    }
    fn resize(
        &self,
        gpu: &GpuContext,
        source: &wgpu::Texture,
        size: (u32, u32),
        output: (u32, u32),
    ) -> Arc<wgpu::Texture> {
        let device = gpu.device();
        let texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("inspection_thumbnail"),
            size: wgpu::Extent3d {
                width: output.0,
                height: output.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        }));
        let bytes: Vec<_> = [size.0, size.1, output.0, output.1]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let source_view = source.create_view(&Default::default());
        let target_view = texture.create_view(&Default::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("inspection_thumbnail"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue().submit([encoder.finish()]);
        texture
    }
}

#[derive(Default)]
pub struct FrameReadback {
    downscale: Option<Downscale>,
    staging: Option<wgpu::Buffer>,
    capacity: u64,
}
impl FrameReadback {
    pub fn read(
        &mut self,
        gpu: &GpuContext,
        source: &wgpu::Texture,
        size: (u32, u32),
        output: (u32, u32),
    ) -> Result<Vec<[f32; 4]>, String> {
        if size.0 == 0
            || size.1 == 0
            || output.0 == 0
            || output.1 == 0
            || output.0 > size.0
            || output.1 > size.1
            || size.0 > source.width()
            || size.1 > source.height()
        {
            return Err("Invalid readback dimensions".into());
        }
        let mut resized = None;
        let mut current = size;
        if current != output {
            let downscale = self.downscale.get_or_insert_with(|| Downscale::new(gpu));
            loop {
                // Bound work per pixel even for a 1x1 thumbnail of a 4K frame.
                let next = (
                    output.0.max(current.0.div_ceil(4)),
                    output.1.max(current.1.div_ceil(4)),
                );
                let input = resized.as_deref().unwrap_or(source);
                resized = Some(downscale.resize(gpu, input, current, next));
                current = next;
                if current == output {
                    break;
                }
            }
        }
        let source = resized.as_deref().unwrap_or(source);
        let bytes_per_row = (output.0 * 8).div_ceil(256) * 256;
        let required = u64::from(bytes_per_row) * u64::from(output.1);
        if required > 128 * 1024 * 1024 {
            return Err("Readback exceeds the 128 MiB staging limit".into());
        }
        if required > self.capacity {
            self.staging = Some(gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("inspection_readback"),
                size: required,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }));
            self.capacity = required;
        }
        let buffer = self.staging.as_ref().ok_or("No staging buffer")?;
        let mut encoder = gpu.device().create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            source.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(output.1),
                },
            },
            wgpu::Extent3d {
                width: output.0,
                height: output.1,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue().submit([encoder.finish()]);
        let slice = buffer.slice(..required);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        gpu.device().poll(wgpu::Maintain::Wait);
        receiver
            .recv()
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let raw = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((u64::from(output.0) * u64::from(output.1)) as usize);
        for row in raw.chunks(bytes_per_row as usize).take(output.1 as usize) {
            for pixel in row[..output.0 as usize * 8].chunks_exact(8) {
                pixels.push(std::array::from_fn(|channel| {
                    half_to_float(u16::from_le_bytes([
                        pixel[channel * 2],
                        pixel[channel * 2 + 1],
                    ]))
                }));
            }
        }
        drop(raw);
        buffer.unmap();
        Ok(pixels)
    }
}
fn half_to_float(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let fraction = u32::from(bits & 0x3ff);
    let value = match exponent {
        0 if fraction == 0 => sign,
        0 => {
            let shift = fraction.leading_zeros() - 21;
            sign | ((113 - shift) << 23) | (((fraction << shift) & 0x3ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (fraction << 13),
        _ => sign | ((exponent + 112) << 23) | (fraction << 13),
    };
    f32::from_bits(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn half_conversion_preserves_subnormals_sign_and_special_values() {
        assert_eq!(half_to_float(1), 2.0_f32.powi(-24));
        assert_eq!(half_to_float(0x3ff), 1023.0 * 2.0_f32.powi(-24));
        assert_eq!(half_to_float(0x400), 2.0_f32.powi(-14));
        assert_eq!(half_to_float(0xbc00), -1.0);
        assert_eq!(half_to_float(0x8000).to_bits(), (-0.0_f32).to_bits());
        assert_eq!(half_to_float(0x7c00), f32::INFINITY);
        assert!(half_to_float(0x7e00).is_nan());
    }
    #[test]
    fn gpu_thumbnail_excludes_pool_padding_and_reuses_staging() {
        let Some(gpu) = GpuContext::request_blocking() else {
            assert!(
                std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
                "GPU required"
            );
            return;
        };
        let source = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("readback_padding_fixture"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut bytes = Vec::with_capacity(64 * 64 * 8);
        for y in 0..64 {
            for x in 0..64 {
                let color: [u16; 4] = if x < 7 && y < 3 {
                    [0x3c00, 0, 0, 0x3c00]
                } else {
                    [0, 0, 0x3c00, 0x3c00]
                };
                bytes.extend(color.into_iter().flat_map(u16::to_le_bytes));
            }
        }
        gpu.queue().write_texture(
            source.as_image_copy(),
            &bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(64 * 8),
                rows_per_image: Some(64),
            },
            wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
        );
        let mut readback = FrameReadback::default();
        let first = readback.read(&gpu, &source, (7, 3), (4, 2)).unwrap();
        assert_eq!(first, vec![[1.0, 0.0, 0.0, 1.0]; 8]);
        let capacity = readback.capacity;
        assert_eq!(
            readback.read(&gpu, &source, (7, 3), (1, 1)).unwrap(),
            vec![[1.0, 0.0, 0.0, 1.0]]
        );
        assert_eq!(readback.capacity, capacity);
        assert!(readback.read(&gpu, &source, (7, 3), (8, 3)).is_err());
    }
}
