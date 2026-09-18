//! Time ruler, playhead, and marker row (04 §2.1, 13 §1.1).
//!
//! The ruler shows zoom-adaptive tick labels (timecode), a click/drag-to-scrub
//! surface that moves the session playhead (frame-snapped), and marker diamonds
//! read from `Sequence.markers`. Marker edits route through `ops_bridge` like
//! every other timeline mutation (04 §2.3): double-click empty ruler space adds
//! one, drag retimes one (commit once on release, one undo step per gesture —
//! same discipline as clip drags), right-click opens a rename/remove menu.

use super::layout::TimelineView;
use super::ops_bridge;
use photonic_core::document::Document;
use photonic_core::history::CommandHistory;
use photonic_core::timeline::{
    FrameRate, MarkerGlyph, MarkerId, Sequence, SequenceId, Tick, TICKS_PER_SECOND,
};

/// Format a tick as `HH:MM:SS:FF` timecode at `fr` (04 §3.2).
pub(crate) fn timecode(t: Tick, fr: FrameRate) -> String {
    let fps = ((fr.num as f64 / fr.den.max(1) as f64).round() as i64).max(1);
    let f = fr.frame_at(t).max(0);
    let frames = f % fps;
    let total_secs = f / fps;
    let s = total_secs % 60;
    let m = (total_secs / 60) % 60;
    let h = total_secs / 3600;
    format!("{h:02}:{m:02}:{s:02}:{frames:02}")
}

/// Choose a "nice" label interval (in ticks) so labels land roughly every
/// `target_px`. Steps through frame, then 1/2/5/10/15/30/60/120/300/600 s.
fn label_interval(view: &TimelineView, fr: FrameRate, target_px: f32) -> Tick {
    let tpf = fr.ticks_per_frame().0.max(1);
    let candidates_secs = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600];
    // Start with a single frame.
    let mut best = tpf;
    if view.ticks_to_px(Tick(tpf)) >= target_px {
        return Tick(tpf);
    }
    for s in candidates_secs {
        let ticks = s * TICKS_PER_SECOND;
        best = ticks;
        if view.ticks_to_px(Tick(ticks)) >= target_px {
            break;
        }
    }
    Tick(best)
}

/// Marker diamond hit-test / render radius, in screen px.
const MARKER_R: f32 = 4.0;
/// Marker pointer hit-test radius — a bit larger than the rendered radius so
/// grabbing a marker doesn't require pixel-perfect aim.
const MARKER_HIT_PX: f32 = 6.0;

/// Screen-space outline for a [`MarkerGlyph`] centred on `(x, cy)` (K-A2).
/// `Bar` and `Circle` are painted directly by [`paint_marker_glyph`]; every
/// other glyph is a convex polygon.
pub(crate) fn marker_glyph_points(glyph: MarkerGlyph, x: f32, cy: f32, r: f32) -> Vec<egui::Pos2> {
    match glyph {
        MarkerGlyph::Square => vec![
            egui::pos2(x - r * 0.8, cy - r * 0.8),
            egui::pos2(x + r * 0.8, cy - r * 0.8),
            egui::pos2(x + r * 0.8, cy + r * 0.8),
            egui::pos2(x - r * 0.8, cy + r * 0.8),
        ],
        MarkerGlyph::Triangle => vec![
            egui::pos2(x, cy - r),
            egui::pos2(x + r, cy + r * 0.7),
            egui::pos2(x - r, cy + r * 0.7),
        ],
        MarkerGlyph::Flag => vec![
            egui::pos2(x - r * 0.5, cy - r),
            egui::pos2(x + r, cy - r * 0.4),
            egui::pos2(x - r * 0.5, cy + r * 0.2),
            egui::pos2(x - r * 0.5, cy + r),
        ],
        MarkerGlyph::Bar => vec![
            egui::pos2(x - r * 0.4, cy - r),
            egui::pos2(x + r * 0.4, cy - r),
            egui::pos2(x + r * 0.4, cy + r),
            egui::pos2(x - r * 0.4, cy + r),
        ],
        // `Circle` has no polygon form; the diamond points are its bounding
        // rhombus and are never used for it (see `paint_marker_glyph`).
        MarkerGlyph::Diamond | MarkerGlyph::Circle => marker_diamond(x, cy, r),
    }
}

