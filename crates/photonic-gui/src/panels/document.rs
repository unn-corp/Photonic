use super::*;

/// Human label for an export format.
fn export_format_label(f: crate::app::ExportFormat) -> &'static str {
    use crate::app::ExportFormat::*;
    match f {
        Png => "PNG",
        Jpeg => "JPEG",
        WebP => "WebP",
        Gif => "GIF",
        Tiff => "TIFF",
        Ico => "ICO",
        Svg => "SVG",
    }
}

/// Document-tab Import & Export controls (#176): place an image (embed), and
/// configure export format + scale + area, then open the export dialog seeded
/// with those settings.
pub(crate) fn draw_import_export(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    use crate::app::{ExportArea, ExportFormat};
    if !ctx.matches("Import & Export") {
        return;
    }
    let mut action: Option<PanelAction> = None;
    egui::CollapsingHeader::new("Import & Export")
        .default_open(false)
        .show(ui, |ui| {
            // ── Import ──────────────────────────────────────────────────────
            ui.label(RichText::new("Import").strong().small());
            ui.add_space(2.0);
            if ui
                .button(format!("{}  Place image…", ph::IMAGE))
                .on_hover_text("Embed a PNG / JPEG / WebP / BMP / GIF / TIFF into the document")
                .clicked()
            {
                action = Some(PanelAction::PlaceImageDialog);
            }
            ui.label(
                RichText::new("Images are embedded in the .photon file.")
                    .weak()
                    .small(),
            );

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            // ── Export ──────────────────────────────────────────────────────
            ui.label(RichText::new("Export").strong().small());
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label("Format:");
                egui::ComboBox::from_id_salt("doc_export_format")
                    .selected_text(export_format_label(ctx.doc_export.format))
                    .show_ui(ui, |ui| {
                        for f in [
                            ExportFormat::Png,
                            ExportFormat::Jpeg,
                            ExportFormat::WebP,
                            ExportFormat::Gif,
                            ExportFormat::Tiff,
                            ExportFormat::Ico,
                            ExportFormat::Svg,
                        ] {
                            ui.selectable_value(
                                &mut ctx.doc_export.format,
                                f,
                                export_format_label(f),
                            );
                        }
                    });
            });
            // Scale (raster only — SVG is resolution-independent).
            let is_vector = ctx.doc_export.format == ExportFormat::Svg;
            ui.horizontal(|ui| {
                ui.label("Scale:");
                ui.add_enabled(
                    !is_vector,
                    egui::DragValue::new(&mut ctx.doc_export.scale)
                        .speed(0.05)
                        .range(0.05..=8.0)
                        .suffix("×")
                        .fixed_decimals(2),
                )
                .on_hover_text("Output pixel-size multiplier (ignored for SVG)");
            });
            // Area.
            ui.horizontal(|ui| {
                ui.label("Area:");
                ui.selectable_value(&mut ctx.doc_export.area, ExportArea::Document, "Document")
                    .on_hover_text("Export the whole document canvas");
                ui.selectable_value(
                    &mut ctx.doc_export.area,
                    ExportArea::ContentBounds,
                    "Content",
                )
                .on_hover_text("Crop to the bounding box of all content");
                ui.selectable_value(&mut ctx.doc_export.area, ExportArea::Selection, "Selection")
                    .on_hover_text("Export just the current selection's bounds");
                ui.selectable_value(&mut ctx.doc_export.area, ExportArea::Artboard, "Artboard")
                    .on_hover_text("Export the active artboard's rectangle");
            });
            ui.add_space(6.0);
            if ui
                .button(format!("{}  Export…", ph::EXPORT))
                .on_hover_text("Open the export dialog with these settings, then choose a file")
                .clicked()
            {
                action = Some(PanelAction::OpenExportDialog);
            }
            // Batch export: one file per artboard, over a chosen range.
            if ctx.doc.artboards.len() > 1
                && ui
                    .button(format!("{}  Export Artboards…", ph::EXPORT))
                    .on_hover_text("Export each artboard (all, or a range) to its own image file")
                    .clicked()
            {
                action = Some(PanelAction::OpenArtboardExportDialog);
            }
        });
    ui.add_space(4.0);
    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_export_profiles(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Export Profiles (always visible) ──────────────────────────────────────
    if matches("Export Profiles") {
        egui::CollapsingHeader::new("Export Profiles")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                if doc.export_profiles.is_empty() {
                    ui.label(
                        RichText::new("No profiles. Use add_export_profile MCP tool to add one.")
                            .weak()
                            .small(),
                    );
                } else {
                    for profile in &doc.export_profiles {
                        ui.horizontal(|ui| {
                            ui.label(format!("{} ({})", profile.name, profile.format));
                            if ui
                                .small_button(ph::X)
                                .on_hover_text("Remove this profile")
                                .clicked()
                            {
                                action = Some(PanelAction::RemoveExportProfile {
                                    name: profile.name.clone(),
                                });
                            }
                        });
                    }
                }
            });
        ui.add_space(2.0);
        if ui.small_button("Copy Template JSON")
            .on_hover_text("Copy document structure (layers, guides, export profiles) to clipboard for use with apply_document_template")
            .clicked()
        {
            action = Some(PanelAction::CopyDocumentTemplate);
        }
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_data_visualization(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let active_tool = ctx.active_tool;
    let polygon_sides = &mut *ctx.polygon_sides;
    let star_points = &mut *ctx.star_points;
    let star_inner_ratio = &mut *ctx.star_inner_ratio;
    let rounded_rect_radius = &mut *ctx.rounded_rect_radius;
    let spiral_turns = &mut *ctx.spiral_turns;
    let spiral_inner_radius = &mut *ctx.spiral_inner_radius;
    let spiral_segs_per_turn = &mut *ctx.spiral_segs_per_turn;
    let line_snap_45 = &mut *ctx.line_snap_45;
    let arc_start_angle = &mut *ctx.arc_start_angle;
    let arc_end_angle = &mut *ctx.arc_end_angle;
    let arc_open = &mut *ctx.arc_open;
    let grid_cols = &mut *ctx.grid_cols;
    let grid_rows = &mut *ctx.grid_rows;
    let polar_grid_rings = &mut *ctx.polar_grid_rings;
    let polar_grid_sectors = &mut *ctx.polar_grid_sectors;
    let polar_grid_inner_ratio = &mut *ctx.polar_grid_inner_ratio;
    let magic_wand_attribute = &mut *ctx.magic_wand_attribute;
    let magic_wand_tolerance = &mut *ctx.magic_wand_tolerance;
    let eraser_radius = &mut *ctx.eraser_radius;
    let prop_spread = &mut *ctx.prop_spread;
    let prop_falloff_k = &mut *ctx.prop_falloff_k;
    let q = ctx.q.as_str();
    let matches = |label: &str| -> bool { q.is_empty() || label.to_lowercase().contains(q) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Data Visualization (always visible) ──────────────────────────────────
    if matches("Data Visualization") {
        egui::CollapsingHeader::new("Data Visualization")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Insert sample charts via MCP or the button below.")
                        .weak()
                        .small(),
                );
                if ui
                    .button("Radar Chart (demo)")
                    .on_hover_text(
                        "Create a sample 5-axis radar chart with 2 series at canvas center",
                    )
                    .clicked()
                {
                    action = Some(PanelAction::CreateRadarChart);
                }
                if ui
                    .button("Stacked Column (demo)")
                    .on_hover_text(
                        "Create a sample stacked column chart with 3 series at canvas center",
                    )
                    .clicked()
                {
                    action = Some(PanelAction::CreateStackedBarChart);
                }
                ui.separator();
                ui.label(RichText::new("Parametric Curves").weak().small());
                if ui
                    .button("Lissajous (demo)")
                    .on_hover_text("Create a Lissajous figure (a=3, b=2, δ=π/4)")
                    .clicked()
                {
                    action = Some(PanelAction::CreateParametricShape {
                        shape_type: "lissajous".to_string(),
                    });
                }
                if ui
                    .button("Superellipse (demo)")
                    .on_hover_text("Create a superellipse (Lamé curve, n=2.5)")
                    .clicked()
                {
                    action = Some(PanelAction::CreateParametricShape {
                        shape_type: "superellipse".to_string(),
                    });
                }
                if ui
                    .button("Rose Curve (demo)")
                    .on_hover_text("Create a rose curve (k=5)")
                    .clicked()
                {
                    action = Some(PanelAction::CreateParametricShape {
                        shape_type: "rose".to_string(),
                    });
                }
                ui.separator();
                ui.label(RichText::new("Generative Patterns").weak().small());
                if ui
                    .button("Truchet Arcs (demo)")
                    .on_hover_text("Generate a Truchet tiling with quarter-circle arc tiles")
                    .clicked()
                {
                    action = Some(PanelAction::CreateTruchetTiling {
                        style: "arcs".to_string(),
                    });
                }
                if ui
                    .button("Truchet Triangles (demo)")
                    .on_hover_text("Generate a Truchet tiling with filled triangle tiles")
                    .clicked()
                {
                    action = Some(PanelAction::CreateTruchetTiling {
                        style: "triangles".to_string(),
                    });
                }
            });
        ui.add_space(4.0);
    }

    let tool_label = match active_tool {
        Tool::Polygon => "Polygon Options",
        Tool::Star => "Star Options",
        Tool::Spiral => "Spiral Options",
        Tool::Line => "Line Options",
        Tool::Arc => "Arc Options",
        Tool::Grid => "Grid Options",
        Tool::PolarGrid => "Polar Grid Options",
        Tool::RoundedRect => "Rounded Rect Options",
        Tool::Select => "Select Shortcuts",
        Tool::ShapeBuilder => "Shape Builder",
        Tool::DirectSelect => "Direct Select",
        Tool::ProportionalMove => "Proportional Move Options",
        Tool::MagicWand => "Magic Wand Options",
        Tool::Eraser => "Eraser Options",
        Tool::Knife => "Knife Options",
        _ => "Tool",
    };

    match active_tool {
        Tool::Polygon
        | Tool::Star
        | Tool::Spiral
        | Tool::Line
        | Tool::Arc
        | Tool::Grid
        | Tool::PolarGrid
        | Tool::RoundedRect
        | Tool::Select
        | Tool::ShapeBuilder
        | Tool::DirectSelect
        | Tool::ProportionalMove
        | Tool::MagicWand
        | Tool::Eraser
        | Tool::Knife => {
            if matches(tool_label) {
                egui::CollapsingHeader::new(tool_label)
                    .default_open(false)
                    .open(forced_open)
                    .show(ui, |ui| {
                        match active_tool {
                            Tool::Polygon => {
                                ui.label("Sides");
                                ui.add(egui::Slider::new(polygon_sides, 3..=32));
                            }
                            Tool::Star => {
                                ui.label("Points");
                                ui.add(egui::Slider::new(star_points, 3..=20));
                                ui.label("Inner radius ratio");
                                ui.add(egui::Slider::new(star_inner_ratio, 0.1..=0.9));
                            }
                            Tool::Spiral => {
                                ui.label("Turns");
                                ui.add(egui::Slider::new(spiral_turns, 0.25..=20.0).step_by(0.25));
                                ui.label("Inner radius (px)");
                                ui.add(egui::Slider::new(spiral_inner_radius, 0.0..=500.0).suffix("px"));
                                ui.label("Segments per turn");
                                ui.add(egui::Slider::new(spiral_segs_per_turn, 4..=64));
                            }
                            Tool::Line => {
                                ui.checkbox(line_snap_45, "Snap to 45° angles")
                                    .on_hover_text("Also hold Shift while dragging to constrain to multiples of 45°");
                                ui.label(RichText::new("Drag to draw — Shift constrains angle").weak().small());
                            }
                            Tool::Arc => {
                                ui.label("Start angle (°)");
                                let mut start = *arc_start_angle as f32;
                                if ui.add(egui::Slider::new(&mut start, 0.0..=360.0).suffix("°")).changed() {
                                    *arc_start_angle = start as f64;
                                }
                                ui.label("End angle (°)");
                                let mut end = *arc_end_angle as f32;
                                if ui.add(egui::Slider::new(&mut end, 0.0..=360.0).suffix("°")).changed() {
                                    *arc_end_angle = end as f64;
                                }
                                ui.checkbox(arc_open, "Open arc")
                                    .on_hover_text("Open: draw arc stroke only. Closed: fill pie sector back to center.");
                            }
                            Tool::Grid => {
                                ui.label("Columns");
                                ui.add(egui::Slider::new(grid_cols, 1..=32));
                                ui.label("Rows");
                                ui.add(egui::Slider::new(grid_rows, 1..=32));
                                ui.label(RichText::new("Drag to define grid bounds").weak().small());
                            }
                            Tool::PolarGrid => {
                                ui.label("Rings");
                                ui.add(egui::Slider::new(polar_grid_rings, 1..=20));
                                ui.label("Sectors");
                                ui.add(egui::Slider::new(polar_grid_sectors, 1..=36));
                                ui.label("Inner radius ratio");
                                ui.add(egui::Slider::new(polar_grid_inner_ratio, 0.0..=0.95).step_by(0.05));
                                ui.label(RichText::new("Drag to define outer bounds").weak().small());
                            }
                            Tool::RoundedRect => {
                                ui.label("Corner radius");
                                ui.add(egui::Slider::new(rounded_rect_radius, 0.0..=200.0).suffix("px"));
                            }
                            Tool::Select => {
                                ui.label(RichText::new("Ctrl+]  Bring Forward").weak().small());
                                ui.label(RichText::new("Ctrl+[  Send Backward").weak().small());
                                ui.label(RichText::new("Ctrl+Shift+]  Front").weak().small());
                                ui.label(RichText::new("Ctrl+Shift+[  Back").weak().small());
                                ui.label(RichText::new("Ctrl+G  Group (2+ selected)").weak().small());
                                ui.label(RichText::new("Ctrl+Shift+G  Ungroup").weak().small());
                                ui.label(RichText::new("Shift+Click  Multi-select").weak().small());
                            }
                            Tool::ShapeBuilder => {
                                ui.label(RichText::new("Drag across shapes → merge").weak().small());
                                ui.label(RichText::new("Alt+drag → subtract").weak().small());
                                ui.label(RichText::new("Alt+click → delete shape").weak().small());
                            }
                            Tool::DirectSelect => {
                                ui.label(RichText::new("Click body → select object").weak().small());
                                ui.label(RichText::new("Click anchor → select point").weak().small());
                                ui.label(RichText::new("Shift+click → multi-select").weak().small());
                                ui.label(RichText::new("Drag handle → reshape curve").weak().small());
                                ui.label(RichText::new("Drag ◌ widget → round corner").weak().small());
                                ui.label(RichText::new("Del → delete selected points").weak().small());
                                ui.label(RichText::new("Esc → exit point edit").weak().small());
                            }
                            Tool::MagicWand => {
                                ui.label("Match attribute");
                                egui::ComboBox::from_id_salt("mw_attr")
                                    .selected_text(match magic_wand_attribute {
                                        SelectSameAttr::FillColor    => "Fill Color",
                                        SelectSameAttr::StrokeColor  => "Stroke Color",
                                        SelectSameAttr::StrokeWeight => "Stroke Weight",
                                        SelectSameAttr::Opacity      => "Opacity",
                                        SelectSameAttr::BlendMode    => "Blend Mode",
                                        SelectSameAttr::ObjectType   => "Object Type",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(magic_wand_attribute, SelectSameAttr::FillColor,    "Fill Color");
                                        ui.selectable_value(magic_wand_attribute, SelectSameAttr::StrokeColor,  "Stroke Color");
                                        ui.selectable_value(magic_wand_attribute, SelectSameAttr::StrokeWeight, "Stroke Weight");
                                        ui.selectable_value(magic_wand_attribute, SelectSameAttr::Opacity,      "Opacity");
                                        ui.selectable_value(magic_wand_attribute, SelectSameAttr::BlendMode,    "Blend Mode");
                                        ui.selectable_value(magic_wand_attribute, SelectSameAttr::ObjectType,   "Object Type");
                                    });
                                ui.add_space(4.0);
                                ui.label("Tolerance");
                                let mut tol = *magic_wand_tolerance as f32;
                                if ui.add(egui::Slider::new(&mut tol, 0.0..=1.0).step_by(0.01)).changed() {
                                    *magic_wand_tolerance = tol as f64;
                                }
                                ui.label(RichText::new("Click any object → select all matching").weak().small());
                            }
                            Tool::Eraser => {
                                ui.label("Radius");
                                let mut r = *eraser_radius as f32;
                                if ui.add(egui::Slider::new(&mut r, 1.0..=200.0).suffix("px")).changed() {
                                    *eraser_radius = r as f64;
                                }
                                ui.label(RichText::new("Drag across path art → subtract a swept region").weak().small());
                                ui.label(RichText::new("Cuts every visible, unlocked path it touches").weak().small());
                            }
                            Tool::Knife => {
                                ui.label(RichText::new("Drag a line across filled paths → slice into faces").weak().small());
                                ui.label(RichText::new("Each cut face becomes its own editable path").weak().small());
                            }
                            Tool::ProportionalMove => {
                                ui.label("Spread (falloff radius)");
                                let mut r = *prop_spread as f32;
                                if ui.add(egui::Slider::new(&mut r, 1.0..=2000.0).suffix("px").logarithmic(true)).changed() {
                                    *prop_spread = r as f64;
                                }
                                ui.label("Falloff curve");
                                let mut k = *prop_falloff_k as f32;
                                if ui.add(egui::Slider::new(&mut k, 0.1..=8.0).text("k")).changed() {
                                    *prop_falloff_k = k as f64;
                                }
                                ui.label(RichText::new("Drag an anchor → neighbours follow along the falloff").weak().small());
                                ui.label(RichText::new("While dragging: scroll = spread, Shift+scroll = curve").weak().small());
                            }
                            _ => {}
                        }
                    });
            }
        }
        _ => {
            if q.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "Tool: {} {}",
                        active_tool.icon(),
                        active_tool.label()
                    ))
                    .weak(),
                );
            }
        }
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_analysis(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let matches = |label: &str| -> bool { ctx.matches(label) };
    // ── Analysis ─────────────────────────────────────────────────────────────
    if matches("Distances")
        || matches("Dimension Annotations")
        || matches("Composition Analysis")
        || matches("Document Grammar")
        || matches("Analysis")
    {
        ui.add_space(2.0);
        ui.separator();
        ui.label(
            RichText::new("Analysis")
                .small()
                .color(crate::theme::section_header_color(ui)),
        );
        ui.add_space(2.0);
    }
}

