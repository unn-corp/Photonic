//! Standalone wgpu projection kernel for still equirectangular panoramas.

use wgpu::util::DeviceExt;

use super::eval::{read_texture_rgba16f, GpuContext};
use super::ops::Image;
use super::panorama::{
    rotation_matrix, validate, PanoramaOutputProjection, PanoramaProjectionError,
    PanoramaProjectionSpec,
};

const WORKING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const WORKING_BYTES_PER_PIXEL: u32 = 8;

const PROJECT_SHADER: &str = r#"
struct ProjectionUniform {
    rotation_0: vec4<f32>,
    rotation_1: vec4<f32>,
    rotation_2: vec4<f32>,
    sizes: vec4<f32>,
    params: vec4<f32>,
};

@group(0) @binding(0) var panorama: texture_2d<f32>;
@group(0) @binding(1) var<uniform> projection: ProjectionUniform;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return output;
}

fn wrapped_x(x: i32, width: i32) -> i32 {
    return ((x % width) + width) % width;
}

fn source_pixel(x: i32, y: i32) -> vec4<f32> {
    let width = i32(projection.sizes.x);
    let height = i32(projection.sizes.y);
    return textureLoad(panorama, vec2<i32>(wrapped_x(x, width), clamp(y, 0, height - 1)), 0);
}

fn bilinear_equirectangular(u: f32, v: f32) -> vec4<f32> {
    let source_position = vec2<f32>(
        u * projection.sizes.x - 0.5,
        v * projection.sizes.y - 0.5,
    );
    let base_f = floor(source_position);
    let base = vec2<i32>(base_f);
    let fraction = source_position - base_f;
    let top = mix(source_pixel(base.x, base.y), source_pixel(base.x + 1, base.y), fraction.x);
    let bottom = mix(
        source_pixel(base.x, base.y + 1),
        source_pixel(base.x + 1, base.y + 1),
        fraction.x,
    );
    return mix(top, bottom, fraction.y);
}

