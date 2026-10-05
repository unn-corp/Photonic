//! Isolated ACEScg/AP1 linear ↔ ACEScct grading transfer on the GPU.
//!
//! Managed sequence rendering selects these passes explicitly; Legacy SDR does not.
//! It preserves premultiplied alpha and extended signed values. The constants
//! follow https://docs.acescentral.com/encodings/acescct/ .

use crate::pipeline::WORKING_FORMAT;

const SHADER: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
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

// Make the RGBA16F storage boundary deterministic across GPU backends.
fn working_rgba(rgb: vec3<f32>, alpha: f32) -> vec4<f32> {
    return vec4<f32>(clamp(rgb, vec3<f32>(-65504.0), vec3<f32>(65504.0)), alpha);
}

fn to_cct(value: f32) -> f32 {
    if (value <= 0.0078125) {
        return value * 10.5402377416545 + 0.0729055341958355;
    }
    return (log2(value) + 9.72) / 17.52;
}

fn to_linear(value: f32) -> f32 {
    if (value <= 0.155251141552511) {
        return (value - 0.0729055341958355) / 10.5402377416545;
    }
    let maximum = (log2(65504.0) + 9.72) / 17.52;
    if (value >= maximum) { return 65504.0; }
    return exp2(value * 17.52 - 9.72);
}

@fragment
fn fs_encode(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureSample(source, nearest_sampler, in.uv);
    if (sample.a <= 0.0) { return vec4<f32>(0.0); }
    let straight = sample.rgb / sample.a;
    return working_rgba(
        vec3<f32>(to_cct(straight.r), to_cct(straight.g), to_cct(straight.b)) * sample.a, sample.a);
}

@fragment
fn fs_decode(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureSample(source, nearest_sampler, in.uv);
    if (sample.a <= 0.0) { return vec4<f32>(0.0); }
    let straight = sample.rgb / sample.a;
    return working_rgba(
        vec3<f32>(to_linear(straight.r), to_linear(straight.g), to_linear(straight.b)) * sample.a, sample.a);
}

struct ExposureParams { values: vec4<f32>, slope: vec4<f32>, offset: vec4<f32>, power: vec4<f32> }
@group(1) @binding(0) var<uniform> exposure: ExposureParams;

@fragment
fn fs_exposure(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureSample(source, nearest_sampler, in.uv);
    return working_rgba(sample.rgb * exp2(exposure.values.x), sample.a);
}

@fragment
fn fs_offset(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureSample(source, nearest_sampler, in.uv);
    return working_rgba(sample.rgb + exposure.values.xyz * sample.a, sample.a);
}

@fragment
fn fs_printer(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureSample(source, nearest_sampler, in.uv);
    return working_rgba(sample.rgb * exp2(exposure.values.xyz / 12.0), sample.a);
}

@fragment
fn fs_rolloff(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureSample(source, nearest_sampler, in.uv);
    if (sample.a <= 0.0) { return sample; }
    let straight = sample.rgb / sample.a;
    let peak = max(straight.r, max(straight.g, straight.b));
    let knee = exposure.values.x;
    let strength = exposure.values.y;
    if (strength == 0.0 || peak <= knee || peak <= 0.0) { return sample; }
    let excess = peak - knee;
    let mapped_peak = knee + excess / (1.0 + strength * excess);
    return working_rgba(sample.rgb * (mapped_peak / peak), sample.a);
}

@fragment
fn fs_still(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureLoad(source, vec2<i32>(in.pos.xy), 0);
    if (sample.a <= 0.0) { return vec4<f32>(0.0); }
    return working_rgba(vec3<f32>(
        dot(sample.rgb, vec3<f32>(0.6130974024, 0.3395231462, 0.0473794514)),
        dot(sample.rgb, vec3<f32>(0.0701937225, 0.9163538791, 0.0134523985)),
        dot(sample.rgb, vec3<f32>(0.0206155929, 0.1095697729, 0.8698146342))), sample.a);
}