/// Paint one marker glyph. Split from [`marker_glyph_points`] so the polygon
/// geometry stays unit-testable without a painter.
fn paint_marker_glyph(
    painter: &egui::Painter,
    glyph: MarkerGlyph,
    x: f32,
    cy: f32,
    r: f32,
    fill: egui::Color32,
    stroke: egui::Stroke,
) {
    if glyph == MarkerGlyph::Circle {
        painter.circle(egui::pos2(x, cy), r * 0.85, fill, stroke);
        return;
    }
    painter.add(egui::Shape::convex_polygon(
        marker_glyph_points(glyph, x, cy, r),
        fill,
        stroke,
    ));
}

fn marker_diamond(x: f32, cy: f32, r: f32) -> Vec<egui::Pos2> {
    vec![
        egui::pos2(x, cy - r),
        egui::pos2(x + r, cy),
        egui::pos2(x, cy + r),
        egui::pos2(x - r, cy),
    ]
}

/// The nearest marker to `pos` within [`MARKER_HIT_PX`], if any (04 §2.6).
fn marker_at(
    seq: &Sequence,
    view: &TimelineView,
    lane_left: f32,
    ruler_rect: egui::Rect,
    pos: egui::Pos2,
) -> Option<MarkerId> {
    let cy = ruler_rect.top() + 5.0;
    if (pos.y - cy).abs() > MARKER_HIT_PX {
        return None;
    }
    seq.markers
        .iter()
        .map(|m| (m.id, (view.tick_to_x(m.at, lane_left) - pos.x).abs()))
        .filter(|(_, dx)| *dx <= MARKER_HIT_PX)
        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
        .map(|(id, _)| id)
}

fn marker_rename_id() -> egui::Id {
    egui::Id::new("timeline_marker_rename")
}

