use super::*;
use crate::welcome::NewDocumentSpec;

impl PhotonicApp {
    /// Build a fresh [`Document`] from a [`NewDocumentSpec`] (pure — no install).
    /// Shared by the welcome screen's New Canvas panel and the File ▸ New modal.
    pub(crate) fn build_document_from_spec(spec: NewDocumentSpec) -> Document {
        let NewDocumentSpec {
            name,
            width,
            height,
            bleed_mm,
            slug_mm,
            margin,
            artboards,
            history_max_mb,
        } = spec;
        let mut new_doc = photonic_core::Document::new(name, width, height);
        new_doc.bleed_mm = bleed_mm;
        new_doc.slug_mm = slug_mm;
        new_doc.history_max_mb = Some(history_max_mb);
        new_doc.margin_top = margin;
        new_doc.margin_right = margin;
        new_doc.margin_bottom = margin;
        new_doc.margin_left = margin;
        // Multiple artboards: lay out N same-size boards in a grid.
        if artboards > 1 {
            let gap = (width * 0.06).max(40.0);
            let cols = (artboards as f64).sqrt().ceil().max(1.0) as usize;
            let mut boards = Vec::with_capacity(artboards);
            for i in 0..artboards {
                let col = (i % cols) as f64;
                let row = (i / cols) as f64;
                boards.push(photonic_core::Artboard::new(
                    format!("Artboard {}", i + 1),
                    col * (width + gap),
                    row * (height + gap),
                    width,
                    height,
                ));
            }
            new_doc.active_artboard = boards.first().map(|a| a.id);
            new_doc.artboards = boards;
        }
        new_doc
    }

    /// Build a document from a spec and install it into the live document in place
    /// (replacing it), resetting history/selection and queuing a viewport fit. Used
    /// for the FIRST document created from the welcome screen (before any tab
    /// exists); the in-editor File ▸ New opens a new tab via `open_in_new_tab`.
    pub(crate) fn create_document_from_spec(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        spec: NewDocumentSpec,
    ) {
        *doc = Self::build_document_from_spec(spec);
        // Fresh document — drop any prior project's history so it can't bleed in.
        history.reset();
        // Fit all artboards to the viewport on the next frame (once the real
        // viewport rect is known).
        self.fit_pending = true;
        self.current_file = None;
        self.selected_id = None;
    }

    /// In-editor File ▸ New modal — draws the shared new-document form in a centered
    /// window. On commit it opens the new document in a **new tab** (leaving other
    /// open documents untouched), and closes on Escape, the window's close button, or after
    /// creating. Returns `true` when a new document was created.
    pub(crate) fn draw_new_document_modal(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        view: &mut CanvasView,
        history: &mut CommandHistory,
    ) -> bool {
        let Some(form) = &mut self.new_document_modal else {
            return false;
        };

        let screen = ctx.screen_rect();
        let panel_w = (screen.width() * 0.72).clamp(560.0, 1160.0);
        let mut open = true;
        let mut spec: Option<NewDocumentSpec> = None;

        egui::Window::new(RichText::new(format!("{}  New document", ph::FILE_PLUS)).size(16.0))
            .id(egui::Id::new("new_document_modal"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(panel_w);
                // No entrance fade inside the modal (opacity 1.0).
                spec = form.draw(ui, ctx, panel_w, 1.0);
            });

        // Escape dismisses the modal, matching the welcome screen's back gesture.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }

        if let Some(spec) = spec {
            let new_doc = Self::build_document_from_spec(spec);
            self.open_in_new_tab(doc, history, view, new_doc, CommandHistory::default(), None);
            self.new_document_modal = None;
            self.file_status = Some("New document".into());
            return true;
        }
        if !open {
            self.new_document_modal = None;
        }
        false
    }

