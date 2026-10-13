//! Bounded display-linear glyph outlines, rounded boxes and active-word underlines.
//! This pass is selected explicitly by managed text; Legacy captions are unchanged.
use crate::pipeline::WORKING_FORMAT;
use wgpu::util::DeviceExt;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptionDecoration {
    /// Straight encoded-sRGB bytes and an authored-pixel outer outline width.
    pub stroke: Option<([u8; 4], f32)>,
    /// Straight encoded-sRGB bytes, authored-pixel padding, corner radius.
    pub background: Option<([u8; 4], f32, f32)>,
}
impl CaptionDecoration {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .stroke
            .is_some_and(|(_, width)| !width.is_finite() || !(0.0..=8.0).contains(&width))
            || self.background.is_some_and(|(_, padding, radius)| {
                !padding.is_finite()
                    || !(0.0..=128.0).contains(&padding)
                    || !radius.is_finite()
                    || !(0.0..=128.0).contains(&radius)
            })
        {
            return Err(
                "managed text outline must be 0..=8 pixels and box padding/radius 0..=128 pixels",
            );
        }
        Ok(())
    }
    pub fn scaled(&self, scale: f32) -> Self {
        Self {
            stroke: self.stroke.map(|(c, w)| (c, w * scale)),
            background: self.background.map(|(c, p, r)| (c, p * scale, r * scale)),
        }
    }
}

/// Resolved active-word underline; width is in authored pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionUnderline {
    pub word: usize,
    pub color: [u8; 4],
    pub width: f32,
}

/// Resolved intra-word left-to-right glyph split. Colors include word animation alpha.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionSweep {
    pub word: usize,
    pub fraction: f32,
    pub active: [u8; 4],
    pub inactive: [u8; 4],
}

/// Per-cue paint metadata. Empty mask opacity means opaque glyph geometry;
/// otherwise it holds animation-only word alpha, independent of authored fill.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionPaint {
    pub decoration: CaptionDecoration,
    pub word_opacity: Vec<u8>,
    pub opacity: f32,
    pub underlines: Vec<CaptionUnderline>,
    pub sweeps: Vec<CaptionSweep>,
}
impl Default for CaptionPaint {
    fn default() -> Self {
        Self {
            decoration: Default::default(),
            word_opacity: Vec::new(),
            opacity: 1.0,
            underlines: Vec::new(),
            sweeps: Vec::new(),
        }
    }
}
impl CaptionPaint {
    pub fn validate(&self, words: usize) -> Result<(), &'static str> {
        self.decoration.validate()?;
        let mut sweep_words = std::collections::HashSet::new();
        if self.sweeps.len() > words
            || self.sweeps.iter().any(|s| {
                s.word >= words
                    || !s.fraction.is_finite()
                    || !(0.0..=1.0).contains(&s.fraction)
                    || !sweep_words.insert(s.word)
            })
        {
            return Err("caption sweeps require unique word indexes and finite fractions 0..1");
        }
        if self.underlines.len() > words
            || self
                .underlines
                .iter()
                .any(|u| u.word >= words || !u.width.is_finite() || !(0.0..=8.0).contains(&u.width))
        {
            return Err("caption underlines require valid word indexes and widths 0..8 pixels");
        }
        let mut seen = std::collections::HashSet::new();
        if self.underlines.iter().any(|u| !seen.insert(u.word)) {
            return Err("duplicate caption underline word");
        }
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            return Err("caption group opacity must be finite in 0..=1");
        }
        if !self.word_opacity.is_empty() && self.word_opacity.len() != words {
            return Err("managed caption mask opacity must match its word runs");
        }
        Ok(())
    }
    pub fn scaled(&self, scale: f32) -> Self {
        Self {
            decoration: self.decoration.scaled(scale),
            sweeps: self.sweeps.clone(),
            word_opacity: self.word_opacity.clone(),
            opacity: self.opacity,
            underlines: self
                .underlines
                .iter()
                .map(|u| CaptionUnderline {
                    word: u.word,
                    color: u.color,
                    width: u.width * scale,
                })
                .collect(),
        }
    }
}

