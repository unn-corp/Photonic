//! Interactive Width tool — shape variable-width strokes by dragging width
//! handles directly on a path.
//!
//! Hovering a `Path` stroke shows two diamond handles (top = left side, bottom
//! = right side) at every sample of the stroke's [`WidthProfile`]. Clicking an
//! empty spot on the stroke inserts a new width sample; the first click on a
//! stroke with no profile creates a uniform one and attaches it. Dragging a
//! handle changes the width (Alt+drag affects only the dragged side, producing
//! an asymmetric profile). Delete removes the selected sample. Every edit is a
//! single undoable [`Command::SetWidthProfiles`] step (or a `Batch` when a new
//! profile is also attached to the node).
//!
//! Variable-width *rendering* is handled separately by the tessellator from the
//! profile's `widths`; this tool produces the real, editable profile data.

use super::*;
use photonic_core::WidthProfile;

/// Pixel radius (screen space) for grabbing a width handle.
const HANDLE_GRAB_PX: f32 = 7.0;
/// Pixel radius for the drawn diamond handles.
const HANDLE_DRAW_PX: f32 = 5.0;

impl PhotonicApp {
    pub(crate) fn handle_width_tool(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        doc: &mut Document,
        view: &CanvasView,
        doc_modified: &mut bool,
        history: &mut CommandHistory,
    ) {
        let ctx = ui.ctx();
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);

        let alt = ui.input(|i| i.modifiers.alt);
        let delete_pressed =
            ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
        let pointer = ui.input(|i| i.pointer.hover_pos());

        // ── Begin a handle drag ───────────────────────────────────────────────
        if response.drag_started() && self.width_tool_drag_origin_y.is_none() {
            if let Some(p) = response.interact_pointer_pos() {
                if let Some((nid, idx, is_right)) = self.width_handle_at(doc, view, p) {
                    let (_cx, cy) = view.screen_to_canvas(p.x as f64, p.y as f64);
                    self.width_tool_hovered_node = Some(nid);
                    self.width_tool_selected_point = Some(idx);
                    self.width_tool_drag_right = is_right;
                    self.width_tool_drag_origin_y = Some(cy);
                    self.width_tool_profiles_before = Some(doc.width_profiles.clone());
                }
            }
        }

