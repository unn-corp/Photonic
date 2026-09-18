use super::*;

pub(crate) fn draw_navigator_section(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selected_id = ctx.selected_id;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Navigator Panel ───────────────────────────────────────────────────────
    if matches("Navigator") {
        if let Some(a) = navigator::draw_navigator(ui, doc, selected_id, forced_open) {
            action = Some(a);
        }
        ui.add_space(2.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_selected_node(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selected_node = ctx.selected_node;
    let selected_id = ctx.selected_id;
    let selection_count = ctx.selection_count;
    let selected_ids = ctx.selected_ids;
    let shear_x = &mut *ctx.shear_x;
    let shear_y = &mut *ctx.shear_y;
    let color_guide_rule = &mut *ctx.color_guide_rule;
    let recolor_palette_input = &mut *ctx.recolor_palette_input;
    let bleed_mm_input = &mut *ctx.bleed_mm_input;
    let slug_mm_input = &mut *ctx.slug_mm_input;
    let construction_angle = &mut *ctx.construction_angle;
    let construction_x = &mut *ctx.construction_x;
    let construction_y = &mut *ctx.construction_y;
    let margin_top = &mut *ctx.margin_top;
    let margin_right = &mut *ctx.margin_right;
    let margin_bottom = &mut *ctx.margin_bottom;
    let margin_left = &mut *ctx.margin_left;
    let raster_mask_tolerance = &mut *ctx.raster_mask_tolerance;
    let raster_mask_contiguous = &mut *ctx.raster_mask_contiguous;
    let raster_color_range_target = ctx.raster_color_range_target;
    let rmbg_model_cached = ctx.rmbg_model_cached;
    let q = ctx.q.as_str();
    let matches = |label: &str| -> bool { q.is_empty() || label.to_lowercase().contains(q) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Selected node info ────────────────────────────────────────────────────
    if let Some(node) = selected_node {
        ui.label(RichText::new("Selected").strong());
        ui.label(format!("Name:    {}", node.name));
        egui::CollapsingHeader::new("Transform")
            .default_open(true)
            .id_salt("transform_section")
            .open(forced_open)
            .show(ui, |ui| {
                ui.label(format!("Opacity: {:.0}%", node.opacity * 100.0));

                let [_a, _b, _c, _d, tx, ty] = node.transform.matrix;
                if let Some(nid) = selected_id {
                    let mut px = tx;
                    let mut py = ty;
                    egui::Grid::new("node_pos_grid")
                        .num_columns(4)
                        .spacing([4.0, 2.0])
                        .show(ui, |ui| {
                            ui.label("X:");
                            let x_resp =
                                ui.add(egui::DragValue::new(&mut px).speed(1.0).fixed_decimals(1));
                            ui.label("Y:");
                            let y_resp =
                                ui.add(egui::DragValue::new(&mut py).speed(1.0).fixed_decimals(1));
                            ui.end_row();
                            if x_resp.changed() || y_resp.changed() {
                                action = Some(PanelAction::SetNodePosition {
                                    node_id: nid,
                                    x: px,
                                    y: py,
                                });
                            }
                        });
                } else {
                    ui.label(format!("X: {:.1}   Y: {:.1}", tx, ty));
                }

                // Rotation input — available for any node type when one node is selected.
                if let Some(nid) = selected_id {
                    let [a, b, _c, _d, _tx, _ty] = node.transform.matrix;
                    // Extract current rotation angle in degrees from the matrix column vectors.
                    let current_deg = b.atan2(a).to_degrees();
                    let mut angle_deg = current_deg;
                    egui::Grid::new("node_rot_grid")
                        .num_columns(2)
                        .spacing([4.0, 2.0])
                        .show(ui, |ui| {
                            ui.label("R°:");
                            let rot_resp = ui.add(
                                egui::DragValue::new(&mut angle_deg)
                                    .speed(0.5)
                                    .fixed_decimals(1)
                                    .suffix("°"),
                            );
                            if rot_resp.changed() {
                                // Primary first so its current angle defines the delta;
                                // the whole selection rotates about its shared center.
                                let mut node_ids = vec![nid];
                                node_ids.extend(selected_ids.iter().copied().filter(|&i| i != nid));
                                action = Some(PanelAction::RotateNode {
                                    node_ids,
                                    angle_deg,
                                });
                            }
                        });
                }

                if let (Some(nid), photonic_core::SceneNodeKind::Path(pn)) =
                    (selected_id, &node.kind)
                {
                    if let Some(local_r) = pn.path_data.bounding_box() {
                        // Compute world-space W/H by transforming the four local corners.
                        let affine = node.transform.to_kurbo();
                        let cx = [local_r.x0, local_r.x1, local_r.x1, local_r.x0];
                        let cy = [local_r.y0, local_r.y0, local_r.y1, local_r.y1];
                        let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
                        let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
                        for i in 0..4 {
                            let p = affine * kurbo::Point::new(cx[i], cy[i]);
                            if p.x < min_x {
                                min_x = p.x;
                            }
                            if p.x > max_x {
                                max_x = p.x;
                            }
                            if p.y < min_y {
                                min_y = p.y;
                            }
                            if p.y > max_y {
                                max_y = p.y;
                            }
                        }
                        let mut world_w = (max_x - min_x).max(0.1);
                        let mut world_h = (max_y - min_y).max(0.1);
                        egui::Grid::new("node_size_grid")
                            .num_columns(4)
                            .spacing([4.0, 2.0])
                            .show(ui, |ui| {
                                ui.label("W:");
                                let w_resp = ui.add(
                                    egui::DragValue::new(&mut world_w)
                                        .speed(1.0)
                                        .fixed_decimals(1),
                                );
                                ui.label("H:");
                                let h_resp = ui.add(
                                    egui::DragValue::new(&mut world_h)
                                        .speed(1.0)
                                        .fixed_decimals(1),
                                );
                                ui.end_row();
                                if (w_resp.changed() || h_resp.changed())
                                    && world_w > 0.1
                                    && world_h > 0.1
                                {
                                    action = Some(PanelAction::SetNodeSize {
                                        node_id: nid,
                                        width: world_w,
                                        height: world_h,
                                    });
                                }
                            });
                    }
                }

                // ── Visibility / Lock toggles ─────────────────────────────────────
                if let Some(nid) = selected_id {
                    ui.horizontal(|ui| {
                        let eye_icon = if node.visible { ph::EYE } else { ph::EYE_SLASH };
                        let eye_tip = if node.visible {
                            "Hide this node"
                        } else {
                            "Show this node"
                        };
                        if ui
                            .button(eye_icon.to_string())
                            .on_hover_text(eye_tip)
                            .clicked()
                        {
                            action = Some(PanelAction::SetVisible {
                                node_id: nid,
                                visible: !node.visible,
                            });
                        }

                        let lock_icon = if node.locked { ph::LOCK } else { ph::LOCK_OPEN };
                        let lock_tip = if node.locked {
                            "Unlock this node"
                        } else {
                            "Lock this node (prevents canvas selection)"
                        };
                        if ui
                            .button(lock_icon.to_string())
                            .on_hover_text(lock_tip)
                            .clicked()
                        {
                            action = Some(PanelAction::SetLocked {
                                node_id: nid,
                                locked: !node.locked,
                            });
                        }
                    });
                }
            });
        ui.add_space(2.0);

        // ── Path node accordions (alphabetical) ───────────────────────────
        if let (Some(nid), SceneNodeKind::Path(pn)) = (selected_id, &node.kind) {
            // Fill
            if matches("Fill") {
                egui::CollapsingHeader::new("Fill")
                    .default_open(true)
                    .open(forced_open)
                    .show(ui, |ui| {
                        // When 2+ nodes are selected, a fill edit broadcasts to
                        // the whole selection; with one node it targets just it.
                        let multi = selection_count >= 2;
                        if multi {
                            ui.label(
                                RichText::new(format!(
                                    "Editing fill for {selection_count} selected objects"
                                ))
                                .weak()
                                .small(),
                            );
                        }
                        // Clicking the fill preview opens the movable fill color
                        // popup (full picker + slide-out gradient drawer).
                        ui.horizontal(|ui| {
                            if ColorPopup::fill_preview(ui, &pn.fill, egui::vec2(56.0, 22.0))
                                .on_hover_text("Edit fill — opens the color popup")
                                .clicked()
                            {
                                action = Some(PanelAction::OpenColorPopup {
                                    node_id: nid,
                                    stroke: false,
                                });
                            }
                            let label = {
                                use photonic_core::style::FillKind as FK;
                                match &pn.fill.kind {
                                    FK::None => "None",
                                    FK::Solid(_) => "Solid",
                                    FK::Gradient(_) => "Gradient",
                                    FK::FluidGradient(_) => "Fluid",
                                    FK::MeshGradient(_) => "Mesh",
                                    FK::Pattern(_) => "Pattern",
                                }
                            };
                            ui.label(RichText::new(label).weak().small());
                        });
                        // ── Recent colors swatches ──────────────────────────
                        if !doc.recent_colors.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new("Recent").weak().small());
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
                                for rc in &doc.recent_colors {
                                    let c32 = Color32::from_rgba_unmultiplied(
                                        (rc.r * 255.0) as u8,
                                        (rc.g * 255.0) as u8,
                                        (rc.b * 255.0) as u8,
                                        (rc.a * 255.0) as u8,
                                    );
                                    let (rect, resp) = ui.allocate_exact_size(
                                        egui::vec2(16.0, 16.0),
                                        egui::Sense::click(),
                                    );
                                    ui.painter().rect_filled(rect, 2.0, c32);
                                    ui.painter().rect_stroke(
                                        rect,
                                        2.0,
                                        egui::Stroke::new(0.5, Color32::from_gray(100)),
                                    );
                                    if resp.clicked() {
                                        use photonic_core::{Color, Fill};
                                        let fill = Fill::solid(Color {
                                            r: rc.r,
                                            g: rc.g,
                                            b: rc.b,
                                            a: rc.a,
                                        });
                                        action = Some(if multi {
                                            PanelAction::UpdateNodesFill {
                                                node_ids: selected_ids.to_vec(),
                                                fill,
                                            }
                                        } else {
                                            PanelAction::UpdateNodeFill { node_id: nid, fill }
                                        });
                                    }
                                    if resp.hovered() {
                                        resp.on_hover_text(format!(
                                            "#{:02X}{:02X}{:02X}{:02X}",
                                            (rc.r * 255.0) as u8,
                                            (rc.g * 255.0) as u8,
                                            (rc.b * 255.0) as u8,
                                            (rc.a * 255.0) as u8,
                                        ));
                                    }
                                }
                            });
                        }
                    });
            }

            // Stroke
            if matches("Stroke") {
                egui::CollapsingHeader::new("Stroke")
                    .default_open(true)
                    .open(forced_open)
                    .show(ui, |ui| {
                        let multi = selection_count >= 2;
                        let mut d = false;
                        if let Some(new_stroke) = draw_stroke_editor(ui, &pn.stroke, &mut d) {
                            action = Some(if multi {
                                PanelAction::UpdateNodesStroke {
                                    node_ids: selected_ids.to_vec(),
                                    stroke: new_stroke,
                                }
                            } else {
                                PanelAction::UpdateNodeStroke {
                                    node_id: nid,
                                    stroke: new_stroke,
                                }
                            });
                        }
                        if d {
                            // With 2+ selected, the stroke eyedropper recolors
                            // every selected object's stroke; otherwise just this one.
                            action = Some(PanelAction::StartEyedropper(if multi {
                                EyedropperTarget::NodesStroke {
                                    node_ids: selected_ids.to_vec(),
                                }
                            } else {
                                EyedropperTarget::NodeStroke { node_id: nid }
                            }));
                        }

                        // Outline Stroke — convert the stroke into a fillable
                        // shape (Illustrator's Object ▸ Path ▸ Outline Stroke).
                        if pn.stroke.enabled && pn.stroke.width > 0.0 {
                            ui.add_space(4.0);
                            let targets: Vec<NodeId> = if multi {
                                selected_ids.to_vec()
                            } else {
                                vec![nid]
                            };
                            if ui
                                .button("Outline Stroke")
                                .on_hover_text(
                                    "Convert the stroke into a filled shape you can \
                                     edit, like Illustrator's Outline Stroke",
                                )
                                .clicked()
                            {
                                action = Some(PanelAction::OutlineStroke { node_ids: targets });
                            }
                        }
                    });
            }

            // Color Guide — only shown when the node has a solid fill
            if matches("Color Guide") {
                use photonic_core::style::FillKind;
                if let FillKind::Solid(base_color) = &pn.fill.kind {
                    if pn.fill.enabled {
                        let base = *base_color;
                        egui::CollapsingHeader::new("Color Guide")
                            .default_open(false)
                            .open(forced_open)
                            .show(ui, |ui| {
                                // Rule selector buttons
                                ui.horizontal_wrapped(|ui| {
                                    for rule in &[
                                        "complementary",
                                        "analogous",
                                        "triadic",
                                        "split_complementary",
                                        "tetradic",
                                        "monochromatic",
                                    ] {
                                        let selected = color_guide_rule.as_str() == *rule;
                                        let label = rule.replace('_', " ");
                                        if ui.selectable_label(selected, label).clicked() {
                                            *color_guide_rule = rule.to_string();
                                        }
                                    }
                                });
                                ui.add_space(4.0);
                                // Swatches
                                let palette = base.harmony(color_guide_rule);
                                ui.horizontal_wrapped(|ui| {
                                    for (i, swatch) in palette.iter().enumerate() {
                                        let c32 = Color32::from_rgb(
                                            (swatch.r * 255.0).round() as u8,
                                            (swatch.g * 255.0).round() as u8,
                                            (swatch.b * 255.0).round() as u8,
                                        );
                                        let (rect, resp) = ui.allocate_exact_size(
                                            egui::vec2(24.0, 24.0),
                                            egui::Sense::click(),
                                        );
                                        ui.painter().rect_filled(rect, 3.0, c32);
                                        if i == 0 {
                                            ui.painter().rect_stroke(
                                                rect,
                                                3.0,
                                                egui::Stroke::new(2.0, Color32::WHITE),
                                            );
                                        }
                                        let hex = swatch.to_hex();
                                        if resp.on_hover_text(hex).clicked() {
                                            let mut new_fill = pn.fill.clone();
                                            new_fill.kind = FillKind::Solid(*swatch);
                                            action = Some(PanelAction::UpdateNodeFill {
                                                node_id: nid,
                                                fill: new_fill,
                                            });
                                        }
                                    }
                                });
                            });
                    }
                }
            }

            // Recolor
            if matches("Recolor") {
                egui::CollapsingHeader::new("Recolor")
                    .default_open(false)
                    .open(forced_open)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Map fills to nearest palette color.")
                                .weak()
                                .small(),
                        );
                        ui.label("Palette (hex, comma-separated):");
                        ui.add(
                            egui::TextEdit::singleline(recolor_palette_input)
                                .hint_text("#FF0000, #00FF00, #0000FF")
                                .desired_width(ui.available_width()),
                        );
                        if ui
                            .button("Apply to Selection")
                            .on_hover_text(
                                "Remap every solid fill to the nearest color in the palette above",
                            )
                            .clicked()
                        {
                            // Parse palette from input string.
                            let palette: Vec<[f32; 4]> = recolor_palette_input
                                .split(',')
                                .filter_map(|hex| {
                                    photonic_core::color::Color::from_hex(hex.trim())
                                        .map(|c| [c.r, c.g, c.b, c.a])
                                })
                                .collect();
                            if !palette.is_empty() {
                                action = Some(PanelAction::RecolorArtwork {
                                    node_ids: vec![nid],
                                    palette,
                                });
                            }
                        }
                    });
            }

            // ── Effects ───────────────────────────────────────────────────────
            if matches("Effects")
                || matches("Inner Glow")
                || matches("Outer Glow")
                || matches("Gaussian Glow")
            {
                egui::CollapsingHeader::new("Effects")
                    .default_open(false)
                    .id_salt("effects_section")
                    .open(forced_open)
                    .show(ui, |ui| {
                        // ── Layer Styles (reorderable effect stack) ─────────────
                        egui::CollapsingHeader::new("Layer Styles")
                            .default_open(true)
                            .id_salt("layer_styles")
                            .open(forced_open)
                            .show(ui, |ui| {
                                use photonic_core::effects::{
                                    ColorOverlay, GradientOverlay, LayerEffect, StrokeEffect,
                                };
                                let mut effects = node.effects.clone();
                                let mut changed = false;
                                let mut remove: Option<usize> = None;
                                for (i, eff) in effects.iter_mut().enumerate() {
                                    match eff {
                                        LayerEffect::ColorOverlay(co) => {
                                            ui.horizontal(|ui| {
                                                if ui.checkbox(&mut co.enabled, "").changed() {
                                                    changed = true;
                                                }
                                                ui.label("Color Overlay");
                                                let mut col = egui::Color32::from_rgb(
                                                    (co.color.r * 255.0) as u8,
                                                    (co.color.g * 255.0) as u8,
                                                    (co.color.b * 255.0) as u8,
                                                );
                                                if ui.color_edit_button_srgba(&mut col).changed() {
                                                    co.color.r = col.r() as f32 / 255.0;
                                                    co.color.g = col.g() as f32 / 255.0;
                                                    co.color.b = col.b() as f32 / 255.0;
                                                    changed = true;
                                                }
                                                if ui.small_button("Remove").clicked() {
                                                    remove = Some(i);
                                                }
                                            });
                                            if ui
                                                .add(
                                                    egui::Slider::new(&mut co.opacity, 0.0..=1.0)
                                                        .text("opacity"),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                        }
                                        LayerEffect::Stroke(st) => {
                                            ui.horizontal(|ui| {
                                                if ui.checkbox(&mut st.enabled, "").changed() {
                                                    changed = true;
                                                }
                                                ui.label("Stroke");
                                                if let photonic_core::style::FillKind::Solid(sc) =
                                                    &mut st.fill.kind
                                                {
                                                    let mut col = egui::Color32::from_rgb(
                                                        (sc.r * 255.0) as u8,
                                                        (sc.g * 255.0) as u8,
                                                        (sc.b * 255.0) as u8,
                                                    );
                                                    if ui
                                                        .color_edit_button_srgba(&mut col)
                                                        .changed()
                                                    {
                                                        sc.r = col.r() as f32 / 255.0;
                                                        sc.g = col.g() as f32 / 255.0;
                                                        sc.b = col.b() as f32 / 255.0;
                                                        changed = true;
                                                    }
                                                }
                                                if ui.small_button("Remove").clicked() {
                                                    remove = Some(i);
                                                }
                                            });
                                            if ui
                                                .add(
                                                    egui::Slider::new(&mut st.width, 0.0..=50.0)
                                                        .text("width"),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                        }
                                        LayerEffect::GradientOverlay(go) => {
                                            ui.horizontal(|ui| {
                                                if ui.checkbox(&mut go.enabled, "").changed() {
                                                    changed = true;
                                                }
                                                ui.label("Gradient Overlay");
                                                if ui.small_button("Remove").clicked() {
                                                    remove = Some(i);
                                                }
                                            });
                                            if ui
                                                .add(
                                                    egui::Slider::new(&mut go.opacity, 0.0..=1.0)
                                                        .text("opacity"),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                            if ui
                                                .add(
                                                    egui::Slider::new(
                                                        &mut go.angle,
                                                        -180.0..=180.0,
                                                    )
                                                    .text("angle"),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                            if ui
                                                .add(
                                                    egui::Slider::new(&mut go.scale, 0.1..=3.0)
                                                        .text("scale"),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                        }
                                        other => {
                                            ui.horizontal(|ui| {
                                                ui.label(other.kind());
                                                if ui.small_button("Remove").clicked() {
                                                    remove = Some(i);
                                                }
                                            });
                                        }
                                    }
                                }
                                if let Some(i) = remove {
                                    effects.remove(i);
                                    changed = true;
                                }
                                ui.horizontal(|ui| {
                                    if ui.button("+ Color Overlay").clicked() {
                                        effects.push(LayerEffect::ColorOverlay(
                                            ColorOverlay::default(),
                                        ));
                                        changed = true;
                                    }
                                    if ui.button("+ Gradient Overlay").clicked() {
                                        effects.push(LayerEffect::GradientOverlay(
                                            GradientOverlay::default(),
                                        ));
                                        changed = true;
                                    }
                                    if ui.button("+ Stroke").clicked() {
                                        effects.push(LayerEffect::Stroke(StrokeEffect::default()));
                                        changed = true;
                                    }
                                });
                                if changed {
                                    action = Some(PanelAction::SetNodeEffects {
                                        node_id: nid,
                                        effects,
                                    });
                                }
                            });
                        // Inner Glow
                        if matches("Inner Glow") || matches("Effects") {
                            egui::CollapsingHeader::new("Inner Glow")
                                .default_open(false)
                                .open(forced_open)
                                .show(ui, |ui| {
                                    let mut d = false;
                                    if let Some(new_ig) =
                                        draw_glow_editor(ui, &node.inner_glow, &mut d)
                                    {
                                        action = Some(PanelAction::UpdateNodeInnerGlow {
                                            node_id: nid,
                                            glow: new_ig,
                                        });
                                    }
                                    if d {
                                        action = Some(PanelAction::StartEyedropper(
                                            EyedropperTarget::NodeInnerGlow { node_id: nid },
                                        ));
                                    }
                                });
                        }
                        // Outer Glow
                        if matches("Outer Glow") || matches("Effects") {
                            egui::CollapsingHeader::new("Outer Glow")
                                .default_open(false)
                                .open(forced_open)
                                .show(ui, |ui| {
                                    let mut d = false;
                                    if let Some(new_og) =
                                        draw_glow_editor(ui, &node.outer_glow, &mut d)
                                    {
                                        action = Some(PanelAction::UpdateNodeOuterGlow {
                                            node_id: nid,
                                            glow: new_og,
                                        });
                                    }
                                    if d {
                                        action = Some(PanelAction::StartEyedropper(
                                            EyedropperTarget::NodeOuterGlow { node_id: nid },
                                        ));
                                    }
                                });
                        }
                        // Gaussian Glow
                        if matches("Gaussian Glow") || matches("Effects") {
                            egui::CollapsingHeader::new("Gaussian Glow")
                                .default_open(false)
                                .open(forced_open)
                                .show(ui, |ui| {
                                    let mut d = false;
                                    if let Some(new_gg) =
                                        draw_gaussian_glow_editor(ui, &node.gaussian_glow, &mut d)
                                    {
                                        action = Some(PanelAction::UpdateNodeGaussianGlow {
                                            node_id: nid,
                                            glow: new_gg,
                                        });
                                    }
                                    if d {
                                        action = Some(PanelAction::StartEyedropper(
                                            EyedropperTarget::NodeGaussianGlow { node_id: nid },
                                        ));
                                    }
                                });
                        }
                    });
                ui.add_space(2.0);
            }

            // ── Path / Geometry ───────────────────────────────────────────────
            if matches("Path / Geometry")
                || matches("Path Operations")
                || matches("Shear")
                || matches("Flip")
                || matches("Radial Copies")
                || matches("Pin Guides")
                || matches("Select Similar")
                || matches("Snap to Pixel")
            {
                egui::CollapsingHeader::new("Path / Geometry")
                    .default_open(false)
                    .id_salt("path_geometry_section")
                    .open(forced_open)
                    .show(ui, |ui| {
            // Path Operations
            if matches("Path Operations") {
                egui::CollapsingHeader::new("Path Operations")
                    .default_open(false)
                    .open(forced_open)
                    .show(ui, |ui| {
                        if ui.button("Add Anchor Points")
                            .on_hover_text("Insert a midpoint anchor on every path segment")
                            .clicked()
                        {
                            action = Some(PanelAction::AddAnchorPoints { node_id: nid });
                        }
                        ui.horizontal(|ui| {
                            if ui.button("To Smooth")
                                .on_hover_text("Make anchor junction handles collinear (smooth bezier curves)")
                                .clicked()
                            {
                                action = Some(PanelAction::ConvertToSmooth { node_ids: vec![nid] });
                            }
                            if ui.button("To Corner")
                                .on_hover_text("Retract cubic handles to anchor points (sharp cusps / straight lines)")
                                .clicked()
                            {
                                action = Some(PanelAction::ConvertToCorner { node_ids: vec![nid] });
                            }
                        });
                        if ui.button("Average Anchors")
                            .on_hover_text("Move all anchor points to their centroid")
                            .clicked()
                        {
                            action = Some(PanelAction::AverageAnchorPoints { node_id: nid });
                        }
                        if ui.button("Convert to Grayscale")
                            .on_hover_text("Convert all fill and stroke colors to grayscale")
                            .clicked()
                        {
                            action = Some(PanelAction::ConvertToGrayscale { node_ids: vec![nid] });
                        }
                        // Outline Stroke lives in the Stroke section (next to the
                        // stroke it converts).
                        if ui.button("Expand (+2 px)")
                            .on_hover_text("Offset path outward by 2 px, creating a copy")
                            .clicked()
                        {
                            action = Some(PanelAction::OffsetPath { node_ids: vec![nid], distance: 2.0 });
                        }
                        if ui.button("Contract (−2 px)")
                            .on_hover_text("Offset path inward by 2 px, creating a copy")
                            .clicked()
                        {
                            action = Some(PanelAction::OffsetPath { node_ids: vec![nid], distance: -2.0 });
                        }
                        if ui.button("Reverse Direction")
                            .on_hover_text("Reverse the winding direction of this path")
                            .clicked()
                        {
                            action = Some(PanelAction::ReversePathDirection { node_id: nid });
                        }
                        if ui.button("Close Path")
                            .on_hover_text("Append ClosePath to every open subpath")
                            .clicked()
                        {
                            action = Some(PanelAction::JoinPaths { node_ids: vec![nid] });
                        }
                        if ui.button("Divide Objects Below")
                            .on_hover_text("Use this path as a cutting edge to split all objects beneath it; cutter is removed")
                            .clicked()
                        {
                            action = Some(PanelAction::DivideObjectsBelow { node_id: nid });
                        }
                        if ui.button("Round Corners")
                            .on_hover_text("Replace sharp corners with smooth arc fillets")
                            .clicked()
                        {
                            action = Some(PanelAction::RoundCorners {
                                node_ids: vec![nid],
                                radius: 10.0,
                            });
                        }
                        if ui.button("Zig Zag")
                            .on_hover_text("Apply zig-zag wave distortion to this path")
                            .clicked()
                        {
                            action = Some(PanelAction::ZigZagPath {
                                node_ids: vec![nid],
                                size: 10.0,
                                ridges: 4,
                                smooth: false,
                            });
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Pucker")
                                .on_hover_text("Contract path points inward toward centroid")
                                .clicked()
                            {
                                action = Some(PanelAction::PuckerBloat {
                                    node_ids: vec![nid],
                                    strength: -0.3,
                                });
                            }
                            if ui.button("Bloat")
                                .on_hover_text("Expand path points outward from centroid")
                                .clicked()
                            {
                                action = Some(PanelAction::PuckerBloat {
                                    node_ids: vec![nid],
                                    strength: 0.3,
                                });
                            }
                        });
                        if ui.button("Roughen")
                            .on_hover_text("Randomly displace path points for a hand-drawn look")
                            .clicked()
                        {
                            action = Some(PanelAction::RoughenPath {
                                node_ids: vec![nid],
                                size: 5.0,
                                detail: 0,
                                seed: 42,
                            });
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Wave Deform")
                                .on_hover_text("Apply smooth sinusoidal displacement (both axes)")
                                .clicked()
                            {
                                action = Some(PanelAction::NoiseDeform {
                                    node_ids: vec![nid],
                                    amplitude: 8.0,
                                    style: "both".to_string(),
                                });
                            }
                            if ui.button("Swell")
                                .on_hover_text("Sinusoidal deform on Y axis only (bulge effect)")
                                .clicked()
                            {
                                action = Some(PanelAction::NoiseDeform {
                                    node_ids: vec![nid],
                                    amplitude: 12.0,
                                    style: "y".to_string(),
                                });
                            }
                        });
                        if ui.button("Twirl")
                            .on_hover_text("Spiral-rotate path points around centroid (90°)")
                            .clicked()
                        {
                            action = Some(PanelAction::TwirlPath {
                                node_ids: vec![nid],
                                angle_deg: 90.0,
                            });
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Scallop")
                                .on_hover_text("Replace segments with smooth inward arcs")
                                .clicked()
                            {
                                action = Some(PanelAction::ScallopPath {
                                    node_ids: vec![nid],
                                    depth: 10.0,
                                    count: 1,
                                });
                            }
                            if ui.button("Crystallize")
                                .on_hover_text("Add sharp outward spikes to segments")
                                .clicked()
                            {
                                action = Some(PanelAction::CrystallizePath {
                                    node_ids: vec![nid],
                                    size: 10.0,
                                    count: 3,
                                });
                            }
                        });
                        if ui.button("Drop Shadow")
                            .on_hover_text("Add an offset shadow copy behind this path")
                            .clicked()
                        {
                            action = Some(PanelAction::AddDropShadow { node_id: nid });
                        }
                        ui.horizontal(|ui| {
                            for (label, warp) in &[("Arc", "arc"), ("Wave", "wave"), ("Bulge", "bulge"), ("Flag", "flag")] {
                                if ui.button(*label)
                                    .on_hover_text(format!("Apply '{}' warp envelope", warp))
                                    .clicked()
                                {
                                    action = Some(PanelAction::WarpEnvelope {
                                        node_ids: vec![nid],
                                        warp_type: warp.to_string(),
                                        bend: 0.5,
                                    });
                                }
                            }
                        });
                    });
            }

            // Shear / Skew
            if matches("Shear") {
                egui::CollapsingHeader::new("Shear")
                    .default_open(false)
                    .open(forced_open)
                    .show(ui, |ui| {
                        ui.label("Skew the node along the X or Y axis.");
                        egui::Grid::new("shear_grid")
                            .num_columns(2)
                            .spacing([8.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("Shear X:");
                                ui.add(
                                    egui::DragValue::new(shear_x)
                                        .speed(0.01)
                                        .range(-10.0..=10.0),
                                );
                                ui.end_row();
                                ui.label("Shear Y:");
                                ui.add(
                                    egui::DragValue::new(shear_y)
                                        .speed(0.01)
                                        .range(-10.0..=10.0),
                                );
                                ui.end_row();
                            });
                        ui.horizontal(|ui| {
                            if ui
                                .button("Apply Shear")
                                .on_hover_text("Apply the shear transform around the node's centre")
                                .clicked()
                                && (*shear_x != 0.0 || *shear_y != 0.0) {
                                    let mut node_ids = vec![nid];
                                    node_ids
                                        .extend(selected_ids.iter().copied().filter(|&i| i != nid));
                                    action = Some(PanelAction::ShearNode {
                                        node_ids,
                                        shear_x: *shear_x,
                                        shear_y: *shear_y,
                                    });
                                    *shear_x = 0.0;
                                    *shear_y = 0.0;
                                }
                            if ui
                                .button("Reset")
                                .on_hover_text("Clear shear values")
                                .clicked()
                            {
                                *shear_x = 0.0;
                                *shear_y = 0.0;
                            }
                        });
                    });
            }

            // Flip
            if matches("Flip") {
                ui.horizontal(|ui| {
                    if ui
                        .button("Flip H")
                        .on_hover_text("Flip horizontally")
                        .clicked()
                    {
                        let mut node_ids = vec![nid];
                        node_ids.extend(selected_ids.iter().copied().filter(|&i| i != nid));
                        action = Some(PanelAction::FlipNodes {
                            node_ids,
                            horizontal: true,
                        });
                    }
                    if ui
                        .button("Flip V")
                        .on_hover_text("Flip vertically")
                        .clicked()
                    {
                        let mut node_ids = vec![nid];
                        node_ids.extend(selected_ids.iter().copied().filter(|&i| i != nid));
                        action = Some(PanelAction::FlipNodes {
                            node_ids,
                            horizontal: false,
                        });
                    }
                    if ui
                        .button("Mirror H Copy")
                        .on_hover_text("Duplicate and flip a copy left-right")
                        .clicked()
                    {
                        action = Some(PanelAction::MirrorCopy {
                            node_ids: vec![nid],
                            axis: "horizontal".to_string(),
                        });
                    }
                    if ui
                        .button("Mirror V Copy")
                        .on_hover_text("Duplicate and flip a copy top-bottom")
                        .clicked()
                    {
                        action = Some(PanelAction::MirrorCopy {
                            node_ids: vec![nid],
                            axis: "vertical".to_string(),
                        });
                    }
                });
                ui.add_space(4.0);
            }

            // Radial Copies
            if matches("Radial Copies") {
                thread_local! {
                    static RADIAL_COUNT: std::cell::RefCell<usize> = const { std::cell::RefCell::new(6) };
                }
                RADIAL_COUNT.with(|v| {
                    let mut count = *v.borrow();
                    ui.horizontal(|ui| {
                        ui.label("Radial copies:");
                        ui.add(egui::DragValue::new(&mut count).range(2..=64).speed(1.0));
                        if ui.small_button("Apply").on_hover_text(
                            format!("Create {} evenly-spaced rotational copies around this node's center", count)
                        ).clicked() {
                            action = Some(PanelAction::RotateCopies { node_id: nid, count });
                        }
                    });
                    *v.borrow_mut() = count;
                });
                ui.add_space(4.0);
            }

            // Pin Guides
            if matches("Pin Guides") {
                if ui
                    .button("Pin Guides")
                    .on_hover_text("Add ruler guides at this node's edges and center")
                    .clicked()
                {
                    action = Some(PanelAction::PinObjectGuides {
                        node_ids: vec![nid],
                    });
                }
                ui.add_space(2.0);
            }

            // Select Similar
            if matches("Select Similar") {
                if ui
                    .button("Select Similar Fill")
                    .on_hover_text("Select all nodes with the same fill color (±5 per channel)")
                    .clicked()
                {
                    action = Some(PanelAction::SelectSimilar {
                        node_ids: vec![nid],
                        match_by: "fill_color".to_string(),
                    });
                }
                ui.add_space(2.0);
            }

            // Snap to Pixel
            if matches("Snap to Pixel") {
                egui::CollapsingHeader::new("Snap to Pixel")
                    .default_open(false)
                    .open(forced_open)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Round position to integer coordinates.")
                                .weak()
                                .small(),
                        );
                        if ui
                            .button("Snap to Pixel")
                            .on_hover_text("Round the node's X/Y position to the nearest integer")
                            .clicked()
                        {
                            action = Some(PanelAction::SnapToPixel {
                                node_ids: vec![nid],
                            });
                        }
                    });
            }
                    });
                ui.add_space(2.0);
            }
        }

        // ── Group Operations ───────────────────────────────────────────────
        if let (SceneNodeKind::Group(gn), Some(gid)) = (&node.kind, selected_id) {
            if gn.children.len() > 1 && matches("Reverse Order") {
                if ui
                    .button("Reverse Order")
                    .on_hover_text(
                        "Reverse the front-to-back stacking order of this group's children",
                    )
                    .clicked()
                {
                    action = Some(PanelAction::ReverseNodeOrder {
                        node_ids: vec![gid],
                    });
                }
                ui.add_space(2.0);
            }
            if gn.children.len() > 1 && matches("Flex Layout") {
                egui::CollapsingHeader::new("Flex Layout")
                    .default_open(false)
                    .id_salt("flex_layout_header")
                    .show(ui, |ui| {
                        ui.label(RichText::new("Distribute children in a row or column.").weak().small());
                        ui.horizontal(|ui| {
                            ui.label("Direction:");
                            // We borrow the direction from a thread_local to avoid extra params
                            egui::ComboBox::from_id_salt("flex_dir")
                                .selected_text("row")
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut "row".to_string(), "row".to_string(), "row");
                                    ui.selectable_value(&mut "column".to_string(), "column".to_string(), "column");
                                });
                        });
                        if ui.button("Apply Flex (row, gap=8)")
                            .on_hover_text("Distribute children left-to-right with 8px gap, centered vertically")
                            .clicked()
                        {
                            action = Some(PanelAction::ApplyFlexLayout {
                                group_id: gid,
                                direction: "row".into(),
                                gap: 8.0,
                                align: "center".into(),
                                padding: 0.0,
                            });
                        }
                        if ui.button("Apply Flex (column, gap=8)")
                            .on_hover_text("Distribute children top-to-bottom with 8px gap, centered horizontally")
                            .clicked()
                        {
                            action = Some(PanelAction::ApplyFlexLayout {
                                group_id: gid,
                                direction: "column".into(),
                                gap: 8.0,
                                align: "center".into(),
                                padding: 0.0,
                            });
                        }
                        ui.separator();
                        ui.label(RichText::new("Grid Layout").weak().small());
                        ui.horizontal(|ui| {
                            if ui.button("Grid (3 cols)")
                                .on_hover_text("Arrange children in a 3-column grid with 8px gaps")
                                .clicked()
                            {
                                action = Some(PanelAction::ApplyGridLayout {
                                    group_id: gid, columns: 3, gap_x: 8.0, gap_y: 8.0,
                                });
                            }
                            if ui.button("Grid (4 cols)")
                                .on_hover_text("Arrange children in a 4-column grid with 8px gaps")
                                .clicked()
                            {
                                action = Some(PanelAction::ApplyGridLayout {
                                    group_id: gid, columns: 4, gap_x: 8.0, gap_y: 8.0,
                                });
                            }
                            if ui.button("Stack (center)")
                                .on_hover_text("Stack all children at the same center point (Z-stack)")
                                .clicked()
                            {
                                action = Some(PanelAction::ApplyStackLayout {
                                    group_id: gid,
                                    align_h: "center".to_string(),
                                    align_v: "center".to_string(),
                                });
                            }
                        });
                    });
                ui.add_space(2.0);
            }

            // ── Expand Blend ─────────────────────────────────────────────
            if matches("Expand Blend") {
                if ui.button("Expand Blend")
                    .on_hover_text("Dissolve this group and place all child objects as standalone nodes at the parent layer")
                    .clicked()
                {
                    action = Some(PanelAction::ExpandBlend { group_id: gid });
                }
                ui.add_space(2.0);
            }

            // ── Blend Spine ───────────────────────────────────────────────
            if matches("Blend Spine") {
                egui::CollapsingHeader::new("Blend Spine")
                    .default_open(false)
                    .id_salt("blend_spine_header")
                    .show(ui, |ui| {
                        let spine_label = gn.blend_spine_id
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "None".into());
                        ui.label(RichText::new(format!("Current spine: {}", spine_label)).weak().small());
                        ui.separator();
                        ui.label(RichText::new("Select a path node from the scene by entering its ID or name:").weak().small());
                        // Use a thread_local for the input string to avoid extra params
                        thread_local! {
                            static SPINE_INPUT: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
                        }
                        SPINE_INPUT.with(|s| {
                            let mut val = s.borrow().clone();
                            ui.text_edit_singleline(&mut val);
                            *s.borrow_mut() = val;
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Set Spine")
                                .on_hover_text("Assign the entered path node as the blend spine for this group")
                                .clicked()
                            {
                                let path_str = SPINE_INPUT.with(|s| s.borrow().clone());
                                if !path_str.is_empty() {
                                    if let Ok(path_id) = uuid::Uuid::parse_str(&path_str) {
                                        action = Some(PanelAction::SetBlendSpine { group_id: gid, path_id });
                                    }
                                }
                            }
                            if gn.blend_spine_id.is_some() {
                                if ui.button("Reverse Spine")
                                    .on_hover_text("Reverse the direction of the blend spine path, inverting the interpolation order")
                                    .clicked()
                                {
                                    action = Some(PanelAction::ReverseBlendSpine { group_id: gid });
                                }
                                if ui.button("Clear Spine")
                                    .on_hover_text("Remove the blend spine assignment from this group")
                                    .clicked()
                                {
                                    action = Some(PanelAction::ClearBlendSpine { group_id: gid });
                                }
                            }
                        });
                    });
                ui.add_space(2.0);
            }
        }

        // ── Per-Node Undo ──────────────────────────────────────────────────
        if let Some(nid) = selected_id {
            if matches("Revert Node") {
                ui.horizontal(|ui| {
                    if ui.button("↩ Revert Last Edit")
                        .on_hover_text("Undo the last edit to this node only, without affecting any other nodes")
                        .clicked()
                    {
                        action = Some(PanelAction::UndoNode { node_id: nid, steps: 1 });
                    }
                    if ui.button("↩↩ Revert 3 Edits")
                        .on_hover_text("Revert this node to its state 3 edits ago")
                        .clicked()
                    {
                        action = Some(PanelAction::UndoNode { node_id: nid, steps: 3 });
                    }
                });
                ui.add_space(2.0);
            }
        }

        // ── Prompt History (read-only) ─────────────────────────────────────
        if !node.prompt_history.is_empty() && matches("Origin") {
            egui::CollapsingHeader::new("Origin (Prompt History)")
                .default_open(false)
                .open(forced_open)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("AI prompts that created or modified this node:")
                            .weak()
                            .small(),
                    );
                    for (i, prompt) in node.prompt_history.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{}.", i + 1)).weak().small());
                            ui.label(RichText::new(prompt).small());
                        });
                    }
                });
            ui.add_space(2.0);
        }

        // ── Asset Export ───────────────────────────────────────────────────
        if matches("Asset Export") {
            let nid = node.id;
            egui::CollapsingHeader::new("Asset Export")
                .default_open(node.export_spec.is_some())
                .open(forced_open)
                .show(ui, |ui| {
                    if let Some(spec) = &node.export_spec {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Tagged: {} ({})", spec.name, spec.format))
                                    .small(),
                            );
                            if !spec.scales.is_empty() && spec.format != "svg" {
                                let scale_str: Vec<_> =
                                    spec.scales.iter().map(|s| format!("{}x", s)).collect();
                                ui.label(RichText::new(scale_str.join(", ")).weak().small());
                            }
                        });
                        if ui
                            .small_button("Remove Tag")
                            .on_hover_text("Remove this node's asset export tag")
                            .clicked()
                        {
                            action = Some(PanelAction::RemoveExportTag { node_id: nid });
                        }
                    } else {
                        ui.label(RichText::new("Not tagged for export.").weak().small());
                        if ui
                            .button("Tag as SVG Asset")
                            .on_hover_text("Tag this node for batch SVG export using its name")
                            .clicked()
                        {
                            action = Some(PanelAction::TagNodeForExport {
                                node_id: nid,
                                name: if node.name.is_empty() {
                                    format!("asset-{}", &nid.to_string()[..8])
                                } else {
                                    node.name.clone()
                                },
                                format: "svg".to_string(),
                            });
                        }
                    }
                });
            ui.add_space(2.0);
        }

        // ── Typography & Text ─────────────────────────────────────────────
        if let SceneNodeKind::Text(_) = &node.kind {
            ui.add_space(2.0);
            ui.separator();
            ui.label(
                RichText::new("Typography")
                    .small()
                    .color(crate::theme::section_header_color(ui)),
            );
            ui.add_space(2.0);
        }

        // ── Text Operations ────────────────────────────────────────────────
        if let SceneNodeKind::Text(tn) = &node.kind {
            let text_nid = node.id;
            if matches("Text Operations") {
                let mut line_h = tn.line_height;
                let mut letter_sp = tn.letter_spacing;
                egui::CollapsingHeader::new("Text Operations")
                    .default_open(true)
                    .open(forced_open)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Line Height");
                            if ui.add(egui::DragValue::new(&mut line_h).speed(0.05).range(0.5..=5.0)).changed() {
                                action = Some(PanelAction::SetTextTypography { node_id: text_nid, line_height: Some(line_h), letter_spacing: None });
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Letter Spacing");
                            if ui.add(egui::DragValue::new(&mut letter_sp).speed(0.1).range(-20.0..=50.0).suffix(" px")).changed() {
                                action = Some(PanelAction::SetTextTypography { node_id: text_nid, line_height: None, letter_spacing: Some(letter_sp) });
                            }
                        });
                        // Paragraph spacing and indent
                        ui.horizontal(|ui| {
                            let mut sp_before = tn.paragraph_spacing_before;
                            let mut sp_after  = tn.paragraph_spacing_after;
                            let mut t_indent  = tn.text_indent;
                            let mut changed = false;
                            ui.label("¶ Before:");
                            if ui.add(egui::DragValue::new(&mut sp_before).speed(0.5).range(0.0..=200.0)).changed() { changed = true; }
                            ui.label("After:");
                            if ui.add(egui::DragValue::new(&mut sp_after).speed(0.5).range(0.0..=200.0)).changed() { changed = true; }
                            ui.label("Indent:");
                            if ui.add(egui::DragValue::new(&mut t_indent).speed(0.5).range(-200.0..=200.0)).changed() { changed = true; }
                            if changed {
                                action = Some(PanelAction::SetParagraphOptions {
                                    node_id: text_nid,
                                    spacing_before: sp_before,
                                    spacing_after:  sp_after,
                                    indent:         t_indent,
                                });
                            }
                        });
                        // Tab Stops panel
                        ui.collapsing("Tab Stops", |ui| {
                            thread_local! {
                                static TAB_STOP_INPUT: std::cell::RefCell<f64> = const { std::cell::RefCell::new(50.0) };
                            }
                            let current_stops = tn.tab_stops.clone();
                            if current_stops.is_empty() {
                                ui.label(RichText::new("Default tab spacing (every 4 em)").weak().small());
                            } else {
                                for (i, &stop) in current_stops.iter().enumerate() {
                                    ui.label(format!("  {}: {:.1} px", i + 1, stop));
                                }
                            }
                            ui.horizontal(|ui| {
                                TAB_STOP_INPUT.with(|v| {
                                    ui.label("Add stop:");
                                    ui.add(egui::DragValue::new(&mut *v.borrow_mut()).speed(1.0).range(1.0..=2000.0).suffix(" px"));
                                    if ui.small_button("+").on_hover_text("Add this tab stop position").clicked() {
                                        let new_stop = *v.borrow();
                                        let mut stops = current_stops.clone();
                                        if !stops.contains(&new_stop) {
                                            stops.push(new_stop);
                                            stops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                                            action = Some(PanelAction::SetTabStops { node_id: text_nid, stops });
                                        }
                                    }
                                });
                            });
                            if !current_stops.is_empty()
                                && ui.small_button("Clear All").on_hover_text("Remove all custom tab stops").clicked() {
                                    action = Some(PanelAction::ClearTabStops { node_id: text_nid });
                                }
                        });
                        if ui.button("Find / Replace…")
                            .on_hover_text("Search and replace text content across text nodes")
                            .clicked()
                        {
                            action = Some(PanelAction::OpenFindReplaceTextDialog);
                        }
                        ui.horizontal(|ui| {
                            // Bold toggle
                            let is_bold = tn.font_weight >= 700;
                            let b_label = RichText::new("B").strong();
                            let b_btn = egui::Button::new(b_label)
                                .selected(is_bold)
                                .min_size(egui::vec2(22.0, 0.0));
                            if ui.add(b_btn)
                                .on_hover_text(if is_bold { "Remove Bold (set weight 400)" } else { "Bold (set weight 700)" })
                                .clicked()
                            {
                                let new_w = if is_bold { 400 } else { 700 };
                                action = Some(PanelAction::SetFontWeight { node_id: text_nid, weight: new_w });
                            }
                            // Italic toggle
                            use photonic_core::node::FontStyle;
                            let is_italic = tn.font_style == FontStyle::Italic;
                            let i_label = RichText::new("I").italics();
                            let i_btn = egui::Button::new(i_label)
                                .selected(is_italic)
                                .min_size(egui::vec2(22.0, 0.0));
                            if ui.add(i_btn)
                                .on_hover_text(if is_italic { "Remove Italic" } else { "Italic" })
                                .clicked()
                            {
                                let new_style = if is_italic { "normal".to_string() } else { "italic".to_string() };
                                action = Some(PanelAction::SetFontStyle { node_id: text_nid, style: new_style });
                            }
                        });
                        ui.horizontal(|ui| {
                            let is_vertical = tn.vertical;
                            let label = if is_vertical { "↕ Vertical (click to switch)" } else { "↔ Horizontal (click to switch)" };
                            if ui.small_button(label)
                                .on_hover_text("Toggle between horizontal and vertical text layout")
                                .clicked()
                            {
                                action = Some(PanelAction::SetTextDirection { node_id: text_nid, vertical: !is_vertical });
                            }
                        });
                        // Decoration buttons: U (underline), S (strikethrough), O (overline)
                        ui.horizontal(|ui| {
                            let cur = tn.text_decoration.as_str();
                            let u_active = cur == "underline";
                            let s_active = cur == "line-through";
                            let o_active = cur == "overline";
                            let u_btn = egui::Button::new(RichText::new("U").underline());
                            let s_btn = egui::Button::new(RichText::new("S").strikethrough());
                            let o_btn = egui::Button::new("O̅");
                            if ui.add(u_btn.selected(u_active))
                                .on_hover_text("Underline").clicked()
                            {
                                let dec = if u_active { "" } else { "underline" };
                                action = Some(PanelAction::SetTextDecoration { node_id: text_nid, decoration: dec.to_string() });
                            }
                            if ui.add(s_btn.selected(s_active))
                                .on_hover_text("Strikethrough").clicked()
                            {
                                let dec = if s_active { "" } else { "line-through" };
                                action = Some(PanelAction::SetTextDecoration { node_id: text_nid, decoration: dec.to_string() });
                            }
                            if ui.add(o_btn.selected(o_active))
                                .on_hover_text("Overline").clicked()
                            {
                                let dec = if o_active { "" } else { "overline" };
                                action = Some(PanelAction::SetTextDecoration { node_id: text_nid, decoration: dec.to_string() });
                            }
                        });
                        // Advanced character metrics: super/subscript + baseline shift.
                        ui.horizontal(|ui| {
                            use photonic_core::node::ScriptPosition;
                            let cur_script = tn.script_position;
                            let normal_active = cur_script == ScriptPosition::Normal;
                            let sup_active = cur_script == ScriptPosition::Superscript;
                            let sub_active = cur_script == ScriptPosition::Subscript;
                            ui.label("Script:");
                            if ui.add(egui::Button::new("N").selected(normal_active))
                                .on_hover_text("Normal baseline").clicked() && !normal_active
                            {
                                action = Some(PanelAction::SetCharacterMetrics { node_id: text_nid, baseline_shift: tn.baseline_shift, script_position: ScriptPosition::Normal });
                            }
                            if ui.add(egui::Button::new(RichText::new("x²").small()).selected(sup_active))
                                .on_hover_text("Superscript").clicked()
                            {
                                let next = if sup_active { ScriptPosition::Normal } else { ScriptPosition::Superscript };
                                action = Some(PanelAction::SetCharacterMetrics { node_id: text_nid, baseline_shift: tn.baseline_shift, script_position: next });
                            }
                            if ui.add(egui::Button::new(RichText::new("x₂").small()).selected(sub_active))
                                .on_hover_text("Subscript").clicked()
                            {
                                let next = if sub_active { ScriptPosition::Normal } else { ScriptPosition::Subscript };
                                action = Some(PanelAction::SetCharacterMetrics { node_id: text_nid, baseline_shift: tn.baseline_shift, script_position: next });
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Baseline shift:");
                            let mut bshift = tn.baseline_shift;
                            if ui.add(egui::DragValue::new(&mut bshift).speed(0.25).range(-200.0..=200.0).suffix(" px"))
                                .on_hover_text("Baseline shift in document units (positive raises the text)")
                                .changed()
                            {
                                action = Some(PanelAction::SetCharacterMetrics { node_id: text_nid, baseline_shift: bshift, script_position: tn.script_position });
                            }
                        });
                    });
            }
        }

        // ── Character Styles (shown for text nodes when styles exist) ─────
        if let SceneNodeKind::Text(_) = &node.kind {
            let text_nid = node.id;
            if !doc.character_styles.is_empty() && matches("Character Styles") {
                egui::CollapsingHeader::new("Character Styles")
                    .default_open(false)
                    .open(forced_open)
                    .show(ui, |ui| {
                        for style in &doc.character_styles {
                            ui.horizontal(|ui| {
                                let label = if let Some(fs) = style.font_size {
                                    format!("{} ({}pt)", style.name, fs as u32)
                                } else {
                                    style.name.clone()
                                };
                                ui.label(RichText::new(&label).small());
                                if ui
                                    .small_button("Apply")
                                    .on_hover_text(format!(
                                        "Apply '{}' to this text node",
                                        style.name
                                    ))
                                    .clicked()
                                {
                                    action = Some(PanelAction::ApplyCharacterStyle {
                                        node_id: text_nid,
                                        style_name: style.name.clone(),
                                    });
                                }
                                if ui
                                    .small_button(ph::X)
                                    .on_hover_text("Delete this character style")
                                    .clicked()
                                {
                                    action = Some(PanelAction::DeleteCharacterStyle {
                                        name: style.name.clone(),
                                    });
                                }
                            });
                        }
                    });
                ui.add_space(2.0);
            }
        }

        // ── Paragraph Styles (shown for text nodes when styles exist) ────
        if let SceneNodeKind::Text(_) = &node.kind {
            let text_nid = node.id;
            if !doc.paragraph_styles.is_empty() && matches("Paragraph Styles") {
                egui::CollapsingHeader::new("Paragraph Styles")
                    .default_open(false)
                    .open(forced_open)
                    .show(ui, |ui| {
                        for style in &doc.paragraph_styles {
                            ui.horizontal(|ui| {
                                let label = if let Some(a) = &style.align {
                                    format!("{} ({})", style.name, a)
                                } else {
                                    style.name.clone()
                                };
                                ui.label(RichText::new(&label).small());
                                if ui
                                    .small_button("Apply")
                                    .on_hover_text(format!(
                                        "Apply '{}' to this text node",
                                        style.name
                                    ))
                                    .clicked()
                                {
                                    action = Some(PanelAction::ApplyParagraphStyle {
                                        node_id: text_nid,
                                        style_name: style.name.clone(),
                                    });
                                }
                                if ui
                                    .small_button(ph::X)
                                    .on_hover_text("Delete this paragraph style")
                                    .clicked()
                                {
                                    action = Some(PanelAction::DeleteParagraphStyle {
                                        name: style.name.clone(),
                                    });
                                }
                            });
                        }
                    });
                ui.add_space(2.0);
            }
        }

        // ── Type on a Path (shown for text nodes) ────────────────────────
        if let SceneNodeKind::Text(ref tn) = node.kind {
            let text_nid = node.id;
            if matches("Type on a Path") {
                egui::CollapsingHeader::new("Type on a Path")
                    .default_open(true)
                    .open(forced_open)
                    .show(ui, |ui| {
                        if let Some(spine_id) = tn.path_spine_id {
                            let spine_name = doc.nodes.get(&spine_id)
                                .map(|n| n.name.clone())
                                .unwrap_or_else(|| spine_id.to_string());
                            ui.label(RichText::new(format!("Spine: {}", spine_name)).small());
                            ui.label(RichText::new(format!("Offset: {:.1} px", tn.path_offset)).small().weak());
                            if ui.button("Clear Path")
                                .on_hover_text("Remove the path spine and revert to normal positioned text")
                                .clicked()
                            {
                                action = Some(PanelAction::ClearTextPath { text_node_id: text_nid });
                            }
                        } else {
                            // Look for a path in the current selection to pair with
                            let path_node_id: Option<NodeId> = doc.selection.ids()
                                .find(|&&sid| sid != text_nid && doc.nodes.get(&sid).is_some_and(|n| matches!(n.kind, SceneNodeKind::Path(_))))
                                .copied();
                            if let Some(pid) = path_node_id {
                                let path_name = doc.nodes.get(&pid).map(|n| n.name.clone()).unwrap_or_default();
                                ui.label(RichText::new(format!("Selected path: {}", path_name)).small().weak());
                                if ui.button("Set as Path Spine")
                                    .on_hover_text("Place this text along the selected path")
                                    .clicked()
                                {
                                    action = Some(PanelAction::SetTextPath {
                                        text_node_id: text_nid,
                                        path_node_id: pid,
                                        offset: 0.0,
                                    });
                                }
                            } else {
                                ui.label(RichText::new("Select a text node + a path node, then click Set as Path Spine.").weak().small());
                            }
                        }
                    });
                ui.add_space(2.0);
            }
        }

        // ── Area Type (shown for text nodes) ─────────────────────────────
        if let SceneNodeKind::Text(ref tn) = node.kind {
            let text_nid = node.id;
            if matches("Area Type") {
                egui::CollapsingHeader::new("Area Type")
                    .default_open(true)
                    .open(forced_open)
                    .show(ui, |ui| {
                        if let Some(area_id) = tn.area_path_id {
                            let area_name = doc.nodes.get(&area_id)
                                .map(|n| n.name.clone())
                                .unwrap_or_else(|| area_id.to_string());
                            ui.label(RichText::new(format!("Area: {}", area_name)).small());
                            if ui.button("Clear Area")
                                .on_hover_text("Remove the area boundary and revert to normal point text")
                                .clicked()
                            {
                                action = Some(PanelAction::ClearTextArea { text_node_id: text_nid });
                            }
                        } else {
                            let area_node_id: Option<NodeId> = doc.selection.ids()
                                .find(|&&sid| sid != text_nid && doc.nodes.get(&sid).is_some_and(|n| matches!(n.kind, SceneNodeKind::Path(_))))
                                .copied();
                            if let Some(aid) = area_node_id {
                                let area_name = doc.nodes.get(&aid).map(|n| n.name.clone()).unwrap_or_default();
                                ui.label(RichText::new(format!("Selected path: {}", area_name)).small().weak());
                                if ui.button("Set as Area Boundary")
                                    .on_hover_text("Flow this text inside the selected closed path")
                                    .clicked()
                                {
                                    action = Some(PanelAction::SetTextArea {
                                        text_node_id: text_nid,
                                        area_path_id: aid,
                                    });
                                }
                            } else {
                                ui.label(RichText::new("Select a text node + a closed path, then click Set as Area Boundary.").weak().small());
                            }
                        }
                    });
                ui.add_space(2.0);
            }
        }

        // ── OpenType Features ─────────────────────────────────────────────
        if let SceneNodeKind::Text(ref tn) = node.kind {
            let text_nid = node.id;
            if matches("OpenType Features") {
                const OTF_FEATURES: &[(&str, &str)] = &[
                    ("liga", "Standard Ligatures"),
                    ("calt", "Contextual Alternates"),
                    ("frac", "Fractions"),
                    ("smcp", "Small Caps"),
                    ("sups", "Superscript"),
                    ("subs", "Subscript"),
                    ("ordn", "Ordinals"),
                    ("swsh", "Swashes"),
                    ("dlig", "Discretionary Ligatures"),
                    ("onum", "Oldstyle Figures"),
                    ("tnum", "Tabular Figures"),
                    ("zero", "Slashed Zero"),
                ];
                egui::CollapsingHeader::new("OpenType Features")
                    .default_open(false)
                    .id_salt("opentype_features_header")
                    .show(ui, |ui| {
                        let mut new_features = tn.opentype_features.clone();
                        let mut changed = false;
                        ui.label(
                            RichText::new("Enable typographic features (font support varies).")
                                .weak()
                                .small(),
                        );
                        ui.add_space(2.0);
                        for (tag, label) in OTF_FEATURES {
                            let mut enabled = new_features.contains(&tag.to_string());
                            if ui.checkbox(&mut enabled, *label).changed() {
                                changed = true;
                                if enabled {
                                    new_features.push(tag.to_string());
                                } else {
                                    new_features.retain(|f| f != *tag);
                                }
                            }
                        }
                        if changed {
                            action = Some(PanelAction::SetOpenTypeFeatures {
                                node_id: text_nid,
                                features: new_features,
                            });
                        }
                    });
                ui.add_space(2.0);
            }
        }

        // ── Text Frame Threading ─────────────────────────────────────────
        if let SceneNodeKind::Text(ref tn) = node.kind {
            let text_nid = node.id;
            if matches("Text Frame Threading") {
                egui::CollapsingHeader::new("Text Frame Threading")
                    .default_open(false)
                    .id_salt("text_frame_thread_header")
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(
                                "Chain text nodes so overflow flows from one to the next.",
                            )
                            .weak()
                            .small(),
                        );
                        ui.add_space(2.0);

                        // Show current chain state.
                        if tn.prev_frame.is_some() || tn.next_frame.is_some() {
                            if let Some(pid) = tn.prev_frame {
                                let pname = doc
                                    .nodes
                                    .get(&pid)
                                    .map(|n| n.name.clone())
                                    .unwrap_or_else(|| pid.to_string());
                                ui.label(RichText::new(format!("← from: {}", pname)).small());
                            }
                            if let Some(nid) = tn.next_frame {
                                let nname = doc
                                    .nodes
                                    .get(&nid)
                                    .map(|n| n.name.clone())
                                    .unwrap_or_else(|| nid.to_string());
                                ui.label(RichText::new(format!("→ to: {}", nname)).small());
                            }
                            if ui.button("Unlink Frame").clicked() {
                                action = Some(PanelAction::UnlinkTextFrames { node_id: text_nid });
                            }
                        } else {
                            // Find another text node in selection to link to.
                            let other_text: Option<NodeId> = doc
                                .selection
                                .ids()
                                .find(|&&sid| {
                                    sid != text_nid
                                        && doc.nodes.get(&sid).is_some_and(|n| {
                                            matches!(n.kind, SceneNodeKind::Text(_))
                                        })
                                })
                                .copied();
                            if let Some(other_id) = other_text {
                                let other_name = doc
                                    .nodes
                                    .get(&other_id)
                                    .map(|n| n.name.clone())
                                    .unwrap_or_default();
                                ui.label(
                                    RichText::new(format!("Selected text node: {}", other_name))
                                        .small()
                                        .weak(),
                                );
                                ui.horizontal(|ui| {
                                    if ui
                                        .button("Link Frame →")
                                        .on_hover_text(
                                            "This node overflows into the selected text node",
                                        )
                                        .clicked()
                                    {
                                        action = Some(PanelAction::LinkTextFrames {
                                            from_id: text_nid,
                                            to_id: other_id,
                                        });
                                    }
                                    if ui
                                        .button("← Link Frame")
                                        .on_hover_text(
                                            "The selected text node overflows into this node",
                                        )
                                        .clicked()
                                    {
                                        action = Some(PanelAction::LinkTextFrames {
                                            from_id: other_id,
                                            to_id: text_nid,
                                        });
                                    }
                                });
                            } else {
                                ui.label(
                                    RichText::new("Select two text nodes to link them.")
                                        .weak()
                                        .small(),
                                );
                            }
                        }
                    });
                ui.add_space(2.0);
            }
        }

        // ── Select Same ──────────────────────────────────────────────────
        if let Some(ref_id) = selected_id {
            if matches("Select Same") {
                egui::CollapsingHeader::new("Select Same")
                    .default_open(false)
                    .open(forced_open)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Select all nodes sharing this attribute")
                                .weak()
                                .small(),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .button("Fill Color")
                                .on_hover_text("Select nodes with the same solid fill color")
                                .clicked()
                            {
                                action = Some(PanelAction::SelectSame {
                                    node_id: ref_id,
                                    attribute: SelectSameAttr::FillColor,
                                });
                            }
                            if ui
                                .button("Stroke Color")
                                .on_hover_text("Select nodes with the same stroke color")
                                .clicked()
                            {
                                action = Some(PanelAction::SelectSame {
                                    node_id: ref_id,
                                    attribute: SelectSameAttr::StrokeColor,
                                });
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui
                                .button("Stroke Weight")
                                .on_hover_text("Select nodes with the same stroke width")
                                .clicked()
                            {
                                action = Some(PanelAction::SelectSame {
                                    node_id: ref_id,
                                    attribute: SelectSameAttr::StrokeWeight,
                                });
                            }
                            if ui
                                .button("Opacity")
                                .on_hover_text("Select nodes with the same opacity")
                                .clicked()
                            {
                                action = Some(PanelAction::SelectSame {
                                    node_id: ref_id,
                                    attribute: SelectSameAttr::Opacity,
                                });
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui
                                .button("Blend Mode")
                                .on_hover_text("Select nodes with the same blend mode")
                                .clicked()
                            {
                                action = Some(PanelAction::SelectSame {
                                    node_id: ref_id,
                                    attribute: SelectSameAttr::BlendMode,
                                });
                            }
                            if ui
                                .button("Object Type")
                                .on_hover_text(
                                    "Select all nodes of the same type (path/group/text)",
                                )
                                .clicked()
                            {
                                action = Some(PanelAction::SelectSame {
                                    node_id: ref_id,
                                    attribute: SelectSameAttr::ObjectType,
                                });
                            }
                        });
                    });
            }
        }

        // ── Raster Masking (raster layers only) ───────────────────────────
        // Non-destructive: both operations write the node's layer mask (which
        // the compositor multiplies into source alpha), never the pixels, and
        // commit as one undoable UpdateNode.
        if let (Some(nid), Some(node)) = (selected_id, selected_node) {
            let is_plain_raster = matches!(
                &node.kind,
                SceneNodeKind::Raster(r) if !r.is_adjustment_layer()
            );
            if is_plain_raster && matches("Raster Masking") {
                egui::CollapsingHeader::new("Raster Masking")
                    .default_open(true)
                    .id_salt("raster_masking_header")
                    .open(forced_open)
                    .show(ui, |ui| {
                        // ── Color range ────────────────────────────────────
                        ui.label(
                            RichText::new(
                                "Hide pixels by color (like Select > Color Range + delete, \
                                 but reversible via the layer mask).",
                            )
                            .weak()
                            .small(),
                        );
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.label("Fuzziness:");
                            let tol = ui.add(
                                egui::Slider::new(raster_mask_tolerance, 0.0..=1.0)
                                    .fixed_decimals(2),
                            );
                            if tol.changed() && raster_color_range_target.is_some() {
                                action = Some(PanelAction::SetRasterColorRangeParams {
                                    tolerance: *raster_mask_tolerance,
                                    contiguous: *raster_mask_contiguous,
                                });
                            }
                        });
                        ui.horizontal(|ui| {
                            let cont = ui
                                .checkbox(raster_mask_contiguous, "Contiguous")
                                .on_hover_text(
                                    "On: only the connected region under the click \
                                     (magic wand). Off: every matching pixel in the layer \
                                     (color range).",
                                );
                            if cont.changed() && raster_color_range_target.is_some() {
                                action = Some(PanelAction::SetRasterColorRangeParams {
                                    tolerance: *raster_mask_tolerance,
                                    contiguous: *raster_mask_contiguous,
                                });
                            }
                        });
                        match raster_color_range_target {
                            None => {
                                if ui
                                    .button(format!("{} Pick Color to Hide", ph::EYEDROPPER))
                                    .on_hover_text(
                                        "Click a color on the canvas; matching pixels \
                                         preview as hidden, then Apply or Cancel",
                                    )
                                    .clicked()
                                {
                                    action =
                                        Some(PanelAction::StartRasterColorRange { node_id: nid });
                                }
                            }
                            Some(rgba) => {
                                ui.horizontal(|ui| {
                                    ui.label("Hiding:");
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(18.0, 14.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(
                                        rect,
                                        3.0,
                                        Color32::from_rgb(rgba[0], rgba[1], rgba[2]),
                                    );
                                    ui.painter().rect_stroke(
                                        rect,
                                        3.0,
                                        egui::Stroke::new(1.0, Color32::WHITE),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "#{:02X}{:02X}{:02X}",
                                            rgba[0], rgba[1], rgba[2]
                                        ))
                                        .small()
                                        .weak(),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    if ui.button("Apply").clicked() {
                                        action = Some(PanelAction::ApplyRasterColorRange);
                                    }
                                    if ui.button("Cancel").clicked() {
                                        action = Some(PanelAction::CancelRasterColorRange);
                                    }
                                    if ui
                                        .button(format!("{} Repick", ph::EYEDROPPER))
                                        .on_hover_text("Sample a different color")
                                        .clicked()
                                    {
                                        action = Some(PanelAction::StartRasterColorRange {
                                            node_id: nid,
                                        });
                                    }
                                });
                            }
                        }

                        // ── Remove background ─────────────────────────────
                        ui.add_space(6.0);
                        ui.separator();
                        ui.label(
                            RichText::new(
                                "Detect the subject with a local model (offline, on-device) \
                                 and mask out the background.",
                            )
                            .weak()
                            .small(),
                        );
                        ui.add_space(2.0);
                        if ui
                            .button(format!("{} Remove Background", ph::PERSON_SIMPLE))
                            .on_hover_text(if rmbg_model_cached {
                                "Runs the local U²-Net model and applies the result as a \
                                 non-destructive layer mask"
                            } else {
                                "First use downloads the small model (~4.7 MB) to the \
                                 Photonic cache, then works offline"
                            })
                            .clicked()
                        {
                            action = Some(PanelAction::RasterRemoveBackground { node_id: nid });
                        }
                        if ui
                            .small_button("Clear Layer Mask")
                            .on_hover_text("Remove the layer mask, revealing all pixels again")
                            .clicked()
                        {
                            action = Some(PanelAction::ClearRasterMask { node_id: nid });
                        }
                    });
                ui.add_space(2.0);
            }
            if is_plain_raster && matches("Raster Layer") {
                egui::CollapsingHeader::new("Raster Layer")
                    .default_open(true)
                    .id_salt("raster_layer_header")
                    .open(forced_open)
                    .show(ui, |ui| {
                        if ui
                            .button(format!("{} Crop to Artboard", ph::CROP))
                            .on_hover_text(
                                "Trim the image (and its layer mask) to the artboard \
                                 bounds, discarding pixels outside. Destructive but \
                                 undoable. Requires an unrotated image.",
                            )
                            .clicked()
                        {
                            action = Some(PanelAction::CropRasterToArtboard { node_id: nid });
                        }
                    });
                ui.add_space(2.0);
            }
        }

        ui.add_space(4.0);
    } else {
        // ── Document Info (shown when no node is selected) ─────────────────
        ui.label(RichText::new("Document").strong());
        ui.add_space(2.0);
        ui.label(format!(
            "Canvas: {}×{}",
            doc.width as u32, doc.height as u32
        ));
        ui.label(format!("Layers: {}", doc.layers.len()));

        // Count nodes by kind
        let mut n_path = 0usize;
        let mut n_text = 0usize;
        let mut n_group = 0usize;
        for node in doc.nodes.values() {
            match &node.kind {
                SceneNodeKind::Path(_) => n_path += 1,
                SceneNodeKind::Text(_) => n_text += 1,
                SceneNodeKind::Group(_) => n_group += 1,
                // raster nodes are not counted in the vector node summary
                SceneNodeKind::Raster(_) => {}
            }
        }
        let total = n_path + n_text + n_group;
        ui.label(format!(
            "Nodes: {} ({} path, {} text, {} group)",
            total, n_path, n_text, n_group
        ));

        // ── Print Settings ────────────────────────────────────────────────
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Print Settings")
            .default_open(false)
            .id_salt("print_settings_header")
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Bleed and slug for print production.")
                        .weak()
                        .small(),
                );
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label("Bleed (mm):")
                        .on_hover_text("Extra artwork past trim edge (typically 3 mm)");
                    ui.add(
                        egui::DragValue::new(bleed_mm_input)
                            .speed(0.1)
                            .range(0.0..=25.0)
                            .suffix(" mm"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Slug (mm):")
                        .on_hover_text("Area outside bleed for printer marks");
                    ui.add(
                        egui::DragValue::new(slug_mm_input)
                            .speed(0.1)
                            .range(0.0..=25.0)
                            .suffix(" mm"),
                    );
                });
                ui.add_space(2.0);
                if ui.button("Apply Print Settings").clicked() {
                    action = Some(PanelAction::SetDocumentBleed {
                        bleed_mm: *bleed_mm_input,
                        slug_mm: *slug_mm_input,
                    });
                }
                if doc.bleed_mm > 0.0 || doc.slug_mm > 0.0 {
                    ui.label(
                        RichText::new(format!(
                            "Current: bleed={:.2} mm, slug={:.2} mm",
                            doc.bleed_mm, doc.slug_mm
                        ))
                        .small()
                        .weak(),
                    );
                }
            });

        // ── Artboard Margins ──────────────────────────────────────────────
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Artboard Margins")
            .default_open(false)
            .id_salt("artboard_margins_header")
            .show(ui, |ui| {
                ui.label(RichText::new("Safe-area guides inside the artboard boundary.").weak().small());
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label("Top:");
                    ui.add(egui::DragValue::new(margin_top).speed(1.0).range(0.0..=2000.0).suffix(" px"));
                    ui.label("Right:");
                    ui.add(egui::DragValue::new(margin_right).speed(1.0).range(0.0..=2000.0).suffix(" px"));
                });
                ui.horizontal(|ui| {
                    ui.label("Bottom:");
                    ui.add(egui::DragValue::new(margin_bottom).speed(1.0).range(0.0..=2000.0).suffix(" px"));
                    ui.label("Left:");
                    ui.add(egui::DragValue::new(margin_left).speed(1.0).range(0.0..=2000.0).suffix(" px"));
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.button("Apply Margins").clicked() {
                        action = Some(PanelAction::SetArtboardMargins {
                            top: *margin_top,
                            right: *margin_right,
                            bottom: *margin_bottom,
                            left: *margin_left,
                        });
                    }
                    if ui.small_button("Reset").clicked() {
                        *margin_top = 0.0; *margin_right = 0.0;
                        *margin_bottom = 0.0; *margin_left = 0.0;
                        action = Some(PanelAction::SetArtboardMargins {
                            top: 0.0, right: 0.0, bottom: 0.0, left: 0.0
                        });
                    }
                });
                let has_margins = doc.margin_top > 0.0 || doc.margin_right > 0.0
                    || doc.margin_bottom > 0.0 || doc.margin_left > 0.0;
                if has_margins {
                    ui.label(
                        RichText::new(format!(
                            "Current: T={:.0} R={:.0} B={:.0} L={:.0}",
                            doc.margin_top, doc.margin_right, doc.margin_bottom, doc.margin_left
                        ))
                        .small().weak(),
                    );
                    ui.add_space(2.0);
                    if ui.add_enabled(
                        selection_count > 0 || !doc.nodes.is_empty(),
                        egui::Button::new("Fit to Margins"),
                    )
                    .on_hover_text("Scale and center selected nodes (or all nodes) to fill the artboard safe area")
                    .clicked()
                    {
                        action = Some(PanelAction::FitToMargins);
                    }
                }
            });

        // ── Construction Lines ────────────────────────────────────────────
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Construction Lines")
            .default_open(false)
            .id_salt("construction_lines_header")
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Infinite non-printing reference lines at any angle.")
                        .weak()
                        .small(),
                );
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label("X:");
                    ui.add(egui::DragValue::new(construction_x).speed(1.0));
                    ui.label("Y:");
                    ui.add(egui::DragValue::new(construction_y).speed(1.0));
                });
                ui.horizontal(|ui| {
                    ui.label("Angle:");
                    ui.add(
                        egui::DragValue::new(construction_angle)
                            .speed(1.0)
                            .range(-360.0..=360.0)
                            .suffix("°"),
                    );
                });
                if ui
                    .button("Add Construction Line")
                    .on_hover_text("Add an infinite angled reference line (non-printing)")
                    .clicked()
                {
                    action = Some(PanelAction::AddConstructionLine {
                        x: *construction_x,
                        y: *construction_y,
                        angle_degrees: *construction_angle,
                    });
                }
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .small_button("H (0°)")
                        .on_hover_text("Add horizontal construction line")
                        .clicked()
                    {
                        action = Some(PanelAction::AddConstructionLine {
                            x: *construction_x,
                            y: *construction_y,
                            angle_degrees: 0.0,
                        });
                    }
                    if ui
                        .small_button("V (90°)")
                        .on_hover_text("Add vertical construction line")
                        .clicked()
                    {
                        action = Some(PanelAction::AddConstructionLine {
                            x: *construction_x,
                            y: *construction_y,
                            angle_degrees: 90.0,
                        });
                    }
                    if ui
                        .small_button("D (45°)")
                        .on_hover_text("Add 45° diagonal construction line")
                        .clicked()
                    {
                        action = Some(PanelAction::AddConstructionLine {
                            x: *construction_x,
                            y: *construction_y,
                            angle_degrees: 45.0,
                        });
                    }
                    if ui
                        .small_button("D (-45°)")
                        .on_hover_text("Add -45° diagonal construction line")
                        .clicked()
                    {
                        action = Some(PanelAction::AddConstructionLine {
                            x: *construction_x,
                            y: *construction_y,
                            angle_degrees: -45.0,
                        });
                    }
                });
            });

        // ── Select by Kind buttons ────────────────────────────────────────
        ui.add_space(4.0);
        ui.label(RichText::new("Select all…").small());
        ui.horizontal_wrapped(|ui| {
            if ui
                .small_button("Paths")
                .on_hover_text("Select all path/shape nodes")
                .clicked()
            {
                action = Some(PanelAction::SelectByKind {
                    kind: "path".to_string(),
                    additive: false,
                });
            }
            if ui
                .small_button("Text")
                .on_hover_text("Select all text nodes")
                .clicked()
            {
                action = Some(PanelAction::SelectByKind {
                    kind: "text".to_string(),
                    additive: false,
                });
            }
            if ui
                .small_button("Groups")
                .on_hover_text("Select all group nodes")
                .clicked()
            {
                action = Some(PanelAction::SelectByKind {
                    kind: "group".to_string(),
                    additive: false,
                });
            }
            if ui
                .small_button("On Layer")
                .on_hover_text("Select all nodes on the active layer")
                .clicked()
            {
                action = Some(PanelAction::SelectByKind {
                    kind: "same_layer".to_string(),
                    additive: false,
                });
            }
        });

        // Unique solid fill colors as swatches
        use photonic_core::style::FillKind;
        let mut fill_colors: Vec<photonic_core::color::Color> = Vec::new();
        for node in doc.nodes.values() {
            let fill_opt = match &node.kind {
                SceneNodeKind::Path(p) => Some(&p.fill),
                SceneNodeKind::Text(t) => Some(&t.fill),
                SceneNodeKind::Group(_) => None,
                // raster nodes have no vector fill
                SceneNodeKind::Raster(_) => None,
            };
            if let Some(fill) = fill_opt {
                if fill.enabled {
                    if let FillKind::Solid(c) = &fill.kind {
                        let hex = c.to_hex();
                        if !fill_colors.iter().any(|existing| existing.to_hex() == hex) {
                            fill_colors.push(*c);
                        }
                    }
                }
            }
            if fill_colors.len() >= 16 {
                break;
            }
        }
        if !fill_colors.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new("Fill colors in document:").small());

            // Click a swatch to recolor every object using that exact color,
            // with a live preview while picking. The in-progress picker state
            // lives in egui temp memory.
            let edit_id = ui.make_persistent_id("recolor_swatch_edit");
            let mut edit = ui.data(|d| d.get_temp::<RecolorSwatchEdit>(edit_id));
            let mut just_opened = false;

            // A pending eyedropper request from the picker's "Pick" button.
            // The eyedropper is started one frame *after* the popup closes so any
            // live preview has already been reverted to the original color first;
            // an Esc during the eyedropper then leaves the document untouched.
            let pending_pick_id = ui.make_persistent_id("recolor_swatch_pending_pick");
            if let Some((ids, from)) =
                ui.data_mut(|d| d.remove_temp::<(Vec<NodeId>, [f32; 4])>(pending_pick_id))
            {
                action = Some(PanelAction::StartEyedropper(
                    EyedropperTarget::RecolorSwatch { ids, from },
                ));
            }

            ui.horizontal_wrapped(|ui| {
                for c in &fill_colors {
                    let rgba = [c.r, c.g, c.b, c.a];
                    let c32 = egui::Color32::from_rgb(
                        (c.r * 255.0).round() as u8,
                        (c.g * 255.0).round() as u8,
                        (c.b * 255.0).round() as u8,
                    );
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                    ui.painter().rect_filled(rect, 2.0, c32);
                    let is_editing = edit.as_ref().is_some_and(|e| e.original == rgba);
                    if resp.hovered() || is_editing {
                        ui.painter().rect_stroke(
                            rect,
                            2.0,
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(110, 86, 207)),
                        );
                    }
                    // Only start a new edit when none is active — finish the
                    // current one (Apply/Cancel/click-away) before switching.
                    if resp.clicked() && edit.is_none() {
                        let ids: Vec<NodeId> = doc
                            .nodes
                            .values()
                            .filter(|n| {
                                let solid = match &n.kind {
                                    SceneNodeKind::Path(p) if p.fill.enabled => {
                                        match &p.fill.kind {
                                            FillKind::Solid(fc) => Some(fc),
                                            _ => None,
                                        }
                                    }
                                    SceneNodeKind::Text(t) if t.fill.enabled => {
                                        match &t.fill.kind {
                                            FillKind::Solid(fc) => Some(fc),
                                            _ => None,
                                        }
                                    }
                                    _ => None,
                                };
                                solid.is_some_and(|fc| fc.to_hex() == c.to_hex())
                            })
                            .map(|n| n.id)
                            .collect();
                        edit = Some(RecolorSwatchEdit {
                            ids,
                            original: rgba,
                            applied: rgba,
                            current: rgba,
                        });
                        just_opened = true;
                    }
                    resp.on_hover_text(format!(
                        "{} — click to recolor every object using this color",
                        c.to_hex()
                    ));
                }
            });

            // Inline picker shown while a swatch is being edited.
            if let Some(e) = edit.clone() {
                let mut current = e.current;
                let mut apply = false;
                let mut cancel = false;
                let mut pick = false;
                let frame_resp = egui::Frame::popup(ui.style())
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Recolor all matching objects (live)")
                                .small()
                                .strong(),
                        );
                        // Shared picker body; alpha preserved (Opaque), rgb
                        // edited in place.
                        ColorPopup::picker_body_simple(ui, &mut current, false);
                        ui.horizontal(|ui| {
                            if ui.button("Apply").clicked() {
                                apply = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                            if ui
                                .button(format!("{} Pick", ph::EYEDROPPER))
                                .on_hover_text(
                                    "Sample a replacement color from the canvas \
                                     (Esc to cancel)",
                                )
                                .clicked()
                            {
                                pick = true;
                            }
                        });
                    })
                    .response;

                // Clicking outside the picker (e.g. another swatch, empty space)
                // keeps the change — same as Apply. Ignore the click that opened it.
                let click_away = !just_opened && frame_resp.clicked_elsewhere();

                if pick {
                    // Hand off to the canvas eyedropper. Revert any live preview
                    // to the original color now, then start the eyedropper next
                    // frame (via the pending marker) so an Esc leaves no change.
                    if e.applied != e.original {
                        action = Some(PanelAction::RecolorPreview {
                            ids: e.ids.clone(),
                            to: e.original,
                        });
                    }
                    ui.data_mut(|d| {
                        d.insert_temp::<(Vec<NodeId>, [f32; 4])>(
                            pending_pick_id,
                            (e.ids.clone(), e.original),
                        )
                    });
                    edit = None;
                } else if apply || click_away {
                    action = Some(PanelAction::RecolorCommit {
                        ids: e.ids.clone(),
                        from: e.original,
                        to: current,
                    });
                    edit = None;
                } else if cancel {
                    // Revert the live preview, no history entry.
                    if e.applied != e.original {
                        action = Some(PanelAction::RecolorPreview {
                            ids: e.ids.clone(),
                            to: e.original,
                        });
                    }
                    edit = None;
                } else {
                    // Live preview: push the new color whenever it changed.
                    if current != e.applied {
                        action = Some(PanelAction::RecolorPreview {
                            ids: e.ids.clone(),
                            to: current,
                        });
                    }
                    edit = Some(RecolorSwatchEdit {
                        ids: e.ids,
                        original: e.original,
                        applied: current,
                        current,
                    });
                }
            }

            // Persist or clear the picker state for next frame.
            match edit {
                Some(e) => ui.data_mut(|d| d.insert_temp(edit_id, e)),
                None => ui.data_mut(|d| d.remove::<RecolorSwatchEdit>(edit_id)),
            }
        }
        ui.add_space(4.0);
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_tool_shape_options(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let fill_color = &mut *ctx.fill_color;
    let q = ctx.q.as_str();
    let matches = |label: &str| -> bool { q.is_empty() || label.to_lowercase().contains(q) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Tool / shape options ──────────────────────────────────────────────────
    if matches("New Shape Fill") {
        egui::CollapsingHeader::new("New Shape Fill")
            .default_open(false)
            .open(forced_open)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // `fill_color` is gamma-sRGB `[f32; 4]` (maps 1:1 into
                    // `Color`), so route it through the shared sRGBA picker to
                    // avoid the linear-`Rgba` swatch shift (issue #185).
                    ColorPopup::swatch_f32(ui, fill_color);
                    if eyedropper_btn(ui) {
                        action = Some(PanelAction::StartEyedropper(EyedropperTarget::NewShapeFill));
                    }
                });
            });
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_symbol_overrides(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let selected_node = ctx.selected_node;
    let selected_id = ctx.selected_id;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let mut action: Option<PanelAction> = None;
    // ── Symbol Instance Overrides ────────────────────────────────────────────
    if let (Some(node), Some(nid)) = (selected_node, selected_id) {
        if node.symbol_ref.is_some() && matches("Symbol Override") {
            egui::CollapsingHeader::new("Symbol Override")
                .default_open(true)
                .id_salt("sym_override_panel")
                .show(ui, |ui| {
                    ui.label(RichText::new("Per-instance color overrides (Dynamic Symbol).").weak().small());
                    let fill_disp = node.symbol_fill_override.as_deref().unwrap_or("(master)");
                    let stroke_disp = node.symbol_stroke_override.as_deref().unwrap_or("(master)");
                    ui.label(RichText::new(format!("Fill: {}  Stroke: {}", fill_disp, stroke_disp)).small());
                    ui.separator();
                    thread_local! {
                        static FILL_HEX: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
                        static STROKE_HEX: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
                    }
                    ui.horizontal(|ui| {
                        ui.label("Fill:");
                        FILL_HEX.with(|s| {
                            let mut val = s.borrow().clone();
                            ui.add(egui::TextEdit::singleline(&mut val).hint_text("#rrggbb").desired_width(70.0));
                            *s.borrow_mut() = val;
                        });
                        ui.label("Stroke:");
                        STROKE_HEX.with(|s| {
                            let mut val = s.borrow().clone();
                            ui.add(egui::TextEdit::singleline(&mut val).hint_text("#rrggbb").desired_width(70.0));
                            *s.borrow_mut() = val;
                        });
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Apply Override")
                            .on_hover_text("Apply fill/stroke color overrides to this instance")
                            .clicked()
                        {
                            let fill_val = FILL_HEX.with(|s| s.borrow().clone());
                            let stroke_val = STROKE_HEX.with(|s| s.borrow().clone());
                            let fill_opt = if fill_val.trim().is_empty() { None } else { Some(fill_val.trim().to_string()) };
                            let stroke_opt = if stroke_val.trim().is_empty() { None } else { Some(stroke_val.trim().to_string()) };
                            if fill_opt.is_some() || stroke_opt.is_some() {
                                action = Some(PanelAction::SetSymbolOverride {
                                    node_id: nid,
                                    fill_hex: fill_opt,
                                    stroke_hex: stroke_opt,
                                });
                            }
                        }
                        if (node.symbol_fill_override.is_some() || node.symbol_stroke_override.is_some())
                            && ui.button("Clear Override")
                                .on_hover_text("Reset this instance to master fill/stroke")
                                .clicked()
                            {
                                action = Some(PanelAction::ClearSymbolOverrides { node_id: nid });
                            }
                    });
                });
            ui.add_space(2.0);
        }
    }

    if action.is_some() {
        ctx.action = action;
    }
}