const SHADER: &str = r#"
@group(0) @binding(0) var mask: texture_2d<f32>;
struct Params {
    canvas: vec4<f32>,
    bounds: vec4<f32>,
    stroke: vec4<f32>,
    background: vec4<f32>,
    metrics: vec4<f32>,
}
@group(0) @binding(1) var<uniform> params: Params;
struct VertexOut { @builtin(position) position: vec4<f32> }
@vertex fn vs_quad(@builtin(vertex_index) index: u32) -> VertexOut {
    var positions=array<vec2<f32>,6>(vec2(-1.0,-1.0),vec2(1.0,-1.0),vec2(1.0,1.0),vec2(-1.0,-1.0),vec2(1.0,1.0),vec2(-1.0,1.0));
    var out:VertexOut;out.position=vec4(positions[index],0.0,1.0);return out;
}
fn coverage(p:vec2<i32>) -> f32 {
    if any(p<vec2<i32>(0)) || any(p>=vec2<i32>(params.canvas.xy)) {return 0.0;}
    return textureLoad(mask,p,0).a;
}
@fragment fn fs_decoration(in:VertexOut) -> @location(0) vec4<f32> {
    let p=in.position.xy;
    if any(p>=params.canvas.xy) {discard;}
    var box_alpha=0.0;
    if params.background.a>0.0 {
        let half_size=(params.bounds.zw-params.bounds.xy)*0.5;
        let center=(params.bounds.xy+params.bounds.zw)*0.5;
        let radius=min(params.metrics.y,min(half_size.x,half_size.y));
        let q=abs(p-center)-half_size+vec2(radius);
        let distance=length(max(q,vec2(0.0)))+min(max(q.x,q.y),0.0)-radius;
        box_alpha=params.background.a*clamp(0.5-distance,0.0,1.0);
    }
    var outline_alpha=0.0;
    if params.metrics.x>0.0 && params.stroke.a>0.0 {
        let ip=vec2<i32>(p);
        let original=coverage(ip);
        var expanded=original;
        let extent=i32(ceil(params.metrics.x+0.5));
        for(var y=-extent;y<=extent;y=y+1) {
            for(var x=-extent;x<=extent;x=x+1) {
                let weight=clamp(params.metrics.x+0.5-length(vec2<f32>(f32(x),f32(y))),0.0,1.0);
                if weight>0.0 {expanded=max(expanded,coverage(ip+vec2(x,y))*weight);}
            }
        }
        // The outline is outside the glyph, even for transparent fill.
        outline_alpha=params.stroke.a*max(expanded-original,0.0);
    }
    let alpha=outline_alpha+box_alpha*(1.0-outline_alpha);
    let rgb=params.stroke.rgb*outline_alpha+params.background.rgb*box_alpha*(1.0-outline_alpha);
    return vec4(rgb,alpha);
}
@fragment fn fs_sweep(in:VertexOut) -> @location(0) vec4<f32> {
    let p=in.position.xy;
    if any(p>=params.canvas.xy) {discard;}
    // The two colors share ONE coverage value: no dark seam or double alpha.
    let f=clamp(params.metrics.x-p.x+0.5,0.0,1.0);
    let coverage=textureLoad(mask,vec2<i32>(p),0).a;
    let active_alpha=params.stroke.a*f;
    let inactive_alpha=params.background.a*(1.0-f);
    return vec4((params.stroke.rgb*active_alpha+params.background.rgb*inactive_alpha)*coverage,(active_alpha+inactive_alpha)*coverage);
}
@fragment fn fs_group(in:VertexOut) -> @location(0) vec4<f32> {
    if any(in.position.xy>=params.canvas.xy) {discard;}
    return textureLoad(mask,vec2<i32>(in.position.xy),0)*params.metrics.z;
}
"#;

