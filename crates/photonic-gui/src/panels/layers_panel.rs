use super::*;
use crate::multi_button::{multi_button, MultiButtonItem};

/// Draw the left layers panel. Returns an optional action triggered by context menus.
/// Recursively render one node row in the Layers tree. Groups get a disclosure
/// header that expands to their children at any depth; every row is selectable
/// (emits `SelectNode`) and carries the z-order / collect context menu.
fn draw_layer_node_row(
    ui: &mut Ui,
    doc: &Document,
    node_id: NodeId,
    parent: photonic_core::document::NodeContainer,
    index_in_parent: usize,
    selected_id: Option<NodeId>,
    action: &mut Option<PanelAction>,
) {
    use photonic_core::document::NodeContainer;
    use photonic_core::node::SceneNodeKind as K;
    let Some(node) = doc.nodes.get(&node_id) else {
        return;
    };
    let is_selected = selected_id == Some(node_id);

    // Reserve a paint slot for the hover highlight (filled behind the row).
    let hover_bg = ui.painter().add(egui::Shape::Noop);
    let (response, name_resp) = match &node.kind {
        SceneNodeKind::Group(g) => {
            // Manual disclosure (persisted open flag) so we control the header
            // row layout — a grip + folder label — and still get an indented body.
            let open_id = egui::Id::new(("layer_group_open", node_id));
            let mut open = ui.data_mut(|d| d.get_temp::<bool>(open_id).unwrap_or(true));
            let clip = g.clip_node_id.is_some();
            let row = ui.horizontal(|ui| {
                // Caret — OUTSIDE the drag hitbox so clicking it toggles the
                // folder open/closed instead of starting a drag.
                let tri = if open {
                    ph::CARET_DOWN
                } else {
                    ph::CARET_RIGHT
                };
                if ui
                    .add(egui::Label::new(RichText::new(tri).weak()).sense(egui::Sense::click()))
                    .clicked()
                {
                    open = !open;
                }
                // The folder name is the drag handle (grab to reparent).
                let name_resp = ui
                    .dnd_drag_source(egui::Id::new(("node_drag", node_id)), node_id, |ui| {
                        let name = if clip {
                            format!("{} {} {}", ph::FOLDER_SIMPLE, node.name, ph::SCISSORS)
                        } else {
                            format!("{} {}", ph::FOLDER_SIMPLE, node.name)
                        };
                        let label = RichText::new(name).color(if is_selected {
                            Color32::from_rgb(184, 164, 255)
                        } else {
                            Color32::from_rgb(144, 119, 224)
                        });
                        let r = ui.selectable_label(is_selected, label);
                        if r.clicked() {
                            *action = Some(PanelAction::SelectNode { node_id });
                        }
                        r
                    })
                    .inner;
                // Right-aligned, frameless ⋯ object options.
                node_options_button(ui, node_id, action);
                name_resp
            });
            ui.data_mut(|d| d.insert_temp(open_id, open));
            let name_resp = row.inner;
            let row_resp = row.response;
            // Drop a node ONTO this group → reparent into it (at the top of the
            // folder). Outline the whole row while hovering so it reads as "drop
            // inside this folder" (distinct from the between-rows insertion line).
            if let Some(p) = row_resp.dnd_hover_payload::<NodeId>() {
                if *p != node_id {
                    ui.painter().rect_stroke(
                        row_resp.rect,
                        egui::Rounding::same(3.0),
                        egui::Stroke::new(2.0, Color32::from_rgb(140, 110, 245)),
                    );
                }
            }
            if let Some(p) = row_resp.dnd_release_payload::<NodeId>() {
                if *p != node_id {
                    *action = Some(PanelAction::ReparentNode {
                        node_id: *p,
                        new: NodeContainer::Group(node_id),
                        new_index: g.children.len(),
                    });
                }
            }
            // Indented children.
            if open {
                ui.indent(egui::Id::new(("layer_group_body", node_id)), |ui| {
                    if g.children.is_empty() {
                        ui.label(RichText::new("(empty group)").weak());
                    }
                    let n = g.children.len();
                    for (ui_i, child_id) in g.children.iter().rev().copied().enumerate() {
                        let cidx = n - 1 - ui_i;
                        draw_layer_node_row(
                            ui,
                            doc,
                            child_id,
                            NodeContainer::Group(node_id),
                            cidx,
                            selected_id,
                            action,
                        );
                    }
                });
            }
            (row_resp, Some(name_resp))
        }
        _ => {
            let compound = matches!(&node.kind, K::Path(p) if p.is_compound);
            let row = ui.horizontal(|ui| {
                // The name is the drag handle (grab to reparent).
                let src =
                    ui.dnd_drag_source(egui::Id::new(("node_drag", node_id)), node_id, |ui| {
                        let name = if compound {
                            format!("• {} {}", node.name, ph::INTERSECT)
                        } else {
                            format!("• {}", node.name)
                        };
                        ui.selectable_label(is_selected, name)
                    });
                if src.inner.clicked() {
                    *action = Some(PanelAction::SelectNode { node_id });
                }
                // Right-aligned, frameless ⋯ object options.
                node_options_button(ui, node_id, action);
                src.inner
            });
            let name_resp = row.inner;
            let row_resp = row.response;
            // Preview: while another object is dragged over this row, show an
            // insertion line above/below depending on the pointer half, so it is
            // clear where the object will land.
            if let Some(p) = row_resp.dnd_hover_payload::<NodeId>() {
                if *p != node_id {
                    let above = pointer_above_center(ui, row_resp.rect);
                    draw_drop_indicator(ui, row_resp.rect, above);
                }
            }
            // Drop onto a leaf → reparent into this leaf's container, landing on
            // the side of the row the pointer is nearest (above = higher slot).
            if let Some(p) = row_resp.dnd_release_payload::<NodeId>() {
                if *p != node_id {
                    let above = pointer_above_center(ui, row_resp.rect);
                    let new_index = if above {
                        index_in_parent + 1
                    } else {
                        index_in_parent
                    };
                    *action = Some(PanelAction::ReparentNode {
                        node_id: *p,
                        new: parent,
                        new_index,
                    });
                }
            }
            (row_resp, Some(name_resp))
        }
    };

    // Subtle animated hover highlight for the row.
    paint_row_hover(
        ui,
        hover_bg,
        response.rect,
        egui::Id::new(("node_hover", node_id)),
        response.contains_pointer(),
    );

    // Right-click anywhere on the row → the same options menu as the ⋯ button.
    if let Some(nr) = &name_resp {
        nr.context_menu(|ui| node_menu_items(ui, node_id, action));
    }
    response
        .interact(egui::Sense::click())
        .context_menu(|ui| node_menu_items(ui, node_id, action));
}

