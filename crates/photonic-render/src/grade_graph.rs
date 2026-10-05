//! Typed grading graph image and key mixing. Matte weights are unassociated;
//! applying them preserves the original image's source coverage exactly once.

use photonic_core::timeline::GradeKeyMixMode;
use wgpu::util::DeviceExt;

use crate::pipeline::WORKING_FORMAT;

const SHADER: &str = r#"
@group(0) @binding(0) var top: texture_2d<f32>;
@group(0) @binding(1) var bottom: texture_2d<f32>;
@group(0) @binding(2) var matte: texture_2d<f32>;
struct Params { flags: vec4<u32>, values: vec4<f32> }
@group(0) @binding(3) var<uniform> params: Params;
@vertex fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0));
    return vec4<f32>(positions[i],0.0,1.0);
}
fn key_sample(pixel: vec2<i32>) -> f32 {
    let bounds=vec2<i32>(i32(params.flags.y)-1,i32(params.flags.z)-1);
    return clamp(textureLoad(top,clamp(pixel,vec2<i32>(0),bounds),0).r,0.0,1.0);
}
@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let a = textureLoad(top,pixel,0);
    let b = textureLoad(bottom,pixel,0);
    let mode = params.flags.x;
    if (mode >= 2u && mode <= 5u) {
        let first = clamp(a.r,0.0,1.0);
        let second = clamp(b.r,0.0,1.0);
        var weight = max(first,second);
        if (mode == 3u) { weight = min(first,second); }
        if (mode == 4u) { weight = max(first-second,0.0); }
        if (mode == 5u) { weight = first*second; }
        return vec4<f32>(vec3<f32>(weight),1.0);
    }
    if (mode >= 6u) {
        var value=key_sample(pixel);
        if (mode == 6u) {
            var neighbors: array<f32,9>;
            var index=0u;
            for (var y=-1; y<=1; y=y+1) { for (var x=-1; x<=1; x=x+1) {
                neighbors[index]=key_sample(pixel+vec2<i32>(x,y)); index=index+1u;
            } }
            for (var i=0u;i<8u;i=i+1u) { for (var j=0u;j<8u-i;j=j+1u) {
                if (neighbors[j] > neighbors[j+1u]) { let swap=neighbors[j]; neighbors[j]=neighbors[j+1u]; neighbors[j+1u]=swap; }
            } }
            value=neighbors[4];
        } else if (mode >= 7u && mode <= 10u) {
            let radius=i32(params.flags.w);
            let horizontal=mode==7u || mode==9u;
            let gaussian=mode>=9u;
            let grow=params.values.y>0.0;
            value=select(1.0,0.0,grow || gaussian);
            var normalization=0.0;
            for (var offset=-radius;offset<=radius;offset=offset+1) {
                let delta=select(vec2<i32>(0,offset),vec2<i32>(offset,0),horizontal);
                let neighbor=key_sample(pixel+delta);
                if (gaussian) {
                    let distance=f32(offset);
                    let weight=exp(-(distance*distance)/(2.0*params.values.x*params.values.x));
                    value=value+neighbor*weight; normalization=normalization+weight;
                } else if (grow) { value=max(value,neighbor); } else { value=min(value,neighbor); }
            }
            if (gaussian) { value=value/normalization; }
        } else if (mode == 11u) { value=(value-params.values.x)/(1.0-params.values.x-params.values.y); }
        return vec4<f32>(vec3<f32>(clamp(value,0.0,1.0)),1.0);
    }
    var weight = params.values.x;
    if (mode == 1u) { weight = textureLoad(matte,pixel,0).r; }
    if (b.a <= 0.0) { return vec4<f32>(0.0); }
    var corrected = vec3<f32>(0.0);
    if (a.a > 0.0) { corrected = a.rgb * (b.a / a.a); }
    return vec4<f32>(clamp(mix(b.rgb,corrected,clamp(weight,0.0,1.0)),vec3<f32>(-65504.0),vec3<f32>(65504.0)),b.a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    flags: [u32; 4],
    values: [f32; 4],
}

