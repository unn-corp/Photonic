use super::*;

pub(crate) fn draw_combine(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    // ── Combine ──────────────────────────────────────────────────────────────
    if selection_count >= 2
        && (matches("Boolean Operations")
            || matches("Blend")
            || matches("Pathfinder")
            || matches("Compound Path")
            || matches("Clipping Mask")
            || matches("Blend Colors")
            || matches("Distribute on Path")
            || matches("Combine"))
    {
        ui.add_space(2.0);
        ui.separator();
        ui.label(
            RichText::new("Combine & Paths")
                .small()
                .color(crate::theme::section_header_color(ui)),
        );
        ui.add_space(2.0);
    }
}

pub(crate) fn draw_boolean_ops(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Boolean operations (visible when exactly 2 path nodes are selected) ──
    if selection_count == 2 && matches("Boolean Operations") {
        egui::CollapsingHeader::new("Boolean Operations")
            .default_open(true)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("lower z = target, upper z = tool")
                        .weak()
                        .small(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button("Union")
                        .on_hover_text("Merge both shapes")
                        .clicked()
                    {
                        action = Some(PanelAction::BooleanOp(BooleanOp::Union));
                    }
                    if ui
                        .button("Subtract")
                        .on_hover_text("Cut upper shape from lower")
                        .clicked()
                    {
                        action = Some(PanelAction::BooleanOp(BooleanOp::Subtract));
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .button("Intersect")
                        .on_hover_text("Keep only the overlapping area")
                        .clicked()
                    {
                        action = Some(PanelAction::BooleanOp(BooleanOp::Intersect));
                    }
                    if ui
                        .button("Exclude")
                        .on_hover_text("Remove the overlapping area")
                        .clicked()
                    {
                        action = Some(PanelAction::BooleanOp(BooleanOp::Exclude));
                    }
                });
                if ui
                    .button("Join Paths")
                    .on_hover_text(
                        "Connect the nearest endpoints of both paths into a single merged path",
                    )
                    .clicked()
                {
                    action = Some(PanelAction::JoinPaths { node_ids: vec![] });
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_blend(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Blend (visible when exactly 2 nodes selected) ─────────────────────────
    if selection_count == 2 && matches("Blend") {
        egui::CollapsingHeader::new("Blend")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(RichText::new("Generate intermediate steps between two paths").weak().small());
                if ui.button("Blend (5 steps)")
                    .on_hover_text("Create 5 interpolated shapes between the two selected paths")
                    .clicked()
                {
                    let ids: Vec<NodeId> = doc.selection.node_ids.iter().copied().collect();
                    if ids.len() == 2 {
                        action = Some(PanelAction::BlendObjects {
                            node_id_a: ids[0],
                            node_id_b: ids[1],
                            steps: 5,
                        });
                    }
                }
                if ui.button("Blend (Smooth Color)")
                    .on_hover_text("Auto-compute steps so each step changes color by ≤ 1/255 (Smooth Color mode)")
                    .clicked()
                {
                    let ids: Vec<NodeId> = doc.selection.node_ids.iter().copied().collect();
                    if ids.len() == 2 {
                        action = Some(PanelAction::BlendObjectsSmoothColor {
                            node_id_a: ids[0],
                            node_id_b: ids[1],
                        });
                    }
                }
                if ui.button("Blend (32 px spacing)")
                    .on_hover_text("Space blend steps 32 px apart along the line between the two shapes (Specified Distance mode)")
                    .clicked()
                {
                    let ids: Vec<NodeId> = doc.selection.node_ids.iter().copied().collect();
                    if ids.len() == 2 {
                        action = Some(PanelAction::BlendObjectsSpacing {
                            node_id_a: ids[0],
                            node_id_b: ids[1],
                            spacing: 32.0,
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

pub(crate) fn draw_pathfinder(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Pathfinder operations (visible when 2+ nodes selected) ───────────────
    if selection_count >= 2 && matches("Pathfinder") {
        egui::CollapsingHeader::new("Pathfinder")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(RichText::new("Multi-object operations — frontmost = crop/subtract mask").weak().small());
                if ui.button("Crop")
                    .on_hover_text("Clip all selected shapes to the boundary of the frontmost shape; frontmost is removed")
                    .clicked()
                {
                    action = Some(PanelAction::PathfinderCrop { node_ids: vec![] });
                }
                if ui.button("Minus Back")
                    .on_hover_text("Subtract all back shapes from the frontmost shape; back shapes are removed")
                    .clicked()
                {
                    action = Some(PanelAction::PathfinderMinusBack { node_ids: vec![] });
                }
                if ui.button("Minus Front")
                    .on_hover_text("Punch the frontmost shape out of all back shapes; frontmost is removed")
                    .clicked()
                {
                    action = Some(PanelAction::PathfinderMinusFront { node_ids: vec![] });
                }
                if ui.button("Trim")
                    .on_hover_text("Remove hidden areas from each shape (parts covered by shapes above); strokes disabled")
                    .clicked()
                {
                    action = Some(PanelAction::PathfinderTrim { node_ids: vec![] });
                }
                if ui.button("Merge")
                    .on_hover_text("Trim hidden areas, then merge shapes that share the same fill color into one; strokes disabled")
                    .clicked()
                {
                    action = Some(PanelAction::PathfinderMerge { node_ids: vec![] });
                }
                if ui.button("Outline")
                    .on_hover_text("Convert fills to stroked outlines; fill color becomes stroke color, fill removed")
                    .clicked()
                {
                    action = Some(PanelAction::PathfinderOutline { node_ids: vec![] });
                }
                if selection_count == 2
                    && ui.button("Divide")
                        .on_hover_text("Split two shapes at every overlap edge into distinct colored face nodes")
                        .clicked()
                    {
                        action = Some(PanelAction::PathfinderDivide { node_ids: vec![] });
                    }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_distribute_on_path(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Distribute on Path (visible when 2+ nodes selected) ─────────────────
    if selection_count >= 2 && matches("Distribute on Path") {
        egui::CollapsingHeader::new("Distribute on Path")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(RichText::new("Place copies of the selected objects along the guide path.").weak().small());
                ui.label(RichText::new("The frontmost selected path is used as the guide; all others are the objects to distribute.").weak().small());
                if ui.button("Distribute on Path")
                    .on_hover_text("Evenly place copies of selected nodes along the frontmost selected path")
                    .clicked()
                {
                    // Pass empty vecs — app.rs resolves from doc.selection.
                    action = Some(PanelAction::DistributeOnPath {
                        path_node_id: uuid::Uuid::nil(),
                        node_ids: vec![],
                        align: false,
                    });
                }
                if ui.button("Distribute + Align")
                    .on_hover_text("Same as above but rotates each copy to face along the path's tangent direction")
                    .clicked()
                {
                    action = Some(PanelAction::DistributeOnPath {
                        path_node_id: uuid::Uuid::nil(),
                        node_ids: vec![],
                        align: true,
                    });
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_compound_path(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selected_node = ctx.selected_node;
    let selected_id = ctx.selected_id;
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Compound Path (visible when 2+ nodes selected, or 1 compound selected) ──
    let is_compound_selected = selected_node
        .and_then(|n| {
            if let photonic_core::node::SceneNodeKind::Path(ref p) = n.kind {
                Some(p.is_compound)
            } else {
                None
            }
        })
        .unwrap_or(false);
    let show_compound = (selection_count >= 2 || is_compound_selected) && matches("Compound Path");
    if show_compound {
        egui::CollapsingHeader::new("Compound Path")
            .default_open(true)
            .open(forced_open)
            .show(ui, |ui| {
                if selection_count >= 2
                    && ui.button("Make Compound Path")
                        .on_hover_text("Combine selected paths into one shape; overlapping areas create holes (even-odd fill rule)")
                        .clicked()
                    {
                        action = Some(PanelAction::MakeCompoundPath { node_ids: vec![] });
                    }
                if is_compound_selected {
                    if let Some(nid) = selected_id {
                        if ui.button("Release Compound Path")
                            .on_hover_text("Split the compound path back into individual path nodes")
                            .clicked()
                        {
                            action = Some(PanelAction::ReleaseCompoundPath { node_id: nid });
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

pub(crate) fn draw_clipping_mask(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selected_node = ctx.selected_node;
    let selected_id = ctx.selected_id;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Clipping Mask (visible when a Group node is selected) ─────────────────
    let is_group_selected = selected_node
        .map(|n| matches!(n.kind, photonic_core::node::SceneNodeKind::Group(_)))
        .unwrap_or(false);
    let has_clip_mask = selected_node
        .and_then(|n| {
            if let photonic_core::node::SceneNodeKind::Group(ref g) = n.kind {
                Some(g.clip_node_id.is_some())
            } else {
                None
            }
        })
        .unwrap_or(false);
    if is_group_selected && matches("Clipping Mask") {
        if let Some(gid) = selected_id {
            egui::CollapsingHeader::new("Clipping Mask")
                .default_open(true)
                .open(forced_open)
                .show(ui, |ui| {
                    if !has_clip_mask {
                        ui.label(RichText::new("Topmost child will become the clip path.").weak().small());
                        if ui.button("Make Clipping Mask")
                            .on_hover_text("Use the topmost child of this group as a clipping path for all other children")
                            .clicked()
                        {
                            action = Some(PanelAction::MakeClippingMask { group_id: gid });
                        }
                    } else {
                        ui.label(RichText::new("Clipping mask active.").small());
                        if ui.button("Release Clipping Mask")
                            .on_hover_text("Remove the clipping mask; all children revert to normal visible objects")
                            .clicked()
                        {
                            action = Some(PanelAction::ReleaseClippingMask { group_id: gid });
                        }
                    }
                });
            ui.add_space(4.0);
        }
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_blend_colors(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Blend Colors (visible when 3+ nodes selected) ────────────────────────
    if selection_count >= 3 && matches("Blend Colors") {
        egui::CollapsingHeader::new("Blend Colors")
            .default_open(true)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Interpolate fill colors from first → last node")
                        .weak()
                        .small(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button("Horizontal")
                        .on_hover_text("Sort left→right by bounding-box center X, then blend")
                        .clicked()
                    {
                        action = Some(PanelAction::BlendColors {
                            node_ids: vec![],
                            direction: "horizontal".to_string(),
                        });
                    }
                    if ui
                        .button("Vertical")
                        .on_hover_text("Sort top→bottom by bounding-box center Y, then blend")
                        .clicked()
                    {
                        action = Some(PanelAction::BlendColors {
                            node_ids: vec![],
                            direction: "vertical".to_string(),
                        });
                    }
                    if ui
                        .button("By Depth")
                        .on_hover_text("Sort bottom→top by z-order, then blend")
                        .clicked()
                    {
                        action = Some(PanelAction::BlendColors {
                            node_ids: vec![],
                            direction: "depth".to_string(),
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

pub(crate) fn draw_adjust_colors(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Adjust Colors (visible when 1+ nodes selected) ───────────────────────
    if selection_count >= 1 && matches("Adjust Colors") {
        egui::CollapsingHeader::new("Adjust Colors")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(RichText::new("Shift RGB(A) channel values").weak().small());

                let id_r = ui.id().with("adj_r");
                let id_g = ui.id().with("adj_g");
                let id_b = ui.id().with("adj_b");
                let id_a = ui.id().with("adj_a");

                let mut dr: f32 = ui.data(|d| d.get_temp(id_r).unwrap_or(0.0));
                let mut dg: f32 = ui.data(|d| d.get_temp(id_g).unwrap_or(0.0));
                let mut db: f32 = ui.data(|d| d.get_temp(id_b).unwrap_or(0.0));
                let mut da: f32 = ui.data(|d| d.get_temp(id_a).unwrap_or(0.0));

                ui.add(
                    egui::Slider::new(&mut dr, -1.0_f32..=1.0)
                        .text("R")
                        .step_by(0.01),
                );
                ui.add(
                    egui::Slider::new(&mut dg, -1.0_f32..=1.0)
                        .text("G")
                        .step_by(0.01),
                );
                ui.add(
                    egui::Slider::new(&mut db, -1.0_f32..=1.0)
                        .text("B")
                        .step_by(0.01),
                );
                ui.add(
                    egui::Slider::new(&mut da, -1.0_f32..=1.0)
                        .text("A")
                        .step_by(0.01),
                );

                ui.data_mut(|d| {
                    d.insert_temp(id_r, dr);
                    d.insert_temp(id_g, dg);
                    d.insert_temp(id_b, db);
                    d.insert_temp(id_a, da);
                });

                ui.horizontal(|ui| {
                    if ui
                        .button("Apply")
                        .on_hover_text("Apply channel adjustments to selected nodes")
                        .clicked()
                    {
                        action = Some(PanelAction::AdjustColors {
                            node_ids: vec![],
                            delta_r: dr,
                            delta_g: dg,
                            delta_b: db,
                            delta_a: da,
                        });
                    }
                    if ui.button("Reset").clicked() {
                        ui.data_mut(|d| {
                            d.insert_temp(id_r, 0.0_f32);
                            d.insert_temp(id_g, 0.0_f32);
                            d.insert_temp(id_b, 0.0_f32);
                            d.insert_temp(id_a, 0.0_f32);
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

pub(crate) fn draw_flatten_transparency(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Flatten Transparency ──────────────────────────────────────────────────
    if selection_count >= 1 && matches("Flatten Transparency") {
        egui::CollapsingHeader::new("Flatten Transparency")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(RichText::new("Bake opacity into color alphas for print-ready output.").weak().small());
                if ui.button("Flatten Transparency")
                    .on_hover_text("Premultiply node and fill opacity into color alpha values, then set opacity to 1.0")
                    .clicked()
                {
                    action = Some(PanelAction::FlattenTransparency);
                }
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_copy_appearance(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selection_count = ctx.selection_count;
    let selected_ids = ctx.selected_ids;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Copy Appearance (visible when 2+ nodes selected) ─────────────────────
    if selection_count >= 2 && matches("Copy Appearance") {
        thread_local! {
            static COPY_FILL: std::cell::RefCell<bool> = const { std::cell::RefCell::new(true) };
            static COPY_STROKE: std::cell::RefCell<bool> = const { std::cell::RefCell::new(true) };
            static COPY_OPACITY: std::cell::RefCell<bool> = const { std::cell::RefCell::new(false) };
        }
        egui::CollapsingHeader::new("Copy Appearance")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Source: first selected  →  all others")
                        .weak()
                        .small(),
                );
                COPY_FILL.with(|cf| {
                    COPY_STROKE.with(|cs| {
                        COPY_OPACITY.with(|co| {
                            let mut fill = *cf.borrow();
                            let mut stroke = *cs.borrow();
                            let mut opacity = *co.borrow();
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut fill, "Fill");
                                ui.checkbox(&mut stroke, "Stroke");
                                ui.checkbox(&mut opacity, "Opacity");
                            });
                            *cf.borrow_mut() = fill;
                            *cs.borrow_mut() = stroke;
                            *co.borrow_mut() = opacity;
                            if ui
                                .add_enabled(
                                    fill || stroke || opacity,
                                    egui::Button::new("Apply Eyedropper"),
                                )
                                .on_hover_text(
                                    "Copy selected attributes from the first node to all others",
                                )
                                .clicked()
                            {
                                if let Some(src) = selected_ids.first().copied() {
                                    let targets: Vec<NodeId> =
                                        selected_ids.iter().skip(1).copied().collect();
                                    if !targets.is_empty() {
                                        action = Some(PanelAction::CopyAppearance {
                                            source_id: src,
                                            target_ids: targets,
                                            copy_fill: fill,
                                            copy_stroke: stroke,
                                            copy_opacity: opacity,
                                        });
                                    }
                                }
                            }
                        })
                    })
                });
            });
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}