/// True when the pointer is in the upper half of `rect` (or off-screen). Used to
/// decide whether a drop lands above or below a row — the same test drives the
/// insertion-line preview and the actual landing index so they always agree.
fn pointer_above_center(ui: &Ui, rect: egui::Rect) -> bool {
    ui.input(|i| i.pointer.hover_pos())
        .is_none_or(|p| p.y < rect.center().y)
}

/// Paint a subtle, animated hover highlight behind a row. `bg` is a paint slot
/// reserved with `painter().add(Shape::Noop)` *before* the row was laid out, so
/// filling it now lands the tint behind the row's content rather than over it.
fn paint_row_hover(
    ui: &Ui,
    bg: egui::layers::ShapeIdx,
    rect: egui::Rect,
    anim_id: egui::Id,
    hovered: bool,
) {
    let t = ui.ctx().animate_bool_with_time(anim_id, hovered, 0.12);
    if t > 0.001 {
        let a = (26.0 * t).round() as u8;
        ui.painter().set(
            bg,
            egui::Shape::rect_filled(
                rect,
                egui::Rounding::same(4.0),
                Color32::from_rgba_unmultiplied(120, 96, 220, a),
            ),
        );
    }
}

/// Paint a prominent insertion indicator — a bar with round end caps — at the
/// top (`above`) or bottom edge of `rect`, marking where a dragged row lands.
fn draw_drop_indicator(ui: &Ui, rect: egui::Rect, above: bool) {
    let y = if above { rect.top() } else { rect.bottom() };
    let color = Color32::from_rgb(150, 120, 255);
    let painter = ui.painter();
    painter.hline(rect.x_range(), y, egui::Stroke::new(2.5, color));
    painter.circle_filled(egui::pos2(rect.left() + 1.0, y), 3.0, color);
    painter.circle_filled(egui::pos2(rect.right() - 1.0, y), 3.0, color);
}