pub struct GradeGraphMixPass {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl GradeGraphMixPass {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grade_graph_mix_layout"),
            entries: &[
                texture(0),
                texture(1),
                texture(2),
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
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grade_graph_mix_shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grade_graph_mix_pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grade_graph_mix_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: WORKING_FORMAT,
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

    pub fn layer_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        top: &wgpu::Texture,
        bottom: &wgpu::Texture,
        output: &wgpu::Texture,
        opacity: f32,
    ) -> Result<(), &'static str> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err("grade layer strength must be finite in [0,1]");
        }
        self.run(device, queue, top, bottom, bottom, output, 0, opacity)
    }

    pub fn key_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        top: &wgpu::Texture,
        bottom: &wgpu::Texture,
        output: &wgpu::Texture,
        mode: GradeKeyMixMode,
    ) -> Result<(), &'static str> {
        let kind = match mode {
            GradeKeyMixMode::Union => 2,
            GradeKeyMixMode::Intersect => 3,
            GradeKeyMixMode::Subtract => 4,
            GradeKeyMixMode::Multiply => 5,
        };
        self.run(device, queue, top, bottom, bottom, output, kind, 0.0)
    }

    pub fn matte_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        corrected: &wgpu::Texture,
        original: &wgpu::Texture,
        matte: &wgpu::Texture,
        output: &wgpu::Texture,
    ) -> Result<(), &'static str> {
        self.run(device, queue, corrected, original, matte, output, 1, 0.0)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn refine_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        logical: (u32, u32),
        refinement: photonic_core::timeline::GradeMatteRefinement,
    ) -> Result<(), &'static str> {
        refinement.validate()?;
        if input.size() != output.size()
            || input.format() != WORKING_FORMAT
            || output.format() != WORKING_FORMAT
            || logical.0 == 0
            || logical.1 == 0
            || logical.0 > 32768
            || logical.1 > 32768
            || logical.0 > input.width()
            || logical.1 > input.height()
            || std::ptr::eq(input, output)
        {
            return Err("matte refinement requires distinct equal-sized working textures and valid logical dimensions");
        }
        let short = logical.0.min(logical.1) as f32;
        let grow = (refinement.grow.abs() * short).round() as u32;
        let sigma = refinement.blur * short;
        let mut stages = Vec::with_capacity(6);
        if refinement.denoise {
            stages.push((6, 0, [0.0; 4]));
        }
        if grow > 0 {
            for mode in [7, 8] {
                stages.push((mode, grow, [0.0, refinement.grow, 0.0, 0.0]));
            }
        }
        if sigma >= 1e-3 {
            for mode in [9, 10] {
                stages.push((mode, (sigma * 3.0).ceil() as u32, [sigma, 0.0, 0.0, 0.0]));
            }
        }
        if refinement.matte_levels != [0.0; 2] || stages.is_empty() {
            stages.push((
                11,
                0,
                [
                    refinement.matte_levels[0],
                    refinement.matte_levels[1],
                    0.0,
                    0.0,
                ],
            ));
        }
        let scratch: Vec<_> = (0..stages.len().saturating_sub(1).min(2))
            .map(|_| crate::grade_gpu::new_working(device, input.width(), input.height()))
            .collect();
        for (index, (mode, radius, values)) in stages.iter().enumerate() {
            let source = if index == 0 {
                input
            } else {
                &scratch[(index - 1) % 2]
            };
            let target = if index + 1 == stages.len() {
                output
            } else {
                &scratch[index % 2]
            };
            self.run_params(
                device,
                queue,
                source,
                source,
                source,
                target,
                Params {
                    flags: [*mode, logical.0, logical.1, *radius],
                    values: *values,
                },
            )?;
        }
        Ok(())
    }

    fn run(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        top: &wgpu::Texture,
        bottom: &wgpu::Texture,
        matte: &wgpu::Texture,
        output: &wgpu::Texture,
        mode: u32,
        opacity: f32,
    ) -> Result<(), &'static str> {
        if [top, bottom, matte]
            .iter()
            .any(|texture| texture.size() != output.size() || texture.format() != WORKING_FORMAT)
            || output.format() != WORKING_FORMAT
        {
            return Err("grade graph mix requires equal-sized RGBA16F textures");
        }
        let params = Params {
            flags: [mode, 0, 0, 0],
            values: [opacity, 0.0, 0.0, 0.0],
        };
        self.run_params(device, queue, top, bottom, matte, output, params)
    }

    fn run_params(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        top: &wgpu::Texture,
        bottom: &wgpu::Texture,
        matte: &wgpu::Texture,
        output: &wgpu::Texture,
        params: Params,
    ) -> Result<(), &'static str> {
        let views = [top, bottom, matte].map(|texture| texture.create_view(&Default::default()));
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grade_graph_mix_uniform"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grade_graph_mix_bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let target = output.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grade_graph_mix_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
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
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        Ok(())
    }
}