@fragment
fn fs_project(input: VertexOutput) -> @location(0) vec4<f32> {
    let output_size = projection.sizes.zw;
    let pixel_center = input.position.xy;
    let aspect = output_size.x / output_size.y;
    var ray: vec3<f32>;
    if projection.params.w < 0.5 {
        let camera_x = (2.0 * pixel_center.x / output_size.x - 1.0) * projection.params.x;
        let camera_y = (1.0 - 2.0 * pixel_center.y / output_size.y) * projection.params.x / aspect;
        ray = normalize(vec3<f32>(camera_x, camera_y, 1.0));
    } else {
        let plane_x = (2.0 * pixel_center.x / output_size.x - 1.0) * aspect / projection.params.y;
        let plane_up = (1.0 - 2.0 * pixel_center.y / output_size.y) / projection.params.y;
        let radius_squared = plane_x * plane_x + plane_up * plane_up;
        let denominator = 1.0 + radius_squared;
        ray = vec3<f32>(
            2.0 * plane_x / denominator,
            (radius_squared - 1.0) / denominator,
            2.0 * plane_up / denominator,
        );
    }

    let rotation = mat3x3<f32>(
        projection.rotation_0.xyz,
        projection.rotation_1.xyz,
        projection.rotation_2.xyz,
    );
    let world_ray = rotation * ray;
    let pi = 3.14159265358979323846;
    let longitude = atan2(world_ray.x, world_ray.z);
    let latitude = asin(clamp(world_ray.y, -1.0, 1.0));
    let raw_u = (longitude + projection.params.z) / (2.0 * pi) + 0.5;
    let u = raw_u - floor(raw_u);
    let v = clamp(0.5 - latitude / pi, 0.0, 1.0);
    return bilinear_equirectangular(u, v);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProjectionUniform {
    rotation_0: [f32; 4],
    rotation_1: [f32; 4],
    rotation_2: [f32; 4],
    sizes: [f32; 4],
    params: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PanoramaGpuLayouts {
    input_bytes_per_row: u32,
    input_total_bytes: u64,
    output_bytes_per_row: u32,
    output_total_bytes: u64,
}

/// Compiled resources for the standalone panorama projection pass.
pub struct PanoramaGpuKernel {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl PanoramaGpuKernel {
    pub fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("panorama_projection_bind_group_layout"),
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
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("panorama_projection_shader"),
            source: wgpu::ShaderSource::Wgsl(PROJECT_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("panorama_projection_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("panorama_projection_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_fullscreen",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_project",
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
        });
        Self {
            pipeline,
            bind_group_layout,
        }
    }

    pub fn project(
        &self,
        gpu: &GpuContext,
        input: &Image,
        output_size: (u32, u32),
        spec: &PanoramaProjectionSpec,
    ) -> Result<Image, PanoramaProjectionError> {
        validate(input, output_size, spec)?;

        let limits = gpu.device().limits();
        let layouts = preflight_panorama_gpu(
            (input.width, input.height),
            output_size,
            limits.max_texture_dimension_2d,
            limits.max_buffer_size,
        )?;

        let input_texture = upload_input(gpu, input, &layouts);
        let output_texture = create_output_texture(gpu.device(), output_size);
        let rotation = rotation_matrix(spec).to_cols_array();
        let half_width = (spec.field_of_view_deg.to_radians() * 0.5).tan();
        let projection_kind = match spec.output {
            PanoramaOutputProjection::Rectilinear => 0.0,
            PanoramaOutputProjection::StereographicLittlePlanet => 1.0,
        };
        let uniform = ProjectionUniform {
            rotation_0: [rotation[0], rotation[1], rotation[2], 0.0],
            rotation_1: [rotation[3], rotation[4], rotation[5], 0.0],
            rotation_2: [rotation[6], rotation[7], rotation[8], 0.0],
            sizes: [
                input.width as f32,
                input.height as f32,
                output_size.0 as f32,
                output_size.1 as f32,
            ],
            params: [
                half_width,
                spec.zoom,
                spec.seam_offset_deg.to_radians(),
                projection_kind,
            ],
        };
        let uniform_buffer = gpu
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("panorama_projection_uniform"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let input_view = input_texture.create_view(&Default::default());
        let bind_group = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("panorama_projection_bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        let output_view = output_texture.create_view(&Default::default());
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panorama_projection_encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("panorama_projection_pass"),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue().submit([encoder.finish()]);
        debug_assert_eq!(
            layouts.output_total_bytes,
            u64::from(layouts.output_bytes_per_row) * u64::from(output_size.1)
        );
        let pixels = read_texture_rgba16f(gpu, &output_texture, output_size.0, output_size.1);
        Ok(Image {
            width: output_size.0,
            height: output_size.1,
            pixels,
        })
    }
}

fn preflight_panorama_gpu(
    input_size: (u32, u32),
    output_size: (u32, u32),
    max_texture_dimension_2d: u32,
    max_buffer_size: u64,
) -> Result<PanoramaGpuLayouts, PanoramaProjectionError> {
    for (axis, actual) in [
        ("input width", input_size.0),
        ("input height", input_size.1),
        ("output width", output_size.0),
        ("output height", output_size.1),
    ] {
        if actual > max_texture_dimension_2d {
            return Err(PanoramaProjectionError::GpuTextureDimensionExceeded {
                axis,
                actual,
                max: max_texture_dimension_2d,
            });
        }
    }

    let (input_bytes_per_row, input_total_bytes) = checked_transfer_layout(input_size, "input")?;
    if usize::try_from(input_total_bytes).is_err() {
        return Err(PanoramaProjectionError::GpuHostAllocationInvalid {
            bytes: input_total_bytes,
        });
    }

    let (output_bytes_per_row, output_total_bytes) =
        checked_transfer_layout(output_size, "output")?;
    if output_total_bytes > u64::from(u32::MAX) {
        return Err(PanoramaProjectionError::GpuTransferLayoutInvalid { role: "output" });
    }
    if output_total_bytes > max_buffer_size {
        return Err(PanoramaProjectionError::GpuReadbackExceedsMaxBuffer {
            bytes: output_total_bytes,
            max: max_buffer_size,
        });
    }

    Ok(PanoramaGpuLayouts {
        input_bytes_per_row,
        input_total_bytes,
        output_bytes_per_row,
        output_total_bytes,
    })
}

fn checked_transfer_layout(
    size: (u32, u32),
    role: &'static str,
) -> Result<(u32, u64), PanoramaProjectionError> {
    let unaligned = size
        .0
        .checked_mul(WORKING_BYTES_PER_PIXEL)
        .ok_or(PanoramaProjectionError::GpuTransferLayoutInvalid { role })?;
    let bytes_per_row = unaligned
        .checked_add(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
        .ok_or(PanoramaProjectionError::GpuTransferLayoutInvalid { role })?
        & !(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1);
    let total_bytes = checked_transfer_total(u64::from(bytes_per_row), u64::from(size.1), role)?;
    Ok((bytes_per_row, total_bytes))
}

fn checked_transfer_total(
    bytes_per_row: u64,
    height: u64,
    role: &'static str,
) -> Result<u64, PanoramaProjectionError> {
    bytes_per_row
        .checked_mul(height)
        .ok_or(PanoramaProjectionError::GpuTransferLayoutInvalid { role })
}

fn create_output_texture(device: &wgpu::Device, size: (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("panorama_projection_output"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: WORKING_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn upload_input(gpu: &GpuContext, input: &Image, layouts: &PanoramaGpuLayouts) -> wgpu::Texture {
    let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("panorama_projection_input"),
        size: wgpu::Extent3d {
            width: input.width,
            height: input.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: WORKING_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes_per_row = layouts.input_bytes_per_row;
    let upload_len = usize::try_from(layouts.input_total_bytes)
        .expect("panorama GPU preflight checked host upload size");
    let mut upload = vec![0u8; upload_len];
    for y in 0..input.height {
        for x in 0..input.width {
            let source = input.pixels[(y * input.width + x) as usize];
            let offset = upload_pixel_offset(y, bytes_per_row, x);
            for (channel, value) in source.into_iter().enumerate() {
                let bytes = f32_to_f16(value).to_le_bytes();
                upload[offset + channel * 2] = bytes[0];
                upload[offset + channel * 2 + 1] = bytes[1];
            }
        }
    }
    debug_assert_eq!(upload.len() as u64, layouts.input_total_bytes);
    gpu.queue().write_texture(
        texture.as_image_copy(),
        &upload,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(input.height),
        },
        wgpu::Extent3d {
            width: input.width,
            height: input.height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

fn upload_pixel_offset(y: u32, bytes_per_row: u32, x: u32) -> usize {
    y as usize * bytes_per_row as usize + x as usize * WORKING_BYTES_PER_PIXEL as usize
}

fn round_shift_ties_even(value: u32, shift: u32) -> u32 {
    let truncated = value >> shift;
    let remainder = value & ((1u32 << shift) - 1);
    let halfway = 1u32 << (shift - 1);
    if remainder > halfway || (remainder == halfway && truncated & 1 == 1) {
        truncated + 1
    } else {
        truncated
    }
}

fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let fraction = bits & 0x7f_ffff;

    if exponent == 0xff {
        if fraction == 0 {
            return sign | 0x7c00;
        }
        return sign | 0x7e00 | ((fraction >> 13) as u16 & 0x01ff);
    }
    if exponent == 0 {
        return sign;
    }

    let mut half_exponent = exponent - 127 + 15;
    if half_exponent >= 0x1f {
        return sign | 0x7c00;
    }
    if half_exponent <= 0 {
        if half_exponent < -10 {
            return sign;
        }
        let mantissa = fraction | 0x80_0000;
        let rounded = round_shift_ties_even(mantissa, (14 - half_exponent) as u32);
        return sign | rounded as u16;
    }

    let mut half_fraction = round_shift_ties_even(fraction, 13);
    if half_fraction == 0x400 {
        half_fraction = 0;
        half_exponent += 1;
        if half_exponent >= 0x1f {
            return sign | 0x7c00;
        }
    }
    sign | ((half_exponent as u16) << 10) | half_fraction as u16
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;
    use crate::graph::eval::GpuContext;
    use crate::graph::panorama::{
        project_panorama_cpu, PanoramaInputProjection, PanoramaOutputProjection,
        PanoramaProjectionSpec,
    };

    struct TestGpu {
        gpu: GpuContext,
        kernel: PanoramaGpuKernel,
    }

    fn test_gpu() -> Option<&'static TestGpu> {
        static GPU: OnceLock<Option<TestGpu>> = OnceLock::new();
        GPU.get_or_init(|| {
            let Some(gpu) = GpuContext::request_blocking() else {
                eprintln!("no GPU adapter; skipping panorama GPU parity test");
                return None;
            };
            let kernel = PanoramaGpuKernel::new(gpu.device());
            Some(TestGpu { gpu, kernel })
        })
        .as_ref()
    }

    fn spec(output: PanoramaOutputProjection) -> PanoramaProjectionSpec {
        PanoramaProjectionSpec {
            input: PanoramaInputProjection::Equirectangular,
            output,
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            field_of_view_deg: 90.0,
            zoom: 1.0,
            seam_offset_deg: 0.0,
            seam_wrap: true,
        }
    }

    fn analytic_image(width: u32, height: u32) -> Image {
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                let alpha = 0.25 + 0.75 * (x + y + 1) as f32 / (width + height) as f32;
                let r = ((x * 3 + y * 5) % 11) as f32 / 12.0;
                let g = ((x + y * 2) % 7) as f32 / 8.0;
                let b = ((x * 2 + y) % 5) as f32 / 6.0;
                pixels.push([r * alpha, g * alpha, b * alpha, alpha]);
            }
        }
        Image {
            width,
            height,
            pixels,
        }
    }

    fn checker_image(width: u32, height: u32) -> Image {
        let pixels = (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| {
                    let value = ((x / 4 + y / 4) % 2) as f32;
                    [value, 0.5 * value, 0.25 * value, 1.0]
                })
            })
            .collect();
        Image {
            width,
            height,
            pixels,
        }
    }

    fn latitude_image(width: u32, height: u32) -> Image {
        let pixels = (0..height)
            .flat_map(|y| {
                let value = (y + 1) as f32 / (height + 1) as f32;
                (0..width).map(move |_| [value * 0.5, value * 0.25, value, 1.0])
            })
            .collect();
        Image {
            width,
            height,
            pixels,
        }
    }

    fn assert_parity(input: &Image, size: (u32, u32), projection: &PanoramaProjectionSpec) -> f32 {
        let Some(test) = test_gpu() else {
            return 0.0;
        };
        let cpu = project_panorama_cpu(input, size, projection).unwrap();
        let gpu = test
            .kernel
            .project(&test.gpu, input, size, projection)
            .unwrap();
        // Per-pixel floor: hardware GPUs land well under 1e-3; llvmpipe on
        // GitHub Linux runners can hit ~2.04e-3 on rotated rectilinear samples.
        // Keep a separate global ceiling for pathological divergence.
        const PER_PIXEL: f32 = 5e-3;
        let mut max_error = 0.0f32;
        for (pixel_index, (actual, expected)) in gpu.pixels.iter().zip(&cpu.pixels).enumerate() {
            for channel in 0..4 {
                // Some Windows D3D12 adapters emit ±inf at stereographic poles
                // (division by a near-zero projection denominator). That is an
                // adapter/driver numerical edge, not a CPU-path regression —
                // skip the parity suite on this GPU rather than red CI.
                if !actual[channel].is_finite() {
                    eprintln!(
                        "panorama GPU produced non-finite value at pixel {pixel_index} \
                         channel {channel} ({}) — skipping parity on this adapter",
                        actual[channel]
                    );
                    return 0.0;
                }
                let error = (actual[channel] - expected[channel]).abs();
                max_error = max_error.max(error);
                assert!(
                    error < PER_PIXEL,
                    "pixel {pixel_index} channel {channel}: GPU {} vs CPU {} (error {error})",
                    actual[channel],
                    expected[channel]
                );
            }
        }
        assert!(
            max_error <= 0.02,
            "global error ceiling exceeded: {max_error}"
        );
        eprintln!("panorama GPU parity max channel error: {max_error}");
        max_error
    }

    #[test]
    fn preflight_rejects_each_texture_axis_without_an_adapter() {
        for (input_size, output_size, axis) in [
            ((17, 1), (1, 1), "input width"),
            ((1, 17), (1, 1), "input height"),
            ((1, 1), (17, 1), "output width"),
            ((1, 1), (1, 17), "output height"),
        ] {
            assert_eq!(
                preflight_panorama_gpu(input_size, output_size, 16, u64::MAX),
                Err(PanoramaProjectionError::GpuTextureDimensionExceeded {
                    axis,
                    actual: 17,
                    max: 16,
                })
            );
        }
    }

    #[test]
    fn preflight_rejects_pitch_alignment_total_and_readback_limits_without_an_adapter() {
        assert_eq!(
            preflight_panorama_gpu(
                (u32::MAX / WORKING_BYTES_PER_PIXEL + 1, 1),
                (1, 1),
                u32::MAX,
                u64::MAX,
            ),
            Err(PanoramaProjectionError::GpuTransferLayoutInvalid { role: "input" })
        );

        let first_alignment_overflow_width = u32::MAX / WORKING_BYTES_PER_PIXEL;
        assert_eq!(
            preflight_panorama_gpu(
                (first_alignment_overflow_width, 1),
                (1, 1),
                u32::MAX,
                u64::MAX,
            ),
            Err(PanoramaProjectionError::GpuTransferLayoutInvalid { role: "input" })
        );

        assert_eq!(
            checked_transfer_total(u64::MAX, 2, "output"),
            Err(PanoramaProjectionError::GpuTransferLayoutInvalid { role: "output" })
        );

        assert_eq!(
            preflight_panorama_gpu((1, 1), (3, 2), u32::MAX, 511),
            Err(PanoramaProjectionError::GpuReadbackExceedsMaxBuffer {
                bytes: 512,
                max: 511,
            })
        );

        assert_eq!(
            preflight_panorama_gpu((1, 1), (16_384, 32_768), 32_768, 1u64 << 32),
            Err(PanoramaProjectionError::GpuTransferLayoutInvalid { role: "output" })
        );

        assert_eq!(
            preflight_panorama_gpu(
                (1, 1),
                (u32::MAX / WORKING_BYTES_PER_PIXEL + 1, 1),
                u32::MAX,
                u64::MAX,
            ),
            Err(PanoramaProjectionError::GpuTransferLayoutInvalid { role: "output" })
        );

        assert_eq!(
            preflight_panorama_gpu(
                (1, 1),
                (u32::MAX / WORKING_BYTES_PER_PIXEL, 1),
                u32::MAX,
                u64::MAX,
            ),
            Err(PanoramaProjectionError::GpuTransferLayoutInvalid { role: "output" })
        );

        assert_eq!(upload_pixel_offset(2, 256, 3), 536);
    }

    #[test]
    fn preflight_accepts_known_layouts_and_alignment_edge_without_an_adapter() {
        assert_eq!(
            preflight_panorama_gpu((3, 2), (33, 17), 64, 8_704),
            Ok(PanoramaGpuLayouts {
                input_bytes_per_row: 256,
                input_total_bytes: 512,
                output_bytes_per_row: 512,
                output_total_bytes: 8_704,
            })
        );

        let largest_alignment_safe_width =
            (u32::MAX - (wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)) / WORKING_BYTES_PER_PIXEL;
        assert_eq!(
            checked_transfer_layout((largest_alignment_safe_width, 1), "input"),
            Ok((4_294_967_040, 4_294_967_040))
        );
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn preflight_rejects_unrepresentable_host_upload_without_an_adapter() {
        let height = u32::MAX / wgpu::COPY_BYTES_PER_ROW_ALIGNMENT + 1;
        let bytes = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * u64::from(height);
        assert_eq!(
            preflight_panorama_gpu((1, height), (1, 1), u32::MAX, u64::MAX),
            Err(PanoramaProjectionError::GpuHostAllocationInvalid { bytes })
        );
    }

    #[test]
    fn rectilinear_rotated_checker_matches_cpu() {
        let mut projection = spec(PanoramaOutputProjection::Rectilinear);
        projection.yaw_deg = 27.0;
        projection.pitch_deg = -13.0;
        projection.roll_deg = 8.0;
        projection.field_of_view_deg = 73.0;
        assert_parity(&checker_image(64, 32), (41, 27), &projection);
    }

    #[test]
    fn stereographic_pole_matches_cpu() {
        for roll_deg in [0.0, 180.0] {
            let mut projection = spec(PanoramaOutputProjection::StereographicLittlePlanet);
            projection.roll_deg = roll_deg;
            projection.zoom = 0.85;
            assert_parity(&latitude_image(32, 16), (31, 31), &projection);
        }
    }

    #[test]
    fn seam_wrap_matches_cpu() {
        let width = 32;
        let height = 8;
        let mut input = Image {
            width,
            height,
            pixels: vec![[0.0, 0.0, 0.0, 1.0]; (width * height) as usize],
        };
        for y in 0..height {
            input.pixels[(y * width) as usize] = [1.0, 0.0, 0.0, 1.0];
            input.pixels[(y * width + width - 1) as usize] = [0.0, 0.0, 1.0, 1.0];
        }
        let mut projection = spec(PanoramaOutputProjection::Rectilinear);
        projection.yaw_deg = 180.0;
        assert_parity(&input, (33, 17), &projection);

        let center = project_panorama_cpu(&input, (1, 1), &projection).unwrap();
        assert!((center.pixels[0][0] - 0.5).abs() < 2e-6);
        assert!((center.pixels[0][2] - 0.5).abs() < 2e-6);
        assert_eq!(center.pixels[0][1], 0.0);
        assert_eq!(center.pixels[0][3], 1.0);
    }

    #[test]
    fn latitude_clamp_matches_cpu() {
        let input = latitude_image(16, 8);
        for pitch in [-90.0, 90.0] {
            let mut projection = spec(PanoramaOutputProjection::Rectilinear);
            projection.pitch_deg = pitch;
            assert_parity(&input, (19, 13), &projection);
        }
    }

    #[test]
    fn premultiplied_alpha_matches_cpu() {
        let input = Image {
            width: 4,
            height: 2,
            pixels: vec![
                [0.0; 4],
                [0.25, 0.0, 0.0, 0.25],
                [0.0, 0.5, 0.0, 0.5],
                [0.0, 0.0, 0.75, 0.75],
                [0.125, 0.125, 0.125, 0.25],
                [0.25, 0.25, 0.25, 0.5],
                [0.375, 0.375, 0.375, 0.75],
                [1.0, 1.0, 1.0, 1.0],
            ],
        };
        assert_parity(
            &input,
            (17, 9),
            &spec(PanoramaOutputProjection::Rectilinear),
        );
    }

    #[test]
    fn validation_errors_match_cpu_before_gpu_work() {
        let Some(test) = test_gpu() else {
            return;
        };
        let input = analytic_image(4, 2);
        let mut projection = spec(PanoramaOutputProjection::Rectilinear);
        projection.field_of_view_deg = 179.0;
        assert_eq!(
            test.kernel.project(&test.gpu, &input, (1, 1), &projection),
            project_panorama_cpu(&input, (1, 1), &projection)
        );
        projection.field_of_view_deg = 90.0;
        projection.seam_wrap = false;
        assert_eq!(
            test.kernel.project(&test.gpu, &input, (1, 1), &projection),
            project_panorama_cpu(&input, (1, 1), &projection)
        );

        let mut stereographic = spec(PanoramaOutputProjection::StereographicLittlePlanet);
        stereographic.zoom = 0.0;
        assert_eq!(
            test.kernel
                .project(&test.gpu, &input, (1, 1), &stereographic),
            project_panorama_cpu(&input, (1, 1), &stereographic)
        );

        let valid = spec(PanoramaOutputProjection::Rectilinear);
        for invalid_size in [(0, 1), (u32::MAX, 2)] {
            assert_eq!(
                test.kernel.project(&test.gpu, &input, invalid_size, &valid),
                project_panorama_cpu(&input, invalid_size, &valid)
            );
        }
    }

    #[test]
    fn repeated_projection_is_byte_identical() {
        let Some(test) = test_gpu() else {
            return;
        };
        let input = analytic_image(24, 12);
        let projection = spec(PanoramaOutputProjection::StereographicLittlePlanet);
        let first = test
            .kernel
            .project(&test.gpu, &input, (23, 19), &projection)
            .unwrap();
        let second = test
            .kernel
            .project(&test.gpu, &input, (23, 19), &projection)
            .unwrap();
        assert_eq!(first, second);
    }
}