pub(crate) fn draw_text_variable_binding(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let doc = ctx.doc;
    let selected_node = ctx.selected_node;
    let matches = |label: &str| -> bool { ctx.matches(label) };
    let forced_open = ctx.forced_open;
    let mut action: Option<PanelAction> = None;
    // ── Text node: Variable Binding (shown when a text node is selected) ──────
    if let Some(node) = selected_node {
        if let SceneNodeKind::Text(ref tn) = node.kind {
            let text_nid = node.id;
            if !doc.variables.is_empty() && matches("Variable Binding") {
                egui::CollapsingHeader::new("Variable Binding")
                    .default_open(true)
                    .open(forced_open)
                    .show(ui, |ui| {
                        if let Some(ref binding) = tn.variable_binding {
                            ui.label(RichText::new(format!("Bound to: {}", binding)).small());
                            if ui.small_button("Unbind").clicked() {
                                action =
                                    Some(PanelAction::UnbindTextVariable { node_id: text_nid });
                            }
                        } else {
                            ui.label(RichText::new("Bind to variable:").small().weak());
                            for var in &doc.variables {
                                if ui
                                    .small_button(&var.name)
                                    .on_hover_text(format!(
                                        "Bind this text node to '{}' (current value: {})",
                                        var.name, var.value
                                    ))
                                    .clicked()
                                {
                                    action = Some(PanelAction::BindTextVariable {
                                        node_id: text_nid,
                                        variable_name: var.name.clone(),
                                    });
                                }
                            }
                        }
                    });
                ui.add_space(2.0);
            }
        }
    }

    if action.is_some() {
        ctx.action = action;
    }
}
