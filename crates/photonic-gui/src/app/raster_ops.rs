use super::*;

impl PhotonicApp {
    /// Get (or build) the egui texture for a raster node's current pixels, with
    /// its layer mask baked into the alpha. Re-uploads only when content changes.
    fn raster_texture(
        &mut self,
        ctx: &egui::Context,
        id: photonic_core::node::NodeId,
        rn: &photonic_core::node::RasterNode,
    ) -> egui::TextureId {
        // FNV-1a content hash over pixels + mask (cheap change detection).
        let mut hash: u64 = 0xcbf29ce484222325;
        let feed = |bytes: &[u8], hash: &mut u64| {
            for &b in bytes {
                *hash ^= b as u64;
                *hash = hash.wrapping_mul(0x100000001b3);
            }
        };
        feed(&rn.image.pixels, &mut hash);
        if let Some(m) = &rn.mask {
            feed(&m.data, &mut hash);
        }

        if let Some(c) = self.raster_tex_cache.get(&id) {
            if c.hash == hash {
                return c.handle.id();
            }
        }

        let w = rn.image.width as usize;
        let h = rn.image.height as usize;
        let mask_ok = rn
            .mask
            .as_ref()
            .map(|m| m.data.len() == w * h)
            .unwrap_or(false);
        let mut pixels = Vec::with_capacity(w * h);
        for i in 0..(w * h) {
            let p = &rn.image.pixels[i * 4..i * 4 + 4];
            let mut a = p[3];
            if mask_ok {
                let cov = rn.mask.as_ref().unwrap().data[i] as u16;
                a = ((a as u16 * cov) / 255) as u8;
            }
            pixels.push(egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], a));
        }
        let color_img = egui::ColorImage {
            size: [w.max(1), h.max(1)],
            pixels,
        };
        let handle = ctx.load_texture(
            format!("raster_{id}"),
            color_img,
            egui::TextureOptions::LINEAR,
        );
        let tex_id = handle.id();
        self.raster_tex_cache
            .insert(id, RasterTexCache { handle, hash });
        tex_id
    }

    /// Paint all visible raster nodes as textured quads over the GPU vector
    /// layer, transformed by each node's affine and the camera. Mirrors the
    /// headless export compositor (raster composites over vectors).
    pub(crate) fn paint_raster_nodes(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        doc: &Document,
        view: &CanvasView,
    ) {
        let raster_ids: Vec<photonic_core::node::NodeId> = doc
            .nodes_in_draw_order()
            .iter()
            .filter(|n| {
                n.visible && matches!(&n.kind, SceneNodeKind::Raster(r) if !r.is_adjustment_layer())
            })
            .map(|n| n.id)
            .collect();
        for id in raster_ids {
            let Some(node) = doc.get_node(&id) else {
                continue;
            };
            let SceneNodeKind::Raster(rn) = &node.kind else {
                continue;
            };
            if node.opacity <= 0.0 || rn.image.width == 0 || rn.image.height == 0 {
                continue;
            }
            let tex = self.raster_texture(ctx, id, rn);
            let (w, h) = (rn.image.width as f64, rn.image.height as f64);
            let local = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
            let uv = [
                egui::pos2(0.0, 0.0),
                egui::pos2(1.0, 0.0),
                egui::pos2(1.0, 1.0),
                egui::pos2(0.0, 1.0),
            ];
            let a = (node.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
            let tint = egui::Color32::from_white_alpha(a);
            let mut mesh = egui::Mesh::with_texture(tex);
            for (i, (lx, ly)) in local.iter().enumerate() {
                let (dx, dy) = node.transform.apply(*lx, *ly);
                let (sx, sy) = view.canvas_to_screen(dx, dy);
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: egui::pos2(sx as f32, sy as f32),
                    uv: uv[i],
                    color: tint,
                });
            }
            mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
            painter.add(egui::Shape::mesh(mesh));
        }
    }

    /// Toggle Outline Mode, clearing the other (mutually exclusive) view modes.
    pub fn toggle_outline_mode(&mut self) {
        self.outline_mode = !self.outline_mode;
        if self.outline_mode {
            self.pixel_preview = false;
            self.overprint_preview = false;
        }
    }

    /// Toggle Pixel Preview, clearing the other (mutually exclusive) view modes.
    pub fn toggle_pixel_preview(&mut self) {
        self.pixel_preview = !self.pixel_preview;
        if self.pixel_preview {
            self.outline_mode = false;
            self.overprint_preview = false;
        }
        self.preview_tex_cache = None;
    }

    /// Toggle Overprint Preview, clearing the other (mutually exclusive) modes.
    pub fn toggle_overprint_preview(&mut self) {
        self.overprint_preview = !self.overprint_preview;
        if self.overprint_preview {
            self.outline_mode = false;
            self.pixel_preview = false;
        }
        self.preview_tex_cache = None;
    }

    /// True when Pixel or Overprint Preview is active.
    pub(crate) fn preview_active(&self) -> bool {
        self.pixel_preview || self.overprint_preview
    }

    /// Paint the Pixel/Overprint Preview overlay (#22): render the active
    /// artboard through the headless/export path at its native export pixel size
    /// and paint the result as a NEAREST-sampled texture over the artboard rect,
    /// so the user sees the exact bytes the exporter would write (true aliasing,
    /// pixel snapping, and overprint-ink multiply). The render is content-hashed
    /// and only re-run when the document, mode, or target size changes.
    pub(crate) fn paint_preview_overlay(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        doc: &Document,
        view: &CanvasView,
    ) {
        // Region = active artboard (or first artboard, or the full document).
        let (rx, ry, rw, rh) = doc
            .active_artboard
            .and_then(|id| doc.artboards.iter().find(|a| a.id == id))
            .or_else(|| doc.artboards.first())
            .map(|a| (a.x, a.y, a.width, a.height))
            .unwrap_or((0.0, 0.0, doc.width, doc.height));
        let pw = rw.round().max(1.0) as u32;
        let ph = rh.round().max(1.0) as u32;

        // ── Content/mode/size hash (FNV-1a) ──────────────────────────────────
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |bytes: &[u8]| {
            for &b in bytes {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        };
        if let Ok(bytes) = serde_json::to_vec(doc) {
            feed(&bytes);
        }
        feed(&[self.pixel_preview as u8, self.overprint_preview as u8]);
        feed(&pw.to_le_bytes());
        feed(&ph.to_le_bytes());

        // ── Re-render only on change ─────────────────────────────────────────
        let stale = self.preview_tex_cache.as_ref().map(|c| c.hash) != Some(hash);
        if stale {
            if self.preview_renderer.is_none() {
                self.preview_renderer =
                    Some(pollster::block_on(photonic_render::HeadlessRenderer::new()));
            }
            let opts = photonic_render::ExportOptions {
                background: photonic_render::ExportBackground::Artboard,
                region: Some((rx, ry, rw, rh)),
                overprint_preview: self.overprint_preview,
                ..Default::default()
            };
            let renderer = self.preview_renderer.as_ref().unwrap();
            let (rgba, rw_px, rh_px) = renderer.render_rgba_with_opts(doc, pw, ph, &opts);
            // GPU readback can fail (device loss / OOM), in which case the
            // renderer returns empty pixels with a non-zero size. Skip this frame
            // rather than upload a zero-length buffer at a non-zero extent (wgpu
            // validation panic); the cache is left untouched and we retry next frame.
            if rgba.is_empty() {
                return;
            }
            let (iw, ih) = (rw_px as usize, rh_px as usize);
            let mut pixels = Vec::with_capacity(iw * ih);
            for px in rgba.chunks_exact(4) {
                pixels.push(egui::Color32::from_rgba_unmultiplied(
                    px[0], px[1], px[2], px[3],
                ));
            }
            let color_img = egui::ColorImage {
                size: [iw.max(1), ih.max(1)],
                pixels,
            };
            let handle =
                ctx.load_texture("photonic_preview", color_img, egui::TextureOptions::NEAREST);
            self.preview_tex_cache = Some(PreviewTexCache { handle, hash });
        }

        let Some(cache) = &self.preview_tex_cache else {
            return;
        };
        // Paint over the artboard's screen rect.
        let (sx0, sy0) = view.canvas_to_screen(rx, ry);
        let (sx1, sy1) = view.canvas_to_screen(rx + rw, ry + rh);
        let scr = egui::Rect::from_min_max(
            egui::pos2(sx0 as f32, sy0 as f32),
            egui::pos2(sx1 as f32, sy1 as f32),
        );
        let mut mesh = egui::Mesh::with_texture(cache.handle.id());
        mesh.add_rect_with_uv(
            scr,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        painter.add(egui::Shape::mesh(mesh));
    }

    /// Handle the interactive RasterBrush/RasterEraser tools: paint/erase onto
    /// the selected raster layer as the pointer drags, committing one undoable
    /// `UpdateNode` per stroke.
    pub(crate) fn handle_raster_brush(
        &mut self,
        response: &egui::Response,
        doc: &mut Document,
        view: &CanvasView,
        history: &mut CommandHistory,
    ) {
        use photonic_core::raster::brush;
        let erase = self.active_tool == Tool::RasterEraser;

        // Resolve the active raster (non-adjustment) node.
        let Some(nid) = self.selected_id else {
            return;
        };
        let is_raster = matches!(
            doc.get_node(&nid).map(|n| &n.kind),
            Some(SceneNodeKind::Raster(r)) if !r.is_adjustment_layer()
        );
        if !is_raster {
            return;
        }

        if response.drag_started() {
            self.raster_stroke_pts.clear();
            self.raster_stroke_orig = doc.get_node(&nid).cloned().map(|n| (nid, n));
        }

        if response.dragged() || response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                // Screen → canvas → node-local pixel coordinates.
                let (cx, cy) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                let local = doc.get_node(&nid).map(|node| {
                    let inv = node.transform.to_kurbo().inverse();
                    let lp = inv * kurbo::Point::new(cx, cy);
                    (lp.x as f32, lp.y as f32)
                });
                if let Some(p) = local {
                    self.raster_stroke_pts.push(p);
                    let n = self.raster_stroke_pts.len();
                    let tail: Vec<(f32, f32)> = if n >= 2 {
                        self.raster_stroke_pts[n - 2..].to_vec()
                    } else {
                        self.raster_stroke_pts.clone()
                    };
                    let color = [
                        (self.fill_color[0] * 255.0).round() as u8,
                        (self.fill_color[1] * 255.0).round() as u8,
                        (self.fill_color[2] * 255.0).round() as u8,
                        (self.fill_color[3] * 255.0).round() as u8,
                    ];
                    if let Some(node) = doc.get_node_mut(&nid) {
                        if let SceneNodeKind::Raster(rn) = &mut node.kind {
                            let mut b = brush::Brush::new(self.raster_brush_radius, color);
                            b.hardness = self.raster_brush_hardness;
                            if erase {
                                brush::erase(&mut rn.image, &tail, &b, None);
                            } else {
                                brush::stroke(&mut rn.image, &tail, &b, None);
                            }
                        }
                    }
                }
            }
        }

        if response.drag_stopped() {
            if let Some((onid, orig)) = self.raster_stroke_orig.take() {
                if let Some(cur) = doc.get_node(&onid).cloned() {
                    // The stroke was painted live; record it as one undoable step.
                    history.execute(
                        Command::UpdateNode {
                            old: orig,
                            new: cur,
                        },
                        doc,
                    );
                }
            }
            self.raster_stroke_pts.clear();
        }
    }

    // ── Raster masking: color range + remove background ──────────────────────

    /// Sample a raster node's own pixel at canvas coordinates `(cx, cy)`.
    /// Returns the straight-RGBA color and the node-local pixel position, or
    /// `None` if the point is outside the layer (or the node isn't a raster).
    pub(crate) fn sample_raster_pixel(
        &self,
        doc: &Document,
        nid: NodeId,
        cx: f64,
        cy: f64,
    ) -> Option<([u8; 4], (u32, u32))> {
        let node = doc.get_node(&nid)?;
        let SceneNodeKind::Raster(rn) = &node.kind else {
            return None;
        };
        let lp = node.transform.to_kurbo().inverse() * kurbo::Point::new(cx, cy);
        if lp.x < 0.0
            || lp.y < 0.0
            || lp.x >= rn.image.width as f64
            || lp.y >= rn.image.height as f64
        {
            return None;
        }
        let (px, py) = (lp.x as u32, lp.y as u32);
        Some((rn.image.pixel(px, py), (px, py)))
    }

    /// Start (or restart) a color-range session on `nid` with the sampled color.
    pub(crate) fn begin_raster_color_range(
        &mut self,
        doc: &mut Document,
        nid: NodeId,
        target: [u8; 4],
        seed: (u32, u32),
    ) {
        // A prior session's preview must not leak into the new baseline.
        self.cancel_raster_color_range(doc);
        let Some(original) = doc.get_node(&nid).cloned() else {
            return;
        };
        self.raster_color_range = Some(RasterColorRangeSession {
            node_id: nid,
            target,
            seed,
            original,
        });
        self.refresh_raster_color_range(doc);
    }

    /// Rebuild the live preview from the session's `original` using the current
    /// fuzziness/contiguous settings (idempotent — parameter changes never
    /// accumulate).
    pub(crate) fn refresh_raster_color_range(&mut self, doc: &mut Document) {
        let Some(s) = &self.raster_color_range else {
            return;
        };
        let SceneNodeKind::Raster(orig_rn) = &s.original.kind else {
            return;
        };
        let sel = if self.raster_mask_contiguous {
            photonic_core::Mask::magic_wand(
                &orig_rn.image,
                s.seed.0,
                s.seed.1,
                self.raster_mask_tolerance,
            )
        } else {
            photonic_core::Mask::color_range(&orig_rn.image, s.target, self.raster_mask_tolerance)
        };
        let node_id = s.node_id;
        let original = s.original.clone();
        if let Some(node) = doc.get_node_mut(&node_id) {
            *node = original;
            if let SceneNodeKind::Raster(rn) = &mut node.kind {
                rn.hide_selection(&sel);
            }
        }
    }

    /// Commit the active session as one undoable `UpdateNode`. Returns true if
    /// the document changed.
    pub(crate) fn apply_raster_color_range(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) -> bool {
        let Some(s) = self.raster_color_range.take() else {
            return false;
        };
        let Some(current) = doc.get_node(&s.node_id).cloned() else {
            return false;
        };
        // The preview is already live in the doc; record it as one undo step
        // (same pattern as the raster brush stroke).
        history.execute(
            Command::UpdateNode {
                old: s.original,
                new: current,
            },
            doc,
        );
        true
    }

    /// Discard the active session, restoring the node to its pre-preview state.
    pub(crate) fn cancel_raster_color_range(&mut self, doc: &mut Document) {
        if let Some(s) = self.raster_color_range.take() {
            if let Some(node) = doc.get_node_mut(&s.node_id) {
                *node = s.original;
            }
        }
    }

    // ── Image placement ───────────────────────────────────────────────────────

    /// Place an image file (PNG/JPEG/WebP/…) as a new raster layer centred on
    /// the artboard, select it, and record one undoable `AddNode`. This is the
    /// GUI counterpart of the MCP `place_image` tool.
    pub(crate) fn place_image_file(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        path: &std::path::Path,
    ) {
        match std::fs::read(path) {
            Ok(bytes) => self.place_image_bytes(doc, history, &bytes, Some(path)),
            Err(e) => self.file_status = Some(format!("Place image failed: {e}")),
        }
    }

    /// Decode `bytes` and place them as a raster layer (see
    /// [`Self::place_image_file`]). `source` supplies the layer name and the
    /// `source_uri` used for relink/re-export.
    pub(crate) fn place_image_bytes(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        bytes: &[u8],
        source: Option<&std::path::Path>,
    ) {
        let image = match photonic_core::raster::image::RasterImage::from_encoded(bytes) {
            Ok(i) => i,
            Err(e) => {
                self.file_status = Some(format!("Place image failed: {e}"));
                return;
            }
        };
        let (w, h) = (image.width, image.height);
        let mut raster = photonic_core::node::RasterNode::new(image);
        if let Some(p) = source {
            raster.source_uri = Some(p.to_string_lossy().into_owned());
        }
        let name = source
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Image".to_string());
        // Placeholder layer id — `AddNode` reassigns it to the target layer
        // (`layer_id: None` → the document's active layer), same as MCP
        // `place_image`.
        let mut node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Raster(raster));
        // Centre on the artboard (may be negative if the image is larger).
        node.transform = photonic_core::Transform::translate(
            (doc.width - w as f64) / 2.0,
            (doc.height - h as f64) / 2.0,
        );
        let nid = node.id;
        history.execute(
            Command::AddNode {
                node,
                layer_id: None,
            },
            doc,
        );
        doc.selection = Selection::single(nid);
        self.selected_id = Some(nid);
        self.file_status = Some(format!("Placed {name} ({w}×{h})"));
    }

    /// Kick off background removal on `nid`'s pixels in a worker thread (model
    /// load/inference is far too slow for the UI thread). The result lands in
    /// `rmbg_rx` and is applied in the `draw` poll.
    pub(crate) fn start_remove_background(&mut self, doc: &mut Document, nid: NodeId) {
        if self.rmbg_rx.is_some() {
            self.file_status = Some("Background removal is already running…".into());
            return;
        }
        // Discard any live color-range preview on this node first: the job's
        // snapshot and the eventual undo record must both be built from
        // committed state, not an uncommitted preview.
        if self
            .raster_color_range
            .as_ref()
            .is_some_and(|s| s.node_id == nid)
        {
            self.cancel_raster_color_range(doc);
        }
        let Some(node) = doc.get_node(&nid) else {
            return;
        };
        let SceneNodeKind::Raster(rn) = &node.kind else {
            return;
        };
        if rn.is_adjustment_layer() {
            return;
        }
        let img = rn.image.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let res = photonic_matte::remove_background(&img).map_err(|e| format!("{e:#}"));
            let _ = tx.send(res);
        });
        self.rmbg_rx = Some(rx);
        self.rmbg_node = Some(nid);
        self.file_status = Some(if self.rmbg_model_cached {
            "Removing background…".into()
        } else {
            "Downloading the background-removal model (~5 MB), then removing background…".into()
        });
    }

    /// Isolation Mode visual: when a group is isolated (entered via double-click),
    /// dim everything outside its bounds and draw an accent border around it, so
    /// it reads as a real isolation mode rather than just a restricted selection.
    /// No-op when not isolated. Dims the whole canvas if the group has no
    /// measurable bounds.
    pub(crate) fn paint_isolation_scrim(
        &self,
        ui: &egui::Ui,
        doc: &Document,
        view: &CanvasView,
        rect: egui::Rect,
    ) {
        let Some(gid) = self.isolated_group else {
            return;
        };
        let painter = ui.painter_at(rect);
        let scrim = if self.prefs.dark_mode {
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 120)
        } else {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 150)
        };
        let accent = egui::Color32::from_rgb(80, 160, 255);

        // Union world-space AABB of the group's leaf members.
        let mut bbox: Option<(f64, f64, f64, f64)> = None;
        for id in doc.group_member_ids(&gid) {
            if let Some(node) = doc.nodes.get(&id) {
                if let Some((x0, y0, x1, y1)) = node_world_aabb_opt(node) {
                    bbox = Some(match bbox {
                        None => (x0, y0, x1, y1),
                        Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
                    });
                }
            }
        }

        let Some((x0, y0, x1, y1)) = bbox else {
            // No measurable bounds (e.g. text-only group): dim the whole canvas.
            painter.rect_filled(rect, 0.0, scrim);
            return;
        };

        let pad = 6.0_f32;
        let (sx0, sy0) = view.canvas_to_screen(x0, y0);
        let (sx1, sy1) = view.canvas_to_screen(x1, y1);
        let hole = egui::Rect::from_min_max(
            egui::pos2(sx0 as f32 - pad, sy0 as f32 - pad),
            egui::pos2(sx1 as f32 + pad, sy1 as f32 + pad),
        )
        .intersect(rect);

        // Four scrim bands around the clear hole (egui has no "punch-out" fill).
        if hole.min.y > rect.min.y {
            painter.rect_filled(
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, hole.min.y)),
                0.0,
                scrim,
            );
        }
        if hole.max.y < rect.max.y {
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(rect.min.x, hole.max.y), rect.max),
                0.0,
                scrim,
            );
        }
        if hole.min.x > rect.min.x {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.min.x, hole.min.y),
                    egui::pos2(hole.min.x, hole.max.y),
                ),
                0.0,
                scrim,
            );
        }
        if hole.max.x < rect.max.x {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(hole.max.x, hole.min.y),
                    egui::pos2(rect.max.x, hole.max.y),
                ),
                0.0,
                scrim,
            );
        }

        painter.rect_stroke(hole, 2.0, egui::Stroke::new(1.5, accent));
    }
}
