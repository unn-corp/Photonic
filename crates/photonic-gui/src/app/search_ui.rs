use super::*;

impl PhotonicApp {
    /// Apply a global-search result (set a tool or run a command).
    pub(crate) fn apply_search(
        &mut self,
        action: crate::global_search::SearchAction,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        use crate::global_search::SearchAction as A;
        match action {
            A::Tool(t) => {
                // Clear stale point-edit state so entering Direct Select via global
                // search re-seeds from the current selection (#164 finding 1).
                self.clear_point_edit();
                self.active_tool = t;
            }
            A::ToggleGrid => self.prefs.show_grid = !self.prefs.show_grid,
            A::ToggleGuides => self.guides_visible = !self.guides_visible,
            A::ToggleAudit => self.audit.panel_open = !self.audit.panel_open,
            A::FileMenu => {
                if self.active_drawer == Some(DrawerKind::Edit) {
                    self.prefs.save();
                }
                self.active_drawer = Some(DrawerKind::File);
            }
            A::EditMenu => self.active_drawer = Some(DrawerKind::Edit),
            A::ToolsMenu => {
                if self.active_drawer == Some(DrawerKind::Edit) {
                    self.prefs.save();
                }
                self.active_drawer = Some(DrawerKind::Tools);
            }
            A::Undo => {
                if history.undo(doc) {
                    self.invalidate_point_edit(doc);
                }
            }
            A::Redo => {
                if history.redo(doc) {
                    self.invalidate_point_edit(doc);
                }
            }
            A::FitView => self.fit_pending = true,
            A::OutlineMode => self.toggle_outline_mode(),
            A::PixelPreview => self.toggle_pixel_preview(),
            A::OverprintPreview => self.toggle_overprint_preview(),
            A::CheckUpdates => {
                if self.update_rx.is_none() {
                    self.update_rx = Some(crate::update::check_and_update());
                    self.file_status = Some(format!(
                        "Checking for updates… (current {})",
                        crate::update::CURRENT_VERSION
                    ));
                }
            }
        }
    }

