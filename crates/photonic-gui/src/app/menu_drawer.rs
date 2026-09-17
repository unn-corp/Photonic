use super::*;

impl PhotonicApp {
    pub(crate) fn draw_menu_drawer(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        view: &mut CanvasView,
        history: &mut CommandHistory,
        toolbar_rect: egui::Rect,
        mut doc_modified: bool,
    ) -> bool {
        // Height the open menu drawer occupies, used by the left rail/drawer to
        // step out from under it (they are created later in the same frame).
        self.menu_drawer_height = 0.0;
        if let Some(drawer_kind) = self.active_drawer {
            let screen = ctx.screen_rect();
            // Open the drawer just below the top toolbar.
            let toolbar_bottom = toolbar_rect.bottom();
            let drawer_height = (screen.height() * 0.6).max(300.0);
            let content_height = drawer_height - 24.0; // subtract Frame::popup inner_margin (12 * 2)
            let drawer_width = screen.width();

            let drawer_resp = egui::Area::new(egui::Id::new("menu_drawer"))
                .fixed_pos(egui::pos2(0.0, toolbar_bottom))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    // Bound the Area width so horizontal_wrapped has a wrap point.
                    ui.set_width(drawer_width);
                    egui::Frame::popup(&ctx.style())
                        .inner_margin(egui::Margin::same(12.0))
                        .show(ui, |ui| {
                    match drawer_kind {
                        DrawerKind::File => {
                            // ── File drawer ───────────────────────────────────
                            let new_sel = draw_two_column_menu(
                                ui, 160.0, content_height, FILE_OPTIONS,
                                self.selected_drawer_option,
                                |ui, selected| match selected {
                                    None => {
                                        ui.add_space(40.0);
                                        ui.vertical_centered(|ui| {
                                            ui.label(egui::RichText::new("Select an option").weak());
                                        });
                                    }
                                    Some(0) => {
                                        ui.label(RichText::new("Document").strong());
                                        ui.add_space(4.0);
                                        if ui.button("  New  ").clicked() {
                                            // Open the same new-document flow as the
                                            // welcome screen, as a modal over the canvas.
                                            self.new_document_modal =
                                                Some(crate::welcome::NewDocumentForm::new());
                                            self.active_drawer = None;
                                            self.selected_drawer_option = None;
                                        }
                                        if ui.button("  Open…  ").clicked() {
                                            self.active_drawer = None;
                                            self.selected_drawer_option = None;
                                            if let Some(path) = run_file_dialog(|| {
                                                rfd::FileDialog::new()
                                                    .add_filter("Photonic", &[PHOTON_FILE_EXTENSION])
                                                    .add_filter("SVG", &["svg"])
                                                    .add_filter("Images", &IMAGE_EXTENSIONS)
                                                    .add_filter("All supported", &{
                                                        let mut all = vec![PHOTON_FILE_EXTENSION, "svg"];
                                                        all.extend(IMAGE_EXTENSIONS);
                                                        all
                                                    })
                                                    .pick_file()
                                            }) {
                                                // A photo isn't a document: place it
                                                // into the current one as a raster
                                                // layer instead of trying to load it.
                                                if is_image_path(&path) {
                                                    self.place_image_file(doc, history, &path);
                                                    doc_modified = true;
                                                } else {
                                                match load_document(&path) {
                                                    Ok((loaded, hist_snap)) => {
                                                        self.welcome.add_recent(path.clone(), loaded.name.clone());
                                                        // Open in a NEW tab, leaving other docs untouched.
                                                        let mut new_history = CommandHistory::default();
                                                        apply_opened_history(&mut new_history, hist_snap);
                                                        self.file_status = Some(format!("Opened {}", path.file_name().unwrap_or_default().to_string_lossy()));
                                                        let native_path = native_project_path(&path);
                                                        self.open_in_new_tab(doc, history, view, loaded, new_history, native_path);
                                                        doc_modified = true;
                                                    }
                                                    Err(e) => self.file_status = Some(format!("Open failed: {e}")),
                                                }
                                                }
                                            }
                                        }
                                        if ui.button("  Place Image…  ")
                                            .on_hover_text(
                                                "Import a PNG/JPEG/WebP as a raster layer \
                                                 into the current document",
                                            )
                                            .clicked()
                                        {
                                            self.active_drawer = None;
                                            self.selected_drawer_option = None;
                                            if let Some(path) = run_file_dialog(|| {
                                                rfd::FileDialog::new()
                                                    .add_filter("Images", &IMAGE_EXTENSIONS)
                                                    .pick_file()
                                            }) {
                                                self.place_image_file(doc, history, &path);
                                                doc_modified = true;
                                            }
                                        }
                                    }
                                    Some(1) => {
                                        ui.label(RichText::new("Save").strong());
                                        ui.add_space(4.0);
                                        let can_save = self.current_file.is_some();
                                        if ui.add_enabled(can_save, egui::Button::new("  Save  ")).clicked() {
                                            self.active_drawer = None;
                                            self.selected_drawer_option = None;
                                            if let Some(path) = &self.current_file.clone() {
                                                match write_photon_file(path, doc, history) {
                                                    Ok(_) => {
                                                        self.welcome.add_recent(path.clone(), doc.name.clone());
                                                        self.mark_active_tab_saved(history);
                                                        self.file_status = Some(format!("Saved {}", path.file_name().unwrap_or_default().to_string_lossy()));
                                                    }
                                                    Err(e) => self.file_status = Some(format!("Save failed: {e}")),
                                                }
                                            }
                                        }
                                        if ui.button("  Save As…  ").clicked() {
                                            self.active_drawer = None;
                                            self.selected_drawer_option = None;
                                            let default_name = self.current_file.as_ref()
                                                .and_then(|p| p.file_name())
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_else(|| {
                                                    format!("{}.{}", doc.name, PHOTON_FILE_EXTENSION)
                                                });
                                            let start_dir = self.current_file.as_ref()
                                                .and_then(|p| p.parent())
                                                .map(|p| p.to_path_buf());
                                            let mut dialog = rfd::FileDialog::new()
                                                .add_filter("Photonic", &[PHOTON_FILE_EXTENSION])
                                                .set_file_name(&default_name);
                                            if let Some(dir) = start_dir {
                                                dialog = dialog.set_directory(dir);
                                            }
                                            if let Some(path) = run_file_dialog(move || dialog.save_file()) {
                                                let path = if path.extension().is_none() {
                                                    path.with_extension(PHOTON_FILE_EXTENSION)
                                                } else { path };
                                                match write_photon_file(&path, doc, history) {
                                                    Ok(_) => {
                                                        self.welcome.add_recent(path.clone(), doc.name.clone());
                                                        self.file_status = Some(format!("Saved {}", path.file_name().unwrap_or_default().to_string_lossy()));
                                                        self.current_file = Some(path);
                                                        self.mark_active_tab_saved(history);
                                                    }
                                                    Err(e) => self.file_status = Some(format!("Save failed: {e}")),
                                                }
                                            }
                                        }
                                    }
                                    Some(2) => {
                                        ui.label(RichText::new("Export").strong());
                                        ui.add_space(4.0);
                                        if ui.button("  Export…  ").clicked() {
                                            self.active_drawer = None;
                                            self.selected_drawer_option = None;
                                            self.export_dialog = Some(ExportDialog::new(doc));
                                        }
                                    }
                                    _ => {}
                                },
                            );
                            self.selected_drawer_option = new_sel;
                        }

                        DrawerKind::Edit => {
                            // ── Preferences drawer ────────────────────────────
                            let new_sel = draw_two_column_menu(
                                ui, 160.0, content_height, EDIT_OPTIONS,
                                self.selected_drawer_option,
                                |ui, selected| match selected {
                                    None => {
                                        ui.add_space(40.0);
                                        ui.vertical_centered(|ui| {
                                            ui.label(egui::RichText::new("Select an option").weak());
                                        });
                                    }
                                    Some(0) => {
                                        ui.label(RichText::new("Appearance").strong());
                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            use crate::preferences::ThemeMode;
                                            ui.label("Theme");
                                            ui.add_space(4.0);
                                            let mode = self.prefs.theme_mode;
                                            for (opt, label) in [
                                                (ThemeMode::System, format!("{} System", ph::DESKTOP)),
                                                (ThemeMode::Dark, format!("{} Dark", ph::MOON)),
                                                (ThemeMode::Light, format!("{} Light", ph::SUN)),
                                            ] {
                                                if ui.selectable_label(mode == opt, label).clicked() {
                                                    self.prefs.theme_mode = opt;
                                                }
                                            }
                                        })
                                        .response
                                        .on_hover_text(
                                            "System follows the desktop's light/dark setting; \
                                             Dark and Light pin the palette regardless.",
                                        );
                                        ui.horizontal(|ui| {
                                            ui.label("UI Scale");
                                            egui::ComboBox::from_id_salt("ui_scale")
                                                .selected_text(format!("{}%", (self.prefs.ui_scale * 100.0) as u32))
                                                .show_ui(ui, |ui| {
                                                    for &scale in &[0.75f32, 1.0, 1.25, 1.5, 2.0] {
                                                        ui.selectable_value(
                                                            &mut self.prefs.ui_scale,
                                                            scale,
                                                            format!("{}%", (scale * 100.0) as u32),
                                                        );
                                                    }
                                                });
                                        });
                                    }
                                    Some(1) => {
                                        ui.label(RichText::new("Canvas").strong());
                                        ui.add_space(4.0);
                                        ui.checkbox(&mut self.prefs.show_grid, "Show Grid");
                                        ui.add_enabled_ui(self.prefs.show_grid, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label("Grid Size");
                                                egui::ComboBox::from_id_salt("grid_size")
                                                    .selected_text(format!("{}px", self.prefs.grid_size))
                                                    .show_ui(ui, |ui| {
                                                        for size in [8u32, 16, 32, 64] {
                                                            ui.selectable_value(
                                                                &mut self.prefs.grid_size,
                                                                size,
                                                                format!("{}px", size),
                                                            );
                                                        }
                                                    });
                                            });
                                            ui.horizontal(|ui| {
                                                ui.label("Grid Color");
                                                crate::color_popup::ColorPopup::swatch_f32(ui, &mut self.prefs.grid_color);
                                            });
                                            ui.checkbox(&mut self.prefs.snap_to_grid, "Snap to Grid");
                                            ui.checkbox(&mut self.prefs.snap_to_objects, "Snap to Objects")
                                                .on_hover_text("Align edges/centers to nearby objects (and equal-spacing) while dragging (#66).");
                                            ui.checkbox(&mut self.prefs.snap_to_artboard, "Snap to Artboard")
                                                .on_hover_text("Align to the artboard/canvas edges, center, and margins while dragging (#211).");
                                            ui.checkbox(&mut self.prefs.snap_to_anchors, "Snap to Anchor Points")
                                                .on_hover_text("Align to path vertices while dragging — off by default; dense paths add many targets (#211).");
                                            ui.add_enabled_ui(self.prefs.snap_to_objects || self.prefs.snap_to_artboard || self.prefs.snap_to_anchors, |ui| {
                                                ui.checkbox(&mut self.prefs.snap_show_guides, "Show Smart Guides");
                                                ui.horizontal(|ui| {
                                                    ui.label("Snap Tolerance");
                                                    ui.add(
                                                        egui::Slider::new(&mut self.prefs.snap_tolerance_px, 1.0..=20.0)
                                                            .suffix("px"),
                                                    );
                                                });
                                            });
                                        });
                                        ui.checkbox(&mut self.prefs.show_rulers, "Show Rulers");
                                        // #208: icon-design aids.
                                        ui.checkbox(&mut self.prefs.show_keyline_grid, "Icon Keyline Grid")
                                            .on_hover_text("Overlay the Material/Apple icon keyline template (square, circle, portrait & landscape safe areas) centered on the artboard (#208).");
                                        ui.checkbox(&mut self.prefs.snap_to_pixel, "Snap to Pixel")
                                            .on_hover_text("Snap drawing/moving to whole document pixels for crisp icon geometry (#208).");
                                        // The three view-preview modes are mutually exclusive:
                                        // enabling one clears the others.
                                        if ui.checkbox(&mut self.outline_mode, "Outline Mode")
                                            .on_hover_text("Show path wireframes only (no fills or strokes). Shortcut: Ctrl+Y")
                                            .changed() && self.outline_mode
                                        {
                                            self.pixel_preview = false;
                                            self.overprint_preview = false;
                                        }
                                        if ui.checkbox(&mut self.pixel_preview, "Pixel Preview")
                                            .on_hover_text("Show the active artboard at its export pixel size with nearest-neighbour sampling, so aliasing and pixel snapping match the exported file. Shortcut: Ctrl+Alt+Y")
                                            .changed()
                                        {
                                            if self.pixel_preview {
                                                self.outline_mode = false;
                                                self.overprint_preview = false;
                                            }
                                            self.preview_tex_cache = None;
                                        }
                                        if ui.checkbox(&mut self.overprint_preview, "Overprint Preview")
                                            .on_hover_text("Simulate overprint: solid fills matching an overprint-flagged spot color multiply into the backdrop instead of knocking out. Shortcut: Ctrl+Shift+Y")
                                            .changed()
                                        {
                                            if self.overprint_preview {
                                                self.outline_mode = false;
                                                self.pixel_preview = false;
                                            }
                                            self.preview_tex_cache = None;
                                        }
                                        ui.separator();
                                        ui.label(egui::RichText::new("Guides").strong());
                                        ui.checkbox(&mut self.guides_visible, "Show Guides")
                                            .on_hover_text("Show/hide ruler guides on the canvas. Shortcut: Ctrl+;");
                                        let guide_count = doc.guides.len();
                                        ui.add_enabled_ui(guide_count > 0, |ui| {
                                            if ui.button(format!("Clear All Guides ({})", guide_count)).clicked() {
                                                self.pending_panel_actions.push(panels::PanelAction::ClearGuides);
                                            }
                                        });
                                    }
                                    Some(2) => {
                                        ui.label(RichText::new("Tool Defaults").strong());
                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            ui.label("Default Fill");
                                            // Gamma-sRGB store → shared sRGBA picker (issue #185).
                                            if crate::color_popup::ColorPopup::swatch_f32(ui, &mut self.prefs.default_fill_color).changed() {
                                                self.fill_color = self.prefs.default_fill_color;
                                            }
                                        });
                                        ui.checkbox(&mut self.prefs.default_stroke_enabled, "Default Stroke");
                                        ui.add_enabled_ui(self.prefs.default_stroke_enabled, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label("Stroke Color");
                                                // Gamma-sRGB store → shared sRGBA picker (issue #185).
                                                crate::color_popup::ColorPopup::swatch_f32(ui, &mut self.prefs.default_stroke_color);
                                            });
                                            ui.horizontal(|ui| {
                                                ui.label("Stroke Width");
                                                ui.add(
                                                    egui::Slider::new(&mut self.prefs.default_stroke_width, 0.5..=32.0)
                                                        .suffix(" px"),
                                                );
                                            });
                                        });
                                    }
                                    Some(3) => {
                                        ui.label(RichText::new("Behavior").strong());
                                        ui.add_space(4.0);
                                        ui.checkbox(&mut self.prefs.console_open_on_start, "Open Console on Start");
                                        #[cfg(target_os = "linux")]
                                        ui.checkbox(&mut self.prefs.force_x11_backend, "Use X11/XWayland backend")
                                            .on_hover_text("Restart required. Enables file drag-and-drop on Wayland sessions via XWayland (#198).");
                                        ui.add_space(4.0);
                                        ui.checkbox(&mut self.prefs.auto_check_updates, "Check for updates on launch")
                                            .on_hover_text("Once per launch, ask GitHub for a newer release and show a banner if one exists. No automatic download.");
                                        ui.add_space(4.0);
                                        ui.checkbox(&mut self.prefs.reduced_motion, "Reduced motion")
                                            .on_hover_text("Make drawer open/close transitions instant instead of animating the width.");
                                        ui.add_space(4.0);
                                        // Proposal 213 — social-first AS-1 velocity.
                                        ui.checkbox(
                                            &mut self.prefs.auto_place_import_on_timeline,
                                            "Auto-place imports on timeline",
                                        )
                                        .on_hover_text(
                                            "When on, importing media also inserts it on the first \
                                             compatible track at the playhead (CapCut-class default).",
                                        );
                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            if ui
                                                .button("Reset video coach marks")
                                                .on_hover_text(
                                                    "Show the Import → Split → Export coach again \
                                                     next time you enter video mode.",
                                                )
                                                .clicked()
                                            {
                                                self.prefs.video_coach_dismissed = false;
                                                self.prefs.video_coach_shown_once = false;
                                                self.prefs.video_coach_step = 0;
                                                // Re-arm the session latch too,
                                                // so "show it again" means this
                                                // run, not the next launch.
                                                self.coach_allowed_this_session = true;
                                                self.prefs.save();
                                            }
                                        });
                                        ui.add_space(4.0);

                                        // ── Hotbar mode ───────────────────────────────
                                        ui.horizontal(|ui| {
                                            ui.label("Hotbar:");
                                            let mut mode = self.prefs.hotbar_mode;
                                            let changed = ui
                                                .selectable_value(&mut mode, HotbarMode::Static, "Static")
                                                .on_hover_text("Show the curated default set for each selection context.")
                                                .clicked()
                                                | ui
                                                .selectable_value(&mut mode, HotbarMode::Adaptive, "Adaptive")
                                                .on_hover_text("Rank each context's items by your own usage (most-used first), with pinned leading slots.")
                                                .clicked();
                                            if changed && mode != self.prefs.hotbar_mode {
                                                self.prefs.hotbar_mode = mode;
                                                // Force the hotbar order to rebuild under the new mode.
                                                self.hotbar_cache = None;
                                            }
                                        });
                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            ui.label("Arrow nudge (px):");
                                            ui.add(egui::DragValue::new(&mut self.prefs.nudge_distance)
                                                .speed(0.1)
                                                .range(0.1..=100.0)
                                                .fixed_decimals(1))
                                                .on_hover_text("Distance moved per arrow key press (Shift×10)");
                                        });

                                        // ── Autosave ─────────────────────────────────
                                        ui.add_space(10.0);
                                        ui.separator();
                                        ui.label(RichText::new("Autosave").strong());
                                        ui.label(
                                            RichText::new(
                                                "Periodically writes open documents to disk. Saved files record an \
                                                 \"Autosave\" branch in their history; unsaved files go to a recovery \
                                                 folder that's offered back on the next launch.",
                                            )
                                            .weak()
                                            .small(),
                                        );
                                        ui.add_space(4.0);
                                        ui.checkbox(&mut self.prefs.autosave_enabled, "Enable autosave");
                                        ui.add_space(4.0);
                                        ui.add_enabled_ui(self.prefs.autosave_enabled, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label("Every (minutes):");
                                                let mut minutes = self.prefs.autosave_interval_secs / 60.0;
                                                if ui.add(egui::DragValue::new(&mut minutes)
                                                    .speed(0.25)
                                                    .range(0.25..=120.0)
                                                    .fixed_decimals(2))
                                                    .on_hover_text("How often to autosave. Default 5 minutes.")
                                                    .changed()
                                                {
                                                    self.prefs.autosave_interval_secs = (minutes * 60.0).max(15.0);
                                                }
                                            });
                                        });

                                        // ── Project History ──────────────────────────
                                        ui.add_space(10.0);
                                        ui.separator();
                                        ui.label(RichText::new("Project History").strong());
                                        ui.label(
                                            RichText::new(
                                                "Undo/redo, checkpoints, and branches are saved inside the .photon file. \
                                                 This caps how much is kept; the oldest steps are dropped when the cap is hit.",
                                            )
                                            .weak()
                                            .small(),
                                        );
                                        ui.add_space(4.0);
                                        // Retention is size-only (#197): a single byte budget governs
                                        // trimming; you get a proactive warning before anything drops.
                                        ui.horizontal(|ui| {
                                            ui.label("Max history size (MB):");
                                            // Edit the OPEN document's cap — that is the effective limit
                                            // (`doc.history_max_mb` overrides the global preference). The
                                            // slider used to bind only to `self.prefs`, so changing it did
                                            // nothing to an already-open file. Fall back to the pref for the
                                            // display value when the doc has no explicit cap yet.
                                            let mut cap_mb =
                                                doc.history_max_mb.unwrap_or(self.prefs.history_max_mb);
                                            let resp = ui.add(egui::DragValue::new(&mut cap_mb)
                                                .speed(1.0)
                                                .range(1.0..=4000.0)
                                                .fixed_decimals(0))
                                                .on_hover_text("Caps this document's undo/redo + checkpoint history payload. Applies to the open document immediately, and becomes the default for new documents.");
                                            if resp.changed() {
                                                doc.history_max_mb = Some(cap_mb);
                                                self.prefs.history_max_mb = cap_mb;
                                            }
                                        });
                                        ui.add_space(4.0);
                                        // Live readout. history_byte_size serializes the whole history, so
                                        // throttle it to ~2 Hz even while this page is visible.
                                        let hist_steps = history.undo_depth();
                                        let now = ui.input(|i| i.time);
                                        if now - self.cached_history_bytes.0 > 0.5 {
                                            self.cached_history_bytes = (now, history.history_byte_size());
                                        }
                                        let hist_bytes = self.cached_history_bytes.1;
                                        ui.label(
                                            RichText::new(format!(
                                                "Currently: {} step{} · {} of history in file",
                                                hist_steps,
                                                if hist_steps == 1 { "" } else { "s" },
                                                format_bytes(hist_bytes),
                                            ))
                                            .weak()
                                            .small(),
                                        );
                                    }
                                    Some(4) => {
                                        ui.label(RichText::new("Keyboard Shortcuts").strong());
                                        ui.add_space(2.0);
                                        ui.label(
                                            RichText::new(format!(
                                                "{}  Press Ctrl/Cmd+K anywhere to open the command palette.",
                                                ph::COMMAND
                                            ))
                                            .weak()
                                            .small(),
                                        );
                                        ui.add_space(6.0);

                                        ui.horizontal(|ui| {
                                            if ui.button("Import Keymap…").clicked() {
                                                self.import_keymap_dialog();
                                            }
                                            if ui.button("Export Keymap…").clicked() {
                                                self.export_keymap_dialog();
                                            }
                                        });
                                        ui.add_space(6.0);

                                        // While capturing, the next non-modifier key press becomes the
                                        // new binding. Escape cancels.
                                        if let Some(cap_id) = self.shortcut_capture.clone() {
                                            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                                self.shortcut_capture = None;
                                            } else if let Some((k, m)) = ui.input(|i| {
                                                egui::Key::ALL
                                                    .iter()
                                                    .copied()
                                                    .find(|k| {
                                                        *k != egui::Key::Escape && i.key_pressed(*k)
                                                    })
                                                    .map(|k| (k, i.modifiers))
                                            }) {
                                                let b = crate::commands::KeyBinding {
                                                    key: k,
                                                    ctrl: m.ctrl || m.command || m.mac_cmd,
                                                    shift: m.shift,
                                                    alt: m.alt,
                                                    command: false,
                                                };
                                                self.prefs.keymap.insert(cap_id, b);
                                                self.prefs.save();
                                                self.shortcut_capture = None;
                                            }
                                        }

                                        let mut begin: Option<String> = None;
                                        let mut reset: Option<String> = None;
                                        egui::ScrollArea::vertical()
                                            .id_salt("shortcuts_scroll")
                                            .max_height((content_height - 70.0).max(120.0))
                                            .show(ui, |ui| {
                                                egui::Grid::new("shortcuts_grid")
                                                    .num_columns(3)
                                                    .striped(true)
                                                    .spacing([12.0, 6.0])
                                                    .show(ui, |ui| {
                                                        for def in crate::commands::REGISTRY {
                                                            ui.label(def.label);
                                                            let binding =
                                                                self.prefs.resolve_binding(def.id);
                                                            let capturing = self
                                                                .shortcut_capture
                                                                .as_deref()
                                                                == Some(def.id);
                                                            let btn_text = if capturing {
                                                                "Press a key…".to_string()
                                                            } else {
                                                                binding
                                                                    .map(|b| b.display())
                                                                    .unwrap_or_else(|| "—".to_string())
                                                            };
                                                            if ui
                                                                .add_sized(
                                                                    [140.0, 20.0],
                                                                    egui::Button::new(btn_text),
                                                                )
                                                                .on_hover_text(
                                                                    "Click, then press the new shortcut (Esc cancels)",
                                                                )
                                                                .clicked()
                                                            {
                                                                begin = Some(def.id.to_string());
                                                            }
                                                            ui.horizontal(|ui| {
                                                                if let Some(b) = binding {
                                                                    if let Some(other) = self
                                                                        .prefs
                                                                        .binding_conflict(def.id, b)
                                                                    {
                                                                        ui.colored_label(
                                                                            Color32::from_rgb(
                                                                                220, 150, 60,
                                                                            ),
                                                                            format!(
                                                                                "{} conflicts with {}",
                                                                                ph::WARNING, other
                                                                            ),
                                                                        );
                                                                    }
                                                                }
                                                                if self
                                                                    .prefs
                                                                    .keymap
                                                                    .contains_key(def.id)
                                                                    && ui
                                                                        .small_button("Reset")
                                                                        .clicked()
                                                                {
                                                                    reset =
                                                                        Some(def.id.to_string());
                                                                }
                                                            });
                                                            ui.end_row();
                                                        }
                                                    });
                                            });
                                        if let Some(id) = begin {
                                            self.shortcut_capture = Some(id);
                                        }
                                        if let Some(id) = reset {
                                            self.prefs.keymap.remove(&id);
                                            self.prefs.save();
                                        }
                                    }
                                    Some(5) => {
                                        ui.label(RichText::new(format!(
                                            "{}  Privacy & Diagnostics",
                                            ph::SHIELD_CHECK
                                        )).strong());
                                        ui.add_space(4.0);
                                        ui.label(
                                            RichText::new(
                                                "Photonic always writes a local crash report when it \
                                                 panics so a problem can be diagnosed. Reports are \
                                                 stored on this machine only and are never sent \
                                                 automatically.",
                                            )
                                            .weak()
                                            .small(),
                                        );
                                        ui.add_space(8.0);

                                        // Consent toggle (off by default; persisted as Some(_)).
                                        let mut consent =
                                            self.prefs.crash_reporting_consent.unwrap_or(false);
                                        if ui
                                            .checkbox(
                                                &mut consent,
                                                "Offer to file crash reports as GitHub issues",
                                            )
                                            .on_hover_text(
                                                "When enabled, after a crash Photonic offers to open a \
                                                 pre-filled GitHub issue that you review and submit \
                                                 yourself. Nothing leaves this machine without that action.",
                                            )
                                            .changed()
                                        {
                                            self.prefs.crash_reporting_consent = Some(consent);
                                            self.prefs.save();
                                        }
                                        ui.add_space(8.0);

                                        // Plain-language disclosure of collected / excluded data.
                                        ui.label(
                                            RichText::new("A report contains").strong().small(),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "•  App version, UTC time, OS and architecture\n\
                                                 •  The panic message and a backtrace",
                                            )
                                            .weak()
                                            .small(),
                                        );
                                        ui.add_space(4.0);
                                        ui.label(
                                            RichText::new("Never included").strong().small(),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "•  Document content or project data\n\
                                                 •  File names or paths\n\
                                                 •  Environment variables",
                                            )
                                            .weak()
                                            .small(),
                                        );
                                        ui.add_space(10.0);

                                        ui.horizontal(|ui| {
                                            if ui
                                                .button(format!(
                                                    "{}  Open crash-report folder",
                                                    ph::FOLDER_OPEN
                                                ))
                                                .on_hover_text(
                                                    "Reveal the folder holding crash reports \
                                                     (in your Photonic config folder).",
                                                )
                                                .clicked()
                                            {
                                                if let Some(dir) = photonic_core::crash_dir() {
                                                    let _ = std::fs::create_dir_all(&dir);
                                                    open_path_in_file_manager(&dir);
                                                }
                                            }
                                            if ui
                                                .button(format!("{}  Report a bug", ph::BUG))
                                                .on_hover_text(
                                                    "Open a blank GitHub issue in your browser.",
                                                )
                                                .clicked()
                                            {
                                                ui.ctx().open_url(egui::OpenUrl::new_tab(
                                                    blank_issue_url(),
                                                ));
                                            }
                                        });

                                        // Pending local crash reports — let the user file or clear
                                        // them directly from settings regardless of consent state.
                                        let pending =
                                            photonic_core::diagnostics::pending_reports();
                                        if !pending.is_empty() {
                                            ui.add_space(10.0);
                                            ui.separator();
                                            ui.label(
                                                RichText::new(format!(
                                                    "{}  {} pending crash report{}",
                                                    ph::WARNING,
                                                    pending.len(),
                                                    if pending.len() == 1 { "" } else { "s" },
                                                ))
                                                .small(),
                                            );
                                            ui.horizontal(|ui| {
                                                if ui
                                                    .button(format!(
                                                        "{}  Report latest…",
                                                        ph::PAPER_PLANE_TILT
                                                    ))
                                                    .clicked()
                                                {
                                                    if let Some(path) = pending.last() {
                                                        if let Some(report) =
                                                            photonic_core::diagnostics::load_report(
                                                                path,
                                                            )
                                                        {
                                                            ui.ctx().open_url(
                                                                egui::OpenUrl::new_tab(
                                                                    issue_url_for_report(&report),
                                                                ),
                                                            );
                                                        }
                                                        let _ = photonic_core::diagnostics::clear_report(path);
                                                        self.pending_crash_reports.clear();
                                                    }
                                                }
                                                if ui.button("Dismiss all").clicked() {
                                                    for p in &pending {
                                                        let _ = photonic_core::diagnostics::clear_report(p);
                                                    }
                                                    self.pending_crash_reports.clear();
                                                }
                                            });
                                        }
                                    }
                                    _ => {}
                                },
                            );
                            self.selected_drawer_option = new_sel;
                        }

                        DrawerKind::Tools => {
                            // ── Tools drawer ─────────────────────────────────
                            ui.label(
                                RichText::new("TOOLS")
                                    .small()
                                    .color(crate::theme::section_header_color(ui)),
                            );
                            ui.add_space(4.0);

                            const TOOL_CATEGORIES: &[(&str, &[Tool])] = &[
                                ("Selection & Navigation", &[Tool::Select, Tool::DirectSelect, Tool::Pan]),
                                ("Shapes", &[Tool::Rectangle, Tool::RoundedRect, Tool::Ellipse, Tool::Arc, Tool::Polygon, Tool::Star, Tool::Line, Tool::Grid, Tool::PolarGrid]),
                                ("Drawing & Text", &[Tool::Pen, Tool::ShapeBuilder, Tool::Text]),
                                ("Path Editing", &[Tool::Scissors, Tool::Knife, Tool::Eraser, Tool::MagicWand, Tool::Lasso, Tool::Pencil, Tool::Smooth, Tool::Width]),
                                ("Raster", &[Tool::RasterBrush, Tool::RasterEraser]),
                            ];

                            let mut tool_to_activate: Option<Tool> = None;
                            let mut pin_toggle: Option<Tool> = None;

                            egui::ScrollArea::vertical()
                                .id_salt("tools_drawer_scroll")
                                .max_height(content_height)
                                .show(ui, |ui| {
                                    ui.set_min_width(360.0);
                                    for (category, tools) in TOOL_CATEGORIES {
                                        ui.label(
                                            RichText::new(*category)
                                                .small()
                                                .color(Color32::from_rgb(110, 110, 150)),
                                        );
                                        ui.add_space(2.0);
                                        for tool in *tools {
                                            ui.horizontal(|ui| {
                                                let is_active = self.active_tool == *tool;
                                                let pinned = self.prefs.pinned_tools.contains(tool);

                                                let pin_color = if pinned {
                                                    Color32::from_rgb(110, 86, 207)
                                                } else {
                                                    Color32::from_gray(90)
                                                };
                                                let pin_hint = if pinned {
                                                    "Remove from sidebar hotbar"
                                                } else {
                                                    "Pin to sidebar hotbar"
                                                };
                                                if ui
                                                    .button(
                                                        RichText::new(egui_phosphor::regular::PUSH_PIN)
                                                            .color(pin_color),
                                                    )
                                                    .on_hover_text(pin_hint)
                                                    .clicked()
                                                {
                                                    pin_toggle = Some(*tool);
                                                }

                                                let tool_label = format!(
                                                    "{}  {}  —  {}",
                                                    tool.icon(),
                                                    tool.label(),
                                                    tool.description()
                                                );
                                                if ui.selectable_label(is_active, tool_label).clicked() {
                                                    tool_to_activate = Some(*tool);
                                                }
                                            });
                                        }
                                        ui.add_space(6.0);
                                    }

                                    if !self.prefs.pinned_tools.is_empty() {
                                        ui.separator();
                                        let pinned_names = self.prefs.pinned_tools
                                            .iter()
                                            .map(|t| t.label())
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        ui.label(
                                            RichText::new(format!(
                                                "{} Sidebar hotbar: {}",
                                                egui_phosphor::regular::PUSH_PIN,
                                                pinned_names
                                            ))
                                            .weak()
                                            .small(),
                                        );
                                    }
                                });

                            if let Some(tool) = pin_toggle {
                                if self.prefs.pinned_tools.contains(&tool) {
                                    self.prefs.pinned_tools.retain(|t| *t != tool);
                                } else {
                                    self.prefs.pinned_tools.push(tool);
                                }
                                self.prefs.save();
                            }
                            if let Some(tool) = tool_to_activate {
                                self.pen_points.clear();
                                self.pencil_points.clear();
                                self.lasso_points.clear();
                                self.isolated_group = None;
                                self.clear_point_edit();
                                self.active_tool = tool;
                                self.active_drawer = None;
                                self.selected_drawer_option = None;
                            }
                        }
                    } // match
                        }); // Frame::popup
                }); // Area inner

            // Publish the measured height for this frame's left rail/drawer.
            self.menu_drawer_height = drawer_resp.response.rect.height();

            // Close when the user clicks outside the drawer.
            // Also exclude the toolbar and pull-tab strip so their own buttons
            // can handle toggle state without fighting this "click outside" path.
            if ctx.input(|i| i.pointer.any_click()) {
                let clicked_inside = ctx
                    .input(|i| i.pointer.interact_pos())
                    .map(|pos| {
                        drawer_resp.response.rect.contains(pos) || toolbar_rect.contains(pos)
                    })
                    .unwrap_or(false);
                if !clicked_inside {
                    if self.active_drawer == Some(DrawerKind::Edit) {
                        self.prefs.save();
                    }
                    self.active_drawer = None;
                    self.selected_drawer_option = None;
                }
            }
        }
        doc_modified
    }
}