fn cdl_power(value: f32, power: f32) -> f32 {
    if (value <= 0.0) { return max(value, -65504.0); }
    let ceiling = exp2(min(log2(65504.0) / power, 126.0));
    return min(pow(min(value, ceiling), power), 65504.0);
}
@fragment
fn fs_cdl(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureSample(source, nearest_sampler, in.uv);
    if (sample.a <= 0.0) { return vec4<f32>(0.0); }
    let sop = sample.rgb / sample.a * exposure.slope.xyz + exposure.offset.xyz;
    let corrected = vec3<f32>(cdl_power(sop.r, exposure.power.x), cdl_power(sop.g, exposure.power.y), cdl_power(sop.b, exposure.power.z));
    let luma = dot(corrected, vec3<f32>(0.2126, 0.7152, 0.0722));
    return working_rgba((vec3<f32>(luma) + exposure.values.x * (corrected - vec3<f32>(luma))) * sample.a, sample.a);
}
@fragment
fn fs_contrast(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureSample(source, nearest_sampler, in.uv);
    if (sample.a <= 0.0) { return vec4<f32>(0.0); }
    if (exposure.values.y == 0.0) { return sample; }
    let slope = exp2(exposure.values.y);
    return working_rgba((vec3<f32>(exposure.values.x) + (sample.rgb / sample.a - vec3<f32>(exposure.values.x)) * slope) * sample.a, sample.a);
}

@fragment
fn fs_saturation(in: VOut) -> @location(0) vec4<f32> {
    let sample = textureSample(source, nearest_sampler, in.uv);
    if (sample.a <= 0.0) { return vec4<f32>(0.0); }
    if (exposure.values.x == 1.0 && exposure.values.y == 0.0) { return sample; }
    let rgb = sample.rgb / sample.a;
    let luma = dot(rgb, vec3<f32>(0.2722287168, 0.6740817658, 0.0536895174));
    let peak = max(rgb.r, max(rgb.g, rgb.b));
    let trough = min(rgb.r, min(rgb.g, rgb.b));
    let reference = max(max(abs(rgb.r), max(abs(rgb.g), abs(rgb.b))), 0.001);
    let colorfulness = clamp((peak - trough) / reference, 0.0, 1.0);
    let factor = exposure.values.x * max(1.0 + exposure.values.y * (1.0 - colorfulness), 0.0);
    return working_rgba((vec3<f32>(luma) + (rgb - vec3<f32>(luma)) * factor) * sample.a, sample.a);
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcescctDirection {
    Encode,
    Decode,
}

pub struct NativeAcescctPass {
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    encode: wgpu::RenderPipeline,
    decode: wgpu::RenderPipeline,
}

/// Scene-linear AP1 exposure, applied before logarithmic grading. Input and
/// output are premultiplied working textures, so alpha remains unchanged.
pub struct NativeExposurePass {
    source_layout: wgpu::BindGroupLayout,
    parameter_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    offset_pipeline: wgpu::RenderPipeline,
    printer_pipeline: wgpu::RenderPipeline,
    rolloff_pipeline: wgpu::RenderPipeline,
    saturation_pipeline: wgpu::RenderPipeline,
    contrast_pipeline: wgpu::RenderPipeline,
    cdl_pipeline: wgpu::RenderPipeline,
    still_pipeline: wgpu::RenderPipeline,
}

impl NativeAcescctPass {
    pub fn new(device: &wgpu::Device) -> Self {
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("native_acescct_bind_layout"),
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
            label: Some("native_acescct_pipeline_layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native_acescct_shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let build = |entry| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("native_acescct_pipeline"),
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
        let encode = build("fs_encode");
        let decode = build("fs_decode");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("native_acescct_nearest"),
            ..Default::default()
        });
        Self {
            bind_layout,
            sampler,
            encode,
            decode,
        }
    }

    /// Convert a premultiplied AP1 working texture into a fresh texture of the
    /// same dimensions. Encoding changes grading coordinates, not display.
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        direction: AcescctDirection,
    ) -> wgpu::Texture {
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("native_acescct_output"),
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
        self.apply_into(device, queue, input, &output, direction)
            .expect("new ACEScct target matches source dimensions and working format");
        output
    }

    /// Render into a caller-owned RGBA16F target for graph texture pooling.
    pub fn apply_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        direction: AcescctDirection,
    ) -> Result<(), &'static str> {
        if input.size() != output.size() || output.format() != WORKING_FORMAT {
            return Err("ACEScct target must match source size and use RGBA16F");
        }
        let source_view = input.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("native_acescct_bind"),
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
                label: Some("native_acescct_pass"),
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
            pass.set_pipeline(match direction {
                AcescctDirection::Encode => &self.encode,
                AcescctDirection::Decode => &self.decode,
            });
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..6, 0..1);
        }
        queue.submit([encoder.finish()]);
        Ok(())
    }
}

