use super::*;

/// Clamp a requested surface size to the device's maximum 2D texture dimension.
///
/// wgpu panics when a surface — or any texture derived from it (our MSAA /
/// glow / effect targets) — exceeds `max_texture_dimension_2d`, and some
/// compositors momentarily report absurd sizes during monitor hotplug or
/// fractional-scale changes. Clamping (rather than panicking or dropping the
/// frame) keeps the canvas alive at the device's largest supported size.
pub(crate) fn clamp_surface_dims(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    (width.min(max_dim), height.min(max_dim))
}

impl PhotonicRenderer {
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        // Clamp before doing anything else: an oversized width/height would
        // panic `surface.configure` and every `create_*_texture` call below.
        // Clamping the stored dimensions too keeps the surface, the derived
        // textures, and the camera/view all in agreement.
        let (width, height) =
            clamp_surface_dims(width, height, self.device.limits().max_texture_dimension_2d);
        // No-op when the size is unchanged. Compositors (notably KWin on
        // multi-monitor / fractional scaling) can emit a stream of `Resized`
        // events carrying the *same* dimensions; reconfiguring the surface +
        // reallocating MSAA/glow textures on each one causes a relayout storm
        // (the timeline appears to resize endlessly) and needless surface
        // churn. Bailing here breaks that loop and only does real work on a
        // genuine size change. Compared post-clamp so a stream of oversized
        // events that all clamp to the same size also collapses to one resize.
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.view.screen_width = width;
        self.view.screen_height = height;
        self.surface_config.width = width;
        self.surface_config.height = height;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.surface_config);
        }
        // The MSAA / glow / scene targets are the document-pass sRGB `scene_format`
        // (03 §4.5.4), never the non-sRGB presentation `surface_format`.
        let (msaa_texture, msaa_view) =
            create_msaa_texture(&self.device, self.scene_format, width, height);
        self.msaa_texture = msaa_texture;
        self.msaa_view = msaa_view;
        let (glow_tex_a, glow_tex_a_view, glow_tex_b, glow_tex_b_view) =
            create_glow_textures(&self.device, self.scene_format, width, height);
        self.glow_tex_a = glow_tex_a;
        self.glow_tex_a_view = glow_tex_a_view;
        self.glow_tex_b = glow_tex_b;
        self.glow_tex_b_view = glow_tex_b_view;
        let (scene_tex, scene_view) =
            create_scene_texture(&self.device, self.scene_format, width, height);
        self.scene_tex = scene_tex;
        self.scene_view = scene_view;
    }

    /// Present the offscreen document target (03 §4.5.4, audit A-1) into a
    /// swapchain frame: a full-screen pass that samples the sRGB
    /// [`scene_view`](Self::scene_tex) — the sampler hardware-decodes it to linear
    /// — and writes the sRGB-encoded pixel (explicit `srgb_oetf` in the blit
    /// shader, since the `surface_format` view is NOT `*Srgb` and so gives no free
    /// encode) into `target`. The canvas therefore shows exactly what the sRGB
    /// document pass produced, matching the headless export path by construction.
    ///
    /// The document scene is opaque-over-BG, so no unpremultiply is needed here; if
    /// an `ExportBackground::Transparent` scene ever reaches this present path it
    /// must unpremultiply before the OETF.
    pub fn blit_scene_to_surface(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
    ) {
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit_bg"),
            layout: &self.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blit_scene_to_surface"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The quad is full-screen, but `canvas_scissor` may clip it
                    // to the GUI's canvas viewport — so the load *is* what shows
                    // outside that viewport. Clear to the window fill, which is
                    // what the gaps between the floating panel cards should be.
                    load: wgpu::LoadOp::Clear(super::SURFACE_BG),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.blit_pipeline);
        pass.set_bind_group(0, &bind, &[]);
        // Clip the document to the canvas viewport so it cannot bleed into the
        // panel gutters (see `PhotonicRenderer::canvas_scissor`). Clamped to the
        // surface: wgpu rejects a scissor that leaves the attachment.
        if let Some((x, y, w, h)) = self.canvas_scissor {
            let x = x.min(self.width);
            let y = y.min(self.height);
            let w = w.min(self.width - x);
            let h = h.min(self.height - y);
            if w > 0 && h > 0 {
                pass.set_scissor_rect(x, y, w, h);
            }
        }
        pass.draw(0..6, 0..1);
    }

    // ── Frame management ──────────────────────────────────────────────────────

    /// Push camera uniforms and build vertex/index data for this frame.
    pub fn update(&mut self) -> (Vec<Vertex>, Vec<u32>) {
        self.push_camera();
        let (verts, idxs) = self.build_geometry();
        // Size the persistent document buffers to this frame's geometry before
        // the render pass (which only has `&self`) uploads into them (03 §2.3).
        self.ensure_doc_buffers(
            std::mem::size_of_val(verts.as_slice()) as u64,
            std::mem::size_of_val(idxs.as_slice()) as u64,
        );
        (verts, idxs)
    }

    /// Acquire the swapchain frame and record the document render pass into
    /// `FrameHandle::encoder`. Returns `None` on a transient timeout and an
    /// error only when the surface stays invalid after a reconfigure + retry.
    ///
    /// Callers may append further render passes (e.g. egui) to `handle.encoder`
    /// before calling `finish_frame`.
    pub fn begin_frame(
        &self,
        vertices: &[Vertex],
        indices: &[u32],
    ) -> anyhow::Result<Option<FrameHandle>> {
        // Windowless (offscreen) renderers never present — see `new_offscreen`.
        let Some(surface) = self.surface.as_ref() else {
            return Ok(None);
        };
        let surface_texture = match surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                // The compositor invalidated the swapchain (a resize, DPI/scale
                // change, or the window moving to another monitor). This is
                // routine and recoverable: reconfigure with the current config
                // — `surface_config` is kept current by `resize`, and its
                // `present_mode` was validated against this adapter's caps at
                // construction (see `choose_surface_config`; caps are stable for
                // the life of an adapter+surface) — then retry acquisition once.
                surface.configure(&self.device, &self.surface_config);
                match surface.get_current_texture() {
                    Ok(t) => t,
                    // Still not ready after reconfigure — treat a timeout as a
                    // skipped frame rather than a hard error.
                    Err(wgpu::SurfaceError::Timeout) => return Ok(None),
                    Err(e) => {
                        // A surface that stays invalid after a reconfigure+retry
                        // is not the routine Lost/Outdated case — treat it as a
                        // possible device loss and flag the shared health machine
                        // (37 §1.3) before bubbling the error.
                        self.gpu_health.mark_lost();
                        return Err(anyhow::anyhow!(
                            "window surface still invalid after reconfigure: {e:?}"
                        ));
                    }
                }
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(None),
            Err(e) => {
                self.gpu_health.mark_lost();
                return Err(anyhow::anyhow!("failed to acquire window surface: {e:?}"));
            }
        };
        let view = surface_texture.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame_encoder"),
            });
        // Render the document into the offscreen sRGB scene target (linear-light
        // blending, matching headless), then present it into the swapchain frame
        // (03 §4.5.4). egui still paints on top into `handle.view` afterwards.
        self.render_scene(
            &mut encoder,
            &self.msaa_view,
            &self.scene_view,
            self.width,
            self.height,
            vertices,
            indices,
        );
        self.blit_scene_to_surface(&mut encoder, &view);
        Ok(Some(FrameHandle {
            surface_texture,
            view,
            encoder,
        }))
    }

    /// Submit the frame encoder and present the surface texture.
    pub fn finish_frame(&self, handle: FrameHandle) {
        self.queue.submit([handle.encoder.finish()]);
        handle.surface_texture.present();
    }
}

#[cfg(test)]
mod tests {
    use super::clamp_surface_dims;

    #[test]
    fn clamps_each_axis_independently_to_the_device_limit() {
        let max = 8192;
        // Within bounds is untouched.
        assert_eq!(clamp_surface_dims(1920, 1080, max), (1920, 1080));
        // Exactly at the limit is kept.
        assert_eq!(clamp_surface_dims(max, max, max), (max, max));
        // Each axis clamps on its own; a bogus oversized value can't panic
        // surface.configure or the derived MSAA/glow texture allocations.
        assert_eq!(clamp_surface_dims(100_000, 1080, max), (max, 1080));
        assert_eq!(clamp_surface_dims(1920, 100_000, max), (1920, max));
        assert_eq!(clamp_surface_dims(u32::MAX, u32::MAX, max), (max, max));
    }
}
