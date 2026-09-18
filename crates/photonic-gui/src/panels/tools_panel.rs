use super::*;

/// Draw the vertical tools panel. Returns the newly selected tool if changed.
///
/// Every registered [`Tool`] is surfaced here, organised into intent-based
/// sections. The large shapes family collapses into a single hover fly-out
/// (Illustrator-style) so it stays compact without hiding any variant.
pub fn draw_tools_panel(ui: &mut Ui, active: Tool, pinned_tools: &[Tool]) -> Option<Tool> {
    let mut chosen = None;

    // ── Hotbar (pinned tools) ─────────────────────────────────────────────
    if !pinned_tools.is_empty() {
        ui.label(
            RichText::new(format!("{} HOTBAR", egui_phosphor::regular::PUSH_PIN))
                .small()
                .color(Color32::from_rgb(110, 86, 207)),
        );
        ui.add_space(2.0);
        for tool in pinned_tools {
            tool_row(ui, active, *tool, &mut chosen);
        }
        ui.separator();
    }

    // ── Select & navigate ─────────────────────────────────────────────────
    section_header(ui, "SELECT");
    tool_row(ui, active, Tool::Select, &mut chosen);
    // Direct Select family: the anchor editor and its Proportional Move variant.
    tool_group(
        ui,
        active,
        "direct_select_popover",
        "Direct Select",
        &[Tool::DirectSelect, Tool::ProportionalMove],
        &mut chosen,
    );
    for tool in [Tool::Pan, Tool::MagicWand, Tool::Lasso] {
        tool_row(ui, active, tool, &mut chosen);
    }

    // ── Shapes (fly-out group covering every shape primitive) ─────────────
    section_header(ui, "SHAPES");
    tool_group(
        ui,
        active,
        "shapes_popover",
        "Shapes",
        &[
            Tool::Rectangle,
            Tool::RoundedRect,
            Tool::Ellipse,
            Tool::Polygon,
            Tool::Star,
            Tool::Spiral,
            Tool::Line,
            Tool::Arc,
            Tool::Grid,
            Tool::PolarGrid,
        ],
        &mut chosen,
    );

    // ── Draw ──────────────────────────────────────────────────────────────
    section_header(ui, "DRAW");
    for tool in [Tool::Pen, Tool::Pencil, Tool::ShapeBuilder, Tool::Text] {
        tool_row(ui, active, tool, &mut chosen);
    }

    // ── Path editing ──────────────────────────────────────────────────────
    section_header(ui, "EDIT PATH");
    for tool in [
        Tool::Scissors,
        Tool::Knife,
        Tool::Eraser,
        Tool::Smooth,
        Tool::Width,
    ] {
        tool_row(ui, active, tool, &mut chosen);
    }

    // ── Raster painting ───────────────────────────────────────────────────
    section_header(ui, "PAINT");
    for tool in [Tool::RasterBrush, Tool::RasterEraser] {
        tool_row(ui, active, tool, &mut chosen);
    }

    chosen
}

/// A small, muted section heading with consistent spacing above/below.
fn section_header(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        RichText::new(text)
            .small()
            .color(crate::theme::section_header_color(ui)),
    );
    ui.add_space(2.0);
}

/// One selectable tool row with its description as a hover tooltip. Sets
/// `chosen` when clicked.
fn tool_row(ui: &mut Ui, active: Tool, tool: Tool, chosen: &mut Option<Tool>) {
    let label = format!("{} {}", tool.icon(), tool.label());
    if ui
        .selectable_label(tool == active, label)
        .on_hover_text(tool.description())
        .clicked()
    {
        *chosen = Some(tool);
    }
}

/// A collapsed tool family: a single button that shows the active member (or a
/// generic group label when none is active) and opens a hover fly-out popover
/// listing every member. Keeps large families compact without hiding variants.
fn tool_group(
    ui: &mut Ui,
    active: Tool,
    id_salt: &str,
    group_label: &str,
    members: &[Tool],
    chosen: &mut Option<Tool>,
) {
    let popup_id = ui.make_persistent_id(id_salt);
    let active_member = members.iter().copied().find(|t| *t == active);
    let default = members.first().copied().unwrap_or(active);

    // Show the active member when one is selected, otherwise the group label.
    let (group_icon, text) = match active_member {
        Some(t) => (t.icon(), t.label()),
        None => (default.icon(), group_label),
    };

    // "›" signals that sub-tools are available behind the fly-out.
    let btn_text = format!("{} {}  ›", group_icon, text);
    let response = ui.selectable_label(active_member.is_some(), &btn_text);

    // Hover opens the fly-out.
    if response.hovered() {
        ui.memory_mut(|m| m.open_popup(popup_id));
    }

    // A direct click (no hover into the popover) activates the default member.
    if response.clicked() && active_member.is_none() {
        *chosen = Some(default);
    }

    if ui.memory(|m| m.is_popup_open(popup_id)) {
        let pos = egui::pos2(response.rect.right() + 4.0, response.rect.top());

        let area_resp = egui::Area::new(popup_id)
            .kind(egui::UiKind::Popup)
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_TOP)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .show(ui, |ui| {
                        ui.set_min_width(120.0);
                        let mut picked: Option<Tool> = None;
                        for &member in members {
                            let label = format!("{} {}", member.icon(), member.label());
                            if ui
                                .selectable_label(member == active, label)
                                .on_hover_text(member.description())
                                .clicked()
                            {
                                picked = Some(member);
                            }
                        }
                        picked
                    })
                    .inner
            });

        // Close when the pointer leaves both the button and the popover.
        let popup_rect = area_resp.response.rect;
        let pointer_in_popup = ui
            .ctx()
            .pointer_latest_pos()
            .map(|p| popup_rect.contains(p))
            .unwrap_or(false);

        if !response.hovered() && !pointer_in_popup {
            ui.memory_mut(|m| m.close_popup());
        }

        if let Some(tool) = area_resp.inner {
            *chosen = Some(tool);
            ui.memory_mut(|m| m.close_popup());
        }
    }
}