/// Refine unassociated grayscale weights. Geometry uses logical frame pixels,
/// and every spatial sample clamps at the picture edge rather than pool padding.
pub fn refine_matte_cpu(
    pixels: &mut [[f32; 4]],
    width: u32,
    height: u32,
    refinement: photonic_core::timeline::GradeMatteRefinement,
) -> Result<(), &'static str> {
    refinement.validate()?;
    if width == 0
        || height == 0
        || width > 32768
        || height > 32768
        || (width as usize).checked_mul(height as usize) != Some(pixels.len())
        || pixels.iter().any(|pixel| !pixel[0].is_finite())
    {
        return Err("matte refinement requires finite weights and valid logical dimensions");
    }
    let width = width as usize;
    let height = height as usize;
    let mut weights: Vec<f32> = pixels
        .iter()
        .map(|pixel| pixel[0].clamp(0.0, 1.0))
        .collect();
    let sample = |values: &[f32], x: i32, y: i32| {
        values
            [y.clamp(0, height as i32 - 1) as usize * width + x.clamp(0, width as i32 - 1) as usize]
    };
    if refinement.denoise {
        let mut filtered = vec![0.0; weights.len()];
        for y in 0..height {
            for x in 0..width {
                let mut neighbors = [0.0f32; 9];
                let mut index = 0;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        neighbors[index] = sample(&weights, x as i32 + dx, y as i32 + dy);
                        index += 1;
                    }
                }
                neighbors.sort_unstable_by(f32::total_cmp);
                filtered[y * width + x] = neighbors[4];
            }
        }
        weights = filtered;
    }
    let short = width.min(height) as f32;
    let grow = (refinement.grow.abs() * short).round() as i32;
    let sigma = refinement.blur * short;
    for (radius, gaussian) in [(grow, false), ((sigma * 3.0).ceil() as i32, true)] {
        if radius == 0 || (gaussian && sigma < 1e-3) {
            continue;
        }
        let kernel: Vec<f32> = if gaussian {
            (-radius..=radius)
                .map(|offset| (-(offset as f32).powi(2) / (2.0 * sigma * sigma)).exp())
                .collect()
        } else {
            Vec::new()
        };
        let normalization = kernel.iter().sum::<f32>();
        for horizontal in [true, false] {
            let mut filtered = vec![0.0; weights.len()];
            for y in 0..height {
                for x in 0..width {
                    let mut value = if gaussian || refinement.grow > 0.0 {
                        0.0
                    } else {
                        1.0
                    };
                    for offset in -radius..=radius {
                        let neighbor = sample(
                            &weights,
                            x as i32 + if horizontal { offset } else { 0 },
                            y as i32 + if horizontal { 0 } else { offset },
                        );
                        if gaussian {
                            value += neighbor * kernel[(offset + radius) as usize];
                        } else if refinement.grow > 0.0 {
                            value = value.max(neighbor);
                        } else {
                            value = value.min(neighbor);
                        }
                    }
                    filtered[y * width + x] = if gaussian {
                        value / normalization
                    } else {
                        value
                    };
                }
            }
            weights = filtered;
        }
    }
    let [black, white] = refinement.matte_levels;
    for (pixel, weight) in pixels.iter_mut().zip(weights) {
        let weight = ((weight - black) / (1.0 - black - white)).clamp(0.0, 1.0);
        *pixel = [weight, weight, weight, 1.0];
    }
    Ok(())
}

