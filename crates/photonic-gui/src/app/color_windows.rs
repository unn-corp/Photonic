//! On-viewer handles for the renderer's normalized ellipse/rectangle power
//! windows. The overlay is limited to clip, track and master grades, whose
//! masks operate on the sequence canvas after the clip transform. Asset grades
//! operate in source coordinates and need a separate source-viewer overlay.

use egui::{Pos2, Rect, Sense, Stroke, Vec2};
use photonic_core::document::Document;
use photonic_core::history::{Command, CommandHistory};
use photonic_core::timeline::{ops, ClipId, GradeMask, GradeOpId, Tick, VfxOwner, WindowShape};

const HANDLE_RADIUS: f32 = 5.0;

fn point(rect: Rect, center: [f32; 2], size: [f32; 2], rotation: f32, x: f32, y: f32) -> Pos2 {
    let (sin, cos) = rotation.sin_cos();
    let nx = center[0] + x * size[0] * cos - y * size[1] * sin;
    let ny = center[1] + x * size[0] * sin + y * size[1] * cos;
    Pos2::new(
        rect.left() + nx * rect.width(),
        rect.top() + ny * rect.height(),
    )
}

fn normalized_pointer(rect: Rect, pointer: Pos2) -> [f32; 2] {
    [
        (pointer.x - rect.left()) / rect.width().max(1.0),
        (pointer.y - rect.top()) / rect.height().max(1.0),
    ]
}

fn local_pointer(rect: Rect, pointer: Pos2, center: [f32; 2], rotation: f32) -> [f32; 2] {
    let [x, y] = normalized_pointer(rect, pointer);
    let (sin, cos) = rotation.sin_cos();
    let dx = x - center[0];
    let dy = y - center[1];
    [dx * cos + dy * sin, -dx * sin + dy * cos]
}

fn feather_handle(
    rect: Rect,
    shape: WindowShape,
    center: [f32; 2],
    size: [f32; 2],
    rotation: f32,
    softness: f32,
) -> Pos2 {
    let inner = (1.0 - softness).max(0.05);
    let diagonal = if shape == WindowShape::Ellipse {
        std::f32::consts::FRAC_1_SQRT_2
    } else {
        1.0
    };
    point(
        rect,
        center,
        size,
        rotation,
        inner * diagonal,
        inner * diagonal,
    )
}

fn feather_at_pointer(
    rect: Rect,
    pointer: Pos2,
    shape: WindowShape,
    center: [f32; 2],
    size: [f32; 2],
    rotation: f32,
) -> f32 {
    let [x, y] = local_pointer(rect, pointer, center, rotation);
    let nx = x / size[0].max(0.005);
    let ny = y / size[1].max(0.005);
    let distance = if shape == WindowShape::Ellipse {
        nx.hypot(ny)
    } else {
        nx.abs().max(ny.abs())
    };
    (1.0 - distance).clamp(0.0, 1.0)
}