        // ── Live drag update (preview directly on the document) ───────────────
        if let (Some(origin_y), Some(idx), Some(nid)) = (
            self.width_tool_drag_origin_y,
            self.width_tool_selected_point,
            self.width_tool_hovered_node,
        ) {
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(p) = response.interact_pointer_pos() {
                    let (_cx, cy) = view.screen_to_canvas(p.x as f64, p.y as f64);
                    let dy = cy - origin_y;
                    self.apply_width_drag(doc, nid, idx, self.width_tool_drag_right, alt, dy);
                }
            }
        }

        // ── Commit the drag as one undo step ──────────────────────────────────
        if response.drag_stopped() {
            if let Some(before) = self.width_tool_profiles_before.take() {
                let after = doc.width_profiles.clone();
                history.execute(
                    Command::SetWidthProfiles {
                        old: before,
                        new: after,
                    },
                    doc,
                );
                *doc_modified = true;
            }
            self.width_tool_drag_origin_y = None;
        }

        // ── Hover hit-test (skipped while dragging) ───────────────────────────
        let is_dragging = self.width_tool_drag_origin_y.is_some();
        if !is_dragging {
            self.update_width_hover(doc, view, pointer);
        }

        // ── Click: select a handle, or insert / create a width sample ─────────
        if response.clicked_by(egui::PointerButton::Primary) && !is_dragging {
            if let Some(p) = pointer {
                if let Some((nid, idx, _)) = self.width_handle_at(doc, view, p) {
                    self.width_tool_hovered_node = Some(nid);
                    self.width_tool_selected_point = Some(idx);
                } else if let Some(nid) = self.width_tool_hovered_node {
                    self.insert_or_create_width_point(doc, history, nid, doc_modified);
                }
            }
        }

        // ── Delete the selected sample ────────────────────────────────────────
        if delete_pressed {
            self.delete_selected_width_point(doc, history, doc_modified);
        }

        // ── Render handles + hover indicator ──────────────────────────────────
        self.paint_width_overlay(ctx, doc, view);
    }

    /// Update `width_tool_hovered_node` / `width_tool_hovered_t` from the cursor.
    fn update_width_hover(
        &mut self,
        doc: &Document,
        view: &CanvasView,
        pointer: Option<egui::Pos2>,
    ) {
        self.width_tool_hovered_node = None;
        let Some(p) = pointer else {
            return;
        };
        let (cx, cy) = view.screen_to_canvas(p.x as f64, p.y as f64);
        let mut best_dist = 20.0f64 / view.zoom;
        let mut best: Option<(NodeId, f64)> = None;

        for node in doc.nodes.values() {
            if !node.visible {
                continue;
            }
            let pn = match &node.kind {
                SceneNodeKind::Path(p) => p,
                _ => continue,
            };
            if pn.path_data.is_empty() {
                continue;
            }
            let inv = node.transform.to_kurbo().inverse();
            let lpt = inv * kurbo::Point::new(cx, cy);
            for (sx, sy, t) in pn.path_data.sample_positions(64) {
                let d = ((sx - lpt.x).powi(2) + (sy - lpt.y).powi(2)).sqrt();
                if d < best_dist {
                    best_dist = d;
                    best = Some((node.id, t));
                }
            }
        }
        if let Some((nid, t)) = best {
            self.width_tool_hovered_node = Some(nid);
            self.width_tool_hovered_t = t;
        }
    }

    /// Canvas-space path samples `(x, y, t)` for a node, transform applied.
    fn width_canvas_samples(node: &SceneNode) -> Vec<(f64, f64, f64)> {
        let pn = match &node.kind {
            SceneNodeKind::Path(p) => p,
            _ => return Vec::new(),
        };
        let fwd = node.transform.to_kurbo();
        pn.path_data
            .sample_positions(128)
            .into_iter()
            .map(|(x, y, t)| {
                let p = fwd * kurbo::Point::new(x, y);
                (p.x, p.y, t)
            })
            .collect()
    }

    /// Screen position of the path at normalized arc-length `t` (nearest sample).
    fn width_screen_at_t(samples: &[(f64, f64, f64)], view: &CanvasView, t: f64) -> egui::Pos2 {
        let mut best = (f64::INFINITY, 0usize);
        for (i, &(_, _, st)) in samples.iter().enumerate() {
            let d = (st - t).abs();
            if d < best.0 {
                best = (d, i);
            }
        }
        let (x, y, _) = samples.get(best.1).copied().unwrap_or((0.0, 0.0, 0.0));
        let (sx, sy) = view.canvas_to_screen(x, y);
        egui::pos2(sx as f32, sy as f32)
    }

    /// Top (left-side) and bottom (right-side) handle screen positions for each
    /// profile sample of `node`, returned as `(idx, top, bottom)`.
    fn width_handle_positions(
        &self,
        doc: &Document,
        view: &CanvasView,
        node: &SceneNode,
    ) -> Vec<(usize, egui::Pos2, egui::Pos2)> {
        let Some(prof) = width_profile_for(doc, node) else {
            return Vec::new();
        };
        let samples = Self::width_canvas_samples(node);
        if samples.len() < 2 {
            return Vec::new();
        }
        let positions = prof.effective_positions();
        let zoom = view.zoom as f32;
        let mut out = Vec::with_capacity(prof.widths.len());
        for (i, &w) in prof.widths.iter().enumerate() {
            let t = positions.get(i).copied().unwrap_or(0.0);
            let right_half = prof
                .widths_right
                .as_ref()
                .and_then(|r| r.get(i).copied())
                .unwrap_or(w * 0.5);
            let left_half = (w - right_half).max(0.0);
            let base = Self::width_screen_at_t(&samples, view, t);
            let top = egui::pos2(base.x, base.y - left_half as f32 * zoom);
            let bottom = egui::pos2(base.x, base.y + right_half as f32 * zoom);
            out.push((i, top, bottom));
        }
        out
    }

    /// Hit-test the width handles of all visible profiled paths.
    /// Returns `(node, sample_index, is_right_side)`.
    fn width_handle_at(
        &self,
        doc: &Document,
        view: &CanvasView,
        screen: egui::Pos2,
    ) -> Option<(NodeId, usize, bool)> {
        let mut best: Option<(f32, NodeId, usize, bool)> = None;
        for node in doc.nodes.values() {
            if !node.visible || width_profile_for(doc, node).is_none() {
                continue;
            }
            for (idx, top, bottom) in self.width_handle_positions(doc, view, node) {
                for (pos, is_right) in [(top, false), (bottom, true)] {
                    let d = pos.distance(screen);
                    if d <= HANDLE_GRAB_PX && best.is_none_or(|b| d < b.0) {
                        best = Some((d, node.id, idx, is_right));
                    }
                }
            }
        }
        best.map(|(_, nid, idx, right)| (nid, idx, right))
    }

    /// Apply a live width change to sample `idx` of `node`'s profile.
    fn apply_width_drag(
        &mut self,
        doc: &mut Document,
        node_id: NodeId,
        idx: usize,
        is_right: bool,
        alt: bool,
        dy_canvas: f64,
    ) {
        let Some(before) = self.width_tool_profiles_before.as_ref() else {
            return;
        };
        let Some(node) = doc.nodes.get(&node_id) else {
            return;
        };
        let pid = match width_profile_id_for(node) {
            Some(id) => id,
            None => return,
        };
        // Base half-widths from the pre-drag snapshot.
        let Some(base_prof) = before.iter().find(|p| p.id == pid) else {
            return;
        };
        if idx >= base_prof.widths.len() {
            return;
        }
        let base_total = base_prof.widths[idx];
        let base_right = base_prof
            .widths_right
            .as_ref()
            .and_then(|r| r.get(idx).copied())
            .unwrap_or(base_total * 0.5);
        let base_left = (base_total - base_right).max(0.0);

        // Dragging the bottom (right) handle down grows the right side; dragging
        // the top (left) handle up grows the left side.
        let new_half = if is_right {
            (base_right + dy_canvas).max(0.0)
        } else {
            (base_left - dy_canvas).max(0.0)
        };

        let Some(prof) = doc.width_profiles.iter_mut().find(|p| p.id == pid) else {
            return;
        };
        normalize_profile(prof);
        if idx >= prof.widths.len() {
            return;
        }
        if alt {
            // Asymmetric: only the dragged side changes.
            let mut rights = prof
                .widths_right
                .clone()
                .unwrap_or_else(|| prof.widths.iter().map(|w| w * 0.5).collect());
            if rights.len() != prof.widths.len() {
                rights = prof.widths.iter().map(|w| w * 0.5).collect();
            }
            let left = if is_right { base_left } else { new_half };
            let right = if is_right { new_half } else { base_right };
            rights[idx] = right;
            prof.widths[idx] = left + right;
            prof.widths_right = Some(rights);
        } else {
            // Symmetric: both sides equal the dragged half.
            prof.widths[idx] = new_half * 2.0;
            if let Some(rights) = prof.widths_right.as_mut() {
                if idx < rights.len() {
                    rights[idx] = new_half;
                }
            }
        }
    }

    /// Click on an empty stroke spot: insert a sample, or create+attach a
    /// uniform profile if the stroke has none yet.
    fn insert_or_create_width_point(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        node_id: NodeId,
        doc_modified: &mut bool,
    ) {
        let Some(node) = doc.nodes.get(&node_id).cloned() else {
            return;
        };
        let SceneNodeKind::Path(pn) = &node.kind else {
            return;
        };
        let t = self.width_tool_hovered_t.clamp(0.0, 1.0);
        let before = doc.width_profiles.clone();

        if width_profile_for(doc, &node).is_some() {
            // Insert into the existing profile.
            let pid = width_profile_id_for(&node).unwrap();
            let Some(prof) = doc.width_profiles.iter_mut().find(|p| p.id == pid) else {
                return;
            };
            let new_idx = insert_width_sample(prof, t);
            history.execute(
                Command::SetWidthProfiles {
                    old: before,
                    new: doc.width_profiles.clone(),
                },
                doc,
            );
            self.width_tool_selected_point = Some(new_idx);
            *doc_modified = true;
        } else {
            // Create a uniform profile from the current stroke width and attach it.
            let w = if pn.stroke.width > 0.0 {
                pn.stroke.width
            } else {
                1.0
            };
            let name = self.next_width_profile_name(doc, &node);
            let prof = WidthProfile::with_positions(name, vec![0.0, 1.0], vec![w, w]);
            let pid = prof.id;
            let mut new_node = node.clone();
            if let SceneNodeKind::Path(npn) = &mut new_node.kind {
                npn.stroke.width_profile_id = Some(pid);
            }
            let mut after = before.clone();
            after.push(prof);
            history.execute(
                Command::Batch(vec![
                    Command::UpdateNode {
                        old: node.clone(),
                        new: new_node,
                    },
                    Command::SetWidthProfiles {
                        old: before,
                        new: after,
                    },
                ]),
                doc,
            );
            self.width_tool_hovered_node = Some(node_id);
            self.width_tool_selected_point = None;
            *doc_modified = true;
        }
    }

    /// Delete the currently selected width sample (keeps at least two).
    fn delete_selected_width_point(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        let (Some(nid), Some(idx)) = (self.width_tool_hovered_node, self.width_tool_selected_point)
        else {
            return;
        };
        let Some(node) = doc.nodes.get(&nid).cloned() else {
            return;
        };
        let Some(pid) = width_profile_id_for(&node) else {
            return;
        };
        let before = doc.width_profiles.clone();
        let Some(prof) = doc.width_profiles.iter_mut().find(|p| p.id == pid) else {
            return;
        };
        if prof.widths.len() <= 2 || idx >= prof.widths.len() {
            return;
        }
        normalize_profile(prof);
        prof.widths.remove(idx);
        prof.positions.remove(idx);
        if let Some(rights) = prof.widths_right.as_mut() {
            if idx < rights.len() {
                rights.remove(idx);
            }
        }
        history.execute(
            Command::SetWidthProfiles {
                old: before,
                new: doc.width_profiles.clone(),
            },
            doc,
        );
        self.width_tool_selected_point = None;
        *doc_modified = true;
    }

    /// Pick a name for a newly created profile: the user's typed name if set,
    /// otherwise a unique `"<node> width"`.
    fn next_width_profile_name(&self, doc: &Document, node: &SceneNode) -> String {
        let typed = self.width_tool_save_name.trim();
        let base = if typed.is_empty() {
            format!("{} width", node.name)
        } else {
            typed.to_string()
        };
        if !doc.width_profiles.iter().any(|p| p.name == base) {
            return base;
        }
        for n in 2.. {
            let candidate = format!("{base} {n}");
            if !doc.width_profiles.iter().any(|p| p.name == candidate) {
                return candidate;
            }
        }
        base
    }

    /// Draw width handles for the hovered node and an insertion indicator.
    fn paint_width_overlay(&self, ctx: &egui::Context, doc: &Document, view: &CanvasView) {
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("width_tool_overlay"),
        ));
        let Some(nid) = self.width_tool_hovered_node else {
            return;
        };
        let Some(node) = doc.nodes.get(&nid) else {
            return;
        };

        let blue = egui::Color32::from_rgb(0, 140, 255);
        let white = egui::Color32::WHITE;

        if width_profile_for(doc, node).is_some() {
            let handles = self.width_handle_positions(doc, view, node);
            for (idx, top, bottom) in &handles {
                // Connecting bar through the path point.
                painter.line_segment([*top, *bottom], egui::Stroke::new(1.0, blue));
                let selected = self.width_tool_selected_point == Some(*idx);
                for pos in [*top, *bottom] {
                    draw_diamond(&painter, pos, HANDLE_DRAW_PX, blue, white, selected);
                }
            }
        } else {
            // No profile yet: hint where the first profile point would seed.
            let samples = Self::width_canvas_samples(node);
            if samples.len() >= 2 {
                let pos = Self::width_screen_at_t(&samples, view, self.width_tool_hovered_t);
                painter.circle_stroke(pos, HANDLE_DRAW_PX, egui::Stroke::new(1.5, blue));
            }
        }
    }
}