/// Draw the ruler into `ruler_rect` and handle scrub/seek (mutating
/// `playhead`, session state only) plus marker add/retime/rename/remove
/// (undoable, routed through `ops_bridge`). `lane_left` is the x of
/// tick-space origin (the lane's left edge).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_ruler(
    ui: &mut egui::Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    view: &TimelineView,
    ruler_rect: egui::Rect,
    lane_left: f32,
    playhead: &mut Tick,
) {
    let painter = ui.painter_at(ruler_rect);
    let visuals = ui.visuals();
    let tick_col = visuals.weak_text_color();
    let text_col = visuals.text_color();
    let accent = visuals.selection.stroke.color;

    // Background + baseline.
    painter.rect_filled(ruler_rect, 0.0, visuals.faint_bg_color);
    painter.line_segment(
        [ruler_rect.left_bottom(), ruler_rect.right_bottom()],
        egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
    );

    let seq = doc
        .timeline
        .as_ref()
        .unwrap()
        .sequences
        .get(&seq_id)
        .unwrap();

    // Adaptive tick labels.
    let interval = label_interval(view, seq.frame_rate, 72.0);
    if interval.0 > 0 {
        let first_tick = view.scroll_ticks.0;
        let last_tick = view.x_to_tick(ruler_rect.width(), 0.0).0;
        // Align the first label to an interval boundary at/after the left edge.
        let mut t = (first_tick.div_euclid(interval.0)) * interval.0;
        while t <= last_tick {
            let x = view.tick_to_x(Tick(t), lane_left);
            if x >= ruler_rect.left() - 1.0 && x <= ruler_rect.right() + 1.0 {
                painter.line_segment(
                    [
                        egui::pos2(x, ruler_rect.bottom() - 6.0),
                        egui::pos2(x, ruler_rect.bottom()),
                    ],
                    egui::Stroke::new(1.0, tick_col),
                );
                painter.text(
                    egui::pos2(x + 3.0, ruler_rect.top() + 2.0),
                    egui::Align2::LEFT_TOP,
                    timecode(Tick(t), seq.frame_rate),
                    egui::FontId::monospace(10.0),
                    text_col,
                );
            }
            t += interval.0;
        }
    }

    // Marker glyphs + range bars (K-A2). A marker's own `color` wins; failing
    // that its category's colour; failing that the accent. A marker naming a
    // category the project does not have renders NEUTRAL and is flagged with a
    // hollow outline — 35 §1.3's "never silently remapped" rule, drawn.
    let categories = doc
        .timeline
        .as_ref()
        .map(|p| p.marker_categories.clone())
        .unwrap_or_default();
    let cy = ruler_rect.top() + 5.0;
    for m in &seq.markers {
        let x = view.tick_to_x(m.at, lane_left);
        let x_end = view.tick_to_x(m.end(), lane_left);
        if x_end < ruler_rect.left() || x > ruler_rect.right() {
            continue;
        }
        let cat = m
            .category
            .and_then(|id| categories.iter().find(|c| c.id == id));
        let dangling = m.category.is_some() && cat.is_none();
        let col = m
            .color
            .or(cat.map(|c| c.color))
            .map(|c| {
                egui::Color32::from_rgb(
                    (c.r * 255.0) as u8,
                    (c.g * 255.0) as u8,
                    (c.b * 255.0) as u8,
                )
            })
            .unwrap_or(if dangling { tick_col } else { accent });
        // A ranged marker (duration > 0) draws its span, so the K-F2
        // export-per-marker unit is visible rather than implied.
        if m.is_range() && x_end > x + 1.0 {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x, cy - 1.5),
                    egui::pos2(x_end.min(ruler_rect.right()), cy + 1.5),
                ),
                1.0,
                col.gamma_multiply(0.55),
            );
            painter.line_segment(
                [
                    egui::pos2(x_end.min(ruler_rect.right()), cy - MARKER_R),
                    egui::pos2(x_end.min(ruler_rect.right()), cy + MARKER_R),
                ],
                egui::Stroke::new(1.0, col),
            );
        }
        if x < ruler_rect.left() || x > ruler_rect.right() {
            continue;
        }
        let glyph = cat.map(|c| c.glyph).unwrap_or(MarkerGlyph::Diamond);
        let stroke = if dangling {
            egui::Stroke::new(1.0, col)
        } else {
            egui::Stroke::NONE
        };
        let fill = if dangling {
            egui::Color32::TRANSPARENT
        } else {
            col
        };
        paint_marker_glyph(&painter, glyph, x, cy, MARKER_R, fill, stroke);
    }

    // ── Interaction ──────────────────────────────────────────────────────
    // One sense region covers the whole strip; whether a gesture is a plain
    // scrub or a marker grab is resolved by hit-testing the gesture's start
    // position against the markers, mirroring `interact.rs`'s zone-resolved-
    // at-drag-start discipline for clips.
    let resp = ui.interact(
        ruler_rect,
        ui.id().with("timeline_ruler"),
        egui::Sense::click_and_drag(),
    );
    let marker_drag_id = ui.id().with("timeline_marker_drag");

    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let seq = doc
                .timeline
                .as_ref()
                .unwrap()
                .sequences
                .get(&seq_id)
                .unwrap();
            if let Some(mid) = marker_at(seq, view, lane_left, ruler_rect, pos) {
                ui.data_mut(|d| d.insert_temp(marker_drag_id, mid));
            }
        }
    }
    let dragging_marker = ui.data(|d| d.get_temp::<MarkerId>(marker_drag_id));

    if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let seq = doc
                .timeline
                .as_ref()
                .unwrap()
                .sequences
                .get(&seq_id)
                .unwrap();
            let raw = view.x_to_tick(pos.x, lane_left);
            let snapped = seq
                .frame_rate
                .snap(if raw.0 < 0 { Tick::ZERO } else { raw });
            if dragging_marker.is_some() {
                // Ghost preview only — the real marker moves on release.
                let gx = view.tick_to_x(snapped, lane_left);
                painter.add(egui::Shape::convex_polygon(
                    marker_diamond(gx, cy, MARKER_R + 1.5),
                    accent.gamma_multiply(0.5),
                    egui::Stroke::new(1.0, accent),
                ));
            } else {
                *playhead = snapped;
            }
        }
    }

    if resp.drag_stopped() {
        if let Some(mid) = ui.data(|d| d.get_temp::<MarkerId>(marker_drag_id)) {
            ui.data_mut(|d| d.remove::<MarkerId>(marker_drag_id));
            if let Some(pos) = resp.interact_pointer_pos() {
                let seq = doc
                    .timeline
                    .as_ref()
                    .unwrap()
                    .sequences
                    .get(&seq_id)
                    .unwrap();
                let raw = view.x_to_tick(pos.x, lane_left);
                let snapped = seq
                    .frame_rate
                    .snap(if raw.0 < 0 { Tick::ZERO } else { raw });
                ops_bridge::retime_marker(doc, history, seq_id, mid, snapped);
            }
        }
    }

    // Plain click (not the tail end of a marker drag): scrub/seek, frame-
    // snapped. Session state only — never touches history.
    if resp.clicked() && dragging_marker.is_none() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let seq = doc
                .timeline
                .as_ref()
                .unwrap()
                .sequences
                .get(&seq_id)
                .unwrap();
            let raw = view.x_to_tick(pos.x, lane_left);
            *playhead = seq
                .frame_rate
                .snap(if raw.0 < 0 { Tick::ZERO } else { raw });
        }
    }

    // Double-click empty ruler space → add a marker at that tick (04 §2.6).
    if resp.double_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let seq = doc
                .timeline
                .as_ref()
                .unwrap()
                .sequences
                .get(&seq_id)
                .unwrap();
            if marker_at(seq, view, lane_left, ruler_rect, pos).is_none() {
                let raw = view.x_to_tick(pos.x, lane_left);
                let snapped = seq
                    .frame_rate
                    .snap(if raw.0 < 0 { Tick::ZERO } else { raw });
                ops_bridge::add_marker(doc, history, seq_id, snapped, "Marker");
            }
        }
    }

    // Right-click a marker → rename/remove context menu.
    let menu_target = resp.hover_pos().and_then(|pos| {
        let seq = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap();
        marker_at(seq, view, lane_left, ruler_rect, pos)
    });
    resp.context_menu(|ui| marker_context_menu(ui, doc, history, seq_id, menu_target));

    draw_marker_rename_popup(ui, doc, history, seq_id, view, lane_left, ruler_rect);
}