/// The shared body of the object/node options menu: z-order ops and "Collect in
/// New Layer". Used by both the row's right-click menu and its ⋯ options button.
fn node_menu_items(ui: &mut Ui, node_id: NodeId, action: &mut Option<PanelAction>) {
    if ui
        .button(format!("{} Options…", ph::SLIDERS_HORIZONTAL))
        .on_hover_text("Name, blend mode, opacity, visibility, lock — scoped to this object's type")
        .clicked()
    {
        *action = Some(PanelAction::OpenObjectOptions { node_id });
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Bring to Front").clicked() {
        *action = Some(PanelAction::ReorderNode {
            node_id,
            op: ZOrderOp::BringToFront,
        });
        ui.close_menu();
    }
    if ui.button("Bring Forward").clicked() {
        *action = Some(PanelAction::ReorderNode {
            node_id,
            op: ZOrderOp::BringForward,
        });
        ui.close_menu();
    }
    if ui.button("Send Backward").clicked() {
        *action = Some(PanelAction::ReorderNode {
            node_id,
            op: ZOrderOp::SendBackward,
        });
        ui.close_menu();
    }
    if ui.button("Send to Back").clicked() {
        *action = Some(PanelAction::ReorderNode {
            node_id,
            op: ZOrderOp::SendToBack,
        });
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Collect in New Layer").clicked() {
        *action = Some(PanelAction::CollectInNewLayer {
            node_ids: vec![node_id],
        });
        ui.close_menu();
    }
}

/// The right-aligned, frameless "⋯" options button for an object/node row. Opens
/// a popup of [`node_menu_items`], with outer right padding so it never clips.
fn node_options_button(ui: &mut Ui, node_id: NodeId, action: &mut Option<PanelAction>) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(4.0);
        let resp = ui
            .add(egui::Button::new(RichText::new(ph::DOTS_THREE_VERTICAL).weak()).frame(false))
            .on_hover_text("Object options");
        let popup_id = resp.id.with("node_opts_popup");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &resp,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(160.0);
                node_menu_items(ui, node_id, action);
            },
        );
    });
}

pub fn draw_layers_panel(
    ui: &mut Ui,
    doc: &Document,
    selected_layer_ids: &mut Vec<LayerId>,
    selected_id: Option<NodeId>,
) -> Option<PanelAction> {
    let mut action: Option<PanelAction> = None;

    // During the drawer's slide animation the panel is momentarily ~1px wide;
    // the nested top/bottom panels below dislike degenerate widths, so skip
    // layout entirely until there is room (content is ~invisible then anyway).
    if ui.available_width() < 40.0 {
        return None;
    }

    // Prune any stale selected_layer_ids (layers that no longer exist).
    selected_layer_ids.retain(|id| doc.layers.contains_key(id));

    // ── Pinned footer: slide-up adjustment tray + action bar + object count ──
    // Rendered as a bottom panel so it always sits at the base of the drawer,
    // independent of how far the layer list is scrolled.
    egui::TopBottomPanel::bottom("layers_footer")
        .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(2.0, 4.0)))
        .show_inside(ui, |ui| {
            draw_layers_footer(ui, doc, &mut action);
        });

    // ── Scrolling layer tree fills the remaining space above the footer ──────
    // A little left inset so the color-tag swatch isn't clipped by the panel
    // edge when it enlarges on hover.
    egui::CentralPanel::default()
        .frame(egui::Frame::none().inner_margin(egui::Margin {
            left: 4.0,
            ..egui::Margin::ZERO
        }))
        .show_inside(ui, |ui| {
            ui.label(
                RichText::new("LAYERS")
                    .small()
                    .color(crate::theme::section_header_color(ui)),
            );
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .id_salt("layers_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    draw_layers_tree(ui, doc, selected_layer_ids, selected_id, &mut action);
                });
        });

    action
}

