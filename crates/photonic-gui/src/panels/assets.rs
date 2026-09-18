use super::*;

pub(crate) fn draw_color_swatches(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selected_id = ctx.selected_id;
    let swatch_library_selected = &mut *ctx.swatch_library_selected;
    let q = ctx.q.as_str();
    let matches = |label: &str| -> bool { q.is_empty() || label.to_lowercase().contains(q) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Color Swatches ────────────────────────────────────────────────────────
    if matches("Color Swatches") {
        egui::CollapsingHeader::new("Color Swatches")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                if doc.color_swatches.is_empty() {
                    ui.label(
                        RichText::new(
                            "No swatches. Use add_color_swatch MCP tool or load a library below.",
                        )
                        .weak()
                        .small(),
                    );
                } else {
                    for swatch in &doc.color_swatches {
                        ui.horizontal(|ui| {
                            // color preview square
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            if let Some(c) = photonic_core::Color::from_hex(&swatch.color_hex) {
                                ui.painter().rect_filled(
                                    rect,
                                    2.0,
                                    egui::Color32::from_rgb(
                                        (c.r * 255.0) as u8,
                                        (c.g * 255.0) as u8,
                                        (c.b * 255.0) as u8,
                                    ),
                                );
                            }
                            ui.label(RichText::new(&swatch.name).small());
                            ui.label(RichText::new(&swatch.color_hex).small().weak());
                            if let Some(sid) = selected_id {
                                if ui.small_button("Apply").clicked() {
                                    action = Some(PanelAction::ApplyColorSwatch {
                                        node_id: sid,
                                        swatch_name: swatch.name.clone(),
                                    });
                                }
                            }
                            if ui.small_button(ph::X).clicked() {
                                action = Some(PanelAction::DeleteColorSwatch {
                                    name: swatch.name.clone(),
                                });
                            }
                        });
                    }
                }
                ui.add_space(4.0);
                ui.separator();
                ui.label(RichText::new("Load Library").small().strong());
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_source("swatch_library_combo")
                        .selected_text(if swatch_library_selected.is_empty() {
                            "web"
                        } else {
                            swatch_library_selected.as_str()
                        })
                        .show_ui(ui, |ui| {
                            for lib in &[
                                "web",
                                "material",
                                "pastels",
                                "earth_tones",
                                "neon",
                                "grayscale",
                            ] {
                                ui.selectable_value(swatch_library_selected, lib.to_string(), *lib);
                            }
                        });
                    if ui.small_button("Load").clicked() {
                        let lib = if swatch_library_selected.is_empty() {
                            "web".to_string()
                        } else {
                            swatch_library_selected.clone()
                        };
                        action = Some(PanelAction::LoadSwatchLibrary {
                            library: lib,
                            clear_existing: false,
                        });
                    }
                });
                // #207: import brand swatches from a design-tokens file.
                if ui
                    .small_button("Import tokens…")
                    .on_hover_text(
                        "Register named swatches from a CSS/JSON/Style-Dictionary tokens file",
                    )
                    .clicked()
                {
                    action = Some(PanelAction::ImportDesignTokens);
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_spot_colors(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selected_id = ctx.selected_id;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Spot Colors ───────────────────────────────────────────────────────────
    if matches("Spot Colors") {
        egui::CollapsingHeader::new("Spot Colors")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                if doc.spot_colors.is_empty() {
                    ui.label(
                        RichText::new("No spot colors. Use define_spot_color MCP tool to add one.")
                            .weak()
                            .small(),
                    );
                } else {
                    for sc in &doc.spot_colors {
                        ui.horizontal(|ui| {
                            // color preview square
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            if let Some(c) = photonic_core::Color::from_hex(&sc.hex) {
                                ui.painter().rect_filled(
                                    rect,
                                    2.0,
                                    egui::Color32::from_rgb(
                                        (c.r * 255.0) as u8,
                                        (c.g * 255.0) as u8,
                                        (c.b * 255.0) as u8,
                                    ),
                                );
                            }
                            ui.label(RichText::new(&sc.name).small());
                            if sc.overprint {
                                ui.label(
                                    RichText::new("OP")
                                        .small()
                                        .weak()
                                        .color(egui::Color32::from_rgb(200, 140, 40)),
                                );
                            }
                            if let Some(sid) = selected_id {
                                if ui.small_button("Apply").clicked() {
                                    action = Some(PanelAction::ApplySpotColor {
                                        node_id: sid,
                                        color_name: sc.name.clone(),
                                    });
                                }
                            }
                            if ui.small_button(ph::X).clicked() {
                                action = Some(PanelAction::DeleteSpotColor {
                                    name: sc.name.clone(),
                                });
                            }
                        });
                    }
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_gradient_swatches(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selected_node = ctx.selected_node;
    let selected_id = ctx.selected_id;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Gradient Swatches ─────────────────────────────────────────────────────
    if matches("Gradient Swatches") {
        egui::CollapsingHeader::new("Gradient Swatches")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                if doc.gradient_swatches.is_empty() {
                    ui.label(RichText::new("No gradient swatches. Select a node with a gradient fill and click Save.").weak().small());
                } else {
                    for swatch in &doc.gradient_swatches {
                        ui.horizontal(|ui| {
                            // gradient preview stripe
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(28.0, 14.0), egui::Sense::hover());
                            // Simple rainbow-ish stripe as a placeholder indicator
                            let p = ui.painter();
                            p.rect_filled(rect, 2.0, egui::Color32::from_rgb(80, 100, 200));
                            p.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(rect.min.x + rect.width() * 0.4, rect.min.y),
                                    egui::vec2(rect.width() * 0.6, rect.height()),
                                ),
                                0.0,
                                egui::Color32::from_rgba_unmultiplied(220, 120, 50, 200),
                            );
                            ui.label(RichText::new(&swatch.name).small());
                            if let Some(sid) = selected_id {
                                if ui.small_button("Apply")
                                    .on_hover_text(format!("Apply gradient '{}' to selected node", swatch.name))
                                    .clicked()
                                {
                                    action = Some(PanelAction::ApplyGradientSwatch {
                                        node_id: sid,
                                        swatch_name: swatch.name.clone(),
                                    });
                                }
                            }
                            if ui.small_button(ph::X).clicked() {
                                action = Some(PanelAction::DeleteGradientSwatch { name: swatch.name.clone() });
                            }
                        });
                    }
                }
                // Save button — only shown for path nodes with gradient fills
                if let Some(node) = selected_node {
                    use photonic_core::style::FillKind;
                    let has_gradient = if let SceneNodeKind::Path(pn) = &node.kind {
                        matches!(pn.fill.kind, FillKind::Gradient(_) | FillKind::FluidGradient(_) | FillKind::MeshGradient(_))
                    } else {
                        false
                    };
                    if has_gradient {
                        ui.separator();
                        if ui.small_button("Save selected gradient as swatch…")
                            .on_hover_text("Save the selected node's gradient fill as a named swatch")
                            .clicked()
                        {
                            if let Some(nid) = selected_id {
                                action = Some(PanelAction::SaveGradientSwatch {
                                    node_id: nid,
                                    name: format!("{} gradient", node.name),
                                });
                            }
                        }
                    }
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_graphic_styles(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selected_id = ctx.selected_id;
    let graphic_style_name_input = &mut *ctx.graphic_style_name_input;
    let q = ctx.q.as_str();
    let matches = |label: &str| -> bool { q.is_empty() || label.to_lowercase().contains(q) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Graphic Styles ────────────────────────────────────────────────────────
    if matches("Graphic Styles") {
        egui::CollapsingHeader::new("Graphic Styles")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                if doc.graphic_styles.is_empty() {
                    ui.label(
                        RichText::new("No styles saved. Select a node and click Save Style.")
                            .weak()
                            .small(),
                    );
                } else {
                    for gs in &doc.graphic_styles {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&gs.name).small());
                            if let Some(sid) = selected_id {
                                if ui
                                    .small_button("Apply")
                                    .on_hover_text("Apply this style to the selected node")
                                    .clicked()
                                {
                                    action = Some(PanelAction::ApplyGraphicStyle {
                                        node_id: sid,
                                        style_name: gs.name.clone(),
                                    });
                                }
                            }
                            if ui
                                .small_button(ph::X)
                                .on_hover_text("Delete this style")
                                .clicked()
                            {
                                action = Some(PanelAction::DeleteGraphicStyle {
                                    name: gs.name.clone(),
                                });
                            }
                        });
                    }
                }
                if let Some(nid) = selected_id {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.label(RichText::new("Save selected node as style:").small().weak());
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(graphic_style_name_input)
                                .hint_text("Style name…")
                                .desired_width(120.0),
                        );
                        let can_save = !graphic_style_name_input.trim().is_empty();
                        if ui
                            .add_enabled(can_save, egui::Button::new("Save Style").small())
                            .clicked()
                        {
                            action = Some(PanelAction::SaveGraphicStyle {
                                node_id: nid,
                                name: graphic_style_name_input.trim().to_string(),
                            });
                        }
                    });
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_width_profiles(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selected_node = ctx.selected_node;
    let selected_id = ctx.selected_id;
    let width_profile_name_input = &mut *ctx.width_profile_name_input;
    let q = ctx.q.as_str();
    let matches = |label: &str| -> bool { q.is_empty() || label.to_lowercase().contains(q) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Width Profiles ────────────────────────────────────────────────────────
    if matches("Width Profiles") {
        egui::CollapsingHeader::new("Width Profiles")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                if doc.width_profiles.is_empty() {
                    ui.label(
                        RichText::new(
                            "No profiles saved. Use define_width_profile or save from selection.",
                        )
                        .weak()
                        .small(),
                    );
                } else {
                    for wp in &doc.width_profiles {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} ({} pts, avg {:.1}px)",
                                    wp.name,
                                    wp.widths.len(),
                                    wp.average_width()
                                ))
                                .small(),
                            );
                            if let Some(sid) = selected_id {
                                if ui
                                    .small_button("Apply")
                                    .on_hover_text("Set stroke width to profile average")
                                    .clicked()
                                {
                                    action = Some(PanelAction::ApplyWidthProfile {
                                        node_id: sid,
                                        profile_name: wp.name.clone(),
                                    });
                                }
                            }
                            let rename = width_profile_name_input.trim();
                            if ui
                                .add_enabled(
                                    !rename.is_empty(),
                                    egui::Button::new(ph::PENCIL).small(),
                                )
                                .on_hover_text("Rename to the text in the name field below")
                                .clicked()
                            {
                                action = Some(PanelAction::RenameWidthProfile {
                                    old_name: wp.name.clone(),
                                    new_name: rename.to_string(),
                                });
                            }
                            if ui.small_button(ph::X).clicked() {
                                action = Some(PanelAction::DeleteWidthProfile {
                                    name: wp.name.clone(),
                                });
                            }
                        });
                    }
                }
                // Save from selection
                if let Some(node) = selected_node {
                    if let SceneNodeKind::Path(ref pn) = node.kind {
                        ui.add_space(4.0);
                        ui.separator();
                        ui.label(
                            RichText::new(format!(
                                "Save current width ({:.1}px) as profile:",
                                pn.stroke.width
                            ))
                            .small()
                            .weak(),
                        );
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(width_profile_name_input)
                                    .hint_text("Profile name…")
                                    .desired_width(110.0),
                            );
                            let can_save = !width_profile_name_input.trim().is_empty();
                            if ui
                                .add_enabled(can_save, egui::Button::new("Save").small())
                                .clicked()
                            {
                                action = Some(PanelAction::SaveWidthProfile {
                                    stroke_width: pn.stroke.width,
                                    name: width_profile_name_input.trim().to_string(),
                                });
                            }
                        });
                    }
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_symbols_panel(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selected_node = ctx.selected_node;
    let selected_id = ctx.selected_id;
    let mut action: Option<PanelAction> = None;
    // ── Symbols panel ────────────────────────────────────────────────────────
    {
        egui::CollapsingHeader::new("Symbols")
            .default_open(false)
            .show(ui, |ui: &mut Ui| {
                // Define as symbol — only when a node is selected
                if let (Some(node), Some(nid)) = (selected_node, selected_id) {
                    if node.symbol_ref.is_none() {
                        // Not already an instance — offer to define
                        if ui.small_button("Define as Symbol…").clicked() {
                            // Use the node's current name as default symbol name
                            action = Some(PanelAction::DefineSymbol {
                                node_id: nid,
                                name: node.name.clone(),
                            });
                        }
                    } else {
                        // This node is a symbol instance — offer break link
                        if ui.small_button("Break Link to Symbol").clicked() {
                            action = Some(PanelAction::BreakLinkToSymbol { node_id: nid });
                        }
                    }
                    ui.separator();
                }

                // Load built-in library
                egui::CollapsingHeader::new("Load Library…")
                    .default_open(false)
                    .id_salt("sym_load_lib")
                    .show(ui, |ui| {
                        ui.label(RichText::new("Add built-in symbols to this document.").weak().small());
                        ui.horizontal(|ui| {
                            if ui.small_button("Arrows").on_hover_text("Load arrow symbols (6 shapes)").clicked() {
                                action = Some(PanelAction::LoadSymbolLibrary { library_name: "arrows".to_string() });
                            }
                            if ui.small_button("Shapes").on_hover_text("Load shape symbols (diamond, star, cross, etc.)").clicked() {
                                action = Some(PanelAction::LoadSymbolLibrary { library_name: "shapes".to_string() });
                            }
                            if ui.small_button("UI Icons").on_hover_text("Load UI icon symbols (checkbox, radio, close, etc.)").clicked() {
                                action = Some(PanelAction::LoadSymbolLibrary { library_name: "ui".to_string() });
                            }
                        });
                    });
                ui.separator();
                // Symbol library list
                if doc.symbols.is_empty() {
                    ui.label(RichText::new("No symbols defined.").small().weak());
                } else {
                    for sym in &doc.symbols {
                        ui.horizontal(|ui: &mut Ui| {
                            ui.label(RichText::new(&sym.name).small());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut Ui| {
                                if ui.small_button("Del")
                                    .on_hover_text(format!("Delete symbol '{}'", sym.name))
                                    .clicked()
                                {
                                    action = Some(PanelAction::DeleteSymbol { name: sym.name.clone() });
                                }
                                if ui.small_button("Place")
                                    .on_hover_text(format!("Place an instance of '{}'", sym.name))
                                    .clicked()
                                {
                                    action = Some(PanelAction::PlaceSymbol { symbol_name: sym.name.clone() });
                                }
                            });
                        });
                    }

                    ui.separator();
                    // Symbol Sprayer controls
                    thread_local! {
                        static SPRAY_COUNT: std::cell::RefCell<usize> = const { std::cell::RefCell::new(10) };
                        static SPRAY_SPREAD: std::cell::RefCell<f64> = const { std::cell::RefCell::new(100.0) };
                        static SPRAY_SYM: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
                    }
                    egui::CollapsingHeader::new("Symbol Sprayer")
                        .default_open(false)
                        .id_salt("sym_sprayer")
                        .show(ui, |ui| {
                            ui.label(RichText::new("Place N instances scattered around canvas center.").weak().small());
                            SPRAY_SYM.with(|s| {
                                let mut val = s.borrow().clone();
                                ui.horizontal(|ui| {
                                    ui.label("Symbol:");
                                    ui.text_edit_singleline(&mut val).on_hover_text("Symbol name to spray");
                                });
                                *s.borrow_mut() = val;
                            });
                            SPRAY_COUNT.with(|c| {
                                let mut val = *c.borrow();
                                ui.horizontal(|ui| {
                                    ui.label("Count:");
                                    ui.add(egui::DragValue::new(&mut val).range(1..=200).speed(1.0));
                                });
                                *c.borrow_mut() = val;
                            });
                            SPRAY_SPREAD.with(|s| {
                                let mut val = *s.borrow();
                                ui.horizontal(|ui| {
                                    ui.label("Spread:");
                                    ui.add(egui::DragValue::new(&mut val).range(1.0..=2000.0).speed(1.0));
                                });
                                *s.borrow_mut() = val;
                            });
                            if ui.button("Spray").on_hover_text("Scatter instances around (0, 0)").clicked() {
                                let sym = SPRAY_SYM.with(|s| s.borrow().clone());
                                let count = SPRAY_COUNT.with(|c| *c.borrow());
                                let spread = SPRAY_SPREAD.with(|s| *s.borrow());
                                if !sym.is_empty() {
                                    action = Some(PanelAction::SpraySymbolInstances {
                                        symbol_name: sym,
                                        count,
                                        x: 0.0,
                                        y: 0.0,
                                        spread,
                                    });
                                }
                            }
                        });
                }
            });
        ui.add_space(2.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_variables(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Variables ─────────────────────────────────────────────────────────────
    if matches("Variables") {
        egui::CollapsingHeader::new("Variables")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                if doc.variables.is_empty() {
                    ui.label(
                        RichText::new("No variables. Use define_variable MCP tool to add one.")
                            .weak()
                            .small(),
                    );
                } else {
                    for var in &doc.variables {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} =", var.name)).small().strong());
                            ui.label(RichText::new(&var.value).small());
                            if ui.small_button(ph::X).clicked() {
                                action = Some(PanelAction::DeleteVariable {
                                    name: var.name.clone(),
                                });
                            }
                        });
                    }
                    ui.add_space(4.0);
                    if ui
                        .small_button("Apply All Variables")
                        .on_hover_text(
                            "Replace bound text node contents with current variable values",
                        )
                        .clicked()
                    {
                        action = Some(PanelAction::ApplyVariables);
                    }
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_libraries_export(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let matches = |label: &str| -> bool { ctx.matches(label) };
    // ── Libraries & Export ───────────────────────────────────────────────────
    if matches("Color Swatches")
        || matches("Spot Colors")
        || matches("Gradient Swatches")
        || matches("Graphic Styles")
        || matches("Width Profiles")
        || matches("Export Profiles")
        || matches("Libraries")
        || matches("Export")
    {
        ui.add_space(2.0);
        ui.separator();
        ui.label(
            RichText::new("Libraries & Export")
                .small()
                .color(crate::theme::section_header_color(ui)),
        );
        ui.add_space(2.0);
    }
}
