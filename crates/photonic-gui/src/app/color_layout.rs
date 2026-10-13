//! Named local Color arrangements. These methods cannot modify a document/history.
use super::PhotonicApp;

enum LayoutAction {
    Save(String),
    Load(String),
    Remove(String),
    AskRemove(String),
    CancelRemove,
}

impl PhotonicApp {
    pub(super) fn draw_color_layout_menu(&mut self, ui: &mut egui::Ui) {
        let names: Vec<_> = self
            .prefs
            .color_layouts
            .iter()
            .map(|layout| layout.name.clone())
            .collect();
        let mut action = None;
        ui.menu_button("Layouts", |ui| {
            ui.label("Color workspace layouts");
            if names.is_empty() {
                ui.weak("No saved layouts yet");
            }
            for name in &names {
                ui.push_id(("color_layout", name), |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .button(name)
                            .on_hover_text("Restore this local arrangement")
                            .clicked()
                        {
                            action = Some(LayoutAction::Load(name.clone()));
                            ui.close_menu();
                        }
                        if ui.small_button("Remove…").clicked() {
                            action = Some(LayoutAction::AskRemove(name.clone()));
                            ui.close_menu();
                        }
                    });
                });
            }
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut self.color_layout_name)
                    .hint_text("Layout name")
                    .char_limit(64),
            );
            let replacing = names
                .iter()
                .any(|name| name.to_lowercase() == self.color_layout_name.trim().to_lowercase());
            if ui
                .add_enabled(
                    !self.color_layout_name.trim().is_empty(),
                    egui::Button::new(if replacing {
                        "Replace saved layout"
                    } else {
                        "Save layout"
                    }),
                )
                .clicked()
            {
                action = Some(LayoutAction::Save(self.color_layout_name.clone()));
            }
            if let Some(status) = &self.color_layout_status {
                ui.label(status);
            }
        });
        if let Some(name) = self.color_layout_remove.clone() {
            let mut open = true;
            egui::Window::new("Remove Color layout?")
                .id(egui::Id::new("remove_color_layout_confirm"))
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.set_max_width(360.0);
                    ui.label(format!("Remove saved layout “{name}”?"));
                    ui.weak("This changes only the saved arrangement on this computer.");
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() {
                            action = Some(LayoutAction::Remove(name.clone()));
                        }
                        if ui.button("Keep").clicked() {
                            action = Some(LayoutAction::CancelRemove);
                        }
                    });
                });
            if !open || ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
                action = Some(LayoutAction::CancelRemove);
            }
        }
        let Some(action) = action else {
            return;
        };
        match action {
            LayoutAction::AskRemove(name) => {
                self.color_layout_remove = Some(name);
                ui.ctx().request_repaint();
            }
            LayoutAction::CancelRemove => self.color_layout_remove = None,
            LayoutAction::Save(name) => {
                self.prefs.color_workspace_open = self.color_workspace_open;
                self.prefs.scope_window.open = self.scopes_panel_open;
                self.prefs.scope_window.kind = self.scope_kind;
                self.prefs.open_right_drawer = self.open_right_drawer;
                match self.prefs.save_color_layout(&name) {
                    Ok(()) => {
                        self.color_layout_status = Some(match self.prefs.save_checked() {
                            Ok(()) => format!("Saved {}", name.trim()),
                            Err(error) => format!("Kept for this session; couldn't save: {error}"),
                        })
                    }
                    Err(error) => self.color_layout_status = Some(error),
                }
            }
            LayoutAction::Load(name) => {
                if self.apply_color_layout(ui.ctx(), &name) {
                    self.color_layout_name = name.clone();
                    self.color_layout_status = Some(match self.prefs.save_checked() {
                        Ok(()) => format!("Loaded {name}"),
                        Err(error) => format!("Loaded for this session; couldn't save: {error}"),
                    });
                }
            }
            LayoutAction::Remove(name) => {
                self.color_layout_remove = None;
                if self.prefs.remove_color_layout(&name) {
                    self.color_layout_status = Some(match self.prefs.save_checked() {
                        Ok(()) => format!("Removed {name}"),
                        Err(error) => format!("Removed for this session; couldn't save: {error}"),
                    });
                }
            }
        }
    }

    pub(super) fn apply_color_layout(&mut self, ctx: &egui::Context, name: &str) -> bool {
        if !self.prefs.restore_color_layout(name) {
            return false;
        }
        self.color_workspace_open = self.prefs.color_workspace_open;
        self.scopes_panel_open = self.prefs.scope_window.open;
        self.scope_kind = self.prefs.scope_window.kind;
        self.open_right_drawer = self.prefs.open_right_drawer;
        if let Some(group) = self.open_right_drawer {
            self.last_right_drawer_group = group;
        }
        self.color_layout_dirty = false;
        ctx.data_mut(|data| {
            // Defaults alone don't replace egui's retained, previously resized panel dimensions.
            for id in [
                "color_reference_gallery",
                "color_shot_strip",
                "right_properties",
                "scopes_dock",
            ] {
                data.remove::<egui::containers::panel::PanelState>(egui::Id::new(id));
            }
            data.insert_temp(
                egui::Id::new("advanced_color_controls"),
                self.prefs.color_advanced_controls,
            );
        });
        ctx.request_repaint();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn applying_layout_resets_only_color_panel_geometry_and_syncs_runtime() {
        let ctx = egui::Context::default();
        let mut app = PhotonicApp::default();
        app.prefs.color_workspace_open = true;
        app.prefs.scope_window.open = true;
        app.prefs.color_advanced_controls = false;
        app.prefs.color_gallery_width = 300.0;
        app.prefs.save_color_layout("Desk").unwrap();
        app.color_workspace_open = false;
        app.scopes_panel_open = false;
        ctx.data_mut(|data| {
            let panel = egui::containers::panel::PanelState {
                rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(999.0, 100.0)),
            };
            data.insert_persisted(egui::Id::new("color_reference_gallery"), panel);
            data.insert_persisted(egui::Id::new("unrelated_panel"), panel);
        });
        assert!(app.apply_color_layout(&ctx, "Desk"));
        assert!(app.color_workspace_open && app.scopes_panel_open);
        assert!(egui::containers::panel::PanelState::load(
            &ctx,
            egui::Id::new("color_reference_gallery")
        )
        .is_none());
        assert!(
            egui::containers::panel::PanelState::load(&ctx, egui::Id::new("unrelated_panel"))
                .is_some()
        );
        assert_eq!(
            ctx.data(|d| d.get_temp::<bool>(egui::Id::new("advanced_color_controls"))),
            Some(false)
        );
        assert!(!app.apply_color_layout(&ctx, "Missing"));
    }
    #[test]
    fn remove_confirmation_survives_native_pointer_release_and_can_cancel() {
        fn position(shape: &egui::Shape, text: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == text => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| position(shape, text)),
                _ => None,
            }
        }
        let ctx = egui::Context::default();
        let mut app = PhotonicApp::default();
        app.prefs.save_color_layout("review").unwrap();
        let draw = |app: &mut PhotonicApp, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
                        ui.horizontal(|ui| app.draw_color_layout_menu(ui));
                    });
                },
            )
        };
        let find = |out: &egui::FullOutput, text| {
            out.shapes
                .iter()
                .find_map(|s| position(&s.shape, text))
                .unwrap_or_else(|| panic!("missing {text}"))
        };
        let click = |app: &mut PhotonicApp, pos| {
            let _ = draw(
                app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            draw(
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            )
        };
        let out = draw(&mut app, vec![]);
        let at = find(&out, "Layouts");
        let _ = click(&mut app, at);
        let out = draw(&mut app, vec![]);
        let at = find(&out, "Remove…");
        let _ = click(&mut app, at);
        assert_eq!(
            app.color_layout_remove.as_deref(),
            Some("review"),
            "immediately after release"
        );
        let _ = draw(&mut app, vec![]);
        assert_eq!(app.color_layout_remove.as_deref(), Some("review"));
        // A newly opened egui Window has an initial non-painted sizing pass.
        let out = draw(&mut app, vec![]);
        let keep = find(&out, "Keep");
        let _ = click(&mut app, keep);
        assert!(app.color_layout_remove.is_none());
        assert_eq!(app.prefs.color_layouts.len(), 1);
    }
}
