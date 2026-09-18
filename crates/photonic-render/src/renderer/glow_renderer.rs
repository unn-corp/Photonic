use super::*;

impl PhotonicRenderer {
    /// Execute all pending Gaussian glow jobs collected during `update()`.
    ///
    /// Must be called **after** `begin_frame` (so the scene is already rendered on the
    /// surface texture) and **before** `finish_frame`.
    /// Uses an additive blend so the glow brightens the scene without erasing the fill.
    pub fn render_gaussian_glow_pass(&mut self, frame: &mut FrameHandle) {
        if self.pending_gaussian_glows.is_empty() {
            return;
        }

        let jobs: Vec<GaussianGlowJob> = std::mem::take(&mut self.pending_gaussian_glows);

        for job in &jobs {
            if job.verts.is_empty() {
                continue;
            }

            let sigma = job.sigma_px.max(0.5);

            // ── Pass A: render fill silhouette (glow tint colour) → glow_tex_a ──
            {
                let vbuf = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("gglow_vbuf"),
                        contents: bytemuck::cast_slice(&job.verts),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                let ibuf = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("gglow_ibuf"),
                        contents: bytemuck::cast_slice(&job.idxs),
                        usage: wgpu::BufferUsages::INDEX,
                    });
                let mut pass = frame
                    .encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("gglow_shape"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.glow_tex_a_view,
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

            // Helper: create blur bind group for a given source texture view.
            let make_blur_bg = |src_view: &wgpu::TextureView, sigma: f32, horizontal: bool| {
                let params = BlurParams {
                    sigma,
                    horizontal: horizontal as u32,
                    _pad: [0.0; 2],
                };
                let params_buf =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("blur_params"),
                            contents: bytemuck::bytes_of(&params),
                            usage: wgpu::BufferUsages::UNIFORM,
                        });
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("blur_bg"),
                    layout: &self.blur_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(src_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.blur_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: params_buf.as_entire_binding(),
                        },
                    ],
                })
            };

            // ── Pass B: horizontal blur  glow_tex_a → glow_tex_b ────────────────
            {
                let bg = make_blur_bg(&self.glow_tex_a_view, sigma, true);
                let mut pass = frame
                    .encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("gglow_blur_h"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.glow_tex_b_view,
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
                pass.set_pipeline(&self.blur_pipeline_h);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..6, 0..1);
            }

            // ── Pass C: vertical blur  glow_tex_b → surface (additive) ──────────
            // Uses the ADDITIVE blur pipeline so the glow brightens the scene
            // without erasing the fill (this module's contract, top of file).
            // (Pass B writes an intermediate over a cleared target, where the
            // blend is a no-op, so it keeps the premultiplied pipeline.)
            {
                let bg = make_blur_bg(&self.glow_tex_b_view, sigma, false);
                let mut pass = frame
                    .encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("gglow_blur_v"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &frame.view,
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
                pass.set_pipeline(&self.blur_pipeline_v);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..6, 0..1);
            }
        }
    }
}