/// The id of a path node's width profile, if its stroke links one.
fn width_profile_id_for(node: &SceneNode) -> Option<uuid::Uuid> {
    match &node.kind {
        SceneNodeKind::Path(pn) => pn.stroke.width_profile_id,
        _ => None,
    }
}

/// The width profile a path node links to, if present in the document.
fn width_profile_for<'a>(doc: &'a Document, node: &SceneNode) -> Option<&'a WidthProfile> {
    let id = width_profile_id_for(node)?;
    doc.width_profiles.iter().find(|p| p.id == id)
}

/// Ensure `positions` is populated and matches `widths` length.
fn normalize_profile(prof: &mut WidthProfile) {
    if prof.positions.len() != prof.widths.len() {
        prof.positions = prof.effective_positions();
    }
    if let Some(rights) = prof.widths_right.as_mut() {
        if rights.len() != prof.widths.len() {
            *rights = prof.widths.iter().map(|w| w * 0.5).collect();
        }
    }
}

/// Linearly interpolate `values` (paired with sorted `positions`) at `t`.
fn sample_profile(positions: &[f64], values: &[f64], t: f64) -> f64 {
    if values.is_empty() {
        return 1.0;
    }
    if positions.is_empty() || positions.len() != values.len() {
        return values[0];
    }
    if t <= positions[0] {
        return values[0];
    }
    let n = values.len();
    if t >= positions[n - 1] {
        return values[n - 1];
    }
    for i in 1..n {
        if t <= positions[i] {
            let span = (positions[i] - positions[i - 1]).max(1e-9);
            let f = (t - positions[i - 1]) / span;
            return values[i - 1] * (1.0 - f) + values[i] * f;
        }
    }
    values[n - 1]
}

