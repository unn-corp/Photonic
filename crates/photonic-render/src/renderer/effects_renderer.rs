use super::*;

impl PhotonicRenderer {
    /// Single-sample colour texture (render target + sampleable) for the
    /// per-frame effects layer.
    pub(crate) fn make_fx_tex(&self, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fx_tex"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The document-effects intermediates are the sRGB scene format so
            // blur/composite sampling + blending run in linear light (03 §4.5.4),
            // matching every other document-pass target and the headless path.
            format: self.scene_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    /// Bind group for the blur shader: source texture + sampler + params.
    fn effects_blur_bg(
        &self,
        src: &wgpu::TextureView,
        sigma: f32,
        horizontal: bool,
    ) -> wgpu::BindGroup {
        let params = BlurParams {
            sigma,
            horizontal: horizontal as u32,
            _pad: [0.0; 2],
        };
        let buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fx_blur_params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx_blur_bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.blur_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buf.as_entire_binding(),
                },
            ],
        })
    }

    /// Clear `dst` transparent, then straight-alpha composite the (blurred)
    /// effects texture and then the sharp shapes texture on top of it — the
    /// per-layer analogue of `composite_effects`' effect+shape passes, without the
    /// artboard/background. Builds one layer's isolated content (#226 Stage 2).
    pub(crate) fn stack_effects_under_shapes(
        &self,
        enc: &mut wgpu::CommandEncoder,
        dst: &wgpu::TextureView,
        fx_view: &wgpu::TextureView,
        shapes_view: &wgpu::TextureView,
    ) {
        let mut first = true;
        for layer in [fx_view, shapes_view] {
            let bg = self.effects_blur_bg(layer, 0.0, true);
            let load = if first {
                first = false;
                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
            } else {
                wgpu::LoadOp::Load
            };
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iso_layer_stack"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.blur_pipeline_alpha);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..1);
        }
    }

    /// Render each pending blur job (silhouette → H-blur → V-blur) and
    /// accumulate them into a single straight-alpha effects texture.
    /// `layer_ord`: `None` blurs every pending job (the flat effects path);
    /// `Some(ord)` blurs only jobs owned by that layer (per-layer isolation).
    pub(crate) fn render_effects_layer(
        &self,
        enc: &mut wgpu::CommandEncoder,
        w: u32,
        h: u32,
        layer_ord: Option<u32>,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let fx_a = self.make_fx_tex(w, h);
        let fx_b = self.make_fx_tex(w, h);
        let accum = self.make_fx_tex(w, h);
        let (a_view, b_view, accum_view) = (
            fx_a.create_view(&Default::default()),
            fx_b.create_view(&Default::default()),
            accum.create_view(&Default::default()),
        );

        let mut accum_cleared = false;
        for job in &self.pending_blur_jobs {
            if job.idxs.is_empty() {
                continue;
            }
            if let Some(ord) = layer_ord {
                if job.layer_ordinal != ord {
                    continue;
                }
            }
            let sigma = (job.radius_doc * self.view.zoom).max(0.0) as f32;
            let vbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fx_vbuf"),
                    contents: bytemuck::cast_slice(&job.verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let ibuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fx_ibuf"),
                    contents: bytemuck::cast_slice(&job.idxs),
                    usage: wgpu::BufferUsages::INDEX,
                });

            // Pass A: silhouette → fx_a (cleared transparent).
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fx_silhouette"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &a_view,
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
                pass.set_pipeline(&self.fill_pipeline_1spp);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..job.idxs.len() as u32, 0, 0..1);
            }
            // Pass B: horizontal blur fx_a → fx_b.
            {
                let bg = self.effects_blur_bg(&a_view, sigma, true);
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fx_blur_h"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &b_view,
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
                pass.set_pipeline(&self.blur_pipeline_alpha);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..6, 0..1);
            }
            // Pass C: vertical blur fx_b → accum (accumulate).
            {
                let bg = self.effects_blur_bg(&b_view, sigma, false);
                let load = if accum_cleared {
                    wgpu::LoadOp::Load
                } else {
                    accum_cleared = true;
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                };
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fx_blur_v"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &accum_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.blur_pipeline_alpha);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        (accum, accum_view)
    }

    /// Composite the artboard rect, the effects layer, and the sharp shapes onto
    /// `target` (single-sample) over a cleared background.
    pub(crate) fn composite_effects(
        &self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        vertices: &[Vertex],
        artboard_indices: &[u32],
        fx_view: &wgpu::TextureView,
        doc_view: &wgpu::TextureView,
    ) {
        // Pass 1: clear to the canvas background and draw the artboard rect.
        {
            let vbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fx_artboard_vbuf"),
                    contents: bytemuck::cast_slice(vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let ibuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("fx_artboard_ibuf"),
                    contents: bytemuck::cast_slice(artboard_indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx_composite_artboard"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.scene_clear()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if !artboard_indices.is_empty() {
                pass.set_pipeline(&self.fill_pipeline_1spp);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..artboard_indices.len() as u32, 0, 0..1);
            }
        }
        // Passes 2 & 3: effects layer, then sharp shapes (both straight-alpha).
        for layer in [fx_view, doc_view] {
            let bg = self.effects_blur_bg(layer, 0.0, true);
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx_composite_layer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            pass.set_pipeline(&self.blur_pipeline_alpha);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}
