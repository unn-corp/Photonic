//! Keyframe controls for fixed-topology window shapes. Geometry edits still use
//! the caller's one whole-grade gesture/history transaction.
use egui::Ui;
use photonic_core::timeline::{
    BezierWindow, BezierWindowInterpolation, BezierWindowTrack, Tick, MAX_BEZIER_KEYS,
};

pub(super) fn draw(
    ui: &mut Ui,
    track: &mut BezierWindowTrack,
    clock: Option<Tick>,
) -> (bool, Option<BezierWindow>) {
    if let Err(error) = track.validate() {
        ui.colored_label(ui.visuals().warn_fg_color, error);
        return (false, None);
    }
    let selected_id = ui.id().with("bezier_selected_key");
    let mut selected = ui
        .data(|d| d.get_temp::<Tick>(selected_id))
        .filter(|at| track.keys.iter().any(|k| k.at == *at))
        .unwrap_or(track.keys[0].at);
    let mut discrete = false;
    let mut freeze = None;
    ui.label("Animated Bézier · fixed knot topology");
    if let Some(at) = clock {
        ui.label(format!("Stage playhead: {:.3} s", at.as_seconds_f64()));
    } else {
        ui.label("Move the playhead onto this grade's scope to add keys or make it static.");
    }
    ui.horizontal(|ui| {
        let add = ui.add_enabled(
            clock.is_some()
                && (track.keys.len() < MAX_BEZIER_KEYS
                    || clock.is_some_and(|at| track.keys.iter().any(|k| k.at == at))),
            egui::Button::new("Add / select key here"),
        );
        #[cfg(test)]
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::new("bezier_add_key_test_rect"), add.rect));
        if add.clicked() {
            if let Some(at) = clock {
                match track.insert_sampled(at) {
                    Ok(changed) => {
                        discrete |= changed;
                        selected = at;
                    }
                    Err(error) => {
                        ui.colored_label(ui.visuals().warn_fg_color, error);
                    }
                }
            }
        }
        if ui
            .add_enabled(clock.is_some(), egui::Button::new("Make static here"))
            .clicked()
        {
            if let Some(at) = clock {
                match track.sample(at) {
                    Ok(window) => freeze = Some(window),
                    Err(error) => {
                        ui.colored_label(ui.visuals().warn_fg_color, error);
                    }
                }
            }
        }
    });
    egui::ComboBox::from_id_salt("bezier_key_selector")
        .selected_text(format!("Key at {:.3} s", selected.as_seconds_f64()))
        .show_ui(ui, |ui| {
            for key in &track.keys {
                ui.selectable_value(
                    &mut selected,
                    key.at,
                    format!("{:.3} s · {} ticks", key.at.as_seconds_f64(), key.at.0),
                );
            }
        });
    if ui
        .add_enabled(
            track.keys.len() > 1,
            egui::Button::new("Remove selected key"),
        )
        .clicked()
    {
        track.keys.retain(|k| k.at != selected);
        selected = track.keys[0].at;
        discrete = true;
    }
    let key = track
        .keys
        .iter_mut()
        .find(|k| k.at == selected)
        .expect("selected key exists");
    ui.horizontal(|ui| {
        ui.label("To next key");
        for (value, label) in [
            (BezierWindowInterpolation::Linear, "Linear"),
            (BezierWindowInterpolation::Hold, "Hold"),
        ] {
            discrete |= ui
                .selectable_value(&mut key.interpolation, value, label)
                .changed();
        }
    });
    ui.label(format!(
        "Editing key at {:.3} s. Invert is held until the next key.",
        selected.as_seconds_f64()
    ));
    discrete |= super::bezier_window::draw_with_topology(ui, &mut key.window, false);
    ui.data_mut(|d| d.insert_temp(selected_id, selected));
    (discrete, freeze)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn animated_bezier_view_without_clock_never_materializes_keys() {
        let ctx = egui::Context::default();
        let mut track = BezierWindowTrack::new(Tick(0), BezierWindow::default()).unwrap();
        let original = track.clone();
        for _ in 0..3 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 1200.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        assert_eq!(draw(ui, &mut track, None), (false, None));
                    });
                },
            );
        }
        assert_eq!(track, original);
    }
}