pub(crate) fn draw_composition_analysis(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let composition_findings = ctx.composition_findings;
    let rhythm_findings = ctx.rhythm_findings;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Composition Analysis ──────────────────────────────────────────────────
    if matches("Composition Analysis") {
        egui::CollapsingHeader::new("Composition Analysis")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Analyze balance, density, overlap, and color usage.")
                        .weak()
                        .small(),
                );
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.button("Analyze Canvas").clicked() {
                        action = Some(PanelAction::AnalyzeComposition);
                    }
                    if ui.button("Detect Rhythms").clicked() {
                        action = Some(PanelAction::DetectRhythms);
                    }
                });
                if !composition_findings.is_empty() {
                    ui.add_space(4.0);
                    for finding in composition_findings {
                        ui.label(RichText::new(finding).small());
                    }
                }
                if !rhythm_findings.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Rhythms:").small().strong());
                    for finding in rhythm_findings {
                        ui.label(RichText::new(finding).small());
                    }
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_document_grammar(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let grammar_rules = ctx.grammar_rules;
    let grammar_rule_name_input = &mut *ctx.grammar_rule_name_input;
    let grammar_rule_type_selected = &mut *ctx.grammar_rule_type_selected;
    let grammar_rule_params_input = &mut *ctx.grammar_rule_params_input;
    let grammar_check_results = ctx.grammar_check_results;
    let q = ctx.q.as_str();
    let matches = |label: &str| -> bool { q.is_empty() || label.to_lowercase().contains(q) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Document Grammar ─────────────────────────────────────────────────────
    if matches("Document Grammar") {
        egui::CollapsingHeader::new("Document Grammar")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Define design rules the document must satisfy.")
                        .weak()
                        .small(),
                );
                ui.add_space(2.0);
                // Rule list
                if grammar_rules.is_empty() {
                    ui.label(RichText::new("No rules defined.").weak().small());
                } else {
                    for (name, rule_type) in grammar_rules {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} ({})", name, rule_type)).small());
                            if ui
                                .small_button(ph::X)
                                .on_hover_text("Delete rule")
                                .clicked()
                            {
                                action =
                                    Some(PanelAction::DeleteGrammarRule { name: name.clone() });
                            }
                        });
                    }
                }
                ui.add_space(4.0);
                // Define new rule
                ui.label(RichText::new("Add rule:").small().strong());
                ui.add(
                    egui::TextEdit::singleline(grammar_rule_name_input)
                        .hint_text("Rule name…")
                        .desired_width(ui.available_width()),
                );
                ui.add_space(2.0);
                egui::ComboBox::from_id_salt("grammar_rule_type_combo")
                    .selected_text(if grammar_rule_type_selected.is_empty() {
                        "Rule type…"
                    } else {
                        grammar_rule_type_selected.as_str()
                    })
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for rt in [
                            "palette_includes",
                            "max_colors",
                            "min_text_size",
                            "required_layer",
                            "max_node_count",
                        ] {
                            ui.selectable_value(grammar_rule_type_selected, rt.to_string(), rt);
                        }
                    });
                ui.add_space(2.0);
                ui.add(
                    egui::TextEdit::singleline(grammar_rule_params_input)
                        .hint_text(r#"Params JSON, e.g. {"count": 5}"#)
                        .desired_width(ui.available_width()),
                );
                ui.add_space(2.0);
                let can_define = !grammar_rule_name_input.trim().is_empty()
                    && !grammar_rule_type_selected.is_empty()
                    && !grammar_rule_params_input.trim().is_empty();
                if ui
                    .add_enabled(can_define, egui::Button::new("Define Rule").small())
                    .clicked()
                {
                    let name = grammar_rule_name_input.trim().to_string();
                    let rule_type = grammar_rule_type_selected.clone();
                    let params_json = grammar_rule_params_input.trim().to_string();
                    action = Some(PanelAction::DefineGrammarRule {
                        name,
                        rule_type,
                        params_json,
                    });
                    grammar_rule_name_input.clear();
                    grammar_rule_params_input.clear();
                }
                ui.add_space(4.0);
                if ui.button("Check Grammar").clicked() {
                    action = Some(PanelAction::CheckGrammar);
                }
                if !grammar_check_results.is_empty() {
                    ui.add_space(4.0);
                    for (rule_name, passed, message) in grammar_check_results {
                        let icon = if *passed { ph::CHECK } else { ph::X };
                        let color = if *passed {
                            Color32::from_rgb(60, 160, 60)
                        } else {
                            ui.visuals().error_fg_color
                        };
                        ui.label(
                            RichText::new(format!("{} {}: {}", icon, rule_name, message))
                                .small()
                                .color(color),
                        );
                    }
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_document_workflow(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let matches = |label: &str| -> bool { ctx.matches(label) };
    // ── Document & Workflow ───────────────────────────────────────────────────
    if matches("Actions")
        || matches("History")
        || matches("Event Triggers")
        || matches("Workspaces")
        || matches("Branches")
        || matches("Variables")
        || matches("Variable Binding")
        || matches("Symbol Override")
        || matches("Symbols")
        || matches("Document")
        || matches("Workflow")
    {
        ui.add_space(2.0);
        ui.separator();
        ui.label(
            RichText::new("Document & Workflow")
                .small()
                .color(crate::theme::section_header_color(ui)),
        );
        ui.add_space(2.0);
    }
}

pub(crate) fn draw_actions(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let action_names = ctx.action_names;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Actions ───────────────────────────────────────────────────────────────
    if matches("Actions") {
        egui::CollapsingHeader::new("Actions")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(
                        "Replayable MCP tool sequences. Use define_action MCP tool to record.",
                    )
                    .weak()
                    .small(),
                );
                ui.add_space(2.0);
                if action_names.is_empty() {
                    ui.label(RichText::new("No actions defined.").weak().small());
                } else {
                    for (name, step_count) in action_names {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} ({} step{})",
                                    name,
                                    step_count,
                                    if *step_count == 1 { "" } else { "s" }
                                ))
                                .small(),
                            );
                            if ui
                                .small_button("▶")
                                .on_hover_text(format!("Play '{}'", name))
                                .clicked()
                            {
                                action = Some(PanelAction::PlayAction { name: name.clone() });
                            }
                            if ui
                                .small_button(ph::X)
                                .on_hover_text(format!("Delete '{}'", name))
                                .clicked()
                            {
                                action = Some(PanelAction::DeleteAction { name: name.clone() });
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

pub(crate) fn draw_event_triggers(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let action_names = ctx.action_names;
    let event_trigger_event = &mut *ctx.event_trigger_event;
    let event_trigger_action = &mut *ctx.event_trigger_action;
    let q = ctx.q.as_str();
    let matches = |label: &str| -> bool { q.is_empty() || label.to_lowercase().contains(q) };
    let mut action: Option<PanelAction> = None;
    // ── Event Triggers ────────────────────────────────────────────────────────
    if matches("Event Triggers") {
        egui::CollapsingHeader::new("Event Triggers")
            .default_open(false)
            .id_salt("event_triggers_panel")
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Map document events to named actions.")
                        .weak()
                        .small(),
                );
                ui.add_space(2.0);

                // List existing triggers.
                let triggers: Vec<(String, String)> = doc
                    .event_triggers
                    .iter()
                    .map(|t| (t.event.clone(), t.action_name.clone()))
                    .collect();
                for (ev, an) in &triggers {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} → {}", ev, an)).small());
                        if ui.small_button(ph::X).clicked() {
                            action = Some(PanelAction::RemoveEventTrigger {
                                event: ev.clone(),
                                action_name: Some(an.clone()),
                            });
                        }
                    });
                }

                ui.add_space(2.0);
                ui.separator();
                ui.add_space(2.0);

                // Add new trigger.
                ui.label(RichText::new("Add trigger:").small());
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("event_trigger_event_combo")
                        .selected_text(if event_trigger_event.is_empty() {
                            "Event"
                        } else {
                            event_trigger_event.as_str()
                        })
                        .show_ui(ui, |ui| {
                            for ev in &[
                                "on_open",
                                "on_save",
                                "on_node_create",
                                "on_selection_change",
                            ] {
                                ui.selectable_value(event_trigger_event, ev.to_string(), *ev);
                            }
                        });
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("event_trigger_action_combo")
                        .selected_text(if event_trigger_action.is_empty() {
                            "Action"
                        } else {
                            event_trigger_action.as_str()
                        })
                        .show_ui(ui, |ui| {
                            for (name, _) in action_names {
                                ui.selectable_value(
                                    event_trigger_action,
                                    name.clone(),
                                    name.as_str(),
                                );
                            }
                        });
                    if ui.button("Register").clicked()
                        && !event_trigger_event.is_empty()
                        && !event_trigger_action.is_empty()
                    {
                        action = Some(PanelAction::RegisterEventTrigger {
                            event: event_trigger_event.clone(),
                            action_name: event_trigger_action.clone(),
                        });
                    }
                });
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_workspaces(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let prop_search = &mut *ctx.prop_search;
    let workspace_name_input = &mut *ctx.workspace_name_input;
    let q = ctx.q.as_str();
    let matches = |label: &str| -> bool { q.is_empty() || label.to_lowercase().contains(q) };
    let mut action: Option<PanelAction> = None;
    // ── Workspaces ────────────────────────────────────────────────────────────
    if matches("Workspaces") {
        egui::CollapsingHeader::new("Workspaces")
            .default_open(false)
            .id_salt("workspaces_panel")
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Named panel filter presets. Load to switch panel layout.")
                        .weak()
                        .small(),
                );
                ui.add_space(2.0);
                // Save new workspace
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(workspace_name_input)
                            .hint_text("Workspace name…")
                            .desired_width(ui.available_width() - 60.0),
                    );
                    let can_save = !workspace_name_input.trim().is_empty();
                    if ui
                        .add_enabled(can_save, egui::Button::new("Save").small())
                        .on_hover_text("Save current panel search query as a workspace")
                        .clicked()
                    {
                        action = Some(PanelAction::SaveWorkspace {
                            name: workspace_name_input.trim().to_string(),
                            search_query: prop_search.clone(),
                        });
                    }
                });
                ui.separator();
                // List workspaces
                if doc.workspaces.is_empty() {
                    ui.label(RichText::new("No workspaces saved.").weak().small());
                } else {
                    for ws in &doc.workspaces {
                        ui.horizontal(|ui| {
                            if ui
                                .button(&ws.name)
                                .on_hover_text(format!(
                                    "Load workspace '{}' (filter: {:?})",
                                    ws.name, ws.search_query
                                ))
                                .clicked()
                            {
                                action = Some(PanelAction::LoadWorkspace {
                                    name: ws.name.clone(),
                                });
                            }
                            if ui
                                .small_button(ph::X)
                                .on_hover_text(format!("Delete workspace '{}'", ws.name))
                                .clicked()
                            {
                                action = Some(PanelAction::DeleteWorkspace {
                                    name: ws.name.clone(),
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