/// Render the scrolling layer stack (top→bottom) with per-layer right-click
/// menus, inline rename, drag-reorder, and the contextual Merge/Flatten rows.
fn draw_layers_tree(
    ui: &mut Ui,
    doc: &Document,
    selected_layer_ids: &mut Vec<LayerId>,
    selected_id: Option<NodeId>,
    action: &mut Option<PanelAction>,
) {
    // Which layer (if any) is currently being renamed inline, and its buffer.
    let rename_target: Option<LayerId> = ui.data(|d| d.get_temp(rename_target_id()));

    // Layers from top to bottom in UI (reversed from draw order)
    for layer_id in doc.layer_order.iter().rev() {
        let Some(layer) = doc.layers.get(layer_id) else {
            continue;
        };
        let lid = *layer_id;

        let is_layer_selected = selected_layer_ids.contains(&lid);
        // Persisted disclosure state for this layer's object list.
        let open_id = egui::Id::new(("layer_open", lid));
        let mut open = ui.data_mut(|d| d.get_temp::<bool>(open_id).unwrap_or(true));

        // ── Header row: [swatch] [caret] [name] … [options ⋯] ────────────────
        // The name/row body is itself the drag handle — grab it to reorder the
        // stack (#169), no separate grip. Shift-click the name toggles a layer
        // into the multi-selection; a plain click selects just that layer.
        // Reserve a paint slot for the hover highlight (filled behind the row).
        let hover_bg = ui.painter().add(egui::Shape::Noop);
        let row = ui.horizontal(|ui| {
            // Color swatch — kept OUTSIDE the drag hitbox so a click cycles the
            // color tag instead of starting a layer drag.
            let swatch_color = match layer.color {
                Some([r, g, b, a]) => Color32::from_rgba_unmultiplied(
                    (r * 255.0) as u8,
                    (g * 255.0) as u8,
                    (b * 255.0) as u8,
                    (a * 255.0) as u8,
                ),
                None => Color32::from_gray(60),
            };
            // Cycle: None → Red → Orange → Yellow → Green → Blue → Purple → None
            const LAYER_COLORS: &[Option<[f32; 4]>] = &[
                None,
                Some([0.85, 0.20, 0.20, 1.0]),
                Some([0.90, 0.55, 0.10, 1.0]),
                Some([0.85, 0.80, 0.10, 1.0]),
                Some([0.20, 0.70, 0.25, 1.0]),
                Some([0.15, 0.45, 0.85, 1.0]),
                Some([0.60, 0.20, 0.80, 1.0]),
            ];
            let swatch_btn = egui::Button::new("")
                .fill(swatch_color)
                .min_size(egui::vec2(10.0, 10.0));
            if ui
                .add(swatch_btn)
                .on_hover_text("Click to cycle layer color tag")
                .clicked()
            {
                let cur_idx = LAYER_COLORS
                    .iter()
                    .position(|c| *c == layer.color)
                    .unwrap_or(0);
                let next_color = LAYER_COLORS[(cur_idx + 1) % LAYER_COLORS.len()];
                *action = Some(PanelAction::SetLayerColor {
                    layer_id: lid,
                    color: next_color,
                });
            }

            // Disclosure caret — also OUTSIDE the drag hitbox so clicking it
            // expands/collapses the layer's contents instead of dragging.
            let tri = if open {
                ph::CARET_DOWN
            } else {
                ph::CARET_RIGHT
            };
            if ui
                .add(egui::Label::new(RichText::new(tri).weak()).sense(egui::Sense::click()))
                .clicked()
            {
                open = !open;
            }

            // Name slot — this is the drag handle: grab it to reorder the stack.
            // Inline rename takes it over while renaming; otherwise it is a
            // selectable label whose click drives selection (shift = toggle).
            let name_resp = ui
                .dnd_drag_source(egui::Id::new(("layer_grip", lid)), lid, |ui| {
                    if rename_target == Some(lid) {
                        let mut buf: String =
                            ui.data(|d| d.get_temp(rename_buf_id()).unwrap_or_default());
                        let edit_id = egui::Id::new(("layer_rename_edit", lid));
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut buf)
                                .id(edit_id)
                                .desired_width(140.0),
                        );
                        if resp.lost_focus() {
                            // Commit on Enter (and on click-away); Escape cancels.
                            let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
                            let trimmed = buf.trim();
                            if !escaped && !trimmed.is_empty() && trimmed != layer.name {
                                *action = Some(PanelAction::RenameLayer {
                                    layer_id: lid,
                                    name: trimmed.to_string(),
                                });
                            }
                            ui.data_mut(|d| {
                                d.remove::<LayerId>(rename_target_id());
                                d.remove::<String>(rename_buf_id());
                            });
                        } else {
                            ui.data_mut(|d| d.insert_temp(rename_buf_id(), buf));
                        }
                        None
                    } else {
                        let layer_label = if layer.is_template {
                            RichText::new(format!("{} [T]", layer.name))
                                .italics()
                                .weak()
                        } else if layer.visible {
                            RichText::new(layer.name.to_string())
                        } else {
                            RichText::new(format!("{} (hidden)", layer.name)).weak()
                        };
                        let r = ui.selectable_label(is_layer_selected, layer_label);
                        if r.clicked() {
                            if ui.input(|i| i.modifiers.shift) {
                                // Shift-click: toggle this layer in the selection.
                                if let Some(pos) = selected_layer_ids.iter().position(|x| x == &lid)
                                {
                                    selected_layer_ids.remove(pos);
                                } else {
                                    selected_layer_ids.push(lid);
                                }
                            } else {
                                // Plain click: select just this layer.
                                selected_layer_ids.clear();
                                selected_layer_ids.push(lid);
                            }
                        }
                        Some(r)
                    }
                })
                .inner;

            // Right-aligned per-layer options button (frameless ⋯) — its popup
            // carries the template toggle and the same actions as right-click.
            layer_options_button(ui, doc, lid, layer, action);
            name_resp
        });
        ui.data_mut(|d| d.insert_temp(open_id, open));

        // Subtle animated hover highlight for the row.
        paint_row_hover(
            ui,
            hover_bg,
            row.response.rect,
            egui::Id::new(("layer_hover", lid)),
            row.response.contains_pointer(),
        );

        // Right-click anywhere on the row → the same options menu as the ⋯ button.
        // Attach to the name (covers the label) and to a click-sensing pass over
        // the whole row rect (covers the gaps the interactive children don't).
        if let Some(nr) = &row.inner {
            nr.context_menu(|ui| layer_menu_items(ui, doc, lid, layer, action));
        }
        row.response
            .interact(egui::Sense::click())
            .context_menu(|ui| layer_menu_items(ui, doc, lid, layer, action));

        // Indented object list for this layer.
        if open {
            ui.indent(egui::Id::new(("layer_body", lid)), |ui| {
                use photonic_core::document::NodeContainer;
                if layer.node_ids.is_empty() {
                    ui.label(RichText::new("  (empty)").weak());
                }
                let n = layer.node_ids.len();
                for (ui_i, node_id) in layer.node_ids.iter().rev().copied().enumerate() {
                    let idx = n - 1 - ui_i;
                    draw_layer_node_row(
                        ui,
                        doc,
                        node_id,
                        NodeContainer::Layer(lid),
                        idx,
                        selected_id,
                        action,
                    );
                }
            });
        }

        // ── Drag-to-reorder drop handling (#169) ─────────────────────────────
        // Show an insertion line while a layer is dragged over this row, and on
        // release rebuild the stack order and emit a single undoable reorder.
        if let Some(p) = row.response.dnd_hover_payload::<LayerId>() {
            if *p != lid {
                let above = pointer_above_center(ui, row.response.rect);
                draw_drop_indicator(ui, row.response.rect, above);
            }
        }
        if let Some(payload) = row.response.dnd_release_payload::<LayerId>() {
            let dragged = *payload;
            if dragged != lid {
                let before = pointer_above_center(ui, row.response.rect);
                // Work in UI order (top→bottom = layer_order reversed).
                let mut ui_order: Vec<LayerId> = doc.layer_order.iter().rev().copied().collect();
                ui_order.retain(|&x| x != dragged);
                if let Some(idx) = ui_order.iter().position(|&x| x == lid) {
                    let at = (if before { idx } else { idx + 1 }).min(ui_order.len());
                    ui_order.insert(at, dragged);
                    let new_order: Vec<LayerId> = ui_order.into_iter().rev().collect();
                    *action = Some(PanelAction::ReorderLayers { new_order });
                }
            }
        }
        // Preview: dragging an object onto a layer header lands it at the top of
        // that layer — outline the header so that destination reads clearly.
        if row.response.dnd_hover_payload::<NodeId>().is_some() {
            ui.painter().rect_stroke(
                row.response.rect,
                egui::Rounding::same(3.0),
                egui::Stroke::new(2.0, Color32::from_rgb(140, 110, 245)),
            );
        }
        // Drop a NODE onto this layer row → reparent it to this layer's top (#210),
        // i.e. pull it out of a group / move it between layers.
        if let Some(payload) = row.response.dnd_release_payload::<NodeId>() {
            *action = Some(PanelAction::ReparentNode {
                node_id: *payload,
                new: photonic_core::document::NodeContainer::Layer(lid),
                new_index: doc.layers.get(&lid).map_or(0, |l| l.node_ids.len()),
            });
        }
    }

    // Show "Merge Selected" when 2+ layers are checked.
    if selected_layer_ids.len() >= 2 {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button(format!("Merge {} Layers", selected_layer_ids.len()))
                .on_hover_text(
                    "Merge selected layers into one (bottom-most in stack order is kept)",
                )
                .clicked()
            {
                *action = Some(PanelAction::MergeLayers {
                    layer_ids: selected_layer_ids.clone(),
                });
                selected_layer_ids.clear();
            }
        });
    }

    // Flatten Artwork + Reverse Order (shown when > 1 layer)
    if doc.layer_order.len() > 1 {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button("Flatten Artwork")
                .on_hover_text("Merge all layers into one; bottom-most layer is kept")
                .clicked()
            {
                *action = Some(PanelAction::FlattenArtwork);
            }
            if ui
                .button(format!("{} Reverse Order", ph::ARROWS_DOWN_UP))
                .on_hover_text("Reverse the stacking order of all layers")
                .clicked()
            {
                *action = Some(PanelAction::ReorderLayers {
                    new_order: doc.layer_order.iter().rev().copied().collect(),
                });
            }
        });
    }
}