fn marker_context_menu(
    ui: &mut egui::Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    target: Option<MarkerId>,
) {
    let Some(mid) = target else {
        ui.label("No marker");
        return;
    };
    if ui.button("Rename").clicked() {
        let name = doc
            .timeline
            .as_ref()
            .and_then(|p| p.sequences.get(&seq_id))
            .and_then(|s| s.markers.iter().find(|m| m.id == mid))
            .map(|m| m.name.clone())
            .unwrap_or_default();
        ui.data_mut(|d| d.insert_temp(marker_rename_id(), (mid, name)));
        ui.close_menu();
    }
    if ui.button("Remove").clicked() {
        ops_bridge::remove_marker(doc, history, seq_id, mid);
        ui.close_menu();
    }
}

/// Floating rename box for the marker `marker_context_menu`'s "Rename"
/// started editing (mirrors `tracks.rs`'s inline track rename, but as a
/// popup — the 24px ruler strip has no room for an inline text box).
#[allow(clippy::too_many_arguments)]
fn draw_marker_rename_popup(
    ui: &mut egui::Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    view: &TimelineView,
    lane_left: f32,
    ruler_rect: egui::Rect,
) {
    let Some((mid, mut buf)) = ui.data(|d| d.get_temp::<(MarkerId, String)>(marker_rename_id()))
    else {
        return;
    };
    let Some(at) = doc
        .timeline
        .as_ref()
        .and_then(|p| p.sequences.get(&seq_id))
        .and_then(|s| s.markers.iter().find(|m| m.id == mid))
        .map(|m| m.at)
    else {
        ui.data_mut(|d| d.remove::<(MarkerId, String)>(marker_rename_id()));
        return;
    };
    let x = view.tick_to_x(at, lane_left);
    egui::Area::new(egui::Id::new(("timeline_marker_rename_popup", mid)))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(x, ruler_rect.bottom() + 2.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut buf).hint_text("Marker name"));
                resp.request_focus();
                if resp.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !buf.trim().is_empty() {
                        ops_bridge::rename_marker(
                            doc,
                            history,
                            seq_id,
                            mid,
                            buf.trim().to_string(),
                        );
                    }
                    ui.data_mut(|d| d.remove::<(MarkerId, String)>(marker_rename_id()));
                } else {
                    ui.data_mut(|d| d.insert_temp(marker_rename_id(), (mid, buf)));
                }
            });
        });
}