#[cfg(test)]
mod refinement_tests {
    use super::*;
    use photonic_core::timeline::GradeMatteRefinement;

    #[test]
    fn matte_refinement_known_impulses_morphology_and_gaussian() {
        let mut impulse = vec![[0.0, 0.0, 0.0, 1.0]; 10000];
        impulse[5050] = [1.0; 4];
        let mut denoised = impulse.clone();
        refine_matte_cpu(
            &mut denoised,
            100,
            100,
            GradeMatteRefinement {
                denoise: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(denoised.iter().all(|pixel| *pixel == [0.0, 0.0, 0.0, 1.0]));
        let mut grown = impulse.clone();
        refine_matte_cpu(
            &mut grown,
            100,
            100,
            GradeMatteRefinement {
                grow: 0.01,
                ..Default::default()
            },
        )
        .unwrap();
        for (index, pixel) in grown.iter().enumerate() {
            let expected =
                if (49..=51).contains(&(index % 100)) && (49..=51).contains(&(index / 100)) {
                    1.0
                } else {
                    0.0
                };
            assert_eq!(*pixel, [expected, expected, expected, 1.0]);
        }
        refine_matte_cpu(
            &mut grown,
            100,
            100,
            GradeMatteRefinement {
                grow: -0.01,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(grown, impulse);
        for size in [100usize, 200] {
            let center = size / 2;
            let mut image = vec![[0.0, 0.0, 0.0, 1.0]; size * size];
            image[center * size + center] = [1.0; 4];
            refine_matte_cpu(
                &mut image,
                size as u32,
                size as u32,
                GradeMatteRefinement {
                    blur: 0.01,
                    ..Default::default()
                },
            )
            .unwrap();
            let sigma = size as f64 * 0.01;
            let radius = (3.0 * sigma).ceil() as i32;
            let normal = (-radius..=radius)
                .map(|offset| (-(offset as f64).powi(2) / (2.0 * sigma * sigma)).exp())
                .sum::<f64>();
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let expected = (-(f64::from(dx).powi(2) + f64::from(dy).powi(2))
                        / (2.0 * sigma * sigma))
                        .exp()
                        / (normal * normal);
                    let pixel =
                        image[(center as i32 + dy) as usize * size + (center as i32 + dx) as usize];
                    assert!((f64::from(pixel[0]) - expected).abs() < 1e-6);
                    assert_eq!(pixel[3], 1.0);
                }
            }
        }
        let mut constant = vec![[0.25, 0.25, 0.25, 1.0]; 10000];
        refine_matte_cpu(
            &mut constant,
            100,
            100,
            GradeMatteRefinement {
                blur: f32::MIN_POSITIVE,
                matte_levels: [0.2, 0.3],
                ..Default::default()
            },
        )
        .unwrap();
        for pixel in constant {
            assert!((pixel[0] - 0.1).abs() < 1e-6);
            assert_eq!(pixel[3], 1.0);
        }
    }

    #[test]
    fn matte_refinement_invalid_parameters_leave_pixels_unchanged() {
        let original = vec![[0.25, 0.25, 0.25, 1.0]; 4];
        for refinement in [
            GradeMatteRefinement {
                grow: 0.021,
                ..Default::default()
            },
            GradeMatteRefinement {
                blur: f32::NAN,
                ..Default::default()
            },
            GradeMatteRefinement {
                matte_levels: [0.5, 0.5],
                ..Default::default()
            },
        ] {
            let mut pixels = original.clone();
            assert!(refine_matte_cpu(&mut pixels, 2, 2, refinement).is_err());
            assert_eq!(pixels, original);
        }
    }
}