/// Insert a new sample at normalized position `t`, interpolating the width
/// (and right-side width, if asymmetric). Returns the new sample's index.
fn insert_width_sample(prof: &mut WidthProfile, t: f64) -> usize {
    normalize_profile(prof);
    let t = t.clamp(0.0, 1.0);
    let positions = prof.positions.clone();
    let w = sample_profile(&positions, &prof.widths, t);
    let right = prof
        .widths_right
        .as_ref()
        .map(|r| sample_profile(&positions, r, t));

    let idx = positions
        .iter()
        .position(|&p| p > t)
        .unwrap_or(positions.len());
    prof.positions.insert(idx, t);
    prof.widths.insert(idx, w);
    if let (Some(rights), Some(rv)) = (prof.widths_right.as_mut(), right) {
        rights.insert(idx, rv);
    }
    idx
}

/// Draw a diamond handle: filled when `selected`, outlined otherwise.
fn draw_diamond(
    painter: &egui::Painter,
    center: egui::Pos2,
    r: f32,
    fill: egui::Color32,
    outline: egui::Color32,
    selected: bool,
) {
    let pts = vec![
        egui::pos2(center.x, center.y - r),
        egui::pos2(center.x + r, center.y),
        egui::pos2(center.x, center.y + r),
        egui::pos2(center.x - r, center.y),
    ];
    let fill_col = if selected { fill } else { egui::Color32::WHITE };
    painter.add(egui::Shape::convex_polygon(
        pts,
        fill_col,
        egui::Stroke::new(1.5, if selected { outline } else { fill }),
    ));
}