// ── Footer / context-menu / adjustment-tray helpers ─────────────────────────

/// egui memory id: which layer is being renamed inline (holds a `LayerId`).
fn rename_target_id() -> egui::Id {
    egui::Id::new("layers_rename_target")
}
/// egui memory id: the working text buffer for the inline rename (a `String`).
fn rename_buf_id() -> egui::Id {
    egui::Id::new("layers_rename_buf")
}
/// egui memory id: whether the adjustment-layer slide-up tray is open (a `bool`).
fn adjust_tray_open_id() -> egui::Id {
    egui::Id::new("layers_adjust_tray_open")
}

/// The right-aligned, frameless "⋯" options button for a layer row. Opens a
/// popup of the layer actions ([`layer_menu_items`]). A little outer right
/// padding keeps the icon from clipping the panel edge.
fn layer_options_button(
    ui: &mut Ui,
    doc: &Document,
    lid: LayerId,
    layer: &photonic_core::layer::Layer,
    action: &mut Option<PanelAction>,
) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(4.0);
        let resp = ui
            .add(egui::Button::new(RichText::new(ph::DOTS_THREE_VERTICAL).weak()).frame(false))
            .on_hover_text("Layer options");
        let popup_id = resp.id.with("layer_opts_popup");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &resp,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(160.0);
                layer_menu_items(ui, doc, lid, layer, action);
            },
        );
    });
}

