use super::*;

/// Context-aware inspector shown while the Direct Selection tool is editing a
/// path. Displays only anchor/vertex properties for the current selection.
pub(crate) fn draw_vertex_panel(
    ui: &mut Ui,
    node: &SceneNode,
    node_id: NodeId,
    selected: &[usize],
    action: &mut Option<PanelAction>,
) {
    use kurbo::PathEl;

    ui.label(
        RichText::new("ANCHOR POINTS")
            .small()
            .color(crate::theme::section_header_color(ui)),
    );
    ui.add_space(2.0);

    let SceneNodeKind::Path(pn) = &node.kind else {
        ui.label(
            RichText::new("Select a path to edit its points.")
                .weak()
                .small(),
        );
        return;
    };
    let bez = pn.path_data.to_bez_path();

    // Local-space endpoint of every anchor, keyed by element index.
    let anchor_at = |idx: usize| -> Option<kurbo::Point> {
        bez.elements().get(idx).and_then(|el| match el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => Some(*p),
            PathEl::CurveTo(_, _, p) => Some(*p),
            PathEl::QuadTo(_, p) => Some(*p),
            PathEl::ClosePath => None,
        })
    };

    if selected.is_empty() {
        ui.label(
            RichText::new("Editing path — click an anchor to select it.")
                .weak()
                .small(),
        );
        ui.add_space(6.0);
        ui.label(RichText::new("Click body → select object").weak().small());
        ui.label(RichText::new("Click anchor → select point").weak().small());
        ui.label(
            RichText::new("Shift+click → add to selection")
                .weak()
                .small(),
        );
        ui.label(RichText::new("Drag handle → reshape curve").weak().small());
        ui.label(RichText::new("Drag ◌ widget → round corner").weak().small());
        ui.label(RichText::new("Esc → exit point edit").weak().small());
        return;
    }

    ui.label(
        RichText::new(if selected.len() == 1 {
            "1 anchor selected".to_string()
        } else {
            format!("{} anchors selected", selected.len())
        })
        .strong(),
    );
    ui.add_space(4.0);

    // Position — only meaningful for a single anchor.
    if selected.len() == 1 {
        if let Some(p) = anchor_at(selected[0]) {
            let (mut x, mut y) = (p.x, p.y);
            egui::Grid::new("vtx_pos_grid")
                .num_columns(4)
                .spacing([4.0, 2.0])
                .show(ui, |ui| {
                    ui.label("X:");
                    let xr = ui.add(egui::DragValue::new(&mut x).speed(0.5).fixed_decimals(1));
                    ui.label("Y:");
                    let yr = ui.add(egui::DragValue::new(&mut y).speed(0.5).fixed_decimals(1));
                    ui.end_row();
                    if xr.changed() || yr.changed() {
                        *action = Some(PanelAction::SetAnchorPosition {
                            node_id,
                            index: selected[0],
                            x,
                            y,
                        });
                    }
                });
        }
    }

    ui.add_space(4.0);

    // Point type — corner vs smooth, applied to the whole selection.
    ui.label(RichText::new("Point type").weak().small());
    ui.horizontal(|ui| {
        if ui
            .button("Corner")
            .on_hover_text("Retract this anchor's handles → sharp corner")
            .clicked()
        {
            *action = Some(PanelAction::ConvertAnchorType {
                node_id,
                indices: selected.to_vec(),
                smooth: false,
            });
        }
        if ui
            .button("Smooth / Curved")
            .on_hover_text(
                "Make this anchor's handles collinear → smooth curve (also on right-click)",
            )
            .clicked()
        {
            *action = Some(PanelAction::ConvertAnchorType {
                node_id,
                indices: selected.to_vec(),
                smooth: true,
            });
        }
    });

    ui.add_space(6.0);

    // Corner rounding — numeric counterpart to the canvas Live-Corners widget.
    ui.label(RichText::new("Round corners").weak().small());
    ui.label(
        RichText::new("(applies to straight corners)")
            .weak()
            .small()
            .italics(),
    );
    ui.horizontal(|ui| {
        for r in [4.0_f64, 8.0, 16.0, 32.0] {
            if ui
                .button(format!("{r:.0}"))
                .on_hover_text("Round selected corners by this radius")
                .clicked()
            {
                *action = Some(PanelAction::RoundSelectedCorners {
                    node_id,
                    indices: selected.to_vec(),
                    radius: r,
                });
            }
        }
    });

    ui.add_space(6.0);

    if ui
        .button(format!("{}  Remove anchor(s)", ph::TRASH))
        .on_hover_text("Delete the selected anchor points")
        .clicked()
    {
        *action = Some(PanelAction::DeleteAnchors {
            node_id,
            indices: selected.to_vec(),
        });
    }
}
