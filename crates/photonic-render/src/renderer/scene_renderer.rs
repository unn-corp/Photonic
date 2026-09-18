use super::*;

impl PhotonicRenderer {
    /// Grow the persistent document vertex/index buffers to hold at least
    /// `vbytes`/`ibytes`, by doubling (03 §2.3). Called from `update()` (a `&mut`
    /// context) before `record_document_pass` writes them, so the render pass
    /// itself only needs `&self` + `queue.write_buffer`.
    pub(crate) fn ensure_doc_buffers(&mut self, vbytes: u64, ibytes: u64) {
        if vbytes > self.doc_vbuf_cap {
            let cap = vbytes.max(self.doc_vbuf_cap * 2);
            self.doc_vbuf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("doc_vbuf"),
                size: cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.doc_vbuf_cap = cap;
        }
        if ibytes > self.doc_ibuf_cap {
            let cap = ibytes.max(self.doc_ibuf_cap * 2);
            self.doc_ibuf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("doc_ibuf"),
                size: cap,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.doc_ibuf_cap = cap;
        }
    }

    /// Record the document render pass into an existing command encoder.
    ///
    /// `msaa_view` is the 4× multisampled render target; `resolve_view` is the
    /// single-sample destination (surface texture or offscreen capture texture).
    fn record_document_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        msaa_view: &wgpu::TextureView,
        resolve_view: &wgpu::TextureView,
        vertices: &[Vertex],
        indices: &[u32],
        clear: wgpu::Color,
    ) {
        if !vertices.is_empty() {
            // Persistent, growable buffers (03 §2.3): `update()` sized them to fit
            // this frame's geometry, so upload in place rather than allocating a
            // fresh buffer every frame. The caller's exact slice lands at offset 0,
            // reproducing the previous `create_buffer_init(slice)` layout byte for
            // byte (the effects path's `&indices[skip..]` sub-slice included).
            let vbytes = std::mem::size_of_val(vertices) as u64;
            let ibytes = std::mem::size_of_val(indices) as u64;
            debug_assert!(
                vbytes <= self.doc_vbuf_cap && ibytes <= self.doc_ibuf_cap,
                "doc buffers must be grown via ensure_doc_buffers() before recording the pass"
            );
            self.queue
                .write_buffer(&self.doc_vbuf, 0, bytemuck::cast_slice(vertices));
            self.queue
                .write_buffer(&self.doc_ibuf, 0, bytemuck::cast_slice(indices));
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fill_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa_view,
                    resolve_target: Some(resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(0, self.doc_vbuf.slice(0..vbytes));
            pass.set_index_buffer(self.doc_ibuf.slice(0..ibytes), wgpu::IndexFormat::Uint32);
            draw_segments(
                &mut pass,
                &self.draw_segments,
                &self.blend_pipelines,
                &self.fill_pipeline,
                indices.len() as u32,
            );
        } else {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa_view,
                    resolve_target: Some(resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
    }

    /// Render the document to `target_view`, inserting the live-effects blur
    /// layer (drop shadow / object blur / feather) between the artboard
    /// background and the sharp shapes when any effect is active. With no
    /// effects this is the original single-pass document render.
    pub(crate) fn render_scene(
        &self,
        enc: &mut wgpu::CommandEncoder,
        msaa_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        w: u32,
        h: u32,
        vertices: &[Vertex],
        indices: &[u32],
    ) {
        // Per-layer isolated compositing (#226): when a layer carries opacity < 1
        // or a non-Normal blend, composite each layer as its own unit through the
        // composite shader — including that layer's blur effects (Stage 2). Opaque-
        // Normal-only documents skip this and take the single-pass fast path (or
        // the flat effects path when only per-node effects are present).
        if self.layer_runs.iter().any(LayerRun::is_nontrivial) {
            self.render_scene_isolated(enc, msaa_view, target_view, w, h, vertices, indices);
            return;
        }
        if self.pending_blur_jobs.is_empty() {
            self.record_document_pass(
                enc,
                msaa_view,
                target_view,
                vertices,
                indices,
                self.scene_clear(),
            );
            return;
        }

        // The artboard rects are the preamble built by build_geometry (6 indices
        // each); render the rest (shapes) to a transparent offscreen texture so
        // the effects layer can sit beneath them. `artboard_idx_end` is 0 for a
        // transparent export (boards suppressed), so all geometry is shapes.
        let skip = (self.artboard_idx_end as usize).min(indices.len());
        let doc_tex = self.make_fx_tex(w, h);
        let doc_view = doc_tex.create_view(&Default::default());
        self.record_document_pass(
            enc,
            msaa_view,
            &doc_view,
            vertices,
            &indices[skip..],
            wgpu::Color::TRANSPARENT,
        );

        let (fx_tex, fx_view) = self.render_effects_layer(enc, w, h, None);

        // Composite onto the target: background → artboard → effects → shapes.
        self.composite_effects(
            enc,
            target_view,
            vertices,
            &indices[..skip],
            &fx_view,
            &doc_view,
        );
        drop(fx_tex);
        drop(doc_tex);
    }

    /// Per-layer isolated compositing (#226). Renders the artboard background,
    /// then each layer's geometry to its own offscreen texture, and composites
    /// them bottom-to-top through the composite shader honouring each layer's
    /// opacity + blend mode. The final layer composites straight to `target_view`.
    fn render_scene_isolated(
        &self,
        enc: &mut wgpu::CommandEncoder,
        msaa_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        w: u32,
        h: u32,
        vertices: &[Vertex],
        indices: &[u32],
    ) {
        use wgpu::util::DeviceExt;
        // Shared geometry, uploaded once and drawn as index sub-ranges per layer.
        let vbuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("iso_vbuf"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let ibuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("iso_ibuf"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        // Accumulator starts as the artboard-background quads over the window BG.
        let artboard_segs = [DrawSegment {
            mode: BlendMode::Normal,
            start: 0,
            count: self.artboard_idx_end,
        }];
        let mut acc_tex = self.make_fx_tex(w, h);
        let mut acc_view = acc_tex.create_view(&Default::default());
        self.record_range_pass(
            enc,
            msaa_view,
            &acc_view,
            &vbuf,
            &ibuf,
            &artboard_segs,
            self.scene_clear(),
        );

        let n = self.layer_runs.len();
        if n == 0 {
            // No shape geometry: pass the (opaque) artboard through to the target.
            self.composite_over(
                enc,
                target_view,
                &acc_view,
                &acc_view,
                blend_mode_index(BlendMode::Normal),
                1.0,
            );
            return;
        }

        for (i, run) in self.layer_runs.iter().enumerate() {
            // Draw this layer's nodes in isolation (transparent background), with
            // each node's own blend mode clipped to the layer's index range.
            let segs = clip_segments(&self.draw_segments, run.idx_start, run.idx_end);
            let shapes_tex = self.make_fx_tex(w, h);
            let shapes_view = shapes_tex.create_view(&Default::default());
            self.record_range_pass(
                enc,
                msaa_view,
                &shapes_view,
                &vbuf,
                &ibuf,
                &segs,
                wgpu::Color::TRANSPARENT,
            );

            // If this layer owns blur effects (drop shadow / object blur / feather),
            // blur them and place them *under* the shapes inside the layer's own
            // texture, so the effects composite at the layer's opacity + blend too.
            let has_fx = self
                .pending_blur_jobs
                .iter()
                .any(|j| j.layer_ordinal == run.ordinal && !j.idxs.is_empty());
            let combined_tex;
            let layer_view = if has_fx {
                let (_fx_tex, fx_view) = self.render_effects_layer(enc, w, h, Some(run.ordinal));
                combined_tex = self.make_fx_tex(w, h);
                let combined_view = combined_tex.create_view(&Default::default());
                self.stack_effects_under_shapes(enc, &combined_view, &fx_view, &shapes_view);
                combined_view
            } else {
                shapes_view
            };

            // Composite the isolated layer over the accumulator with the layer's
            // opacity + blend. The last layer writes straight to the target.
            let mode = blend_mode_index(run.blend);
            if i + 1 == n {
                self.composite_over(enc, target_view, &acc_view, &layer_view, mode, run.opacity);
            } else {
                let next_tex = self.make_fx_tex(w, h);
                let next_view = next_tex.create_view(&Default::default());
                self.composite_over(enc, &next_view, &acc_view, &layer_view, mode, run.opacity);
                acc_tex = next_tex;
                acc_view = next_view;
            }
        }
        let _ = acc_tex; // kept alive above; views own their GPU resource
    }

    /// Draw an index sub-range (given as clipped `DrawSegment`s) into
    /// `resolve_view` via the shared MSAA target, clearing to `clear` first.
    /// Empty `segments` produce a clear-only pass (an isolated layer with no
    /// drawable geometry must not fall back to drawing the whole buffer).
    fn record_range_pass(
        &self,
        enc: &mut wgpu::CommandEncoder,
        msaa_view: &wgpu::TextureView,
        resolve_view: &wgpu::TextureView,
        vbuf: &wgpu::Buffer,
        ibuf: &wgpu::Buffer,
        segments: &[DrawSegment],
        clear: wgpu::Color,
    ) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iso_layer_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: msaa_view,
                resolve_target: Some(resolve_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if segments.is_empty() {
            return; // clear-only
        }
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
        draw_segments(
            &mut pass,
            segments,
            &self.blend_pipelines,
            &self.fill_pipeline,
            0,
        );
    }

    /// Full-screen composite of `src` over `backdrop` into `dst` with the given
    /// blend-mode index + opacity (the composite shader writes finished pixels).
    fn composite_over(
        &self,
        enc: &mut wgpu::CommandEncoder,
        dst: &wgpu::TextureView,
        backdrop: &wgpu::TextureView,
        src: &wgpu::TextureView,
        mode: u32,
        opacity: f32,
    ) {
        use wgpu::util::DeviceExt;
        let params = CompositeParams {
            mode,
            opacity,
            _pad: [0.0, 0.0],
        };
        let params_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("composite_params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite_bg"),
            layout: &self.composite_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(backdrop),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.composite_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params_buf.as_entire_binding(),
                },
            ],
        });
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("composite_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
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
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// Clip per-node draw segments to the index sub-range `[start, end)`, truncating
/// segments that straddle the boundary. Used to draw one layer's geometry.
fn clip_segments(all: &[DrawSegment], start: u32, end: u32) -> Vec<DrawSegment> {
    all.iter()
        .filter_map(|seg| {
            let a = seg.start.max(start);
            let b = (seg.start + seg.count).min(end);
            (b > a).then(|| DrawSegment {
                mode: seg.mode,
                start: a,
                count: b - a,
            })
        })
        .collect()
}