fn outline(
    rect: Rect,
    shape: WindowShape,
    center: [f32; 2],
    size: [f32; 2],
    rotation: f32,
) -> Vec<Pos2> {
    match shape {
        WindowShape::Gradient => [(-2.0, -1.0), (2.0, -1.0), (2.0, 1.0), (-2.0, 1.0)]
            .into_iter()
            .map(|(x, y)| point(rect, center, [1.0, size[1]], rotation, x, y))
            .collect(),
        WindowShape::Rectangle => [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
            .into_iter()
            .map(|(x, y)| point(rect, center, size, rotation, x, y))
            .collect(),
        WindowShape::Ellipse => (0..64)
            .map(|index| {
                let angle = index as f32 * std::f32::consts::TAU / 64.0;
                point(rect, center, size, rotation, angle.cos(), angle.sin())
            })
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_window_handles(
    ui: &mut egui::Ui,
    video_rect: Rect,
    doc: &mut Document,
    history: &mut CommandHistory,
    selection: &[ClipId],
    selected_op: Option<GradeOpId>,
    playhead: Tick,
) {
    let ([clip_id], Some(op_id)) = (selection, selected_op) else {
        return;
    };
    let clip_id = *clip_id;
    let Some(project) = doc.timeline.as_ref() else {
        return;
    };
    let Some(sequence_id) = project.active_sequence else {
        return;
    };
    let Some(sequence) = project.sequences.get(&sequence_id) else {
        return;
    };
    let Some((track_id, clip)) = sequence.video_tracks.iter().find_map(|track| {
        track
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .map(|clip| (track.id, clip))
    }) else {
        return;
    };
    if playhead < clip.start || playhead >= clip.end() {
        return;
    }
    let owner = ui
        .ctx()
        .data(|data| {
            data.get_temp::<VfxOwner>(crate::panels::video::color_page::active_grade_scope_id(
                clip_id,
            ))
        })
        .unwrap_or(VfxOwner::Clip(clip_id));
    if !matches!(owner, VfxOwner::Clip(id) if id == clip_id)
        && !matches!(owner, VfxOwner::Track(id) if id == track_id)
        && !matches!(owner, VfxOwner::Master(id) if id == sequence_id)
        && !matches!(owner, VfxOwner::GroupPre(id) | VfxOwner::GroupPost(id) if clip.group.is_some_and(|group| sequence.group_chain(group).contains(&id)))
    {
        return;
    }
    let Ok(Some(grade)) = ops::scope_grade(project, owner) else {
        return;
    };
    let mut grade = grade.clone();
    let editable = ops::set_grade_scoped(project, owner, Some(grade.clone())).is_ok();
    let Some(op) = grade.ops.iter_mut().find(|op| op.id == op_id) else {
        return;
    };
    let Some(GradeMask::PowerWindow {
        shape,
        center,
        size,
        rotation,
        softness,
        ..
    }) = op.mask.as_mut()
    else {
        return;
    };
    let accent = ui.visuals().selection.stroke.color;
    let painter = ui.painter_at(video_rect);
    painter.add(egui::Shape::closed_line(
        outline(video_rect, *shape, *center, *size, *rotation),
        Stroke::new(1.5, accent),
    ));
    if *shape != WindowShape::Gradient && *softness > 0.0 {
        let inner = [
            size[0] * (1.0 - *softness).max(0.0),
            size[1] * (1.0 - *softness).max(0.0),
        ];
        painter.add(egui::Shape::closed_line(
            outline(video_rect, *shape, *center, inner, *rotation),
            Stroke::new(0.75, accent.gamma_multiply(0.55)),
        ));
    }
    let center_pos = point(video_rect, *center, *size, *rotation, 0.0, 0.0);
    let x_pos = point(video_rect, *center, *size, *rotation, 1.0, 0.0);
    let y_pos = point(video_rect, *center, *size, *rotation, 0.0, 1.0);
    let rotate_pos = point(video_rect, *center, *size, *rotation, 0.0, -1.25);
    if *shape != WindowShape::Gradient {
        painter.line_segment([center_pos, x_pos], Stroke::new(0.75, accent));
    }
    painter.line_segment([center_pos, y_pos], Stroke::new(0.75, accent));
    painter.line_segment(
        [
            point(video_rect, *center, *size, *rotation, 0.0, -1.0),
            rotate_pos,
        ],
        Stroke::new(0.75, accent),
    );
    for (index, handle) in [center_pos, x_pos, y_pos, rotate_pos]
        .into_iter()
        .enumerate()
    {
        if *shape == WindowShape::Gradient && index == 1 {
            continue;
        }
        painter.circle_filled(handle, HANDLE_RADIUS, accent);
    }
    let feather_pos = (*shape != WindowShape::Gradient)
        .then(|| feather_handle(video_rect, *shape, *center, *size, *rotation, *softness));
    if let Some(handle) = feather_pos {
        painter.circle_filled(handle, HANDLE_RADIUS, accent.gamma_multiply(0.7));
        painter.circle_stroke(handle, HANDLE_RADIUS + 1.0, Stroke::new(1.0, accent));
    }
    if !editable {
        return; // selected clip/track is locked; show geometry but do not edit it
    }
    let handles = [center_pos, x_pos, y_pos, rotate_pos];
    let mut changed = false;
    for (index, handle) in handles.into_iter().enumerate() {
        if *shape == WindowShape::Gradient && index == 1 {
            continue;
        }
        let hit = Rect::from_center_size(handle, Vec2::splat(HANDLE_RADIUS * 3.0));
        let response = ui.interact(
            hit,
            ui.id().with(("grade_window", op_id, index)),
            Sense::drag(),
        );
        if !response.dragged() {
            continue;
        }
        match index {
            0 => {
                let delta = ui.input(|input| input.pointer.delta());
                center[0] = (center[0] + delta.x / video_rect.width().max(1.0)).clamp(0.0, 1.0);
                center[1] = (center[1] + delta.y / video_rect.height().max(1.0)).clamp(0.0, 1.0);
                changed = true;
            }
            1 | 2 => {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let local = local_pointer(video_rect, pointer, *center, *rotation);
                    size[index - 1] = local[index - 1].abs().clamp(0.005, 2.0);
                    changed = true;
                }
            }
            _ => {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let [x, y] = normalized_pointer(video_rect, pointer);
                    *rotation = (y - center[1]).atan2(x - center[0]) + std::f32::consts::FRAC_PI_2;
                    changed = true;
                }
            }
        }
    }
    if let Some(handle) = feather_pos {
        let hit = Rect::from_center_size(handle, Vec2::splat(HANDLE_RADIUS * 3.0));
        let response = ui
            .interact(
                hit,
                ui.id().with(("grade_window", op_id, "feather")),
                Sense::drag(),
            )
            .on_hover_text("Drag to adjust window feather");
        if response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                *softness =
                    feather_at_pointer(video_rect, pointer, *shape, *center, *size, *rotation);
                changed = true;
            }
        }
    }
    if changed {
        let Some(project) = doc.timeline.as_ref() else {
            return;
        };
        if let Ok(command) = ops::set_grade_scoped(project, owner, Some(grade)) {
            history.execute(Command::Timeline(command), doc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        Clip, ClipSource, FrameRate, Grade, GradeOp, GradeOpKind, GradeOpParams, Sequence,
        TimelineProject, Track, TrackKind,
    };

    #[test]
    fn normalized_window_geometry_matches_renderer_coordinates() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(400.0, 200.0));
        let center = [0.5, 0.25];
        let size = [0.2, 0.1];
        let p = point(rect, center, size, 0.0, 1.0, 0.0);
        assert_eq!(p, Pos2::new(290.0, 70.0));
        let local = local_pointer(rect, p, center, 0.0);
        assert!((local[0] - size[0]).abs() < 1e-6);
        assert!(local[1].abs() < 1e-6);
        let rotated = point(rect, center, size, std::f32::consts::FRAC_PI_2, 1.0, 0.0);
        assert!((rotated.x - 210.0).abs() < 1e-4);
        assert!((rotated.y - 110.0).abs() < 1e-4);
        assert_eq!(
            outline(rect, WindowShape::Rectangle, center, size, 0.0).len(),
            4
        );
        assert_eq!(
            outline(rect, WindowShape::Ellipse, center, size, 0.0).len(),
            64
        );
        let gradient = outline(rect, WindowShape::Gradient, center, size, 0.0);
        assert_eq!(gradient.len(), 4);
        assert!((gradient[0].y - 50.0).abs() < 1e-4);
        assert!((gradient[2].y - 90.0).abs() < 1e-4);
    }

    #[test]
    fn feather_handle_roundtrips_for_rotated_ellipse_and_rectangle() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(400.0, 200.0));
        for shape in [WindowShape::Ellipse, WindowShape::Rectangle] {
            let center = [0.55, 0.45];
            let size = [0.24, 0.32];
            let rotation = 0.63;
            let handle = feather_handle(rect, shape, center, size, rotation, 0.35);
            let recovered = feather_at_pointer(rect, handle, shape, center, size, rotation);
            assert!((recovered - 0.35).abs() < 1e-5, "{shape:?}: {recovered}");
            let outside = point(rect, center, size, rotation, 2.0, 2.0);
            assert_eq!(
                feather_at_pointer(rect, outside, shape, center, size, rotation),
                0.0
            );
        }
    }

    #[test]
    fn dragging_center_changes_one_undoable_grade() {
        let mut doc = Document::new("window", 16.0, 16.0);
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("cut", FrameRate::FPS_30, 16, 16);
        let mut track = Track::new(TrackKind::Video, "V1");
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick(10_000));
        let clip_id = clip.id;
        let mut grade = Grade::new();
        let mut op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        op.mask = Some(GradeMask::PowerWindow {
            shape: WindowShape::Ellipse,
            center: [0.5, 0.5],
            size: [0.2, 0.2],
            rotation: 0.0,
            softness: 0.1,
            invert: false,
        });
        let op_id = op.id;
        grade.ops.push(op);
        clip.grade = Some(grade.clone());
        track.clips.push(clip);
        sequence.video_tracks.push(track);
        project.insert_sequence(sequence);
        doc.timeline = Some(project);
        let mut history = CommandHistory::new(100);
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::splat(400.0));
        let draw = |events: Vec<egui::Event>,
                    time: f64,
                    doc: &mut Document,
                    history: &mut CommandHistory| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(800.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    draw_window_handles(
                        ui,
                        rect,
                        doc,
                        history,
                        &[clip_id],
                        Some(op_id),
                        Tick::ZERO,
                    );
                });
            });
        };
        draw(Vec::new(), 0.0, &mut doc, &mut history);
        history.begin_coalescing();
        draw(
            vec![
                egui::Event::PointerMoved(Pos2::new(300.0, 300.0)),
                egui::Event::PointerButton {
                    pos: Pos2::new(300.0, 300.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            0.016,
            &mut doc,
            &mut history,
        );
        draw(
            vec![egui::Event::PointerMoved(Pos2::new(340.0, 300.0))],
            0.032,
            &mut doc,
            &mut history,
        );
        draw(
            vec![egui::Event::PointerButton {
                pos: Pos2::new(340.0, 300.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            0.048,
            &mut doc,
            &mut history,
        );
        history.end_coalescing();
        let project = doc.timeline.as_ref().unwrap();
        let sequence = &project.sequences[&project.active_sequence.unwrap()];
        let moved = sequence.video_tracks[0].clips[0].grade.as_ref().unwrap();
        let Some(GradeMask::PowerWindow { center, .. }) = &moved.ops[0].mask else {
            panic!("mask disappeared")
        };
        assert!(center[0] > 0.5, "window center did not move: {center:?}");
        assert!(history.undo(&mut doc));
        let project = doc.timeline.as_ref().unwrap();
        let restored = project.sequences[&project.active_sequence.unwrap()].video_tracks[0].clips
            [0]
        .grade
        .as_ref()
        .unwrap();
        assert_eq!(restored, &grade);
        assert!(!history.undo(&mut doc));
    }
}