/// The shared body of the layer options menu: template toggle, rename, add
/// sublayer, show/hide, lock, and delete. Emits the matching [`PanelAction`].
fn layer_menu_items(
    ui: &mut Ui,
    doc: &Document,
    lid: LayerId,
    layer: &photonic_core::layer::Layer,
    action: &mut Option<PanelAction>,
) {
    {
        // Layer Options… — the full modal (name, blend, opacity, colour, template…).
        if ui
            .button(format!("{} Layer Options…", ph::SLIDERS_HORIZONTAL))
            .on_hover_text("Blend mode, opacity, name, colour, template — all in one dialog")
            .clicked()
        {
            *action = Some(PanelAction::OpenLayerOptions { layer_id: lid });
            ui.close_menu();
        }
        if ui
            .button(format!("{} Duplicate Layer", ph::COPY))
            .on_hover_text("Copy this layer and all its objects into a new layer above")
            .clicked()
        {
            *action = Some(PanelAction::DuplicateLayer { layer_id: lid });
            ui.close_menu();
        }
        ui.separator();
        // Template toggle — relocated here from the old inline "T" button. A
        // template layer is locked and dimmed as a tracing reference.
        let t_label = if layer.is_template {
            format!("{} Template Layer", ph::CHECK)
        } else {
            "     Make Template".to_string()
        };
        if ui
            .button(t_label)
            .on_hover_text(if layer.is_template {
                "Template layer (locked, dimmed) — click to disable"
            } else {
                "Make this a template layer (locked, dimmed reference)"
            })
            .clicked()
        {
            *action = Some(PanelAction::SetLayerTemplate {
                layer_id: lid,
                is_template: !layer.is_template,
            });
            ui.close_menu();
        }
        ui.separator();
        if ui.button(format!("{} Rename", ph::PENCIL_SIMPLE)).clicked() {
            // Arm inline rename and focus the edit box on the next frame.
            ui.data_mut(|d| {
                d.insert_temp(rename_target_id(), lid);
                d.insert_temp(rename_buf_id(), layer.name.clone());
            });
            ui.ctx()
                .memory_mut(|m| m.request_focus(egui::Id::new(("layer_rename_edit", lid))));
            ui.close_menu();
        }
        if ui
            .button(format!("{} Add Sublayer", ph::FOLDER_SIMPLE_PLUS))
            .clicked()
        {
            *action = Some(PanelAction::AddSublayer);
            ui.close_menu();
        }
        ui.separator();
        let (vis_icon, vis_label) = if layer.visible {
            (ph::EYE_SLASH, "Hide Layer")
        } else {
            (ph::EYE, "Show Layer")
        };
        if ui.button(format!("{} {}", vis_icon, vis_label)).clicked() {
            *action = Some(PanelAction::SetLayerVisible {
                layer_id: lid,
                visible: !layer.visible,
            });
            ui.close_menu();
        }
        let (lock_icon, lock_label) = if layer.locked {
            (ph::LOCK_SIMPLE_OPEN, "Unlock Layer")
        } else {
            (ph::LOCK_SIMPLE, "Lock Layer")
        };
        if ui.button(format!("{} {}", lock_icon, lock_label)).clicked() {
            *action = Some(PanelAction::SetLayerLocked {
                layer_id: lid,
                locked: !layer.locked,
            });
            ui.close_menu();
        }
        ui.separator();
        // Delete — refuse when this is the only remaining layer.
        let can_delete = doc.layer_order.len() > 1;
        if ui
            .add_enabled(
                can_delete,
                egui::Button::new(format!("{} Delete Layer", ph::TRASH)),
            )
            .clicked()
        {
            *action = Some(PanelAction::DeleteLayer { layer_id: lid });
            ui.close_menu();
        }
    }
}

