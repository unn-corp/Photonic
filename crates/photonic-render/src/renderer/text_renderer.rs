use super::*;

impl PhotonicRenderer {
    /// Render any text nodes collected during `update()` into the frame.
    ///
    /// Call this after `begin_frame` and before the egui pass.
    /// No-op if there are no text nodes this frame.
    pub fn render_text_pass(&mut self, frame: &mut FrameHandle) {
        if self.pending_texts.is_empty() {
            return;
        }

        // Update the glyphon viewport to the current screen resolution.
        self.text_viewport.update(
            &self.queue,
            Resolution {
                width: self.width,
                height: self.height,
            },
        );

        // Build one glyphon Buffer per text node and collect TextAreas.
        let snapshots = &self.pending_texts;
        let mut buffers: Vec<Buffer> = Vec::with_capacity(snapshots.len());
        for snap in snapshots.iter() {
            let font_size = snap.font_size.max(1.0);
            let font_style = match snap.font_style {
                1 => photonic_core::node::FontStyle::Italic,
                2 => photonic_core::node::FontStyle::Oblique,
                _ => photonic_core::node::FontStyle::Normal,
            };
            let buf = crate::text_layout::layout_text_buffer(
                &mut self.font_system,
                &snap.content,
                &snap.font_family,
                font_size,
                crate::TextLayoutOptions {
                    font_weight: snap.font_weight,
                    font_style,
                    line_height_mul: snap.line_height_mul,
                    letter_spacing: snap.letter_spacing,
                    vertical: snap.vertical,
                },
            );
            buffers.push(buf);
        }

        let text_areas: Vec<TextArea> = snapshots
            .iter()
            .zip(buffers.iter())
            .map(|(snap, buf)| TextArea {
                buffer: buf,
                left: snap.screen_x,
                top: snap.screen_y + snap.top_offset,
                scale: 1.0,
                bounds: TextBounds {
                    left: i32::MIN,
                    top: i32::MIN,
                    right: i32::MAX,
                    bottom: i32::MAX,
                },
                default_color: GlyphonColor::rgba(
                    snap.color[0],
                    snap.color[1],
                    snap.color[2],
                    snap.color[3],
                ),
                custom_glyphs: &[],
            })
            .collect();

        if let Err(e) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.text_atlas,
            &self.text_viewport,
            text_areas,
            &mut self.swash_cache,
        ) {
            tracing::warn!("glyphon prepare failed: {:?}", e);
            return;
        }

        {
            let mut pass = frame
                .encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("text_pass"),
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
                })
                .forget_lifetime();

            if let Err(e) =
                self.text_renderer
                    .render(&self.text_atlas, &self.text_viewport, &mut pass)
            {
                tracing::warn!("glyphon render failed: {:?}", e);
            }
        }

        self.text_atlas.trim();
    }
}