pub struct CaptionDecorationPass {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    group_pipeline: wgpu::RenderPipeline,
    sweep_pipeline: wgpu::RenderPipeline,
}
impl CaptionDecorationPass {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("caption-decoration-layout"),
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
            label: Some("caption-decoration-pipeline-layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("caption-decoration-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let build = |entry| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("caption-decoration-pipeline"),
                layout: Some(&pipeline_layout),
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
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview: None,
                cache: None,
            })
        };
        let pipeline = build("fs_decoration");
        let group_pipeline = build("fs_group");
        let sweep_pipeline = build("fs_sweep");
        Self {
            layout,
            pipeline,
            group_pipeline,
            sweep_pipeline,
        }
    }

    /// Composite onto a premultiplied linear-Rec.709 image. `glyph_mask` must
    /// contain opaque-white glyph coverage independent of the authored fill.
    /// Bounds are the shaped text's logical line rectangle before padding.
    #[allow(clippy::too_many_arguments)]
    pub fn composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        glyph_mask: &wgpu::Texture,
        target: &wgpu::Texture,
        canvas: (u32, u32),
        bounds: [f32; 4],
        style: &CaptionDecoration,
    ) -> Result<(), &'static str> {
        style.validate()?;
        if canvas.0 == 0
            || canvas.1 == 0
            || canvas.0 > target.width()
            || canvas.1 > target.height()
            || glyph_mask.size() != target.size()
            || target.format() != WORKING_FORMAT
            || glyph_mask.format() != WORKING_FORMAT
            || std::ptr::eq(glyph_mask, target)
            || bounds
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 16_777_216.0)
            || bounds[2] < bounds[0]
            || bounds[3] < bounds[1]
        {
            return Err("invalid managed text decoration target, mask or layout bounds");
        }
        let linear = |color: [u8; 4]| -> [f32; 4] {
            let mut result = color.map(|x| x as f32 / 255.0);
            for v in &mut result[..3] {
                *v = if *v <= 0.04045 {
                    *v / 12.92
                } else {
                    ((*v + 0.055) / 1.055).powf(2.4)
                };
            }
            result
        };
        let (stroke, width) = style.stroke.unwrap_or(([0; 4], 0.0));
        let (background, padding, radius) = style.background.unwrap_or(([0; 4], 0.0, 0.0));
        let values = [
            [canvas.0 as f32, canvas.1 as f32, 0.0, 0.0],
            [
                bounds[0] - padding,
                bounds[1] - padding,
                bounds[2] + padding,
                bounds[3] + padding,
            ],
            linear(stroke),
            linear(background),
            [width, radius, 0.0, 0.0],
        ];
        // Background-only paints (including word underlines) cannot affect
        // pixels beyond their padded rectangle plus the one-pixel AA fringe.
        // Bound their fragment work instead of shading the whole frame per word.
        let scissor = if width <= 0.0 || stroke[3] == 0 {
            let x0 = (bounds[0] - padding - 1.0)
                .floor()
                .clamp(0.0, canvas.0 as f32) as u32;
            let y0 = (bounds[1] - padding - 1.0)
                .floor()
                .clamp(0.0, canvas.1 as f32) as u32;
            let x1 = (bounds[2] + padding + 1.0)
                .ceil()
                .clamp(0.0, canvas.0 as f32) as u32;
            let y1 = (bounds[3] + padding + 1.0)
                .ceil()
                .clamp(0.0, canvas.1 as f32) as u32;
            if x1 <= x0 || y1 <= y0 {
                return Ok(());
            }
            Some([x0, y0, x1 - x0, y1 - y0])
        } else {
            None
        };
        self.draw_values(device, queue, glyph_mask, target, &values, 0, scissor);
        Ok(())
    }
    /// Colorize one word's white coverage mask. Wrapping consumes the sweep
    /// distance across shaped line advances; each pixel is shaded once. The
    /// line's Voronoi band includes glyph overhang beyond its typographic box.
    #[allow(clippy::too_many_arguments)]
    pub fn composite_sweep(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mask: &wgpu::Texture,
        target: &wgpu::Texture,
        canvas: (u32, u32),
        spans: &[[f32; 4]],
        sweep: &CaptionSweep,
    ) -> Result<(), &'static str> {
        if canvas.0 == 0
            || canvas.1 == 0
            || canvas.0 > target.width()
            || canvas.1 > target.height()
            || mask.size() != target.size()
            || mask.format() != WORKING_FORMAT
            || target.format() != WORKING_FORMAT
            || std::ptr::eq(mask, target)
            || !sweep.fraction.is_finite()
            || !(0.0..=1.0).contains(&sweep.fraction)
            || spans.len() > 128
            || spans.iter().any(|b| {
                b.iter().any(|v| !v.is_finite() || v.abs() > 16_777_216.0)
                    || b[2] <= b[0]
                    || b[3] <= b[1]
            })
            || spans.windows(2).any(|p| p[0][1] >= p[1][1])
        {
            return Err("invalid caption sweep mask, fraction or ordered line spans");
        }
        let total: f32 = spans.iter().map(|b| b[2] - b[0]).sum();
        let mut remaining = total * sweep.fraction;
        let linear = |color: [u8; 4]| {
            let mut c = color.map(|v| v as f32 / 255.0);
            for v in &mut c[..3] {
                *v = if *v <= 0.04045 {
                    *v / 12.92
                } else {
                    ((*v + 0.055) / 1.055).powf(2.4)
                }
            }
            c
        };
        for (i, b) in spans.iter().enumerate() {
            let width = b[2] - b[0];
            // Saturated portions include negative/positive glyph overhang.
            let split = if remaining <= 0.0 {
                -16_777_216.0
            } else if remaining >= width {
                16_777_216.0
            } else {
                b[0] + remaining
            };
            remaining -= width;
            let band = |a: [f32; 4], b: [f32; 4]| {
                ((a[1] + a[3] + b[1] + b[3]) * 0.25)
                    .ceil()
                    .clamp(0.0, canvas.1 as f32) as u32
            };
            let top = if i == 0 { 0 } else { band(spans[i - 1], *b) };
            let bottom = if i + 1 == spans.len() {
                canvas.1
            } else {
                band(*b, spans[i + 1])
            };
            if bottom <= top {
                continue;
            }
            let values = [
                [canvas.0 as f32, canvas.1 as f32, 0.0, 0.0],
                *b,
                linear(sweep.active),
                linear(sweep.inactive),
                [split, 0.0, 0.0, 0.0],
            ];
            self.draw_values(
                device,
                queue,
                mask,
                target,
                &values,
                2,
                Some([0, top, canvas.0, bottom - top]),
            );
        }
        Ok(())
    }
    /// Fade a fully composited cue once, preserving correct group alpha-over.
    #[allow(clippy::too_many_arguments)]
    pub fn composite_group(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Texture,
        target: &wgpu::Texture,
        canvas: (u32, u32),
        opacity: f32,
    ) -> Result<(), &'static str> {
        if !opacity.is_finite()
            || !(0.0..=1.0).contains(&opacity)
            || canvas.0 == 0
            || canvas.1 == 0
            || canvas.0 > target.width()
            || canvas.1 > target.height()
            || source.size() != target.size()
            || source.format() != WORKING_FORMAT
            || target.format() != WORKING_FORMAT
            || std::ptr::eq(source, target)
        {
            return Err("invalid caption group target or opacity");
        }
        let values = [
            [canvas.0 as f32, canvas.1 as f32, 0.0, 0.0],
            [0.0; 4],
            [0.0; 4],
            [0.0; 4],
            [0.0, 0.0, opacity, 0.0],
        ];
        self.draw_values(device, queue, source, target, &values, 1, None);
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn draw_values(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        glyph_mask: &wgpu::Texture,
        target: &wgpu::Texture,
        values: &[[f32; 4]; 5],
        mode: u8,
        scissor: Option<[u32; 4]>,
    ) {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("caption-decoration-params"),
            contents: bytemuck::cast_slice(values),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mask_view = glyph_mask.create_view(&Default::default());
        let target_view = target.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("caption-decoration-bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("caption-decoration-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(match mode {
                1 => &self.group_pipeline,
                2 => &self.sweep_pipeline,
                _ => &self.pipeline,
            });
            pass.set_bind_group(0, &bind, &[]);
            if let Some([x, y, w, h]) = scissor {
                pass.set_scissor_rect(x, y, w, h);
            }
            pass.draw(0..6, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}