/// The pinned footer: the adjustment slide-up tray, the four layer-action
/// buttons, and the object-count readout.
fn draw_layers_footer(ui: &mut Ui, doc: &Document, action: &mut Option<PanelAction>) {
    // Slide-up tray drawn first so it appears *above* the buttons, rising up.
    let open = ui.data(|d| d.get_temp::<bool>(adjust_tray_open_id()).unwrap_or(false));
    let t = ui
        .ctx()
        .animate_bool_with_time(egui::Id::new("layers_adjust_tray_anim"), open, 0.18);
    if t > 0.001 {
        draw_adjustment_tray(ui, t, action);
    }

    // Action bar — the four layer actions as a segmented multi-button pill:
    // icon-only at rest, each expands its label on hover.
    let items = [
        MultiButtonItem::new(
            ph::STACK_PLUS,
            "New Layer",
            "Add a new empty layer at the top of the stack",
        ),
        MultiButtonItem::new(
            ph::FOLDER_SIMPLE_PLUS,
            "Sublayer",
            "Add a nested group container to the active layer",
        ),
        MultiButtonItem::new(
            ph::SELECTION,
            "Mask",
            "Add a mask for the current selection:\n• a raster node → editable alpha mask\n• 2+ objects → clipping mask (top object clips)",
        ),
        MultiButtonItem::new(
            ph::SUN,
            "Adjust",
            "Add a non-destructive adjustment layer",
        ),
    ];
    // Even padding above/below, and centered horizontally in the drawer.
    ui.add_space(6.0);
    let clicked = ui
        .vertical_centered(|ui| multi_button(ui, "layers_action_bar", &items))
        .inner;
    ui.add_space(6.0);
    if let Some(idx) = clicked {
        match idx {
            0 => *action = Some(PanelAction::AddLayer),
            1 => *action = Some(PanelAction::AddSublayer),
            2 => *action = Some(PanelAction::AddLayerMaskSmart),
            // Adjust → toggle the slide-up tray of adjustment presets.
            _ => ui.data_mut(|d| {
                let cur = d.get_temp::<bool>(adjust_tray_open_id()).unwrap_or(false);
                d.insert_temp(adjust_tray_open_id(), !cur);
            }),
        }
    }

    ui.separator();
    ui.label(
        RichText::new(format!("{} objects", doc.node_count()))
            .weak()
            .small(),
    );
}