/// Draw the full-height playhead line across the ruler + all lanes (13 §1.1).
pub(crate) fn draw_playhead_line(
    painter: &egui::Painter,
    view: &TimelineView,
    playhead: Tick,
    content_rect: egui::Rect,
    lane_left: f32,
    accent: egui::Color32,
) {
    let x = view.tick_to_x(playhead, lane_left);
    if x < lane_left - 1.0 || x > content_rect.right() + 1.0 {
        return;
    }
    painter.line_segment(
        [
            egui::pos2(x, content_rect.top()),
            egui::pos2(x, content_rect.bottom()),
        ],
        egui::Stroke::new(1.0, accent),
    );
    // Small top handle so the playhead is grabbable-looking in the ruler.
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(x - 4.0, content_rect.top()),
            egui::pos2(x + 4.0, content_rect.top()),
            egui::pos2(x, content_rect.top() + 6.0),
        ],
        accent,
        egui::Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecode_formats_hms_frames_at_30fps() {
        let fr = FrameRate::FPS_30;
        assert_eq!(timecode(Tick::ZERO, fr), "00:00:00:00");
        // 1 second + 5 frames.
        let tpf = fr.ticks_per_frame().0;
        let t = Tick(TICKS_PER_SECOND + 5 * tpf);
        assert_eq!(timecode(t, fr), "00:00:01:05");
        // 1 hour exactly.
        assert_eq!(timecode(Tick::from_seconds(3600), fr), "01:00:00:00");
    }

    #[test]
    fn label_interval_grows_when_zoomed_out() {
        let mut v = TimelineView::default();
        let fr = FrameRate::FPS_30;
        let zoomed_in = label_interval(&v, fr, 72.0);
        // Zoom way out → interval must not shrink.
        v.pixels_per_tick /= 100.0;
        let zoomed_out = label_interval(&v, fr, 72.0);
        assert!(zoomed_out.0 >= zoomed_in.0);
    }

    /// K-A2: each [`MarkerGlyph`] must actually draw differently, and stay
    /// inside the marker radius so the ruler strip's 24px height and the
    /// `MARKER_HIT_PX` hit test still make sense. A copy-paste that gave two
    /// categories the same shape would make the glyph field decorative.
    #[test]
    fn every_marker_glyph_is_distinct_and_bounded() {
        use photonic_core::timeline::MarkerGlyph as G;
        let (x, cy, r) = (100.0_f32, 10.0_f32, 4.0_f32);
        let all = [
            G::Diamond,
            G::Circle,
            G::Square,
            G::Triangle,
            G::Flag,
            G::Bar,
        ];
        let shapes: Vec<Vec<egui::Pos2>> = all
            .iter()
            .map(|g| marker_glyph_points(*g, x, cy, r))
            .collect();
        for (i, a) in shapes.iter().enumerate() {
            assert!(a.len() >= 3, "{:?} is not a polygon", all[i]);
            for p in a {
                assert!(
                    (p.x - x).abs() <= r + 0.01 && (p.y - cy).abs() <= r + 0.01,
                    "{:?} escapes the marker radius at {p:?}",
                    all[i]
                );
            }
            for (j, b) in shapes.iter().enumerate() {
                // Circle shares Diamond's polygon by design (it is painted as a
                // real circle, never as these points) — every other pair must
                // differ.
                let circle_pair = matches!(
                    (all[i], all[j]),
                    (G::Diamond, G::Circle) | (G::Circle, G::Diamond)
                );
                if i != j && !circle_pair {
                    assert_ne!(a, b, "{:?} and {:?} paint the same shape", all[i], all[j]);
                }
            }
        }
    }
}

