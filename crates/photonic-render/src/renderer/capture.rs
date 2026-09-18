use super::*;

impl PhotonicRenderer {
    /// Poll the capture channel and service any pending screenshot requests.
    pub fn service_captures(&mut self, vertices: &[Vertex], indices: &[u32]) {
        while let Ok(reply_tx) = self.capture_rx.try_recv() {
            tracing::info!(
                "render: capture_png starting ({}x{})",
                self.width,
                self.height
            );
            let png = self.capture_png(vertices, indices);
            tracing::info!(
                "render: capture_png done ({} bytes) — sending reply",
                png.len()
            );
            let _ = reply_tx.send(png);
            tracing::info!("render: capture reply sent — render loop resuming");
        }
    }

    /// Build this frame's geometry and render it to an offscreen texture, returning
    /// the RGBA8 PNG bytes. Drives the real windowed pipeline (`render_scene`) with
    /// no surface — used by headless captures and tests.
    #[allow(dead_code)] // test/headless helper — unused in a plain release build
    pub(crate) fn render_capture(&mut self) -> Vec<u8> {
        let (verts, idxs) = self.update();
        self.capture_png(&verts, &idxs)
    }

    /// Render to an offscreen texture, read back pixels, encode as PNG. Thin
    /// wrapper over [`Self::capture_rgba`] (the shared draw + readback path).
    pub(crate) fn capture_png(&mut self, vertices: &[Vertex], indices: &[u32]) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let pixels = self.capture_rgba(vertices, indices);
        if pixels.is_empty() {
            return vec![];
        }
        let img: ImageBuffer<Rgba<u8>, _> =
            ImageBuffer::from_raw(w, h, pixels).unwrap_or_else(|| ImageBuffer::new(w, h));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap_or_default();
        png
    }

    /// Render to an offscreen texture and read the raw RGBA8 pixels back. This is
    /// the exact editor draw path (`render_scene` + the glyphon text pass), so any
    /// caller — the interactive screenshot ([`capture_png`](Self::capture_png)) and
    /// PNG/artboard export ([`render_export_rgba`](Self::render_export_rgba)) —
    /// produces pixel-identical output. Raster nodes are composited here as the
    /// final shared step because they are CPU-painted over the GPU vector result.
    /// Returns an empty vec on readback failure.
    pub(crate) fn capture_rgba(&mut self, vertices: &[Vertex], indices: &[u32]) -> Vec<u8> {
        let w = self.width;
        let h = self.height;
        // Offscreen capture reuses the persistent document buffers; ensure they
        // fit these vertices/indices before `record_document_pass` writes them.
        self.ensure_doc_buffers(
            std::mem::size_of_val(vertices) as u64,
            std::mem::size_of_val(indices) as u64,
        );

        // Offscreen resolve target (single-sample, read back as PNG)
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture_tex"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Offscreen capture shares the document-pass pipelines, which target
            // the sRGB `scene_format` (03 §4.5.4); the capture + MSAA targets and
            // the text pass therefore MUST be `scene_format` too, or wgpu panics on
            // a colour-target format mismatch. Reading back this sRGB texture yields
            // the same encoded bytes the headless export path produces.
            format: self.scene_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let tex_view = tex.create_view(&Default::default());

        // MSAA render target for the capture (resolved into tex_view)
        let (capture_msaa_tex, capture_msaa_view) =
            create_msaa_texture(&self.device, self.scene_format, w, h);

        // Draw geometry into the offscreen texture via MSAA (with effects layer).
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.render_scene(
            &mut enc,
            &capture_msaa_view,
            &tex_view,
            w,
            h,
            vertices,
            indices,
        );

        // Render text nodes on top (same encoder, loads resolved geometry from tex_view)
        if !self.pending_texts.is_empty() {
            self.text_viewport.update(
                &self.queue,
                Resolution {
                    width: w,
                    height: h,
                },
            );

            let mut buffers: Vec<Buffer> = Vec::with_capacity(self.pending_texts.len());
            for snap in self.pending_texts.iter() {
                let font_size = snap.font_size.max(1.0);
                let line_height = font_size * snap.line_height_mul;
                let mut buf =
                    Buffer::new(&mut self.font_system, Metrics::new(font_size, line_height));
                buf.set_size(&mut self.font_system, None, None);
                let glyph_style = match snap.font_style {
                    1 => GlyphonStyle::Italic,
                    2 => GlyphonStyle::Oblique,
                    _ => GlyphonStyle::Normal,
                };
                let attrs = Attrs::new()
                    .family(crate::text_outline::cosmic_family(&snap.font_family))
                    .weight(Weight(snap.font_weight))
                    .style(glyph_style);
                buf.set_text(
                    &mut self.font_system,
                    &snap.content,
                    attrs,
                    Shaping::Advanced,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
                buffers.push(buf);
            }

            let text_areas: Vec<TextArea> = self
                .pending_texts
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

            if self
                .text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.text_atlas,
                    &self.text_viewport,
                    text_areas,
                    &mut self.swash_cache,
                )
                .is_ok()
            {
                {
                    let mut pass = enc
                        .begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("capture_text_pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &tex_view,
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
                        tracing::warn!("glyphon render in capture failed: {:?}", e);
                    }
                }
                self.text_atlas.trim();
            }
        }

        self.queue.submit([enc.finish()]);
        drop(capture_msaa_tex); // keep alive until after submit

        // Copy texture → staging buffer (bytes_per_row must be aligned to 256)
        let bpr = align256(w * 4);
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut enc2 = self.device.create_command_encoder(&Default::default());
        enc2.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc2.finish()]);

        // Map & read
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        tracing::info!("render: capture_png — poll(Wait) starting");
        self.device.poll(wgpu::Maintain::Wait);
        tracing::info!("render: capture_png — poll(Wait) done");
        if rx.recv().ok().and_then(|r| r.ok()).is_none() {
            tracing::warn!("render: capture_png — map_async failed");
            return vec![];
        }

        let raw = slice.get_mapped_range();

        // Capture now reads back the sRGB `scene_format` (03 §4.5.4), which is a
        // fixed `Rgba8UnormSrgb` — never a BGRA order — so there is no channel swap.
        let is_bgra = matches!(
            self.scene_format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );

        let mut pixels: Vec<u8> = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * bpr) as usize;
            let end = start + (w * 4) as usize;
            if is_bgra {
                for px in raw[start..end].chunks_exact(4) {
                    pixels.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            } else {
                pixels.extend_from_slice(&raw[start..end]);
            }
        }
        drop(raw);
        staging.unmap();

        // Raster/pixel layers are painted after the GPU vector and text passes.
        // Keep this in the common capture path so an editor screenshot cannot
        // omit nodes that artboard export includes.
        {
            let document = self.document.blocking_lock();
            crate::headless::composite_raster_nodes(&mut pixels, w, h, &document, &self.view);
        }

        pixels
    }

    /// Render an arbitrary `document` to an RGBA8 buffer (`(pixels, w, h)`) through
    /// the editor's exact pipeline, surfacelessly — the WYSIWYG export entry point.
    ///
    /// The document is installed into this renderer, the viewport is fit to
    /// `opts.region` (or the whole document) with **no margin** so the region maps
    /// 1:1 onto the `w×h` output, and `opts.background` selects the white artboard
    /// or full transparency. Because it shares `render_scene` + the glyphon text
    /// pass with the on-canvas frame, text layout, gradient shading, stroke width
    /// and opacity are identical to what the editor shows. Raster (pixel) nodes
    /// are composited by the shared capture path, so screenshot and export cannot
    /// diverge. `opts.crop_to_content`, `ico_sizes`, `jpeg_quality` and
    /// `overprint_preview` are not consulted here. Empty pixels on readback failure.
    pub fn render_export_rgba(
        &mut self,
        document: &Document,
        w: u32,
        h: u32,
        opts: &crate::headless::ExportOptions,
    ) -> (Vec<u8>, u32, u32) {
        let w = w.max(1);
        let h = h.max(1);

        // Install the document to render (the editor build path reads it back out
        // of `self.document` under a short lock).
        {
            let mut guard = self.document.blocking_lock();
            *guard = document.clone();
        }

        if w != self.width || h != self.height {
            self.resize(w, h);
        }

        // Exact (marginless) fit of the export region onto the output.
        let (rx, ry, rw, rh) = opts
            .region
            .unwrap_or((0.0, 0.0, document.width, document.height));
        self.view.screen_width = w;
        self.view.screen_height = h;
        self.view.fit_to_rect_exact(rx, ry, rw, rh);

        // Steer the shared draw path for export, render, then always restore the
        // normal editor mode so a later interactive frame is unaffected.
        self.export_bg = Some(opts.background);
        let (verts, idxs) = self.update();
        let pixels = self.capture_rgba(&verts, &idxs);
        self.export_bg = None;

        if pixels.is_empty() {
            return (vec![], w, h);
        }

        (pixels, w, h)
    }
}