    /// Draw the global search box and (when there's a query) a results popup
    /// listing direct matches first, then semantic ("Related") matches.
    pub(crate) fn global_search_ui(
        &mut self,
        ui: &mut egui::Ui,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.global_search)
                .hint_text(format!("{}  Search tools & actions", ph::MAGNIFYING_GLASS))
                .desired_width(230.0),
        );
        // Drive the on-device semantic index.
        self.semantic.pump();
        let raw_q = self.global_search.trim().to_string();
        self.semantic.set_query(&raw_q);
        if !raw_q.is_empty() {
            ui.ctx().request_repaint(); // pick up async semantic results
        }

        let q = raw_q.to_lowercase();
        if q.is_empty() {
            return;
        }

        let items = crate::global_search::items();
        let mut direct: Vec<&crate::global_search::SearchItem> = items
            .iter()
            .filter(|it| it.title.to_lowercase().contains(&q))
            .collect();
        direct.sort_by_key(|it| (!it.title.to_lowercase().starts_with(&q), it.title.len()));

        // Semantic "Related": cosine-ranked embedding results when the on-device
        // model is ready; otherwise a keyword/fuzzy fallback (no AI needed).
        let semantic: Vec<&crate::global_search::SearchItem> =
            if self.semantic.is_ready() && !self.semantic.results.is_empty() {
                self.semantic
                    .results
                    .iter()
                    .filter(|(idx, score)| {
                        *score > 0.25
                            && *idx < items.len()
                            && !items[*idx].title.to_lowercase().contains(&q)
                    })
                    .take(6)
                    .map(|(idx, _)| &items[*idx])
                    .collect()
            } else {
                items
                    .iter()
                    .filter(|it| {
                        let tl = it.title.to_lowercase();
                        if tl.contains(&q) {
                            return false;
                        }
                        let hay = format!(
                            "{} {} {}",
                            tl,
                            it.description.to_lowercase(),
                            it.keywords.join(" ")
                        );
                        q.split_whitespace().all(|t| hay.contains(t))
                            || crate::global_search::fuzzy_subseq(&q, &tl)
                    })
                    .collect()
            };

        let mut chosen: Option<crate::global_search::SearchAction> = None;
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));

        egui::Area::new(ui.id().with("global_search_popup"))
            .fixed_pos(resp.rect.left_bottom() + egui::vec2(0.0, 4.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(280.0);
                    ui.set_max_width(340.0);
                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .show(ui, |ui| {
                            if direct.is_empty() && semantic.is_empty() {
                                ui.label(RichText::new("No matches").weak());
                            }
                            for it in &direct {
                                if search_result_row(ui, it.icon, &it.title, &it.description, false)
                                {
                                    chosen = Some(it.action);
                                }
                            }
                            if !semantic.is_empty() {
                                ui.add_space(4.0);
                                ui.label(RichText::new("Related").small().weak());
                                ui.add_space(2.0);
                                for it in &semantic {
                                    if search_result_row(
                                        ui,
                                        it.icon,
                                        &it.title,
                                        &it.description,
                                        true,
                                    ) {
                                        chosen = Some(it.action);
                                    }
                                }
                            }
                        });
                });
            });

        if chosen.is_none() && enter {
            chosen = direct
                .first()
                .or_else(|| semantic.first())
                .map(|it| it.action);
        }
        if let Some(a) = chosen {
            self.apply_search(a, doc, history);
            self.global_search.clear();
        } else if escape {
            self.global_search.clear();
        }
    }

    pub(crate) fn apply_eyedropper_color(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        picked: photonic_core::Color,
        doc_modified: &mut bool,
    ) {
        use photonic_core::history::Command;
        use photonic_core::{style::FillKind, SceneNodeKind};

        match self.eyedropper.target.take() {
            Some(EyedropperTarget::NewShapeFill) => {
                self.fill_color = [picked.r, picked.g, picked.b, picked.a];
            }
            Some(EyedropperTarget::NodeFillSolid { node_id }) => {
                let new_fill = Fill::solid(picked);
                if let Some(node) = doc.get_node(&node_id) {
                    let mut updated = node.clone();
                    if let SceneNodeKind::Path(pn) = &mut updated.kind {
                        pn.fill = new_fill;
                    }
                    history.execute(
                        Command::UpdateNode {
                            old: node.clone(),
                            new: updated,
                        },
                        doc,
                    );
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::NodeFillGradStop { node_id, idx }) => {
                if let Some(node) = doc.get_node(&node_id) {
                    let mut updated = node.clone();
                    if let SceneNodeKind::Path(pn) = &mut updated.kind {
                        if let FillKind::Gradient(ref mut g) = pn.fill.kind {
                            if let Some(s) = g.stops.get_mut(idx) {
                                s.color = picked;
                            }
                        }
                    }
                    history.execute(
                        Command::UpdateNode {
                            old: node.clone(),
                            new: updated,
                        },
                        doc,
                    );
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::NodeFillFluid { node_id, idx }) => {
                if let Some(node) = doc.get_node(&node_id) {
                    let mut updated = node.clone();
                    if let SceneNodeKind::Path(pn) = &mut updated.kind {
                        if let FillKind::FluidGradient(ref mut fg) = pn.fill.kind {
                            if let Some(p) = fg.points.get_mut(idx) {
                                p.color = picked;
                            }
                        }
                    }
                    history.execute(
                        Command::UpdateNode {
                            old: node.clone(),
                            new: updated,
                        },
                        doc,
                    );
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::NodeFillMesh { node_id, idx }) => {
                if let Some(node) = doc.get_node(&node_id) {
                    let mut updated = node.clone();
                    if let SceneNodeKind::Path(pn) = &mut updated.kind {
                        if let FillKind::MeshGradient(ref mut mg) = pn.fill.kind {
                            if let Some(c) = mg.cell_colors.get_mut(idx) {
                                *c = picked;
                            }
                        }
                    }
                    history.execute(
                        Command::UpdateNode {
                            old: node.clone(),
                            new: updated,
                        },
                        doc,
                    );
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::NodeStroke { node_id }) => {
                if let Some(node) = doc.get_node(&node_id) {
                    let mut updated = node.clone();
                    if let SceneNodeKind::Path(pn) = &mut updated.kind {
                        pn.stroke.color = picked;
                    }
                    history.execute(
                        Command::UpdateNode {
                            old: node.clone(),
                            new: updated,
                        },
                        doc,
                    );
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::NodesStroke { node_ids }) => {
                // Broadcast the sampled color to every selected node's stroke,
                // as one undoable batch; each node keeps its own width/dash.
                let mut cmds: Vec<Command> = Vec::new();
                for id in &node_ids {
                    if let Some(node) = doc.get_node(id) {
                        let mut updated = node.clone();
                        let changed = match &mut updated.kind {
                            SceneNodeKind::Path(pn) => {
                                pn.stroke.color = picked;
                                true
                            }
                            SceneNodeKind::Text(tn) => {
                                tn.stroke.color = picked;
                                true
                            }
                            _ => false,
                        };
                        if changed {
                            cmds.push(Command::UpdateNode {
                                old: node.clone(),
                                new: updated,
                            });
                        }
                    }
                }
                if !cmds.is_empty() {
                    history.execute(Command::Batch(cmds), doc);
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::NodeOuterGlow { node_id }) => {
                if let Some(node) = doc.get_node(&node_id) {
                    let mut updated = node.clone();
                    updated.outer_glow.color = picked;
                    history.execute(
                        Command::UpdateNode {
                            old: node.clone(),
                            new: updated,
                        },
                        doc,
                    );
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::NodeInnerGlow { node_id }) => {
                if let Some(node) = doc.get_node(&node_id) {
                    let mut updated = node.clone();
                    updated.inner_glow.color = picked;
                    history.execute(
                        Command::UpdateNode {
                            old: node.clone(),
                            new: updated,
                        },
                        doc,
                    );
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::NodeGaussianGlow { node_id }) => {
                if let Some(node) = doc.get_node(&node_id) {
                    let mut updated = node.clone();
                    updated.gaussian_glow.color = picked;
                    history.execute(
                        Command::UpdateNode {
                            old: node.clone(),
                            new: updated,
                        },
                        doc,
                    );
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::NodesFillSolid { node_ids }) => {
                // Broadcast the sampled color as a solid fill to the whole
                // selection, as one undoable batch.
                let new_fill = Fill::solid(picked);
                let mut cmds: Vec<Command> = Vec::new();
                for id in &node_ids {
                    if let Some(node) = doc.get_node(id) {
                        let mut updated = node.clone();
                        let changed = match &mut updated.kind {
                            SceneNodeKind::Path(pn) => {
                                pn.fill = new_fill.clone();
                                true
                            }
                            SceneNodeKind::Text(tn) => {
                                tn.fill = new_fill.clone();
                                true
                            }
                            _ => false,
                        };
                        if changed {
                            cmds.push(Command::UpdateNode {
                                old: node.clone(),
                                new: updated,
                            });
                        }
                    }
                }
                if !cmds.is_empty() {
                    history.execute(Command::Batch(cmds), doc);
                    *doc_modified = true;
                }
            }
            Some(EyedropperTarget::RecolorSwatch { ids, from }) => {
                // Recolor every matching object from its shared `from` color to
                // the sampled color, as one undoable batch — mirrors the
                // RecolorCommit path but with the color chosen via the canvas.
                let from_color = photonic_core::Color {
                    r: from[0],
                    g: from[1],
                    b: from[2],
                    a: from[3],
                };
                if picked != from_color {
                    let mut cmds: Vec<Command> = Vec::new();
                    for id in &ids {
                        if let Some(node) = doc.nodes.get(id) {
                            let mut old_node = node.clone();
                            let mut new_node = node.clone();
                            match &mut old_node.kind {
                                SceneNodeKind::Path(p) => p.fill.kind = FillKind::Solid(from_color),
                                SceneNodeKind::Text(t) => t.fill.kind = FillKind::Solid(from_color),
                                _ => {}
                            }
                            match &mut new_node.kind {
                                SceneNodeKind::Path(p) => p.fill.kind = FillKind::Solid(picked),
                                SceneNodeKind::Text(t) => t.fill.kind = FillKind::Solid(picked),
                                _ => {}
                            }
                            cmds.push(Command::UpdateNode {
                                old: old_node,
                                new: new_node,
                            });
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        *doc_modified = true;
                    }
                }
            }
            // Handled directly in the eyedropper overlay (samples raster pixels
            // and needs the seed coordinates, not just a Color) — never reaches
            // this color-slot dispatcher.
            Some(EyedropperTarget::RasterColorRange { .. }) => {}
            // Seed a clip grade's HSL qualifier from the sampled colour (07 §5).
            // Routes through the one eyedropper idiom → `SetGrade` → history.
            Some(EyedropperTarget::GradeQualifier {
                seq,
                track,
                clip,
                op,
            }) => {
                use photonic_core::timeline::{ops as tlops, GradeOpParams};
                let (h, s, l) =
                    crate::panels::video::color_page::rgb_to_hsl(picked.r, picked.g, picked.b);
                let (nh, ns, nl) = crate::panels::video::color_page::seed_qualifier(h, s, l);
                let grade = doc
                    .timeline
                    .as_ref()
                    .and_then(|p| p.sequences.get(&seq))
                    .and_then(|sq| sq.track(track))
                    .and_then(|t| t.clips.iter().find(|c| c.id == clip))
                    .and_then(|c| c.grade.clone());
                if let Some(mut grade) = grade {
                    let mut seeded = false;
                    if let Some(o) = grade.ops.iter_mut().find(|o| o.id == op) {
                        if let GradeOpParams::HslQualifier { hue, sat, lum, .. } =
                            &mut o.params.base
                        {
                            *hue = nh;
                            *sat = ns;
                            *lum = nl;
                            seeded = true;
                        }
                    }
                    if seeded {
                        let cmd = doc.timeline.as_ref().and_then(|proj| {
                            tlops::set_grade(proj, seq, track, clip, Some(grade)).ok()
                        });
                        if let Some(cmd) = cmd {
                            history.execute_discrete(Command::Timeline(cmd), doc);
                            *doc_modified = true;
                        }
                    }
                }
            }
            None => {}
        }
    }

    pub(crate) fn draw_claude_tab(&mut self, ui: &mut egui::Ui) {
        // bottom_up pins the input row to the bottom; the scroll area fills
        // whatever space remains above it. We read available_height() after
        // the input row and separator are laid out (in bottom_up order) so we
        // can give the ScrollArea an explicit min height — otherwise egui
        // defaults to a tiny minimum and the messages are invisible.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            // ── Input row (pinned to bottom) ─────────────────────────────────
            ui.horizontal(|ui| {
                let send_enabled = !self.claude_chat.busy;
                let resp = ui.add_enabled(
                    send_enabled,
                    egui::TextEdit::singleline(&mut self.claude_chat.input)
                        .desired_width(ui.available_width() - 60.0)
                        .hint_text("Ask Claude to create or edit graphics…"),
                );

                let submitted = resp.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && send_enabled;

                let send_clicked = ui
                    .add_enabled(send_enabled, egui::Button::new("Send"))
                    .clicked();

                if (send_clicked || submitted) && !self.claude_chat.input.trim().is_empty() {
                    let msg = self.claude_chat.input.trim().to_string();
                    self.claude_chat.messages.push((true, msg.clone()));
                    self.claude_chat.pending = Some(msg);
                    self.claude_chat.input.clear();
                    self.claude_chat.busy = true;
                    resp.request_focus();
                }
            });

            ui.separator();

            // ── Message history (scrollable, fills remaining space) ───────────
            // available_height() here is the space above the input row + separator.
            let scroll_h = ui.available_height().max(40.0);
            egui::ScrollArea::vertical()
                .id_salt("claude_chat")
                .min_scrolled_height(scroll_h)
                .max_height(scroll_h)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                if self.claude_chat.messages.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "Ask Claude to create vector graphics — e.g. \"Draw a red star in the centre of the canvas\"",
                        )
                        .weak()
                        .italics(),
                    );
                }

                for (is_user, text) in &self.claude_chat.messages {
                    if *is_user {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                            egui::Frame::none()
                                .fill(Color32::from_rgb(45, 38, 90))
                                .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                                .rounding(6.0)
                                .show(ui, |ui| {
                                    ui.set_max_width(ui.available_width() * 0.75);
                                    ui.add(egui::Label::new(egui::RichText::new(text).color(Color32::WHITE)).wrap());
                                });
                        });
                    } else if text.starts_with("$ ") {
                        // Bash tool log — monospace terminal style
                        egui::Frame::none()
                            .fill(Color32::from_rgb(7, 7, 11))
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                            .rounding(4.0)
                            .show(ui, |ui| {
                                ui.set_max_width(ui.available_width());
                                for line in text.lines() {
                                    let color = if line.starts_with("$ ") {
                                        Color32::from_rgb(52, 211, 153)
                                    } else {
                                        Color32::from_rgb(187, 187, 210)
                                    };
                                    ui.add(egui::Label::new(egui::RichText::new(line).monospace().color(color).small()).wrap());
                                }
                            });
                        ui.add_space(2.0);
                    } else {
                        let is_err = text.starts_with(ph::WARNING);
                        let frame_color = if is_err {
                            Color32::from_rgb(35, 10, 15)
                        } else {
                            Color32::from_rgb(19, 19, 31)
                        };
                        egui::Frame::none()
                            .fill(frame_color)
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                            .rounding(6.0)
                            .show(ui, |ui| {
                                ui.set_max_width(ui.available_width() * 0.85);
                                let text_color = if is_err {
                                    Color32::from_rgb(248, 113, 113)
                                } else {
                                    Color32::from_rgb(187, 187, 210)
                                };
                                ui.add(egui::Label::new(egui::RichText::new(text).color(text_color)).wrap());
                            });
                        ui.add_space(2.0);
                    }
                }

                if self.claude_chat.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Claude is thinking…").weak().italics());
                    });
                }
                }); // end top_down layout
            });
        });
    }

    /// Snap a canvas coordinate to the grid if snap-to-grid is enabled.
    pub(crate) fn snap(&self, v: f64) -> f64 {
        if self.prefs.snap_to_grid {
            let g = self.prefs.grid_size as f64;
            (v / g).round() * g
        } else if self.prefs.snap_to_pixel {
            // #208: snap to whole document pixels for crisp icon geometry.
            v.round()
        } else {
            v
        }
    }
}