/// Lower the existing normalized temperature/tint trim to ACEScg channel gains.
/// This is the v1 artistic control model, not a Kelvin or chromatic-adaptation IDT.
/// Twelve printer points equal one stop, so the native gain pass can be reused.
pub fn white_balance_printer_points(temp: f32, tint: f32) -> Result<[f32; 3], &'static str> {
    if !temp.is_finite()
        || !tint.is_finite()
        || !(-1.0..=1.0).contains(&temp)
        || !(-1.0..=1.0).contains(&tint)
    {
        return Err("native temperature and tint must be finite and within -1..=1");
    }
    Ok([1.0 + 0.4 * temp, 1.0 - 0.2 * tint, 1.0 - 0.4 * temp].map(|gain| 12.0 * gain.log2()))
}

impl NativeExposurePass {
    pub fn new(device: &wgpu::Device) -> Self {
        let source_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("native_exposure_source_layout"),
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
        let parameter_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("native_exposure_parameter_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("native_exposure_pipeline_layout"),
            bind_group_layouts: &[&source_layout, &parameter_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native_exposure_shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let make_pipeline = |entry| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("native_primary_pipeline"),
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
        let pipeline = make_pipeline("fs_exposure");
        let offset_pipeline = make_pipeline("fs_offset");
        let printer_pipeline = make_pipeline("fs_printer");
        let rolloff_pipeline = make_pipeline("fs_rolloff");
        let saturation_pipeline = make_pipeline("fs_saturation");
        let contrast_pipeline = make_pipeline("fs_contrast");
        let cdl_pipeline = make_pipeline("fs_cdl");
        let still_pipeline = make_pipeline("fs_still");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("native_exposure_nearest"),
            ..Default::default()
        });
        Self {
            source_layout,
            parameter_layout,
            sampler,
            pipeline,
            offset_pipeline,
            printer_pipeline,
            rolloff_pipeline,
            saturation_pipeline,
            contrast_pipeline,
            cdl_pipeline,
            still_pipeline,
        }
    }

    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        stops: f32,
    ) -> Result<wgpu::Texture, &'static str> {
        if !stops.is_finite() || !(-32.0..=32.0).contains(&stops) {
            return Err("native exposure must be finite and within -32..=32 stops");
        }
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("native_exposure_output"),
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
        self.apply_into(device, queue, input, &output, stops)?;
        Ok(output)
    }

    /// Render into a caller-owned RGBA16F target for graph texture pooling.
    pub fn apply_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        stops: f32,
    ) -> Result<(), &'static str> {
        if !stops.is_finite() || !(-32.0..=32.0).contains(&stops) {
            return Err("native exposure must be finite and within -32..=32 stops");
        }
        self.render_into(
            device,
            queue,
            input,
            output,
            &self.pipeline,
            [stops, 0.0, 0.0, 0.0],
        )
    }

    /// Add a scene-linear straight-RGB offset while retaining premultiplied
    /// alpha. Negative values and highlights above reference white survive.
    pub fn apply_offset_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        rgb: [f32; 3],
    ) -> Result<(), &'static str> {
        if rgb
            .iter()
            .any(|value| !value.is_finite() || !(-16.0..=16.0).contains(value))
        {
            return Err("native linear offset must be finite and within -16..=16");
        }
        self.render_into(
            device,
            queue,
            input,
            output,
            &self.offset_pipeline,
            [rgb[0], rgb[1], rgb[2], 0.0],
        )
    }

    /// Twelve printer-light points equal one stop per channel in scene light.
    pub fn apply_printer_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        points: [f32; 3],
    ) -> Result<(), &'static str> {
        if points
            .iter()
            .any(|value| !value.is_finite() || !(-120.0..=120.0).contains(value))
        {
            return Err("native printer lights must be finite and within -120..=120 points");
        }
        self.render_into(
            device,
            queue,
            input,
            output,
            &self.printer_pipeline,
            [points[0], points[1], points[2], 0.0],
        )
    }

    /// Compress positive highlights by their shared peak, preserving hue ratios.
    pub fn apply_rolloff_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        knee: f32,
        strength: f32,
    ) -> Result<(), &'static str> {
        if !knee.is_finite()
            || !(0.0..=64.0).contains(&knee)
            || !strength.is_finite()
            || !(0.0..=64.0).contains(&strength)
        {
            return Err("native highlight roll-off parameters must be finite and within 0..=64");
        }
        self.render_into(
            device,
            queue,
            input,
            output,
            &self.rolloff_pipeline,
            [knee, strength, 0.0, 0.0],
        )
    }

    /// Adjust AP1 luminance/chroma in scene light without clipping extended RGB.
    pub fn apply_saturation_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        saturation: f32,
        vibrance: f32,
    ) -> Result<(), &'static str> {
        if !saturation.is_finite()
            || !(0.0..=4.0).contains(&saturation)
            || !vibrance.is_finite()
            || !(-1.0..=1.0).contains(&vibrance)
        {
            return Err("native saturation/vibrance parameters are invalid");
        }
        self.render_into(
            device,
            queue,
            input,
            output,
            &self.saturation_pipeline,
            [saturation, vibrance, 0.0, 0.0],
        )
    }

    /// Contrast in ACEScct coordinates; amount is log2 slope, pivot is log code value.
    pub fn apply_contrast_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        pivot: f32,
        amount: f32,
    ) -> Result<(), &'static str> {
        if !pivot.is_finite()
            || !(0.0..=1.0).contains(&pivot)
            || !amount.is_finite()
            || !(-4.0..=4.0).contains(&amount)
        {
            return Err("native contrast parameters are invalid");
        }
        self.render_into(
            device,
            queue,
            input,
            output,
            &self.contrast_pipeline,
            [pivot, amount, 0.0, 0.0],
        )
    }

    /// No-clamp SOP/saturation in ACEScct coordinates; negative SOP passes through power.
    pub fn apply_cdl_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        c: crate::grade::ResolvedCdl,
    ) -> Result<(), &'static str> {
        validate_native_cdl(&c)?;
        if input.size() != output.size() || output.format() != WORKING_FORMAT {
            return Err("native CDL target must match source and use RGBA16F");
        }
        let mut values = [0.0; 16];
        values[0] = c.sat;
        values[4..7].copy_from_slice(&c.slope);
        values[8..11].copy_from_slice(&c.offset);
        values[12..15].copy_from_slice(&c.power);
        self.render_unchecked(device, queue, input, output, &self.cdl_pipeline, values)
    }

    pub fn apply_still_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
    ) -> Result<(), &'static str> {
        if output.format() != WORKING_FORMAT {
            return Err("native still target must use RGBA16F");
        }
        self.render_unchecked(
            device,
            queue,
            input,
            output,
            &self.still_pipeline,
            [0.0; 16],
        )
    }

    fn render_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        pipeline: &wgpu::RenderPipeline,
        values: [f32; 4],
    ) -> Result<(), &'static str> {
        if input.size() != output.size() || output.format() != WORKING_FORMAT {
            return Err("native exposure target must match source size and use RGBA16F");
        }
        let mut parameters = [0.0; 16];
        parameters[..4].copy_from_slice(&values);
        self.render_unchecked(device, queue, input, output, pipeline, parameters)
    }

    fn render_unchecked(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        pipeline: &wgpu::RenderPipeline,
        values: [f32; 16],
    ) -> Result<(), &'static str> {
        let source_view = input.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let source_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("native_exposure_source_bind"),
            layout: &self.source_layout,
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
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("native_exposure_parameters"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&parameters, 0, bytemuck::cast_slice(&values));
        let parameter_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("native_exposure_parameter_bind"),
            layout: &self.parameter_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: parameters.as_entire_binding(),
            }],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("native_exposure_pass"),
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
            pass.set_bind_group(0, &source_bind, &[]);
            pass.set_bind_group(1, &parameter_bind, &[]);
            pass.draw(0..6, 0..1);
        }
        queue.submit([encoder.finish()]);
        Ok(())
    }
}