/// The adjustment-layer tray: a wrapping grid of preview-square tiles whose
/// revealed height animates with `t` (0→1) for a slide-up effect. Selecting a
/// tile emits [`PanelAction::AddAdjustmentLayer`] and closes the tray.
fn draw_adjustment_tray(ui: &mut Ui, t: f32, action: &mut Option<PanelAction>) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(4.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new("ADJUSTMENT LAYER")
                    .small()
                    .color(Color32::from_rgb(110, 86, 207)),
            );
            egui::ScrollArea::vertical()
                .id_salt("layers_adjust_tray_scroll")
                .max_height(168.0 * t)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (kind, label) in crate::app::layer_ops::ADJUSTMENT_TILES {
                            if adjustment_tile(ui, kind, label) {
                                *action = Some(PanelAction::AddAdjustmentLayer {
                                    kind: (*kind).to_string(),
                                });
                                ui.data_mut(|d| d.insert_temp(adjust_tray_open_id(), false));
                            }
                        }
                    });
                });
        });
}

/// One adjustment tile: a painted preview square with a caption. Returns true
/// when clicked.
fn adjustment_tile(ui: &mut Ui, kind: &str, label: &str) -> bool {
    let sq = 44.0;
    let resp = ui
        .allocate_ui_with_layout(
            egui::vec2(58.0, sq + 18.0),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(sq, sq), egui::Sense::click());
                paint_adjustment_preview(ui.painter(), rect, kind);
                let border = if resp.hovered() {
                    egui::Stroke::new(2.0, Color32::from_rgb(110, 86, 207))
                } else {
                    egui::Stroke::new(1.0, Color32::from_gray(70))
                };
                ui.painter()
                    .rect_stroke(rect, egui::Rounding::same(4.0), border);
                ui.add(egui::Label::new(RichText::new(label).small()).truncate());
                resp
            },
        )
        .inner;
    resp.on_hover_text(format!("Add {label} adjustment layer"))
        .clicked()
}

/// Paint a small representative preview of an adjustment as a horizontal ramp of
/// colour strips (plus a diagonal guide for curve/level-type adjustments).
fn paint_adjustment_preview(painter: &egui::Painter, rect: egui::Rect, kind: &str) {
    let n = 14;
    for i in 0..n {
        let f = i as f32 / (n - 1) as f32;
        let x0 = rect.left() + rect.width() * (i as f32 / n as f32);
        let x1 = rect.left() + rect.width() * ((i + 1) as f32 / n as f32);
        let strip =
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom()));
        painter.rect_filled(strip, egui::Rounding::ZERO, preview_color(kind, f));
    }
    if matches!(kind, "curves" | "levels") {
        painter.line_segment(
            [
                egui::pos2(rect.left(), rect.bottom()),
                egui::pos2(rect.right(), rect.top()),
            ],
            egui::Stroke::new(1.5, Color32::from_white_alpha(200)),
        );
    }
}

/// Sample colour at position `f` (0..1) for an adjustment's preview ramp.
fn preview_color(kind: &str, f: f32) -> Color32 {
    let gray = |v: f32| {
        let c = (v.clamp(0.0, 1.0) * 255.0) as u8;
        Color32::from_rgb(c, c, c)
    };
    match kind {
        "hue_saturation" => Color32::from(egui::ecolor::Hsva::new(f, 1.0, 1.0, 1.0)),
        "vibrance" => Color32::from(egui::ecolor::Hsva::new(f, 0.55, 0.95, 1.0)),
        "invert" => gray(1.0 - f),
        "posterize" => gray((f * 4.0).floor() / 3.0),
        "threshold" => {
            if f < 0.5 {
                Color32::BLACK
            } else {
                Color32::WHITE
            }
        }
        "exposure" => gray((f + 0.15).min(1.0)),
        // brightness_contrast, levels, curves, black_and_white → grayscale ramp.
        _ => gray(f),
    }
}