    pub(crate) fn draw_export_modal(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(dlg) = &mut self.export_dialog else {
            return;
        };

        // Collect the button the user clicked without holding a mutable borrow
        // inside the egui closure at the same time as `.open(&mut open)`.
        #[derive(PartialEq)]
        enum Action {
            None,
            Cancel,
            Export,
        }
        let mut action = Action::None;
        let mut open = true;

        egui::Window::new("Export")
            .collapsible(false)
            .resizable(false)
            .fixed_size([340.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                // ── Format ───────────────────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.label("Format");
                    ui.selectable_value(&mut dlg.format, ExportFormat::Png, "PNG");
                    ui.selectable_value(&mut dlg.format, ExportFormat::Jpeg, "JPEG");
                    ui.selectable_value(&mut dlg.format, ExportFormat::WebP, "WebP");
                    ui.selectable_value(&mut dlg.format, ExportFormat::Gif, "GIF");
                    ui.selectable_value(&mut dlg.format, ExportFormat::Tiff, "TIFF");
                    ui.selectable_value(&mut dlg.format, ExportFormat::Ico, "ICO");
                    ui.selectable_value(&mut dlg.format, ExportFormat::Svg, "SVG");
                });

                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Background (all formats, incl. transparent SVG) + bounds ──
                ui.horizontal(|ui| {
                    ui.label("Background");
                    ui.radio_value(
                        &mut dlg.background,
                        ExportBackground::Transparent,
                        "Transparent",
                    );
                    ui.radio_value(
                        &mut dlg.background,
                        ExportBackground::Artboard,
                        "Artboard (white)",
                    );
                });
                // Bounds/crop only applies to raster export; SVG uses the full artboard viewBox.
                if dlg.format != ExportFormat::Svg {
                    ui.horizontal(|ui| {
                        ui.label("Bounds       ");
                        ui.checkbox(&mut dlg.crop_to_content, "Crop to artwork");
                    });
                }

                // ── Artboard picker (single-image export) ─────────────────
                // Lets the user export a specific artboard from the standard
                // Export dialog (whichever entry point opened it). Hidden in
                // batch mode (that path exports one file per artboard).
                if !doc.artboards.is_empty()
                    && dlg.artboard_export == crate::app::ArtboardExport::Off
                {
                    ui.horizontal(|ui| {
                        ui.label("Artboard");
                        let current = dlg
                            .artboard_target
                            .and_then(|id| doc.artboards.iter().find(|a| a.id == id))
                            .map(|a| a.name.clone())
                            .unwrap_or_else(|| "Whole document".to_string());
                        egui::ComboBox::from_id_source("export_artboard_pick")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                // Whole document (no crop to a board).
                                if ui
                                    .selectable_label(
                                        dlg.artboard_target.is_none(),
                                        "Whole document",
                                    )
                                    .clicked()
                                {
                                    dlg.artboard_target = None;
                                    dlg.region_override = None;
                                    dlg.png_width = doc.width as u32;
                                    dlg.png_height = doc.height as u32;
                                    if doc.height > 0.0 {
                                        dlg.aspect = doc.width / doc.height;
                                    }
                                }
                                for ab in &doc.artboards {
                                    if ui
                                        .selectable_label(
                                            dlg.artboard_target == Some(ab.id),
                                            &ab.name,
                                        )
                                        .clicked()
                                    {
                                        dlg.artboard_target = Some(ab.id);
                                        dlg.region_override =
                                            Some((ab.x, ab.y, ab.width, ab.height));
                                        dlg.png_width = ab.width.round().max(1.0) as u32;
                                        dlg.png_height = ab.height.round().max(1.0) as u32;
                                        if ab.height > 0.0 {
                                            dlg.aspect = ab.width / ab.height;
                                        }
                                    }
                                }
                            });
                    });
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Format-specific settings ──────────────────────────────
                match dlg.format {
                    ExportFormat::Png
                    | ExportFormat::Jpeg
                    | ExportFormat::WebP
                    | ExportFormat::Gif
                    | ExportFormat::Tiff => {
                        // In batch mode each artboard sizes itself (native × scale),
                        // so the single width/height picker is hidden.
                        if dlg.artboard_export == crate::app::ArtboardExport::Off {
                            ui.horizontal(|ui| {
                                ui.label("Width ");
                                let prev_w = dlg.png_width;
                                let r = ui.add(
                                    egui::DragValue::new(&mut dlg.png_width)
                                        .range(1..=8192)
                                        .suffix(" px"),
                                );
                                if r.changed() && dlg.aspect > 0.0 {
                                    dlg.png_height =
                                        ((dlg.png_width as f64 / dlg.aspect) as u32).max(1);
                                }
                                let _ = prev_w;
                                ui.label("  Height ");
                                let r = ui.add(
                                    egui::DragValue::new(&mut dlg.png_height)
                                        .range(1..=8192)
                                        .suffix(" px"),
                                );
                                if r.changed() && dlg.aspect > 0.0 {
                                    dlg.png_width =
                                        ((dlg.png_height as f64 * dlg.aspect) as u32).max(1);
                                }
                            });
                        }
                        if dlg.format == ExportFormat::Jpeg || dlg.format == ExportFormat::WebP {
                            ui.horizontal(|ui| {
                                ui.label("Quality");
                                ui.add(
                                    egui::Slider::new(&mut dlg.jpeg_quality, 1..=100).suffix("%"),
                                );
                            });
                        }
                    }
                    ExportFormat::Ico => {
                        ui.label("Sizes");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut dlg.ico_size_16, "16");
                            ui.checkbox(&mut dlg.ico_size_32, "32");
                            ui.checkbox(&mut dlg.ico_size_48, "48");
                            ui.checkbox(&mut dlg.ico_size_256, "256");
                        });
                    }
                    ExportFormat::Svg => {}
                }

                // ── Batch artboard export ─────────────────────────────────
                if let crate::app::ArtboardExport::Range { start, end } = &mut dlg.artboard_export {
                    let n = doc.artboards.len().max(1);
                    *start = (*start).clamp(1, n);
                    *end = (*end).clamp(*start, n);
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                    let is_svg_or_ico = matches!(dlg.format, ExportFormat::Svg | ExportFormat::Ico);
                    if is_svg_or_ico {
                        ui.colored_label(
                            egui::Color32::from_rgb(210, 150, 60),
                            "Batch export needs a raster format (PNG/JPEG/WebP/GIF/TIFF).",
                        );
                    } else {
                        ui.horizontal(|ui| {
                            ui.label("Artboards");
                            let all = *start <= 1 && *end >= n;
                            if ui.selectable_label(all, "All").clicked() {
                                *start = 1;
                                *end = n;
                            }
                            ui.label("from");
                            ui.add(egui::DragValue::new(start).range(1..=n));
                            ui.label("to");
                            ui.add(egui::DragValue::new(end).range(*start..=n));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Scale");
                            ui.add(
                                egui::DragValue::new(&mut dlg.artboard_scale)
                                    .range(0.05..=8.0)
                                    .speed(0.05)
                                    .suffix("×"),
                            );
                            let count = end.saturating_sub(start.saturating_sub(1));
                            ui.label(format!("· {count} file(s), one per artboard"));
                        });
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Action buttons ────────────────────────────────────────
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Action::Cancel;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Export…").clicked() {
                            action = Action::Export;
                        }
                    });
                });
            });

        // X button closed the window
        if !open {
            self.export_dialog = None;
            return;
        }

        match action {
            Action::Cancel => {
                self.export_dialog = None;
            }
            Action::Export => {
                self.run_export(doc);
            }
            Action::None => {}
        }
    }

    /// Floating fill/stroke color picker raised from the radial menu (path
    /// nodes only). Edits apply live through history. Returns whether the
    /// document changed this frame.
    pub(crate) fn draw_color_popup(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) -> bool {
        let Some(popup) = self.color_popup else {
            return false;
        };

        // Snapshot the target path node; close the popup if it vanished or is
        // no longer a path. Owned clone so `doc` is free for `history.execute`.
        let node = match doc.nodes.get(&popup.node_id) {
            Some(n) if matches!(n.kind, SceneNodeKind::Path(_)) => n.clone(),
            _ => {
                self.color_popup = None;
                return false;
            }
        };

        // Shared color-picker context: recents + document color swatches.
        let recents: Vec<[f32; 4]> = doc
            .recent_colors
            .iter()
            .map(|c| [c.r, c.g, c.b, c.a])
            .collect();
        let swatches: Vec<[f32; 4]> = doc
            .color_swatches
            .iter()
            .filter_map(|s| crate::color_convert::parse_hex(&s.color_hex))
            .collect();
        let color_cfg = crate::color_popup::PickerConfig {
            alpha: true,
            recents: &recents,
            swatches: &swatches,
            eyedropper: true,
            allow_add_swatch: true,
            contrast_ref: None,
        };
        let anchor = ("radial_color_popup", popup.pos.x as i32, popup.pos.y as i32);
        let mut open = true;
        let mut doc_modified = false;
        let add_swatch: Option<[f32; 4]>;
        let mut save_gradient: Option<photonic_core::style::Fill> = None;
        let node_id = popup.node_id;

        if popup.stroke {
            // ── Stroke: solid color only (strokes carry a single color). ──
            let seed = match &node.kind {
                SceneNodeKind::Path(pn) => pn.stroke.color,
                _ => photonic_core::Color::WHITE,
            };
            let mut rgba = [seed.r, seed.g, seed.b, seed.a];
            let out = crate::color_popup::ColorPopup::window(
                ctx,
                anchor,
                "Stroke Color",
                popup.pos,
                &mut rgba,
                &mut open,
                &color_cfg,
            );
            if out.changed {
                let mut new_node = node.clone();
                if let SceneNodeKind::Path(pn) = &mut new_node.kind {
                    pn.stroke.color = photonic_core::Color {
                        r: rgba[0],
                        g: rgba[1],
                        b: rgba[2],
                        a: rgba[3],
                    };
                    pn.stroke.enabled = true;
                }
                history.execute(
                    Command::UpdateNode {
                        old: node,
                        new: new_node,
                    },
                    doc,
                );
                doc_modified = true;
            }
            add_swatch = out.add_swatch;
            if out.eyedropper_clicked {
                self.pending_panel_actions
                    .push(PanelAction::StartEyedropper(EyedropperTarget::NodeStroke {
                        node_id,
                    }));
                self.color_popup = None;
            }
        } else {
            // ── Fill: full fill picker with the slide-out gradient drawer. ──
            let mut fill = match &node.kind {
                SceneNodeKind::Path(pn) => pn.fill.clone(),
                _ => photonic_core::style::Fill::solid(photonic_core::Color::WHITE),
            };
            let grad_swatches: Vec<(String, photonic_core::style::Fill)> = doc
                .gradient_swatches
                .iter()
                .filter_map(|gs| {
                    serde_json::from_str::<photonic_core::style::Fill>(&gs.fill_json)
                        .ok()
                        .map(|f| (gs.name.clone(), f))
                })
                .collect();
            let fill_cfg = crate::color_popup::FillPickerConfig {
                color: color_cfg,
                gradient_swatches: &grad_swatches,
                allow_save_gradient: true,
            };
            let out = crate::color_popup::ColorPopup::fill_window(
                ctx, anchor, "Fill", popup.pos, &mut fill, &mut open, &fill_cfg,
            );
            if out.changed {
                let mut new_node = node.clone();
                if let SceneNodeKind::Path(pn) = &mut new_node.kind {
                    fill.enabled = true;
                    pn.fill = fill;
                }
                history.execute(
                    Command::UpdateNode {
                        old: node,
                        new: new_node,
                    },
                    doc,
                );
                doc_modified = true;
            }
            add_swatch = out.add_swatch;
            save_gradient = out.save_gradient;
            if let Some(slot) = out.eyedropper {
                use crate::panels::FillColorSlot as S;
                let target = match slot {
                    S::Solid => EyedropperTarget::NodeFillSolid { node_id },
                    S::GradientStop(idx) => EyedropperTarget::NodeFillGradStop { node_id, idx },
                    S::FluidPoint(idx) => EyedropperTarget::NodeFillFluid { node_id, idx },
                    S::MeshVertex(idx) => EyedropperTarget::NodeFillMesh { node_id, idx },
                };
                self.pending_panel_actions
                    .push(PanelAction::StartEyedropper(target));
                self.color_popup = None;
            }
        }

        // "Add to swatches": append a document color swatch for the picked color.
        if let Some(c) = add_swatch {
            let hex = crate::color_convert::format_hex(c, false);
            if !doc
                .color_swatches
                .iter()
                .any(|s| s.color_hex.eq_ignore_ascii_case(&hex))
            {
                let mut n = doc.color_swatches.len() + 1;
                let name = loop {
                    let cand = format!("Swatch {n}");
                    if !doc.color_swatches.iter().any(|s| s.name == cand) {
                        break cand;
                    }
                    n += 1;
                };
                doc.color_swatches
                    .push(photonic_core::document::ColorSwatch::new(name, hex));
                doc_modified = true;
            }
        }

        // "Save gradient": append the current gradient to the gradient library.
        if let Some(gfill) = save_gradient {
            if let Ok(json) = serde_json::to_string(&gfill) {
                let mut n = doc.gradient_swatches.len() + 1;
                let name = loop {
                    let cand = format!("Gradient {n}");
                    if !doc.gradient_swatches.iter().any(|s| s.name == cand) {
                        break cand;
                    }
                    n += 1;
                };
                doc.gradient_swatches
                    .push(photonic_core::document::GradientSwatch::new(name, json));
                doc_modified = true;
            }
        }

        if !open {
            self.color_popup = None;
        }
        doc_modified
    }

    pub(crate) fn draw_simplify_dialog(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        if self.simplify_dialog.is_none() {
            return;
        }

        #[derive(PartialEq)]
        enum Action {
            None,
            Cancel,
            Apply,
        }
        let mut action = Action::None;
        let mut open = true;

        let node_name = self.simplify_dialog.as_ref().unwrap().node_name.clone();
        let node_id = self.simplify_dialog.as_ref().unwrap().node_id;

        // Refresh the per-tolerance preview cache (shared with the canvas
        // overlay) so the "Points: N → M" readout is always in sync, regardless
        // of draw order. RDP still runs only when the tolerance changes.
        let orig_points = self.simplify_dialog.as_ref().unwrap().orig_points;
        let new_points = {
            let dlg = self.simplify_dialog.as_mut().unwrap();
            if let Some(node) = doc.nodes.get(&dlg.node_id) {
                if let SceneNodeKind::Path(pn) = &node.kind {
                    dlg.refresh(&pn.path_data);
                }
            }
            dlg.preview
                .as_ref()
                .map(photonic_core::ops::simplify::count_points)
        };

        egui::Window::new("Simplify Path")
            .collapsible(false)
            .resizable(false)
            .fixed_size([260.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("Node: {}", node_name));
                ui.add_space(6.0);

                let dlg = self.simplify_dialog.as_mut().unwrap();
                ui.checkbox(&mut dlg.fit_curves, "Fit curves")
                    .on_hover_text(
                        "Fit smooth Bézier curves to the fewest anchor points \
                         (turns a polyline arch into one curve). Off = reduce to \
                         straight segments.",
                    );

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(if dlg.fit_curves {
                        "Fit tolerance"
                    } else {
                        "Tolerance"
                    });
                    ui.add(
                        egui::Slider::new(&mut dlg.tolerance, 0.05..=50.0)
                            .logarithmic(true)
                            .max_decimals(2),
                    );
                });
                ui.label(
                    RichText::new(if dlg.fit_curves {
                        "Higher = more aggressive (fewer anchors, looser fit)"
                    } else {
                        "Larger = more aggressive reduction"
                    })
                    .weak()
                    .small(),
                );

                if dlg.fit_curves {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Corner angle");
                        ui.add(
                            egui::Slider::new(&mut dlg.corner_angle_deg, 0.0..=90.0)
                                .suffix("°")
                                .max_decimals(0),
                        );
                    });
                    ui.label(
                        RichText::new(
                            "Bends gentler than this smooth into a curve; sharper stay corners",
                        )
                        .weak()
                        .small(),
                    );
                    ui.add_space(2.0);
                    ui.checkbox(&mut dlg.refit_existing, "Refit existing curves")
                        .on_hover_text(
                            "On: flatten and re-fit segments that are already \
                             curved. Off: keep existing curves, fit only \
                             straight-line runs.",
                        );
                }
                ui.add_space(4.0);
                match new_points {
                    Some(new) => {
                        ui.label(format!("Points: {} → {}", orig_points, new));
                    }
                    None => {
                        ui.label(format!("Points: {}", orig_points));
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Action::Cancel;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Apply").clicked() {
                            action = Action::Apply;
                        }
                    });
                });
            });

        let tolerance = self
            .simplify_dialog
            .as_ref()
            .map(|d| d.tolerance)
            .unwrap_or(1.0);

        if !open {
            self.simplify_dialog = None;
            return;
        }

        match action {
            Action::None => {}
            Action::Cancel => {
                self.simplify_dialog = None;
            }
            Action::Apply => {
                // Reuse the preview the dialog/overlay already computed for the
                // current parameters; fall back to recomputing in the selected
                // mode if the cache is somehow empty.
                let dlg = self.simplify_dialog.take();
                if let Some(node) = doc.nodes.get(&node_id) {
                    if let SceneNodeKind::Path(pn) = &node.kind {
                        let result =
                            dlg.as_ref()
                                .and_then(|d| d.preview.clone())
                                .unwrap_or_else(|| match &dlg {
                                    Some(d) => d.compute(&pn.path_data),
                                    None => photonic_core::ops::simplify::simplify_path(
                                        &pn.path_data,
                                        tolerance,
                                    ),
                                });
                        let mut new_path = pn.clone();
                        new_path.path_data = result;
                        let mut new_node = node.clone();
                        new_node.kind = SceneNodeKind::Path(new_path);
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                    }
                }
            }
        }
    }

    /// The Options modal for a Layers-tab row — a whole layer or a single object,
    /// with fields scoped to the target's type (opened from the row's right-click →
    /// Options…). Applied as one UpdateLayer/UpdateNode on OK; Cancel/close discards.
    pub(crate) fn draw_object_options_dialog(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        if self.object_options_dialog.is_none() {
            return;
        }
        // The target may have been deleted since the dialog opened.
        let target_ok = match &self.object_options_dialog.as_ref().unwrap().target {
            OptionsTarget::Layer(lid) => doc.layers.contains_key(lid),
            OptionsTarget::Node(nid) => doc.nodes.contains_key(nid),
        };
        if !target_ok {
            self.object_options_dialog = None;
            return;
        }

        use photonic_core::layer::BlendMode as Bm;
        const MODES: [(Bm, &str); 26] = [
            (Bm::Normal, "Normal"),
            (Bm::Multiply, "Multiply"),
            (Bm::Screen, "Screen"),
            (Bm::Overlay, "Overlay"),
            (Bm::Darken, "Darken"),
            (Bm::Lighten, "Lighten"),
            (Bm::ColorDodge, "Color Dodge"),
            (Bm::ColorBurn, "Color Burn"),
            (Bm::LinearDodge, "Linear Dodge (Add)"),
            (Bm::LinearBurn, "Linear Burn"),
            (Bm::HardLight, "Hard Light"),
            (Bm::SoftLight, "Soft Light"),
            (Bm::VividLight, "Vivid Light"),
            (Bm::LinearLight, "Linear Light"),
            (Bm::PinLight, "Pin Light"),
            (Bm::HardMix, "Hard Mix"),
            (Bm::Difference, "Difference"),
            (Bm::Exclusion, "Exclusion"),
            (Bm::Subtract, "Subtract"),
            (Bm::Divide, "Divide"),
            (Bm::Hue, "Hue"),
            (Bm::Saturation, "Saturation"),
            (Bm::Color, "Color"),
            (Bm::Luminosity, "Luminosity"),
            (Bm::DarkerColor, "Darker Color"),
            (Bm::LighterColor, "Lighter Color"),
        ];
        let blend_label = |m: Bm| {
            MODES
                .iter()
                .find(|(x, _)| *x == m)
                .map(|(_, n)| *n)
                .unwrap_or("Normal")
        };

        #[derive(PartialEq)]
        enum A {
            None,
            Cancel,
            Ok,
        }
        let mut act = A::None;
        let mut open = true;

        let kind_label = self.object_options_dialog.as_ref().unwrap().kind_label;
        egui::Window::new(format!("{}  {kind_label} Options", ph::SLIDERS_HORIZONTAL))
            .collapsible(false)
            .resizable(false)
            .fixed_size([300.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                let dlg = self.object_options_dialog.as_mut().unwrap();
                let is_layer = matches!(dlg.target, OptionsTarget::Layer(_));
                egui::Grid::new("object_options_grid")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut dlg.name);
                        ui.end_row();

                        ui.label("Blend");
                        egui::ComboBox::from_id_salt("layer_opts_blend")
                            .selected_text(blend_label(dlg.blend_mode))
                            .show_ui(ui, |ui| {
                                for (m, name) in MODES {
                                    ui.selectable_value(&mut dlg.blend_mode, m, name);
                                }
                            });
                        ui.end_row();

                        ui.label("Opacity");
                        ui.horizontal(|ui| {
                            ui.add(egui::Slider::new(&mut dlg.opacity, 0.0..=1.0).show_value(false));
                            ui.label(format!("{:.0}%", dlg.opacity * 100.0));
                        });
                        ui.end_row();

                        if is_layer {
                            ui.label("Colour");
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut dlg.color_enabled, "")
                                    .on_hover_text("Give this layer a colour tag");
                                if dlg.color_enabled {
                                    let mut c = egui::Color32::from_rgb(
                                        (dlg.color[0] * 255.0) as u8,
                                        (dlg.color[1] * 255.0) as u8,
                                        (dlg.color[2] * 255.0) as u8,
                                    );
                                    if ui.color_edit_button_srgba(&mut c).changed() {
                                        dlg.color = [
                                            c.r() as f32 / 255.0,
                                            c.g() as f32 / 255.0,
                                            c.b() as f32 / 255.0,
                                            1.0,
                                        ];
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });

                ui.separator();
                ui.checkbox(&mut dlg.visible, "Show");
                ui.checkbox(&mut dlg.locked, "Lock")
                    .on_hover_text("Prevent selecting or editing this item's contents");
                if is_layer {
                    ui.checkbox(&mut dlg.is_template, "Template")
                        .on_hover_text("Locked, dimmed reference layer for tracing over");
                    ui.checkbox(&mut dlg.print, "Print")
                        .on_hover_text("Include in exports/print. Off = visible on canvas but omitted from exported artwork.");
                }
                if dlg.is_group {
                    ui.checkbox(&mut dlg.clip_children, "Clip contents")
                        .on_hover_text("Clip this group's children to its topmost child (clipping mask)");
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        act = A::Cancel;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("OK").clicked() {
                            act = A::Ok;
                        }
                    });
                });
            });

        // ── Live preview ──────────────────────────────────────────────────────
        // Apply the edited values straight to the document every frame so the
        // canvas updates while dragging. This is NOT recorded in history — OK
        // commits a single undo step and Cancel/close reverts to the original.
        {
            let dlg = self.object_options_dialog.as_ref().unwrap();
            match dlg.target {
                OptionsTarget::Layer(lid) => {
                    if let Some(cur) = doc.layers.get(&lid) {
                        let edited = dlg.edited_layer(cur);
                        if let Some(slot) = doc.layers.get_mut(&lid) {
                            *slot = edited;
                        }
                    }
                }
                OptionsTarget::Node(nid) => {
                    if let Some(cur) = doc.nodes.get(&nid) {
                        let edited = dlg.edited_node(cur);
                        if let Some(slot) = doc.nodes.get_mut(&nid) {
                            *slot = edited;
                        }
                    }
                }
            }
        }

        // Cancel or window close: revert the live preview, no undo entry.
        if !open || act == A::Cancel {
            if let Some(dlg) = self.object_options_dialog.take() {
                match dlg.target {
                    OptionsTarget::Layer(lid) => {
                        if let (Some(orig), Some(slot)) = (dlg.orig_layer, doc.layers.get_mut(&lid))
                        {
                            *slot = orig;
                        }
                    }
                    OptionsTarget::Node(nid) => {
                        if let (Some(orig), Some(slot)) = (dlg.orig_node, doc.nodes.get_mut(&nid)) {
                            *slot = orig;
                        }
                    }
                }
            }
            return;
        }

        // OK: the doc already holds the edited state (from the preview). Restore
        // the original, then execute one command orig → edited so it's a single,
        // clean undo step.
        if act == A::Ok {
            if let Some(dlg) = self.object_options_dialog.take() {
                match dlg.target {
                    OptionsTarget::Layer(layer_id) => {
                        if let (Some(orig), Some(edited)) =
                            (dlg.orig_layer.clone(), doc.layers.get(&layer_id).cloned())
                        {
                            // Restore the original, then commit the whole-layer swap
                            // as one undo step (ReplaceLayer carries every field, so
                            // new layer settings need no per-field plumbing).
                            if let Some(slot) = doc.layers.get_mut(&layer_id) {
                                *slot = orig.clone();
                            }
                            history.execute(
                                Command::ReplaceLayer {
                                    old: orig,
                                    new: edited,
                                },
                                doc,
                            );
                        }
                    }
                    OptionsTarget::Node(node_id) => {
                        if let (Some(orig), Some(edited)) =
                            (dlg.orig_node.clone(), doc.nodes.get(&node_id).cloned())
                        {
                            if let Some(slot) = doc.nodes.get_mut(&node_id) {
                                *slot = orig.clone();
                            }
                            let cmd = Command::UpdateNode {
                                old: orig,
                                new: edited,
                            };
                            history.execute(cmd, doc);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn draw_merge_vertices_dialog(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        if self.merge_vertices_dialog.is_none() {
            return;
        }

        #[derive(PartialEq)]
        enum Action {
            None,
            Cancel,
            Apply,
        }
        let mut action = Action::None;
        let mut open = true;

        let node_name = self
            .merge_vertices_dialog
            .as_ref()
            .unwrap()
            .node_name
            .clone();
        let node_id = self.merge_vertices_dialog.as_ref().unwrap().node_id;

        // Refresh the per-threshold preview cache (shared with the canvas
        // overlay) so the "Points: N → M" readout is always in sync, regardless
        // of draw order. The weld still runs only when the threshold changes.
        let orig_points = self.merge_vertices_dialog.as_ref().unwrap().orig_points;
        let new_points = {
            let dlg = self.merge_vertices_dialog.as_mut().unwrap();
            if let Some(node) = doc.nodes.get(&dlg.node_id) {
                if let SceneNodeKind::Path(pn) = &node.kind {
                    if dlg.preview.is_none() || dlg.cached_thr != dlg.threshold {
                        dlg.preview = Some(photonic_core::ops::merge::merge_vertices_by_distance(
                            &pn.path_data,
                            dlg.threshold,
                        ));
                        dlg.cached_thr = dlg.threshold;
                    }
                }
            }
            dlg.preview
                .as_ref()
                .map(photonic_core::ops::simplify::count_points)
        };

        egui::Window::new("Merge Vertices by Distance")
            .collapsible(false)
            .resizable(false)
            .fixed_size([260.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("Node: {}", node_name));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Distance");
                    ui.add(
                        egui::DragValue::new(
                            &mut self.merge_vertices_dialog.as_mut().unwrap().threshold,
                        )
                        .range(0.01..=100.0)
                        .speed(0.05)
                        .max_decimals(2),
                    );
                });
                ui.label(
                    RichText::new("Larger = weld anchors farther apart")
                        .weak()
                        .small(),
                );
                ui.add_space(4.0);
                match new_points {
                    Some(new) => {
                        ui.label(format!("Points: {} → {}", orig_points, new));
                    }
                    None => {
                        ui.label(format!("Points: {}", orig_points));
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Action::Cancel;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Apply").clicked() {
                            action = Action::Apply;
                        }
                    });
                });
            });

        let threshold = self
            .merge_vertices_dialog
            .as_ref()
            .map(|d| d.threshold)
            .unwrap_or(1.0);

        if !open {
            self.merge_vertices_dialog = None;
            return;
        }

        match action {
            Action::None => {}
            Action::Cancel => {
                self.merge_vertices_dialog = None;
            }
            Action::Apply => {
                // Reuse the preview the dialog/overlay already computed for this
                // threshold instead of re-running the weld.
                let cached = self
                    .merge_vertices_dialog
                    .as_mut()
                    .and_then(|d| d.preview.take());
                self.merge_vertices_dialog = None;
                if let Some(node) = doc.nodes.get(&node_id) {
                    if let SceneNodeKind::Path(pn) = &node.kind {
                        let welded = cached.unwrap_or_else(|| {
                            photonic_core::ops::merge::merge_vertices_by_distance(
                                &pn.path_data,
                                threshold,
                            )
                        });
                        let mut new_path = pn.clone();
                        new_path.path_data = welded;
                        let mut new_node = node.clone();
                        new_node.kind = SceneNodeKind::Path(new_path);
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                    }
                }
            }
        }
    }

    pub(crate) fn draw_find_replace_text_dialog(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        if self.find_replace_text_dialog.is_none() {
            return;
        }

        #[derive(PartialEq)]
        enum Action {
            None,
            Cancel,
            Apply,
        }
        let mut action = Action::None;
        let mut open = true;

        egui::Window::new("Find / Replace Text")
            .collapsible(false)
            .resizable(false)
            .fixed_size([320.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                let dlg = self.find_replace_text_dialog.as_mut().unwrap();
                ui.horizontal(|ui| {
                    ui.label("Find    ");
                    ui.add(egui::TextEdit::singleline(&mut dlg.find).desired_width(f32::INFINITY));
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Replace ");
                    ui.add(
                        egui::TextEdit::singleline(&mut dlg.replace).desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(6.0);
                ui.checkbox(&mut dlg.regex, "Regular expression");
                ui.checkbox(&mut dlg.case_sensitive, "Case sensitive");
                ui.checkbox(&mut dlg.selection_only, "Selection only");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Action::Cancel;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Apply").clicked() {
                            action = Action::Apply;
                        }
                    });
                });
            });

        if !open {
            self.find_replace_text_dialog = None;
            return;
        }

        match action {
            Action::None => {}
            Action::Cancel => {
                self.find_replace_text_dialog = None;
            }
            Action::Apply => {
                let dlg = self.find_replace_text_dialog.take().unwrap();

                // Build regex pattern
                let pattern = if dlg.regex {
                    dlg.find.clone()
                } else {
                    regex::escape(&dlg.find)
                };
                let pattern = if dlg.case_sensitive {
                    pattern
                } else {
                    format!("(?i){}", pattern)
                };
                let re = match regex::Regex::new(&pattern) {
                    Ok(r) => r,
                    Err(_) => return,
                };

                // Collect candidates
                let candidate_ids: Vec<NodeId> = if dlg.selection_only {
                    doc.selection.ids().copied().collect()
                } else {
                    doc.nodes
                        .values()
                        .filter(|n| matches!(n.kind, SceneNodeKind::Text(_)))
                        .map(|n| n.id)
                        .collect()
                };

                let mut cmds: Vec<Command> = Vec::new();
                for id in &candidate_ids {
                    if let Some(node) = doc.nodes.get(id) {
                        if let SceneNodeKind::Text(tn) = &node.kind {
                            let new_content = re
                                .replace_all(&tn.content, dlg.replace.as_str())
                                .into_owned();
                            if new_content != tn.content {
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Text(ref mut new_tn) = new_node.kind {
                                    new_tn.content = new_content;
                                }
                                cmds.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                }
                if !cmds.is_empty() {
                    history.execute(Command::Batch(cmds), doc);
                }
            }
        }
    }

    /// Centered modal listing release notes for versions the user just skipped.
    /// Dimming scrim behind, single "Got it" button to dismiss.
    /// Crash-report prompt shown on launch when local reports are pending (#59).
    ///
    /// - consent `None`  → a one-time modal asking whether to enable reporting;
    ///   answering it sets `crash_reporting_consent` to `Some(bool)`.
    /// - consent `Some(true)`  → a dismissable banner offering Report / Dismiss.
    /// - consent `Some(false)` → nothing is offered (reports stay on disk for the
    ///   settings page; we simply stop nagging).
    ///
    /// Reported or dismissed files are deleted so they are not re-offered.
    pub(crate) fn draw_crash_report_prompt(&mut self, ctx: &egui::Context) {
        match self.prefs.crash_reporting_consent {
            None => {
                // One-time consent modal.
                egui::Area::new(egui::Id::new("crash_consent_scrim"))
                    .order(egui::Order::Middle)
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .show(ctx, |ui| {
                        let screen = ctx.screen_rect();
                        ui.painter()
                            .rect_filled(screen, 0.0, Color32::from_black_alpha(160));
                        ui.allocate_rect(screen, egui::Sense::click());
                    });

                let mut choice: Option<bool> = None;
                egui::Window::new(
                    RichText::new(format!("{}  Help improve Photonic", ph::SHIELD_CHECK))
                        .size(16.0),
                )
                .id(egui::Id::new("crash_consent_window"))
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(format!(
                        "Photonic closed unexpectedly. {} local crash report{} {} ready.",
                        self.pending_crash_reports.len(),
                        if self.pending_crash_reports.len() == 1 {
                            ""
                        } else {
                            "s"
                        },
                        if self.pending_crash_reports.len() == 1 {
                            "is"
                        } else {
                            "are"
                        },
                    ));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "May Photonic offer to file these as GitHub issues in future? \
                             A report includes the app version, time, OS/arch, the panic \
                             message and a backtrace. It never includes your document, file \
                             paths, or environment variables — and you always review the \
                             pre-filled issue in your browser before submitting it.",
                        )
                        .weak()
                        .small(),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                RichText::new(format!("{}  Enable & report", ph::PAPER_PLANE_TILT))
                                    .color(Color32::WHITE),
                            )
                            .clicked()
                        {
                            choice = Some(true);
                        }
                        if ui.button("Not now").clicked() {
                            choice = Some(false);
                        }
                    });
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(
                            "You can change this anytime in Edit ▸ Privacy & Diagnostics.",
                        )
                        .weak()
                        .small(),
                    );
                });

                if let Some(allow) = choice {
                    self.prefs.crash_reporting_consent = Some(allow);
                    self.prefs.save();
                    if allow {
                        // File the newest report immediately and clear ONLY that
                        // one from disk. Any remaining reports stay on disk and
                        // are offered again on next launch — deleting them here
                        // would silently destroy unreported diagnostics (the very
                        // data this feature exists to preserve). Delete-all is
                        // reserved for the explicit Dismiss action.
                        if let Some(path) = self.pending_crash_reports.last() {
                            if let Some(report) = photonic_core::diagnostics::load_report(path) {
                                ctx.open_url(egui::OpenUrl::new_tab(issue_url_for_report(&report)));
                            }
                            let _ = photonic_core::diagnostics::clear_report(path);
                        }
                    }
                    // Stop offering for this session; declining ("Not now") leaves
                    // every report on disk (still listed in the settings page).
                    self.pending_crash_reports.clear();
                }
            }
            Some(true) => {
                // Dismissable Report / Dismiss banner.
                let mut report = false;
                let mut dismiss = false;
                let n = self.pending_crash_reports.len();
                egui::Area::new(egui::Id::new("crash_report_banner"))
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 52.0))
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(Color32::from_rgb(48, 33, 33))
                            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(190, 90, 90)))
                            .rounding(10.0)
                            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(ph::BUG)
                                            .size(18.0)
                                            .color(Color32::from_rgb(240, 150, 150)),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "Photonic recovered from a crash — {n} report{} ready",
                                            if n == 1 { "" } else { "s" },
                                        ))
                                        .strong()
                                        .color(Color32::from_rgb(240, 226, 226)),
                                    );
                                    ui.add_space(8.0);
                                    if ui
                                        .button(
                                            RichText::new(format!(
                                                "{}  Report",
                                                ph::PAPER_PLANE_TILT
                                            ))
                                            .color(Color32::WHITE),
                                        )
                                        .clicked()
                                    {
                                        report = true;
                                    }
                                    if ui.button("Dismiss").clicked() {
                                        dismiss = true;
                                    }
                                });
                            });
                    });
                if report {
                    // File the newest report and clear ONLY that one from disk —
                    // identical semantics to the consent dialog and the settings
                    // "Report latest…" path. Remaining reports are re-offered next
                    // launch instead of being destroyed.
                    if let Some(path) = self.pending_crash_reports.last() {
                        if let Some(rep) = photonic_core::diagnostics::load_report(path) {
                            ctx.open_url(egui::OpenUrl::new_tab(issue_url_for_report(&rep)));
                        }
                        let _ = photonic_core::diagnostics::clear_report(path);
                    }
                    self.pending_crash_reports.clear();
                } else if dismiss {
                    // Explicit dismissal is the only delete-all path: discard
                    // every pending report from disk.
                    for p in &self.pending_crash_reports {
                        let _ = photonic_core::diagnostics::clear_report(p);
                    }
                    self.pending_crash_reports.clear();
                }
            }
            Some(false) => {
                // Declined: don't nag. Stop offering this session (reports remain
                // on disk and are still listed in the settings page).
                self.pending_crash_reports.clear();
            }
        }
    }

    pub(crate) fn draw_whats_new(&mut self, ctx: &egui::Context) {
        // Dim the rest of the app.
        egui::Area::new(egui::Id::new("whats_new_scrim"))
            .order(egui::Order::Middle)
            .fixed_pos(egui::pos2(0.0, 0.0))
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                ui.painter()
                    .rect_filled(screen, 0.0, Color32::from_black_alpha(160));
                // Swallow clicks on the backdrop.
                ui.allocate_rect(screen, egui::Sense::click());
            });

        let mut open = true;
        let cur = crate::update::CURRENT_VERSION;
        egui::Window::new(RichText::new(format!("{}  What's New", ph::SPARKLE)).size(17.0))
            .id(egui::Id::new("whats_new_window"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(
                    RichText::new(format!("Photonic updated to v{cur}"))
                        .color(Color32::from_rgb(148, 163, 184)),
                );
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for note in &self.whats_new_notes {
                            let title = match &note.date {
                                Some(d) => format!("v{}  ·  {}", note.version, d),
                                None => format!("v{}", note.version),
                            };
                            ui.label(
                                RichText::new(title)
                                    .strong()
                                    .size(15.0)
                                    .color(Color32::from_rgb(96, 165, 250)),
                            );
                            ui.add_space(2.0);
                            render_changelog_body(ui, &note.body);
                            ui.add_space(10.0);
                        }
                    });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new("Got it").color(Color32::WHITE))
                        .clicked()
                    {
                        open = false;
                    }
                });
            });

        if !open {
            self.show_whats_new = false;
            self.whats_new_notes.clear();
        }
    }

    /// MCP server status + restart modal (#170). Opened by clicking the MCP
    /// indicator in the status bar. Shows whether the server is listening, its
    /// endpoint, and — when it has failed — a button to re-spawn it without
    /// relaunching the app. The actual re-spawn happens in the winit host, which
    /// polls `mcp_restart_requested` each frame.
    pub(crate) fn draw_mcp_modal(&mut self, ctx: &egui::Context, mcp_running: bool) {
        let mut open = true;
        egui::Window::new(format!("{}  MCP Server", ph::PLUGS_CONNECTED))
            .id(egui::Id::new("mcp_modal"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(320.0);
                if mcp_running {
                    ui.label(
                        RichText::new(format!("{}  Running", ph::CHECK))
                            .strong()
                            .color(Color32::from_rgb(52, 211, 153)),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Endpoint:  http://127.0.0.1:{}/mcp", self.mcp_port))
                            .monospace()
                            .small(),
                    );
                    ui.label(
                        RichText::new(
                            "Registered in ~/.claude.json, so Claude Code can drive this document.",
                        )
                        .weak()
                        .small(),
                    );
                } else {
                    ui.label(
                        RichText::new(format!("{}  Offline", ph::X))
                            .strong()
                            .color(Color32::from_rgb(248, 113, 113)),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "The server isn't listening on port {}. Check whether another \
                             application is using this port, then restart the server.",
                            self.mcp_port,
                        ))
                        .weak()
                        .small(),
                    );
                }
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let can_restart = !mcp_running && self.mcp_restart_requested.is_some();
                    if ui
                        .add_enabled(
                            can_restart,
                            egui::Button::new(format!("{}  Restart server", ph::ARROW_CLOCKWISE)),
                        )
                        .on_hover_text("Re-spawn the MCP server thread")
                        .clicked()
                    {
                        if let Some(flag) = &self.mcp_restart_requested {
                            flag.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        self.file_status = Some("Restarting MCP server…".to_string());
                    }
                    if mcp_running {
                        ui.label(
                            RichText::new("Already running — nothing to restart.")
                                .weak()
                                .small(),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.show_mcp_modal = false;
                        }
                    });
                });
            });
        if !open {
            self.show_mcp_modal = false;
        }
    }

    pub(crate) fn run_export(&mut self, doc: &Document) {
        let Some(dlg) = &self.export_dialog else {
            return;
        };
        let format = dlg.format;
        let opts = dlg.export_opts();
        let png_w = dlg.png_width;
        let png_h = dlg.png_height;
        let artboard_export = dlg.artboard_export;
        let artboard_scale = dlg.artboard_scale;

        let (filter_name, ext) = match format {
            ExportFormat::Png => ("PNG image", "png"),
            ExportFormat::Jpeg => ("JPEG image", "jpg"),
            ExportFormat::WebP => ("WebP image", "webp"),
            ExportFormat::Gif => ("GIF image", "gif"),
            ExportFormat::Tiff => ("TIFF image", "tiff"),
            ExportFormat::Ico => ("Icon file", "ico"),
            ExportFormat::Svg => ("SVG vector", "svg"),
        };
        let default_name = format!("{}.{ext}", doc.name);
        let start_dir = self
            .current_file
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        let mut file_dialog = rfd::FileDialog::new()
            .add_filter(filter_name, &[ext])
            .set_file_name(&default_name);
        if let Some(dir) = start_dir {
            file_dialog = file_dialog.set_directory(dir);
        }
        let Some(path) = run_file_dialog(move || file_dialog.save_file()) else {
            return;
        };
        let path = if path.extension().is_none() {
            path.with_extension(ext)
        } else {
            path
        };

        // ── Batch artboard export: one raster file per artboard ──────────────
        // Driven explicitly by the dialog's `artboard_export` range (opened via
        // "Export Artboards…"). Each board renders at its own size × scale into
        // `<stem>_<name>.<ext>`. SVG/ICO have no batch form → fall through to a
        // single-file export.
        if let crate::app::ArtboardExport::Range { start, end } = artboard_export {
            let is_raster = matches!(
                format,
                ExportFormat::Png
                    | ExportFormat::Jpeg
                    | ExportFormat::WebP
                    | ExportFormat::Gif
                    | ExportFormat::Tiff
            );
            let n = doc.artboards.len();
            if is_raster && n > 0 {
                let s = start.clamp(1, n);
                let e = end.clamp(s, n);
                let scale = (artboard_scale.max(0.05)) as f64;
                let renderer = pollster::block_on(photonic_render::HeadlessRenderer::new());
                let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| doc.name.clone());
                let mut err: Option<String> = None;
                let mut count = 0usize;
                for ab in &doc.artboards[s - 1..e] {
                    let mut o = opts.clone();
                    o.region = Some((ab.x, ab.y, ab.width, ab.height));
                    let aw = ((ab.width * scale).round() as i64).clamp(1, 8192) as u32;
                    let ah = ((ab.height * scale).round() as i64).clamp(1, 8192) as u32;
                    let bytes = match format {
                        ExportFormat::Png => renderer.render_png_with_opts(doc, aw, ah, &o),
                        ExportFormat::Jpeg => renderer.render_jpeg_with_opts(doc, aw, ah, &o),
                        ExportFormat::WebP => renderer.render_webp_with_opts(doc, aw, ah, &o),
                        ExportFormat::Gif => renderer.render_gif_with_opts(doc, aw, ah, &o),
                        ExportFormat::Tiff => renderer.render_tiff_with_opts(doc, aw, ah, &o),
                        _ => unreachable!(),
                    };
                    let safe: String = ab
                        .name
                        .chars()
                        .map(|c| if c.is_alphanumeric() { c } else { '_' })
                        .collect();
                    let p = parent.join(format!("{stem}_{safe}.{ext}"));
                    if let Err(e) = std::fs::write(&p, bytes) {
                        err = Some(e.to_string());
                        break;
                    }
                    count += 1;
                }
                self.export_dialog = None;
                self.file_status = Some(match err {
                    None => format!("Exported {count} artboard(s) → {stem}_*.{ext}"),
                    Some(e) => format!("Export failed: {e}"),
                });
                return;
            }
        }

        let result = match format {
            ExportFormat::Svg => {
                // Honor the Background selector: Transparent => no rect,
                // Artboard => a white background rect.
                let background = match opts.background {
                    ExportBackground::Transparent => None,
                    ExportBackground::Artboard => Some(Color::WHITE),
                };
                let svg = photonic_core::export::export_svg(
                    doc,
                    &photonic_core::export::SvgExportOptions {
                        background,
                        ..Default::default()
                    },
                );
                std::fs::write(&path, svg).map_err(|e| e.to_string())
            }
            ExportFormat::Png => {
                let renderer = pollster::block_on(photonic_render::HeadlessRenderer::new());
                let bytes = renderer.render_png_with_opts(doc, png_w, png_h, &opts);
                std::fs::write(&path, bytes).map_err(|e| e.to_string())
            }
            ExportFormat::Jpeg => {
                let renderer = pollster::block_on(photonic_render::HeadlessRenderer::new());
                let bytes = renderer.render_jpeg_with_opts(doc, png_w, png_h, &opts);
                std::fs::write(&path, bytes).map_err(|e| e.to_string())
            }
            ExportFormat::WebP => {
                let renderer = pollster::block_on(photonic_render::HeadlessRenderer::new());
                let bytes = renderer.render_webp_with_opts(doc, png_w, png_h, &opts);
                std::fs::write(&path, bytes).map_err(|e| e.to_string())
            }
            ExportFormat::Gif => {
                let renderer = pollster::block_on(photonic_render::HeadlessRenderer::new());
                let bytes = renderer.render_gif_with_opts(doc, png_w, png_h, &opts);
                std::fs::write(&path, bytes).map_err(|e| e.to_string())
            }
            ExportFormat::Tiff => {
                let renderer = pollster::block_on(photonic_render::HeadlessRenderer::new());
                let bytes = renderer.render_tiff_with_opts(doc, png_w, png_h, &opts);
                std::fs::write(&path, bytes).map_err(|e| e.to_string())
            }
            ExportFormat::Ico => {
                let renderer = pollster::block_on(photonic_render::HeadlessRenderer::new());
                renderer
                    .render_ico_with_opts(doc, &opts)
                    .and_then(|b| std::fs::write(&path, b).map_err(Into::into))
                    .map_err(|e| e.to_string())
            }
        };

        self.export_dialog = None;
        self.file_status = Some(match result {
            Ok(_) => format!(
                "Exported {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            Err(e) => format!("Export failed: {e}"),
        });
    }
}