/// Photonic saturation/vibrance in linear AP1. Luminance is the Y row of the
/// AP1-to-XYZ matrix (Khronos Data Format specification, section 14).
/// https://registry.khronos.org/DataFormat/specs/1.4/dataformat.1.4.html
/// This creative operator is not an ACES output transform.
pub fn saturation_vibrance_ap1(rgb: [f32; 3], saturation: f32, vibrance: f32) -> [f32; 3] {
    if saturation == 1.0 && vibrance == 0.0 {
        return rgb;
    }
    let luma = rgb[0] * 0.27222872 + rgb[1] * 0.67408174 + rgb[2] * 0.053689517;
    let peak = rgb[0].max(rgb[1]).max(rgb[2]);
    let trough = rgb[0].min(rgb[1]).min(rgb[2]);
    let reference = rgb[0].abs().max(rgb[1].abs()).max(rgb[2].abs()).max(0.001);
    let colorfulness = ((peak - trough) / reference).clamp(0.0, 1.0);
    let factor = saturation * (1.0 + vibrance * (1.0 - colorfulness)).max(0.0);
    rgb.map(|value| luma + (value - luma) * factor)
}

/// Bounds keep the native logarithmic SOP/saturation transform finite.
pub fn validate_native_cdl(c: &crate::grade::ResolvedCdl) -> Result<(), &'static str> {
    if !c
        .slope
        .iter()
        .all(|v| v.is_finite() && (0.0..=8.0).contains(v))
        || !c
            .offset
            .iter()
            .all(|v| v.is_finite() && (-4.0..=4.0).contains(v))
        || !c
            .power
            .iter()
            .all(|v| v.is_finite() && (0.1..=10.0).contains(v))
        || !c.sat.is_finite()
        || !(0.0..=4.0).contains(&c.sat)
    {
        return Err("native CDL requires finite slope 0..8, offset -4..4, power 0.1..10 and saturation 0..4");
    }
    Ok(())
}
/// Log-domain no-clamp CDL reference. Fixed Rec.709 saturation coefficients follow CDL.
/// https://opencolorio.readthedocs.io/en/v2.4.2/api/transforms.html#cdltransform
pub fn cdl_no_clamp(rgb: [f32; 3], c: crate::grade::ResolvedCdl) -> [f32; 3] {
    let sop = std::array::from_fn::<_, 3, _>(|i| {
        let v = f64::from(rgb[i]) * f64::from(c.slope[i]) + f64::from(c.offset[i]);
        (if v <= 0.0 {
            v
        } else {
            v.powf(f64::from(c.power[i]))
        })
        .clamp(-65504.0, 65504.0) as f32
    });
    let luma = sop[0] * 0.2126 + sop[1] * 0.7152 + sop[2] * 0.0722;
    sop.map(|v| luma + c.sat * (v - luma))
}

