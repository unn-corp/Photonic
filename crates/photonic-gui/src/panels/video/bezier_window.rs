//! Static closed-window geometry editor. The caller commits the whole grade
//! through its existing lock-aware gesture/structural history path.
use super::param_expr;
use egui::{pos2, vec2, Rect, Sense, Stroke, Ui};
use photonic_core::timeline::{BezierWindow, MAX_BEZIER_KNOTS};

pub(super) fn draw(ui: &mut Ui, window: &mut BezierWindow) -> bool {
    draw_with_topology(ui, window, true)
}
pub(super) fn draw_with_topology(
    ui: &mut Ui,
    window: &mut BezierWindow,
    allow_topology: bool,
) -> bool {
    let mut discrete = false;
    ui.label("Closed Bézier window · even-odd fill");
    ui.label("Drag an anchor or its tangent handles. Coordinates use the frame; feather uses its shorter dimension.");
    let selected_id = ui.id().with("bezier_selected_knot");
    let mut selected = ui
        .data(|d| d.get_temp::<usize>(selected_id))
        .unwrap_or(0)
        .min(window.knots.len().saturating_sub(1));
    if window.knots.len() < 3 {
        ui.label("Invalid path: at least three knots required");
        return false;
    }
    let width = ui.available_width().clamp(120.0, 400.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 180.0), Sense::hover());
    #[cfg(test)]
    ui.ctx()
        .data_mut(|data| data.insert_temp(egui::Id::new("bezier_editor_test_rect"), rect));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
    let map = |p: [f32; 2]| {
        pos2(
            rect.left() + p[0] * rect.width(),
            rect.top() + p[1] * rect.height(),
        )
    };
    if let Ok(path) = window.flatten(rect.width() as u32, rect.height() as u32) {
        painter.add(egui::Shape::closed_line(
            path.into_iter()
                .map(|p| pos2(rect.left() + p[0], rect.top() + p[1]))
                .collect(),
            Stroke::new(1.5_f32, ui.visuals().selection.stroke.color),
        ));
    }
    for i in 0..window.knots.len() {
        let point = map(window.knots[i].anchor);
        let response = ui.interact(
            Rect::from_center_size(point, vec2(12.0, 12.0)),
            ui.id().with(("bezier_anchor", i)),
            Sense::click_and_drag(),
        );
        if response.clicked() || response.drag_started() {
            selected = i;
        }
        if response.dragged() {
            if let Some(p) = response.interact_pointer_pos() {
                let next = [
                    ((p.x - rect.left()) / rect.width()).clamp(-4.0, 4.0),
                    ((p.y - rect.top()) / rect.height()).clamp(-4.0, 4.0),
                ];
                let old = window.knots[i];
                let delta = [next[0] - old.anchor[0], next[1] - old.anchor[1]];
                let knot = &mut window.knots[i];
                knot.anchor = next;
                for axis in 0..2 {
                    knot.incoming[axis] = (old.incoming[axis] + delta[axis]).clamp(-4.0, 4.0);
                    knot.outgoing[axis] = (old.outgoing[axis] + delta[axis]).clamp(-4.0, 4.0);
                }
            }
        }
        painter.circle_filled(
            point,
            if i == selected { 4.5 } else { 3.0 },
            ui.visuals().selection.stroke.color,
        );
    }
    let anchor = window.knots[selected].anchor;
    for incoming in [true, false] {
        let slot = if incoming {
            &mut window.knots[selected].incoming
        } else {
            &mut window.knots[selected].outgoing
        };
        let point = map(*slot);
        painter.line_segment(
            [map(anchor), point],
            Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
        );
        let response = ui.interact(
            Rect::from_center_size(point, vec2(10.0, 10.0)),
            ui.id().with(("bezier_tangent", selected, incoming)),
            Sense::drag(),
        );
        if response.dragged() {
            if let Some(p) = response.interact_pointer_pos() {
                *slot = [
                    ((p.x - rect.left()) / rect.width()).clamp(-4.0, 4.0),
                    ((p.y - rect.top()) / rect.height()).clamp(-4.0, 4.0),
                ];
            }
        }
        painter.circle_stroke(point, 3.5, Stroke::new(1.0_f32, ui.visuals().text_color()));
    }
    ui.horizontal(|ui| {
        ui.label(format!("Knot {} / {}", selected + 1, window.knots.len()));
        if ui.small_button("Previous").clicked() {
            selected = (selected + window.knots.len() - 1) % window.knots.len();
        }
        if ui.small_button("Next").clicked() {
            selected = (selected + 1) % window.knots.len();
        }
    });
    {
        let (label, point) = ("Anchor", &mut window.knots[selected].anchor);
        ui.horizontal(|ui| {
            ui.label(label);
            for axis in 0..2 {
                param_expr::float_drag_f32(
                    ui,
                    &mut point[axis],
                    0.5,
                    Some((-4.0, 4.0)),
                    &Default::default(),
                    0.002,
                );
            }
        });
    }
    // Distinct rows make both tangents editable without a special pointer gesture.
    for incoming in [true, false] {
        let point = if incoming {
            &mut window.knots[selected].incoming
        } else {
            &mut window.knots[selected].outgoing
        };
        ui.horizontal(|ui| {
            ui.label(if incoming { "Incoming" } else { "Outgoing" });
            for value in point {
                param_expr::float_drag_f32(
                    ui,
                    value,
                    0.5,
                    Some((-4.0, 4.0)),
                    &Default::default(),
                    0.002,
                );
            }
        });
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                allow_topology && window.knots.len() < MAX_BEZIER_KNOTS,
                egui::Button::new("Split segment"),
            )
            .clicked()
        {
            discrete |= window.split_after(selected).is_ok();
        }
        if ui
            .add_enabled(
                allow_topology && window.knots.len() > 3,
                egui::Button::new("Remove knot"),
            )
            .clicked()
        {
            window.knots.remove(selected);
            selected = selected.min(window.knots.len() - 1);
            discrete = true;
        }
    });
    if !allow_topology {
        ui.label("Knot topology is fixed while animated. Make the window static to split or remove knots.");
    }
    param_expr::numeric_slider(ui, &mut window.feather, 0.0..=0.25, |v| v.text("Feather"));
    discrete |= ui.checkbox(&mut window.invert, "Invert key").changed();
    ui.data_mut(|d| d.insert_temp(selected_id, selected));
    discrete
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bezier_editor_view_does_not_modify_path() {
        let context = egui::Context::default();
        let mut window = BezierWindow::default();
        let original = window.clone();
        for _ in 0..3 {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 800.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| assert!(!draw(ui, &mut window)));
                },
            );
        }
        assert_eq!(window, original);
    }
}
