use super::*;

pub(crate) fn draw_arrange_align(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    // ── Arrange & Align ──────────────────────────────────────────────────────
    if selection_count >= 2
        && (matches("Alignment")
            || matches("Distribute No Overlap")
            || matches("Align to Artboard")
            || matches("Layer Operations")
            || matches("Arrange")
            || matches("Align"))
    {
        ui.add_space(2.0);
        ui.separator();
        ui.label(
            RichText::new("Arrange & Align")
                .small()
                .color(crate::theme::section_header_color(ui)),
        );
        ui.add_space(2.0);
    }
}

pub(crate) fn draw_alignment(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selected_id = ctx.selected_id;
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Alignment (visible when 2+ nodes selected) ───────────────────────────
    if selection_count >= 2 && matches("Alignment") {
        egui::CollapsingHeader::new("Alignment")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Align relative to selection bounds")
                        .weak()
                        .small(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button("Left")
                        .on_hover_text("Align left edges")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignNodes {
                            operation: "left".into(),
                            key_object_id: None,
                        });
                    }
                    if ui
                        .button("Center H")
                        .on_hover_text("Align horizontal centers")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignNodes {
                            operation: "center_horizontal".into(),
                            key_object_id: None,
                        });
                    }
                    if ui
                        .button("Right")
                        .on_hover_text("Align right edges")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignNodes {
                            operation: "right".into(),
                            key_object_id: None,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Top").on_hover_text("Align top edges").clicked() {
                        action = Some(PanelAction::AlignNodes {
                            operation: "top".into(),
                            key_object_id: None,
                        });
                    }
                    if ui
                        .button("Center V")
                        .on_hover_text("Align vertical centers")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignNodes {
                            operation: "center_vertical".into(),
                            key_object_id: None,
                        });
                    }
                    if ui
                        .button("Bottom")
                        .on_hover_text("Align bottom edges")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignNodes {
                            operation: "bottom".into(),
                            key_object_id: None,
                        });
                    }
                });
                ui.add_space(2.0);
                ui.label(RichText::new("Distribute").weak().small());
                ui.horizontal(|ui| {
                    if ui
                        .button("Dist H")
                        .on_hover_text("Distribute horizontal spacing evenly")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignNodes {
                            operation: "distribute_horizontal".into(),
                            key_object_id: None,
                        });
                    }
                    if ui
                        .button("Dist V")
                        .on_hover_text("Distribute vertical spacing evenly")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignNodes {
                            operation: "distribute_vertical".into(),
                            key_object_id: None,
                        });
                    }
                });
                // Key Object: when a single node ID is provided (selected_id),
                // expose buttons to align the rest of the selection to it.
                if let Some(key_id) = selected_id {
                    ui.add_space(2.0);
                    ui.separator();
                    ui.label(
                        RichText::new("Align to Key Object (primary selection)")
                            .weak()
                            .small(),
                    );
                    ui.horizontal(|ui| {
                        for (label, op) in [
                            ("Left", "left"),
                            ("Ctr H", "center_horizontal"),
                            ("Right", "right"),
                        ] {
                            if ui
                                .button(label)
                                .on_hover_text(format!("Align {} to key object", label))
                                .clicked()
                            {
                                action = Some(PanelAction::AlignNodes {
                                    operation: op.into(),
                                    key_object_id: Some(key_id),
                                });
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        for (label, op) in [
                            ("Top", "top"),
                            ("Ctr V", "center_vertical"),
                            ("Bot", "bottom"),
                        ] {
                            if ui
                                .button(label)
                                .on_hover_text(format!("Align {} to key object", label))
                                .clicked()
                            {
                                action = Some(PanelAction::AlignNodes {
                                    operation: op.into(),
                                    key_object_id: Some(key_id),
                                });
                            }
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

pub(crate) fn draw_distribute_no_overlap(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Distribute No Overlap (visible when 2+ nodes selected) ──────────────
    if selection_count >= 2 && matches("Distribute No Overlap") {
        egui::CollapsingHeader::new("Distribute No Overlap")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Push selected nodes apart until no bounding boxes overlap.")
                        .weak()
                        .small(),
                );
                if ui
                    .button("Distribute (No Overlap)")
                    .on_hover_text(
                        "Iteratively push nodes apart until no bounding boxes overlap (4 px gap)",
                    )
                    .clicked()
                {
                    let ids: Vec<NodeId> = doc.selection.node_ids.iter().copied().collect();
                    action = Some(PanelAction::DistributeNoOverlap { node_ids: ids });
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_align_to_artboard(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Align to Artboard (visible when 1+ nodes selected) ───────────────────
    if selection_count >= 1 && matches("Align to Artboard") {
        egui::CollapsingHeader::new("Align to Artboard")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Align relative to document canvas")
                        .weak()
                        .small(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button("Left")
                        .on_hover_text("Align left edge to artboard left")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignToArtboard {
                            operation: "left".into(),
                        });
                    }
                    if ui
                        .button("Center H")
                        .on_hover_text("Center horizontally on artboard")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignToArtboard {
                            operation: "center_horizontal".into(),
                        });
                    }
                    if ui
                        .button("Right")
                        .on_hover_text("Align right edge to artboard right")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignToArtboard {
                            operation: "right".into(),
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .button("Top")
                        .on_hover_text("Align top edge to artboard top")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignToArtboard {
                            operation: "top".into(),
                        });
                    }
                    if ui
                        .button("Center V")
                        .on_hover_text("Center vertically on artboard")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignToArtboard {
                            operation: "center_vertical".into(),
                        });
                    }
                    if ui
                        .button("Bottom")
                        .on_hover_text("Align bottom edge to artboard bottom")
                        .clicked()
                    {
                        action = Some(PanelAction::AlignToArtboard {
                            operation: "bottom".into(),
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

pub(crate) fn draw_layer_operations(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Layer Operations (visible when 2+ nodes selected) ────────────────────
    if selection_count >= 2 && matches("Layer Operations") {
        egui::CollapsingHeader::new("Layer Operations")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                if ui
                    .button("Release to Layers")
                    .on_hover_text("Move each selected node into its own newly created layer")
                    .clicked()
                {
                    action = Some(PanelAction::ReleaseToLayers { node_ids: vec![] });
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_distances(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let selected_ids = ctx.selected_ids;
    let distance_results = ctx.distance_results;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Distances ─────────────────────────────────────────────────────────────
    if selection_count >= 2 && matches("Distances") {
        egui::CollapsingHeader::new("Distances")
            .default_open(true)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Edge gaps and center distances between selected nodes.")
                        .weak()
                        .small(),
                );
                ui.add_space(2.0);
                if ui.button("Measure Selected").clicked() {
                    action = Some(PanelAction::MeasureDistances {
                        node_ids: selected_ids.to_vec(),
                    });
                }
                if !distance_results.is_empty() {
                    ui.add_space(4.0);
                    for (from, to, h_gap, v_gap, center_dist) in distance_results {
                        ui.label(
                            RichText::new(format!(
                                "{} → {}: H gap {:.1}px, V gap {:.1}px, C→C {:.1}px",
                                from, to, h_gap, v_gap, center_dist
                            ))
                            .small(),
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

pub(crate) fn draw_dimension_annotations(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selection_count = ctx.selection_count;
    let selected_ids = ctx.selected_ids;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Dimension Annotations ─────────────────────────────────────────────────
    if (selection_count == 2 || !doc.dimensions.is_empty()) && matches("Dimension Annotations") {
        egui::CollapsingHeader::new("Dimension Annotations")
            .default_open(true)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Add measurement lines between nodes.")
                        .weak()
                        .small(),
                );
                ui.add_space(2.0);
                if selection_count == 2 {
                    let from_id = selected_ids[0];
                    let to_id = selected_ids[1];
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("Add ↔ H")
                            .on_hover_text("Add horizontal (X-axis) dimension")
                            .clicked()
                        {
                            action = Some(PanelAction::AddDimension {
                                from_id,
                                to_id,
                                axis: "x".to_string(),
                            });
                        }
                        if ui
                            .small_button("Add ↕ V")
                            .on_hover_text("Add vertical (Y-axis) dimension")
                            .clicked()
                        {
                            action = Some(PanelAction::AddDimension {
                                from_id,
                                to_id,
                                axis: "y".to_string(),
                            });
                        }
                        if ui
                            .small_button("Add ↗ D")
                            .on_hover_text("Add diagonal (Euclidean) dimension")
                            .clicked()
                        {
                            action = Some(PanelAction::AddDimension {
                                from_id,
                                to_id,
                                axis: "diagonal".to_string(),
                            });
                        }
                    });
                }
                if !doc.dimensions.is_empty() {
                    ui.add_space(4.0);
                    let to_remove: Option<uuid::Uuid> = {
                        let mut remove_id: Option<uuid::Uuid> = None;
                        for dim in &doc.dimensions {
                            ui.horizontal(|ui| {
                                let label = format!("{} axis: {:.1}px", dim.axis, dim.distance());
                                ui.label(RichText::new(&label).small());
                                if ui
                                    .small_button(ph::X)
                                    .on_hover_text("Remove this dimension")
                                    .clicked()
                                {
                                    remove_id = Some(dim.id);
                                }
                            });
                        }
                        remove_id
                    };
                    if let Some(id) = to_remove {
                        action = Some(PanelAction::RemoveDimension { id });
                    }
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}