#[cfg(test)]
mod offscreen_tests {
    use super::*;
    use photonic_core::{
        color::Color,
        node::{PathNode, SceneNode, SceneNodeKind},
        path::PathData,
        style::Fill,
        Document,
    };

    /// Build a windowless renderer over `doc` at `w×h`, or `None` if this machine
    /// has no GPU adapter (so tests skip cleanly on headless CI).
    pub(crate) fn offscreen(doc: Document, w: u32, h: u32) -> Option<PhotonicRenderer> {
        // The capture channel is unused here (we call `render_capture` directly),
        // so dropping the sender is harmless.
        let (_tx, rx) = std::sync::mpsc::channel();
        pollster::block_on(PhotonicRenderer::new_offscreen(
            w,
            h,
            std::sync::Arc::new(tokio::sync::Mutex::new(doc)),
            rx,
        ))
    }

    /// WYSIWYG parity (the whole point of routing export through this renderer):
    /// the SAME document rendered via the editor's on-canvas path
    /// (`render_capture`) and via the export path (`render_export_rgba`) must be
    /// pixel-identical. The document exercises the three historic divergence points
    /// — glyphon **text** in an installed font, a **linear gradient** fill, and a
    /// thin **faint grid stroke** (low opacity + sub-pixel width) — all of which the
    /// retired HeadlessRenderer drew differently. Both paths share one FontSystem
    /// (same `FontSystem::new()` DB, including `~/.local/share/fonts` on Linux) and
    /// one draw pipeline, so the images match to the byte.
    #[test]
    fn editor_and_export_render_are_pixel_identical() {
        use crate::headless::{ExportBackground, ExportOptions};
        use photonic_core::{
            node::TextNode,
            style::{Fill, Gradient, GradientStop, Stroke},
            transform::Transform,
        };

        const W: u32 = 220;
        const H: u32 = 140;

        let mut doc = Document::new("parity", W as f64, H as f64);
        let lid = doc.active_layer_id.unwrap();

        // 1) Linear-gradient-filled rect spanning most of the artboard.
        let grad = Gradient::linear(
            10.0,
            0.0,
            (W - 10) as f64,
            0.0,
            vec![
                GradientStop::new(0.0, Color::new(0.90, 0.20, 0.20, 1.0)),
                GradientStop::new(1.0, Color::new(0.20, 0.35, 0.95, 1.0)),
            ],
        );
        let rect = SceneNode::new(
            "grad",
            lid,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(10.0, 10.0, (W - 20) as f64, (H - 50) as f64))
                    .with_fill(Fill::gradient(grad)),
            ),
        );
        doc.add_node(rect, None);

        // 2) Thin, faint grid stroke — hairline lines at 25% opacity.
        let grid = PathData::from_svg(
            "M 10 40 L 210 40 M 10 60 L 210 60 M 60 10 L 60 90 M 160 10 L 160 90",
        )
        .unwrap();
        let mut grid_stroke = Stroke::solid(Color::new(0.05, 0.05, 0.08, 1.0), 0.75);
        grid_stroke.opacity = 0.25;
        let grid_node = SceneNode::new(
            "grid",
            lid,
            SceneNodeKind::Path(
                PathNode::new(grid)
                    .with_fill(Fill::none())
                    .with_stroke(grid_stroke),
            ),
        );
        doc.add_node(grid_node, None);

        // 3) A text node (installed-font glyphon layout) low on the artboard.
        let mut tn = TextNode::new("Wave");
        tn.font_size = 28.0;
        tn.font_weight = 700;
        tn.fill = Fill::solid(Color::new(0.05, 0.05, 0.05, 1.0));
        let text = SceneNode::new("text", lid, SceneNodeKind::Text(tn))
            .with_transform(Transform::translate(16.0, 98.0));
        doc.add_node(text, None);

        let Some(mut r) = offscreen(doc.clone(), W, H) else {
            eprintln!("no GPU adapter — skipping WYSIWYG parity test");
            return;
        };

        // Editor path: fit the artboard exactly (marginless) and capture, exactly
        // as the export path frames it, then decode the PNG back to RGBA.
        r.view.screen_width = W;
        r.view.screen_height = H;
        r.view.fit_to_rect_exact(0.0, 0.0, W as f64, H as f64);
        let editor_png = r.render_capture();
        let editor = image::load_from_memory(&editor_png)
            .expect("editor png")
            .to_rgba8();

        // Export path: the surfaceless WYSIWYG entry point.
        let opts = ExportOptions {
            background: ExportBackground::Artboard,
            region: Some((0.0, 0.0, W as f64, H as f64)),
            ..Default::default()
        };
        let (export_px, ew, eh) = r.render_export_rgba(&doc, W, H, &opts);
        assert_eq!((ew, eh), (W, H), "export must honor the requested size");
        let export = image::RgbaImage::from_raw(ew, eh, export_px).expect("export rgba");

        // Sanity: the render is not blank — the gradient produced distinct colours
        // across the artboard (left redder than right).
        let left = editor.get_pixel(20, 30).0;
        let right = editor.get_pixel(W - 20, 30).0;
        assert!(
            left[0] as i32 - left[2] as i32 > 30 && right[2] as i32 - right[0] as i32 > 30,
            "gradient sanity failed: left {left:?} right {right:?}"
        );

        // Whole-image parity: every pixel identical within a 1/255 tolerance for
        // any GPU rounding jitter (in practice the two are bit-identical).
        let mut worst = 0i32;
        let mut mismatches = 0u64;
        for (pe, px) in editor.pixels().zip(export.pixels()) {
            for c in 0..4 {
                let d = (pe.0[c] as i32 - px.0[c] as i32).abs();
                worst = worst.max(d);
                if d > 1 {
                    mismatches += 1;
                }
            }
        }
        assert_eq!(
            mismatches, 0,
            "editor vs export diverged at {mismatches} channel(s), worst Δ={worst}. \
             Export is no longer pixel-identical to the editor."
        );

        // Explicit gradient-pixel parity at sampled points (subsumed by the above,
        // stated separately because gradient quality was a known divergence).
        for &(x, y) in &[(20u32, 30u32), (110, 30), (W - 20, 30)] {
            assert_eq!(
                editor.get_pixel(x, y).0,
                export.get_pixel(x, y).0,
                "gradient pixel ({x},{y}) differs between editor and export"
            );
        }

        // Explicit text-region parity: the glyph band (letter-spacing + advances)
        // must be present and identical. Count dark glyph pixels in the text band
        // in both images — equal, and non-zero (text actually rendered in both).
        let dark = |img: &image::RgbaImage| -> u64 {
            let mut n = 0;
            for y in 95..135u32 {
                for x in 12..208u32 {
                    let p = img.get_pixel(x, y).0;
                    if p[0] < 120 && p[1] < 120 && p[2] < 120 && p[3] > 200 {
                        n += 1;
                    }
                }
            }
            n
        };
        let (te, tx) = (dark(&editor), dark(&export));
        assert!(te > 20, "text did not render in the editor image ({te} px)");
        assert_eq!(
            te, tx,
            "glyph coverage differs (editor {te} vs export {tx}) — text layout diverged"
        );
    }

    /// #233: screenshots must use the same raster compositing step as artboard
    /// export. The placed red image has a half-width mask, exercising both image
    /// placement and mask coverage rather than only the GPU vector path.
    #[test]
    fn screenshot_and_export_both_render_masked_raster() {
        use crate::headless::{ExportBackground, ExportOptions};
        use photonic_core::{
            node::RasterNode, raster::image::RasterImage, transform::Transform, Mask,
        };

        const W: u32 = 64;
        const H: u32 = 48;
        let mut doc = Document::new("raster parity", W as f64, H as f64);
        let layer_id = doc.active_layer_id.unwrap();
        let image = RasterImage::from_rgba(24, 20, [255, 0, 0, 255].repeat(24 * 20))
            .expect("valid raster image");
        let mut raster = RasterNode::new(image);
        raster.mask = Some(Mask::rect(24, 20, 0, 0, 12, 20));
        doc.add_node(
            SceneNode::new("placed raster", layer_id, SceneNodeKind::Raster(raster))
                .with_transform(Transform::translate(16.0, 12.0)),
            None,
        );

        let Some(mut renderer) = offscreen(doc.clone(), W, H) else {
            eprintln!("no GPU adapter — skipping masked-raster screenshot parity test");
            return;
        };
        renderer.view.screen_width = W;
        renderer.view.screen_height = H;
        renderer
            .view
            .fit_to_rect_exact(0.0, 0.0, W as f64, H as f64);
        let screenshot = image::load_from_memory(&renderer.render_capture())
            .expect("screenshot png")
            .to_rgba8();

        let opts = ExportOptions {
            background: ExportBackground::Artboard,
            region: Some((0.0, 0.0, W as f64, H as f64)),
            ..Default::default()
        };
        let (pixels, ew, eh) = renderer.render_export_rgba(&doc, W, H, &opts);
        let export = image::RgbaImage::from_raw(ew, eh, pixels).expect("export rgba");

        let visible_raster = screenshot.get_pixel(20, 20).0;
        assert!(
            visible_raster[0] > 220 && visible_raster[1] < 60 && visible_raster[2] < 60,
            "screenshot omitted the visible red raster pixels: {visible_raster:?}"
        );
        assert_eq!(
            screenshot.get_pixel(34, 20).0,
            [255, 255, 255, 255],
            "screenshot must retain the artboard behind the masked-out raster pixels"
        );
        assert_eq!(
            screenshot.as_raw(),
            export.as_raw(),
            "screenshot and export diverged for a placed masked raster"
        );
    }

    /// Live-context regression: TWO `PhotonicRenderer`s sharing ONE GPU device in
    /// ONE process must both render — the scenario that crashed the running GUI when
    /// export created a *second* wgpu device alongside the windowed one. Here the
    /// first renderer stands in for the live windowed renderer; the second is built
    /// via [`PhotonicRenderer::new_offscreen_shared`] from the first's shared
    /// `device_arc()`/`queue_arc()` (exactly what `register_export_gpu` hands the MCP
    /// export path). Both produce pixels, and the shared export render never disturbs
    /// the "live" renderer's own output.
    #[test]
    fn shared_device_export_renderer_coexists_with_live_renderer() {
        use crate::headless::{ExportBackground, ExportOptions};

        const W: u32 = 40;
        const H: u32 = 40;

        // A blue rect filling the artboard, so both renders are unambiguously non-blank.
        let mut doc = Document::new("shared", W as f64, H as f64);
        let node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, W as f64, H as f64))
                    .with_fill(Fill::solid(Color::new(0.10, 0.30, 0.90, 1.0))),
            ),
        );
        doc.add_node(node, None);

        // Renderer #1 — stands in for the live windowed renderer (owns the device).
        let Some(mut live) = offscreen(doc.clone(), W, H) else {
            eprintln!("no GPU adapter — skipping shared-device test");
            return;
        };

        // Renderer #2 — the export renderer, built on the SAME device/queue (no
        // second wgpu device is created). Its document/size/view are isolated.
        let (_tx, rx) = std::sync::mpsc::channel();
        let export_doc = std::sync::Arc::new(tokio::sync::Mutex::new(Document::new("x", 1.0, 1.0)));
        let mut export = PhotonicRenderer::new_offscreen_shared(
            live.device_arc(),
            live.queue_arc(),
            16,
            16,
            export_doc,
            rx,
        );

        let opts = ExportOptions {
            background: ExportBackground::Artboard,
            region: Some((0.0, 0.0, W as f64, H as f64)),
            ..Default::default()
        };

        // The export renderer renders the doc through the shared device.
        let (px, ew, eh) = export.render_export_rgba(&doc, W, H, &opts);
        assert_eq!(
            (ew, eh),
            (W, H),
            "shared export must honor the requested size"
        );
        let img = image::RgbaImage::from_raw(ew, eh, px).expect("shared export rgba");
        let center = img.get_pixel(W / 2, H / 2).0;
        assert!(
            center[2] > 150 && center[3] > 200,
            "shared-device export produced no pixels: center={center:?}"
        );

        // The "live" renderer still renders correctly AFTER the shared export ran —
        // sharing the device did not corrupt or lose it.
        live.view.screen_width = W;
        live.view.screen_height = H;
        live.view.fit_to_rect_exact(0.0, 0.0, W as f64, H as f64);
        let live_png = live.render_capture();
        let live_img = image::load_from_memory(&live_png)
            .expect("live png")
            .to_rgba8();
        let lc = live_img.get_pixel(W / 2, H / 2).0;
        assert!(
            lc[2] > 150 && lc[3] > 200,
            "live renderer broke after shared export: center={lc:?}"
        );
    }

    /// A-1 invariant (03 §4.5.4): the offscreen document target — the format the
    /// fill/blend/composite pass renders and reads back in — is the sRGB
    /// `SCENE_FORMAT`, never the non-sRGB presentation surface. This is what makes
    /// on-canvas blending run in linear light and match the headless export path;
    /// a real assertion on the wired-up renderer, not a stdlib property check.
    #[test]
    fn document_target_is_srgb() {
        let doc = Document::new("srgb_target", 8.0, 8.0);
        let Some(r) = offscreen(doc, 8, 8) else {
            eprintln!("no GPU adapter — skipping document_target_is_srgb");
            return;
        };
        assert_eq!(
            r.scene_format(),
            crate::pipeline::SCENE_FORMAT,
            "document pass must target the sRGB SCENE_FORMAT",
        );
        assert!(
            r.scene_format().is_srgb(),
            "scene_format must be sRGB so document blending runs in linear light",
        );
    }

    /// Stage 0 smoke test: the windowless renderer drives the real GPU pipeline
    /// and reads pixels back. A red rect filling a 20×20 doc → red at the centre.
    #[test]
    fn offscreen_renderer_reads_back_a_shape() {
        let mut doc = Document::new("smoke", 20.0, 20.0);
        let node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 20.0, 20.0))
                    .with_fill(Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0))),
            ),
        );
        doc.add_node(node, None);
        let Some(mut r) = offscreen(doc, 20, 20) else {
            eprintln!("no GPU adapter — skipping offscreen smoke test");
            return;
        };
        let png = r.render_capture();
        let img = image::load_from_memory(&png).expect("png").to_rgba8();
        let p = img.get_pixel(10, 10).0;
        assert!(
            p[0] > 180 && p[1] < 70 && p[2] < 70,
            "offscreen capture should show the red rect at centre, got {p:?}"
        );
    }

    /// Regression: the SAME `Color` must render identically as glyphon **text**
    /// and as a **vector path fill**. Before the fix the text atlas used glyphon's
    /// default `ColorMode::Accurate`, which linearizes the sRGB colour before
    /// writing to the sRGB target, while vector fills pass sRGB through raw
    /// (`pipeline.rs` `fs_main` → `return in.color;`). A mid-tone like `#C8A24B`
    /// therefore looked visibly different as text vs. as a shape. Constructing the
    /// atlas in `ColorMode::Web` (renderer/mod.rs) makes glyphon match the vector
    /// pipeline. This renders a golden rect and golden text of the SAME colour and
    /// asserts the fully-covered glyph pixels equal the rect pixels within 2/255.
    #[test]
    fn text_colour_matches_vector_fill_for_same_colour() {
        use photonic_core::{node::TextNode, style::Fill, transform::Transform};
        // #C8A24B — a mid-tone where the sRGB-vs-linear gap is largest.
        let gold = Color::new(200.0 / 255.0, 162.0 / 255.0, 75.0 / 255.0, 1.0);

        let mut doc = Document::new("colour_match", 240.0, 120.0);
        let lid = doc.active_layer_id.unwrap();

        // Vector reference: a filled rect on the left half.
        let rect = SceneNode::new(
            "rect",
            lid,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 80.0, 120.0)).with_fill(Fill::solid(gold)),
            ),
        );
        doc.add_node(rect, None);

        // Text of the SAME colour on the right half. A heavy weight at a large size
        // gives thick, fully-covered glyph interiors to sample.
        let mut tn = TextNode::new("B");
        tn.font_size = 80.0;
        tn.font_weight = 900;
        tn.fill = Fill::solid(gold);
        let text = SceneNode::new("text", lid, SceneNodeKind::Text(tn))
            .with_transform(Transform::translate(120.0, 25.0));
        doc.add_node(text, None);

        let Some(mut r) = offscreen(doc, 240, 120) else {
            eprintln!("no GPU adapter — skipping text/vector colour-match test");
            return;
        };
        let img = image::load_from_memory(&r.render_capture())
            .expect("png")
            .to_rgba8();

        // Reference vector colour: a point solidly inside the rect. `fit_to_rect`
        // maps doc→screen as scale 0.9 + (12,6) offset, so doc(40,60) ≈ (48,60).
        let e = img.get_pixel(48, 60).0;
        assert!(
            e[0] > 120 && e[1] > 90 && e[2] < 150,
            "sanity: rect sample should be golden, got {e:?}"
        );

        // The fully-covered interior of the glyph writes the text colour at 100%
        // coverage. Scan the text region (kept strictly inside the artboard, well
        // right of the rect and clear of the dark canvas margin) for the pixel
        // closest to the vector fill `e`, and report that closest match.
        let dist2 = |p: [u8; 4], q: [u8; 4]| {
            let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
            d(p[0], q[0]) + d(p[1], q[1]) + d(p[2], q[2])
        };
        let mut best = [0u8, 0, 0, 0];
        let mut best_d = i32::MAX;
        for y in 12..112u32 {
            for x in 95..224u32 {
                let p = img.get_pixel(x, y).0;
                let dd = dist2(p, e);
                if dd < best_d {
                    best_d = dd;
                    best = p;
                }
            }
        }

        // The best-covered glyph pixel must equal the vector fill within 2/255.
        // Before the fix (ColorMode::Accurate) the glyph interior is the linearized
        // colour and no text pixel lands this close to the raw-sRGB vector fill.
        let near = (best[0] as i32 - e[0] as i32).abs() <= 2
            && (best[1] as i32 - e[1] as i32).abs() <= 2
            && (best[2] as i32 - e[2] as i32).abs() <= 2;
        assert!(
            near,
            "closest glyphon text pixel {best:?} must match vector fill {e:?} within \
             2/255 (expected with ColorMode::Web). A large gap means the atlas \
             linearized the text colour (ColorMode::Accurate)."
        );
    }

    /// Stage 1 (#226): a layer at 50% opacity composites as an **isolated unit**.
    /// Two overlapping opaque rects (red under, blue over) live in one 50%-opacity
    /// layer over a white artboard. Correct per-layer isolation makes the overlap
    /// = blue@50% over white; the *old per-node fold* would double the layer
    /// opacity there. This pins the isolation fix.
    ///
    /// A-1 (03 §4.5.4): the document pass now composites in the sRGB
    /// `scene_format`, so the 50% source-over runs in **linear light** — the
    /// composite shader samples the sRGB layer/backdrop as linear, mixes 50/50 in
    /// linear, and the sRGB store re-encodes: blue@50% over white is
    /// `srgb_oetf(0.5)` ≈ **188** (not the pre-fix gamma-space 128). The isolation
    /// invariant (overlap == blue-only, red kept separate) is unchanged; only the
    /// blend space moved to linear per spec.
    #[test]
    fn half_opacity_layer_composites_as_isolated_unit() {
        let mut doc = Document::new("iso", 20.0, 20.0);
        let lid = doc.active_layer_id.unwrap();
        let red = SceneNode::new(
            "red",
            lid,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 14.0, 14.0))
                    .with_fill(Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0))),
            ),
        );
        let blue = SceneNode::new(
            "blue",
            lid,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(6.0, 6.0, 14.0, 14.0))
                    .with_fill(Fill::solid(Color::new(0.0, 0.0, 1.0, 1.0))),
            ),
        );
        doc.add_node(red, None);
        doc.add_node(blue, None);
        doc.layers.get_mut(&lid).unwrap().opacity = 0.5;

        let Some(mut r) = offscreen(doc, 20, 20) else {
            eprintln!("no GPU adapter — skipping isolation test");
            return;
        };
        let img = image::load_from_memory(&r.render_capture())
            .expect("png")
            .to_rgba8();
        let near = |p: [u8; 4], e: [u8; 3]| {
            (p[0] as i32 - e[0] as i32).abs() < 14
                && (p[1] as i32 - e[1] as i32).abs() < 14
                && (p[2] as i32 - e[2] as i32).abs() < 14
        };
        let overlap = img.get_pixel(10, 10).0; // blue over red → blue on top
        let red_only = img.get_pixel(3, 3).0;
        let blue_only = img.get_pixel(17, 17).0;
        // Linear-light 50/50 over white → srgb_oetf(0.5) ≈ 188 (A-1, §4.5.4).
        assert!(
            near(overlap, [188, 188, 255]),
            "overlap must be blue@50% over white in linear light (isolated), got {overlap:?}"
        );
        assert!(
            near(red_only, [255, 188, 188]),
            "red-only region must be red@50% over white in linear light, got {red_only:?}"
        );
        assert!(
            near(blue_only, [188, 188, 255]),
            "blue-only region must be blue@50% over white in linear light, got {blue_only:?}"
        );
    }

    /// Stage 2 (#226): a non-trivial layer that *also* carries a per-node blur
    /// effect still composites as an isolated unit. Same doc as the isolation test
    /// but the blue rect has a drop shadow. Under Stage 1 this doc fell back to the
    /// flat effects path (no layer isolation) and the overlap would be opaque blue;
    /// with Stage 2 the layer (shadow + shapes) composites at 50%, so the overlap
    /// is still blue@50% over white — in linear light ≈ (188,188,255) post-A-1
    /// (03 §4.5.4, was gamma-space 128 before the sRGB document target).
    #[test]
    fn nontrivial_layer_with_drop_shadow_stays_isolated() {
        use photonic_core::node::DropShadow;
        let mut doc = Document::new("iso_fx", 20.0, 20.0);
        let lid = doc.active_layer_id.unwrap();
        let red = SceneNode::new(
            "red",
            lid,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 14.0, 14.0))
                    .with_fill(Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0))),
            ),
        );
        let mut blue = SceneNode::new(
            "blue",
            lid,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(6.0, 6.0, 14.0, 14.0))
                    .with_fill(Fill::solid(Color::new(0.0, 0.0, 1.0, 1.0))),
            ),
        );
        blue.drop_shadow = DropShadow {
            color: Color::new(0.0, 0.0, 0.0, 1.0),
            opacity: 1.0,
            dx: 3.0,
            dy: 3.0,
            blur: 2.0,
            enabled: true,
        };
        doc.add_node(red, None);
        doc.add_node(blue, None);
        doc.layers.get_mut(&lid).unwrap().opacity = 0.5;

        let Some(mut r) = offscreen(doc, 20, 20) else {
            eprintln!("no GPU adapter — skipping isolation+effects test");
            return;
        };
        let img = image::load_from_memory(&r.render_capture())
            .expect("png")
            .to_rgba8();
        let overlap = img.get_pixel(10, 10).0; // opaque blue over its own shadow
                                               // Linear-light 50% over white → srgb_oetf(0.5) ≈ 188 (A-1, §4.5.4).
        assert!(
            (overlap[0] as i32 - 188).abs() < 16
                && (overlap[1] as i32 - 188).abs() < 16
                && (overlap[2] as i32 - 255).abs() < 16,
            "layer with a drop shadow must still composite at 50% in linear light \
             (isolated), got {overlap:?}"
        );
    }
}