/// Cached-playback status uses the same tick axis as the ruler. The thin
/// coloured stroke is instrumentation; it does not cover ruler text or clips.
pub(crate) fn draw_preview_strip(
    ui: &mut egui::Ui,
    view: &TimelineView,
    ruler_rect: egui::Rect,
    lane_left: f32,
    sequence: &Sequence,
    status: Option<&photonic_video::preview::PreviewStatusSnapshot>,
) {
    use photonic_video::preview::ChunkState;
    let painter = ui.painter_at(ruler_rect);
    let y = ruler_rect.bottom() - 2.0;
    let red = ui.visuals().error_fg_color;
    let yellow = ui.visuals().warn_fg_color;
    // DESIGN.md `success`: export/render pipeline completion token.
    let green = egui::Color32::from_rgb(0x64, 0xc8, 0x7a);
    for (index, zone) in sequence.preview_zones.iter().enumerate() {
        let left = view.tick_to_x(zone.start, lane_left).max(ruler_rect.left());
        let right = view.tick_to_x(zone.end, lane_left).min(ruler_rect.right());
        if right <= left {
            continue;
        }
        painter.line_segment(
            [egui::pos2(left, y), egui::pos2(right, y)],
            egui::Stroke::new(3.0, red),
        );
        if let Some(status) = status {
            for chunk in status.chunks.iter().filter(|chunk| {
                chunk.sequence == sequence.id
                    && chunk.format_index == sequence.active_format
                    && chunk.start < zone.end
                    && zone.start < chunk.end
            }) {
                let start = view
                    .tick_to_x(chunk.start.max(zone.start), lane_left)
                    .max(left);
                let end = view
                    .tick_to_x(chunk.end.min(zone.end), lane_left)
                    .min(right);
                if end <= start {
                    continue;
                }
                let color = match chunk.state {
                    ChunkState::Rendered => green,
                    ChunkState::Queued | ChunkState::Rendering | ChunkState::Paused => yellow,
                    _ => red,
                };
                painter.line_segment(
                    [egui::pos2(start, y), egui::pos2(end, y)],
                    egui::Stroke::new(3.0, color),
                );
                let rect =
                    egui::Rect::from_min_max(egui::pos2(start, y - 3.0), egui::pos2(end, y + 2.0));
                ui.interact(
                    rect,
                    ui.id().with(("preview_chunk", index, chunk.start.0)),
                    egui::Sense::hover(),
                )
                .on_hover_ui(|ui| {
                    ui.label(format!(
                        "Playback preview: {:?} ({}/{})",
                        chunk.state, chunk.frame, chunk.total
                    ));
                    if let Some(error) = &chunk.error {
                        ui.label(error);
                    }
                    if status.cache.pressure {
                        ui.label(format!(
                            "Cache pressure: {} evicted, {} renders could not fit",
                            status.cache.evicted, status.cache.rejected
                        ));
                    }
                });
            }
        }
    }
}
