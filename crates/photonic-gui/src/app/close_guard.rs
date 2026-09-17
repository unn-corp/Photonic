//! Unsaved-changes guards: the Save/Discard/Cancel prompt shown when closing a
//! dirty document tab, and the Save all/Discard all/Cancel prompt shown when the
//! user tries to quit the app with unsaved work. Both reuse the scrim + centered
//! `egui::Window` modal pattern used elsewhere in `dialogs.rs`.

use super::*;

/// The choice made in an unsaved-changes modal.
#[derive(PartialEq)]
enum UnsavedChoice {
    None,
    Save,
    Discard,
    Cancel,
}

impl PhotonicApp {
    /// Save the document in tab `idx` to disk, prompting Save-As when it has no
    /// path yet. Returns `true` on a successful write, `false` if the write failed
    /// or the user cancelled the Save-As dialog. Clears the tab's dirty flag and
    /// removes any recovery file on success.
    pub(crate) fn save_tab(
        &mut self,
        idx: usize,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) -> bool {
        if idx >= self.tabs.len() {
            return false;
        }
        let active = idx == self.active_tab;
        let existing = if active {
            self.current_file.clone()
        } else {
            self.tabs[idx].current_file.clone()
        };

        // Resolve a target path — prompt Save-As for an untitled document.
        let path = match existing {
            Some(p) => p,
            None => {
                let default_name = format!("{}.{}", self.tabs[idx].title, PHOTON_FILE_EXTENSION);
                let dialog = rfd::FileDialog::new()
                    .add_filter("Photonic", &[PHOTON_FILE_EXTENSION])
                    .set_file_name(&default_name);
                match run_file_dialog(move || dialog.save_file()) {
                    Some(p) => {
                        if p.extension().is_none() {
                            p.with_extension(PHOTON_FILE_EXTENSION)
                        } else {
                            p
                        }
                    }
                    None => return false, // cancelled
                }
            }
        };

        // Write (active doc uses the live params; parked uses the owned tab state).
        let ok = if active {
            write_photon_file(&path, doc, history).is_ok()
        } else {
            let tab = &mut self.tabs[idx];
            write_photon_file(&path, &tab.document, &mut tab.history).is_ok()
        };

        if !ok {
            self.file_status = Some("Save failed".into());
            return false;
        }

        let node = if active {
            history.current_node()
        } else {
            self.tabs[idx].history.current_node()
        };
        let doc_name = if active {
            doc.name.clone()
        } else {
            self.tabs[idx].document.name.clone()
        };
        self.welcome.add_recent(path.clone(), doc_name);
        if active {
            self.current_file = Some(path.clone());
        }
        {
            let tab = &mut self.tabs[idx];
            if !active {
                tab.current_file = Some(path.clone());
                tab.title = Self::tab_title(&tab.document, &tab.current_file);
            }
            tab.dirty = false;
            tab.last_saved_node = Some(node);
            if let Some(rp) = tab.recovery_path.take() {
                let _ = std::fs::remove_file(rp);
            }
        }
        self.file_status = Some(format!(
            "Saved {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        true
    }

    /// Draw whichever unsaved-changes modal is active (single-tab close or app
    /// quit). Returns `true` if the active document changed (so `draw` can mark it
    /// modified for a redraw).
    pub(crate) fn draw_unsaved_changes_modals(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        view: &mut CanvasView,
        history: &mut CommandHistory,
    ) -> bool {
        let mut changed = false;
        changed |= self.draw_close_tab_prompt(ctx, doc, view, history);
        changed |= self.draw_quit_prompt(ctx, doc, view, history);
        changed
    }

    /// Save/Discard/Cancel modal for closing one dirty tab.
    fn draw_close_tab_prompt(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        view: &mut CanvasView,
        history: &mut CommandHistory,
    ) -> bool {
        let Some(idx) = self.close_tab_prompt else {
            return false;
        };
        if idx >= self.tabs.len() {
            self.close_tab_prompt = None;
            return false;
        }
        let title = self.tabs[idx].title.clone();

        scrim(ctx, "close_tab_scrim");
        let mut choice = UnsavedChoice::None;
        egui::Window::new(RichText::new(format!("{}  Unsaved changes", ph::WARNING)).size(16.0))
            .id(egui::Id::new("close_tab_prompt"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(format!(
                    "\"{title}\" has unsaved changes. Save before closing?"
                ));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        choice = UnsavedChoice::Save;
                    }
                    if ui.button("Discard").clicked() {
                        choice = UnsavedChoice::Discard;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            choice = UnsavedChoice::Cancel;
                        }
                    });
                });
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            choice = UnsavedChoice::Cancel;
        }

        match choice {
            UnsavedChoice::Save => {
                if self.save_tab(idx, doc, history) {
                    self.close_tab_prompt = None;
                    self.close_tab(idx, doc, history, view);
                    return true;
                }
                // Save-As cancelled → keep the modal up.
                false
            }
            UnsavedChoice::Discard => {
                self.close_tab_prompt = None;
                self.close_tab(idx, doc, history, view);
                true
            }
            UnsavedChoice::Cancel => {
                self.close_tab_prompt = None;
                false
            }
            UnsavedChoice::None => false,
        }
    }

    /// Save all/Discard all/Cancel modal shown when quitting with unsaved work.
    fn draw_quit_prompt(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        _view: &mut CanvasView,
        history: &mut CommandHistory,
    ) -> bool {
        if !self.close_requested {
            return false;
        }
        // Nothing unsaved → allow the quit through immediately.
        if !self.any_unsaved() {
            self.close_requested = false;
            self.close_confirmed = true;
            return false;
        }

        let unsaved: Vec<String> = self
            .tabs
            .iter()
            .filter(|t| t.dirty)
            .map(|t| t.title.clone())
            .collect();

        scrim(ctx, "quit_scrim");
        let mut choice = UnsavedChoice::None;
        let mut save_all = false;
        egui::Window::new(RichText::new(format!("{}  Quit Photonic?", ph::WARNING)).size(16.0))
            .id(egui::Id::new("quit_prompt"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(format!(
                    "{} document(s) have unsaved changes:",
                    unsaved.len()
                ));
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for name in &unsaved {
                            ui.label(RichText::new(format!("  • {name}")).weak());
                        }
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Save all & quit").clicked() {
                        save_all = true;
                    }
                    if ui.button("Discard all & quit").clicked() {
                        choice = UnsavedChoice::Discard;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            choice = UnsavedChoice::Cancel;
                        }
                    });
                });
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            choice = UnsavedChoice::Cancel;
        }
        if save_all {
            choice = UnsavedChoice::Save;
        }

        match choice {
            UnsavedChoice::Save => {
                // Save every dirty tab; abort the quit if any Save-As is cancelled.
                let dirty: Vec<usize> = (0..self.tabs.len())
                    .filter(|&i| self.tabs[i].dirty)
                    .collect();
                let mut all_ok = true;
                for i in dirty {
                    if !self.save_tab(i, doc, history) {
                        all_ok = false;
                        break;
                    }
                }
                if all_ok {
                    self.close_requested = false;
                    self.close_confirmed = true;
                }
                true
            }
            UnsavedChoice::Discard => {
                self.close_requested = false;
                self.close_confirmed = true;
                false
            }
            UnsavedChoice::Cancel => {
                self.close_requested = false;
                false
            }
            UnsavedChoice::None => false,
        }
    }
}

/// Darken the screen behind a modal and swallow clicks to the app underneath.
pub(crate) fn scrim(ctx: &egui::Context, id: &str) {
    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Middle)
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(ctx, |ui| {
            let screen = ctx.screen_rect();
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(160));
            ui.allocate_rect(screen, egui::Sense::click());
        });
}