/// Native keys select HSL of bounded ACEScct/AP1 coordinates; corrections retain
/// the original extended log values. This is a grading key, not display HSL.
pub fn validate_native_qualifier(
    q: &crate::grade::ResolvedHslQualifier,
) -> Result<(), &'static str> {
    fn key(hue: [f32; 2], sat: [f32; 2], lum: [f32; 2], soft: f32) -> bool {
        hue.into_iter()
            .all(|v| v.is_finite() && (-1.0..=2.0).contains(&v))
            && hue[0] <= hue[1]
            && [sat, lum].into_iter().all(|range| {
                range
                    .into_iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(&v))
                    && range[0] <= range[1]
            })
            && soft.is_finite()
            && (0.0..=1.0).contains(&soft)
    }
    if q.key_count as usize > q.keys.len()
        || !key(q.hue, q.sat, q.lum, q.softness)
        || q.matte_levels
            .into_iter()
            .any(|v| !v.is_finite() || !(0.0..=0.49).contains(&v))
        || q.keys[..q.key_count as usize]
            .iter()
            .any(|k| !key(k.hue, k.sat, k.lum, k.softness))
    {
        return Err("native qualifier requires finite ordered key ranges, normalized saturation/lightness/softness and valid matte levels");
    }
    validate_native_cdl(&q.correction)
}

pub fn qualifier_acescct(rgb: [f32; 3], q: &crate::grade::ResolvedHslQualifier) -> [f32; 3] {
    let gate = crate::grade::qualifier_gate(q, rgb.map(|v| v.clamp(0.0, 1.0)));
    let corrected = cdl_no_clamp(rgb, q.correction);
    std::array::from_fn(|c| rgb[c] + gate * (corrected[c] - rgb[c]))
}

/// Endpoint slopes extend sampled logarithmic master/channel curves without clipping headroom.
pub fn curves_acescct(rgb: [f32; 3], curves: &crate::grade::ResolvedCurves) -> [f32; 3] {
    fn sample(table: &[f32; 256], v: f32) -> f32 {
        if v < 0.0 {
            return table[0] + v * 255.0 * (table[1] - table[0]);
        }
        if v > 1.0 {
            return table[255] + (v - 1.0) * 255.0 * (table[255] - table[254]);
        }
        let p = v * 255.0;
        let lo = p.floor() as usize;
        let hi = (lo + 1).min(255);
        table[lo] + (table[hi] - table[lo]) * (p - p.floor())
    }
    let channels = [&curves.red, &curves.green, &curves.blue];
    let original = std::array::from_fn(|c| sample(channels[c], sample(&curves.master, rgb[c])));
    if [
        &curves.hue_vs_hue,
        &curves.hue_vs_sat,
        &curves.hue_vs_luma,
        &curves.luma_vs_sat,
        &curves.sat_vs_sat,
    ]
    .iter()
    .all(|table| table.is_none())
    {
        return original;
    }
    let bounded = original.map(|v| v.clamp(0.0, 1.0));
    let corrected = crate::grade::apply_secondary_curves(bounded, curves, true);
    std::array::from_fn(|c| original[c] + (corrected[c] - bounded[c]))
}
