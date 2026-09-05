//! Interactive Area Trace tool: drag over a raster region and convert the
//! sampled pixels into one undoable editable vector (or color group).

use super::*;
use photonic_core::raster::trace::{trace_bitmap, TraceOptions};

#[derive(Debug, Clone, Copy, PartialEq)]
struct AreaTraceSettings {
    colors: u32,
    detail: f32,
    smoothing: f32,
    min_area: u32,
    ignore_white: bool,
}

#[derive(Debug, Clone)]
struct AreaTraceSample {
    pixels: Vec<[u8; 4]>,
    width: u32,
    height: u32,
    bounds: [f64; 4],
}

/// A non-destructive Area Trace adjustment session. `preview_nodes` are
/// temporarily installed in the document for the normal renderer to display,
/// but are absent from history until Apply is chosen.
#[derive(Debug, Clone)]
pub(crate) struct AreaTraceSession {
    source_id: NodeId,
    start: Point,
    end: Point,
    target_layer: LayerId,
    sample: AreaTraceSample,
    settings: AreaTraceSettings,
    pub(crate) preview_root: Option<NodeId>,
    preview_nodes: Vec<SceneNode>,
    generated_count: usize,
    previous_selection: Selection,
    previous_selected_id: Option<NodeId>,
}

impl PhotonicApp {
    pub(crate) fn handle_area_trace_tool(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        doc: &mut Document,
        view: &CanvasView,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        self.validate_area_trace_session(doc);

        if viewport_kb(ui.ctx()) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.area_trace_start = None;
            self.area_trace_source = None;
            self.cancel_area_trace_preview(doc, true);
            return;
        }
        if self.area_trace_session.is_some()
            && viewport_kb(ui.ctx())
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
        {
            if self.apply_area_trace_preview(doc, history) {
                *doc_modified = true;
            }
            return;
        }

        if self.area_trace_session.is_some() {
            if self.refresh_area_trace_preview_if_needed(doc) {
                ui.ctx().request_repaint();
            }
            if let Some(session) = &self.area_trace_session {
                paint_trace_region(
                    ui,
                    view,
                    session.start,
                    session.end,
                    true,
                    Some(format!(
                        "Live preview · {} shape{}",
                        session.generated_count,
                        if session.generated_count == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )),
                );
            }
        }

        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(current) = response.interact_pointer_pos() {
                self.cancel_area_trace_preview(doc, false);
                let press = current - response.drag_delta();
                let (x, y) = view.screen_to_canvas(press.x as f64, press.y as f64);
                let start = Point::new(x, y);
                self.area_trace_start = Some(start);
                self.area_trace_source = trace_raster_at(doc, start);
            }
        }

        if let (Some(start), Some(current)) =
            (self.area_trace_start, response.interact_pointer_pos())
        {
            let (cx, cy) = view.screen_to_canvas(current.x as f64, current.y as f64);
            if self.area_trace_source.is_none() {
                self.area_trace_source = trace_raster_for_area(doc, start, Point::new(cx, cy));
            }
            let size = format!("{:.0} × {:.0}", (cx - start.x).abs(), (cy - start.y).abs());
            paint_trace_region(
                ui,
                view,
                start,
                Point::new(cx, cy),
                self.area_trace_source.is_some(),
                Some(size),
            );
        }

        if response.drag_stopped_by(egui::PointerButton::Primary) {
            let start = self.area_trace_start.take();
            let source = self.area_trace_source.take();
            let end = response.interact_pointer_pos().map(|pos| {
                let (x, y) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                Point::new(x, y)
            });
            match (start, end, source) {
                (Some(start), Some(end), Some(source)) => {
                    if self.begin_area_trace_preview(doc, source, start, end) {
                        ui.ctx().request_repaint();
                    }
                }
                (Some(_), Some(_), None) => {
                    self.file_status = Some("Area Trace: begin the drag on raster artwork".into());
                }
                _ => {}
            }
        }
    }

    fn area_trace_settings(&self) -> AreaTraceSettings {
        AreaTraceSettings {
            colors: self.area_trace_colors.clamp(1, 24),
            detail: self.area_trace_detail.clamp(0.1, 1.0),
            smoothing: self.area_trace_smoothing.clamp(0.0, 8.0),
            min_area: self.area_trace_min_area.clamp(1, 128),
            ignore_white: self.area_trace_ignore_white,
        }
    }

    fn begin_area_trace_preview(
        &mut self,
        doc: &mut Document,
        source_id: NodeId,
        start: Point,
        end: Point,
    ) -> bool {
        let settings = self.area_trace_settings();
        let Some(source) = doc.nodes.get(&source_id) else {
            return false;
        };
        let Some(target_layer) = trace_target_layer(doc, source.layer_id) else {
            self.file_status = Some("Area Trace: unlock a destination layer first".into());
            return false;
        };
        let sample = match sample_trace_region(doc, source_id, start, end, settings.detail) {
            Ok(sample) => sample,
            Err(message) => {
                self.file_status = Some(message);
                return false;
            }
        };

        let mut session = AreaTraceSession {
            source_id,
            start,
            end,
            target_layer,
            sample,
            settings,
            preview_root: None,
            preview_nodes: Vec::new(),
            generated_count: 0,
            previous_selection: doc.selection.clone(),
            previous_selected_id: self.selected_id,
        };
        rebuild_trace_session(doc, &mut session);
        self.set_area_trace_preview_status(&session);
        self.area_trace_session = Some(session);
        true
    }

    fn refresh_area_trace_preview_if_needed(&mut self, doc: &mut Document) -> bool {
        let settings = self.area_trace_settings();
        let Some(mut session) = self.area_trace_session.take() else {
            return false;
        };
        if session.settings == settings {
            self.area_trace_session = Some(session);
            return false;
        }

        remove_trace_preview_nodes(doc, &session);
        if session.settings.detail != settings.detail {
            match sample_trace_region(
                doc,
                session.source_id,
                session.start,
                session.end,
                settings.detail,
            ) {
                Ok(sample) => session.sample = sample,
                Err(message) => {
                    self.file_status = Some(message);
                    return true;
                }
            }
        }
        session.settings = settings;
        rebuild_trace_session(doc, &mut session);
        self.set_area_trace_preview_status(&session);
        self.area_trace_session = Some(session);
        true
    }

    fn set_area_trace_preview_status(&mut self, session: &AreaTraceSession) {
        self.file_status = Some(if session.generated_count == 0 {
            "Area Trace preview is empty — adjust Ignore white or Minimum area".into()
        } else {
            format!(
                "Area Trace live preview: {} editable color shape{} · Apply when ready",
                session.generated_count,
                if session.generated_count == 1 {
                    ""
                } else {
                    "s"
                }
            )
        });
    }

    pub(crate) fn apply_area_trace_preview(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) -> bool {
        let Some(mut session) = self.area_trace_session.take() else {
            return false;
        };
        let Some(root_id) = session.preview_root else {
            self.file_status =
                Some("Area Trace: adjust the settings until a preview appears".into());
            self.area_trace_session = Some(session);
            return false;
        };
        if doc.is_layer_locked(&session.target_layer) {
            self.file_status =
                Some("Area Trace: unlock the destination layer before applying".into());
            self.area_trace_session = Some(session);
            return false;
        }

        remove_trace_preview_nodes(doc, &session);
        for (index, node) in session.preview_nodes.iter_mut().enumerate() {
            node.name = if node.id == root_id {
                if session.generated_count == 1 {
                    "Area Trace".into()
                } else {
                    format!("Area Trace ({} colors)", session.generated_count)
                }
            } else {
                format!("Trace color {}", index + 1)
            };
        }
        history.execute(
            Command::AddSubtree {
                layer_id: session.target_layer,
                roots: vec![root_id],
                nodes: session.preview_nodes,
            },
            doc,
        );
        doc.selection = Selection::single(root_id);
        self.selected_id = Some(root_id);
        self.file_status = Some(format!(
            "Area Trace created {} editable color shape{}",
            session.generated_count,
            if session.generated_count == 1 {
                ""
            } else {
                "s"
            }
        ));
        true
    }

    pub(crate) fn cancel_area_trace_preview(
        &mut self,
        doc: &mut Document,
        restore_selection: bool,
    ) {
        let Some(session) = self.area_trace_session.take() else {
            return;
        };
        remove_trace_preview_nodes(doc, &session);
        if restore_selection {
            doc.selection = Selection::from_ids(
                session
                    .previous_selection
                    .ids()
                    .copied()
                    .filter(|id| doc.nodes.contains_key(id)),
            );
            self.selected_id = session
                .previous_selected_id
                .filter(|id| doc.nodes.contains_key(id));
            self.file_status = Some("Area Trace preview canceled".into());
        } else {
            for node in &session.preview_nodes {
                doc.selection.remove(&node.id);
            }
        }
    }

    fn validate_area_trace_session(&mut self, doc: &mut Document) {
        let invalid = self.area_trace_session.as_ref().is_some_and(|session| {
            !matches!(
                doc.nodes.get(&session.source_id).map(|node| &node.kind),
                Some(SceneNodeKind::Raster(_))
            ) || !doc.layers.contains_key(&session.target_layer)
                || session
                    .preview_nodes
                    .iter()
                    .any(|node| !doc.nodes.contains_key(&node.id))
        });
        if invalid {
            self.cancel_area_trace_preview(doc, false);
            self.file_status = Some("Area Trace preview ended because its source changed".into());
        }
    }
}

fn sample_trace_region(
    doc: &Document,
    source_id: NodeId,
    start: Point,
    end: Point,
    detail: f32,
) -> Result<AreaTraceSample, String> {
    let x0 = start.x.min(end.x);
    let y0 = start.y.min(end.y);
    let x1 = start.x.max(end.x);
    let y1 = start.y.max(end.y);
    if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
        return Err("Area Trace: drag a larger region".into());
    }

    let source = doc
        .nodes
        .get(&source_id)
        .ok_or_else(|| "Area Trace: the source image is no longer available".to_string())?;
    let SceneNodeKind::Raster(raster) = &source.kind else {
        return Err("Area Trace: the source is no longer a raster image".into());
    };
    let [a, b, c, d, _, _] = source.transform.matrix;
    let determinant = a * d - b * c;
    if determinant.abs() < 1e-12 || !determinant.is_finite() {
        return Err("Area Trace: image transform is not invertible".into());
    }
    let inverse = source.transform.to_kurbo().inverse();
    let inv = inverse.as_coeffs();
    let detail = detail.clamp(0.1, 1.0) as f64;
    let local_per_canvas_x = inv[0].hypot(inv[1]).max(0.05);
    let local_per_canvas_y = inv[2].hypot(inv[3]).max(0.05);
    let mut width = ((x1 - x0) * local_per_canvas_x * detail)
        .round()
        .clamp(2.0, 384.0) as u32;
    let mut height = ((y1 - y0) * local_per_canvas_y * detail)
        .round()
        .clamp(2.0, 384.0) as u32;
    // Bound work and output complexity even for a huge high-DPI image.
    let cells = width as u64 * height as u64;
    if cells > 96_000 {
        let scale = (96_000.0 / cells as f64).sqrt();
        width = (width as f64 * scale).round().max(2.0) as u32;
        height = (height as f64 * scale).round().max(2.0) as u32;
    }

    let mut pixels = Vec::with_capacity(width as usize * height as usize);
    let mask = raster
        .mask
        .as_ref()
        .filter(|mask| mask.width == raster.image.width && mask.height == raster.image.height);
    for sy in 0..height {
        let canvas_y = y0 + (sy as f64 + 0.5) / height as f64 * (y1 - y0);
        for sx in 0..width {
            let canvas_x = x0 + (sx as f64 + 0.5) / width as f64 * (x1 - x0);
            let local = inverse * Point::new(canvas_x, canvas_y);
            if local.x < 0.0
                || local.y < 0.0
                || local.x >= raster.image.width as f64
                || local.y >= raster.image.height as f64
            {
                pixels.push([0, 0, 0, 0]);
                continue;
            }
            let px = local.x.floor() as u32;
            let py = local.y.floor() as u32;
            let mut rgba = raster.image.pixel(px, py);
            let mut alpha = rgba[3] as f32 * source.opacity.clamp(0.0, 1.0);
            if let Some(mask) = mask {
                alpha *= mask.coverage(px, py);
            }
            rgba[3] = alpha.round().clamp(0.0, 255.0) as u8;
            pixels.push(rgba);
        }
    }

    Ok(AreaTraceSample {
        pixels,
        width,
        height,
        bounds: [x0, y0, x1, y1],
    })
}

fn rebuild_trace_session(doc: &mut Document, session: &mut AreaTraceSession) {
    session.preview_root = None;
    session.preview_nodes.clear();
    session.generated_count = 0;

    let shapes = trace_bitmap(
        &session.sample.pixels,
        session.sample.width,
        session.sample.height,
        session.sample.bounds,
        TraceOptions {
            colors: session.settings.colors as usize,
            alpha_threshold: 16,
            min_area: session.settings.min_area,
            smoothing: session.settings.smoothing as f64,
            ignore_white: session.settings.ignore_white,
        },
    );
    if shapes.is_empty() {
        return;
    }

    session.generated_count = shapes.len();
    let mut children = Vec::with_capacity(session.generated_count);
    for (index, shape) in shapes.into_iter().enumerate() {
        let color = Color::new(
            shape.rgba[0] as f32 / 255.0,
            shape.rgba[1] as f32 / 255.0,
            shape.rgba[2] as f32 / 255.0,
            shape.rgba[3] as f32 / 255.0,
        );
        let mut path = PathNode::new(shape.path);
        path.fill = Fill::solid(color);
        path.is_compound = true;
        children.push(SceneNode::new(
            if session.generated_count == 1 {
                "Area Trace Preview".to_string()
            } else {
                format!("Trace preview color {}", index + 1)
            },
            session.target_layer,
            SceneNodeKind::Path(path),
        ));
    }

    let root_id = if children.len() == 1 {
        children[0].id
    } else {
        let child_ids = children.iter().map(|node| node.id).collect();
        let mut group = GroupNode::new();
        group.children = child_ids;
        let group_node = SceneNode::new(
            format!("Area Trace Preview ({} colors)", children.len()),
            session.target_layer,
            SceneNodeKind::Group(group),
        );
        let root_id = group_node.id;
        children.push(group_node);
        root_id
    };

    for node in &children {
        doc.nodes.insert(node.id, node.clone());
    }
    if let Some(layer) = doc.layers.get_mut(&session.target_layer) {
        layer.node_ids.push(root_id);
    }
    session.preview_root = Some(root_id);
    session.preview_nodes = children;
}

fn remove_trace_preview_nodes(doc: &mut Document, session: &AreaTraceSession) {
    for node in &session.preview_nodes {
        doc.nodes.remove(&node.id);
    }
    if let (Some(root), Some(layer)) = (
        session.preview_root,
        doc.layers.get_mut(&session.target_layer),
    ) {
        layer.node_ids.retain(|id| *id != root);
    }
}

fn trace_target_layer(doc: &Document, source_layer: LayerId) -> Option<LayerId> {
    if !doc.is_layer_locked(&source_layer) {
        Some(source_layer)
    } else {
        doc.active_layer_id
            .filter(|id| !doc.is_layer_locked(id))
            .or_else(|| {
                doc.layer_order
                    .iter()
                    .rev()
                    .copied()
                    .find(|id| !doc.is_layer_locked(id))
            })
    }
}

fn paint_trace_region(
    ui: &egui::Ui,
    view: &CanvasView,
    start: Point,
    end: Point,
    source_found: bool,
    label: Option<String>,
) {
    let (sx, sy) = view.canvas_to_screen(start.x, start.y);
    let (ex, ey) = view.canvas_to_screen(end.x, end.y);
    let trace_rect = egui::Rect::from_two_pos(
        egui::pos2(sx as f32, sy as f32),
        egui::pos2(ex as f32, ey as f32),
    );
    let painter = ui.painter();
    painter.rect_filled(
        trace_rect,
        0.0,
        egui::Color32::from_rgba_unmultiplied(91, 124, 250, 24),
    );
    painter.rect_stroke(
        trace_rect,
        0.0,
        egui::Stroke::new(
            1.5,
            if source_found {
                egui::Color32::from_rgb(111, 145, 255)
            } else {
                egui::Color32::from_rgb(240, 92, 92)
            },
        ),
    );
    if let Some(label) = label {
        painter.text(
            trace_rect.left_top() + egui::vec2(5.0, 5.0),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::monospace(11.0),
            egui::Color32::WHITE,
        );
    }
}

fn traceable_raster<'a>(
    doc: &Document,
    node: &'a SceneNode,
) -> Option<&'a photonic_core::node::RasterNode> {
    let SceneNodeKind::Raster(raster) = &node.kind else {
        return None;
    };
    if !node.visible || raster.is_adjustment_layer() {
        return None;
    }
    let layer_visible = doc
        .layers
        .get(&node.layer_id)
        .is_some_and(|layer| layer.visible);
    if !layer_visible {
        return None;
    }
    let [a, b, c, d, _, _] = node.transform.matrix;
    if (a * d - b * c).abs() < 1e-12 {
        return None;
    }
    Some(raster)
}

fn trace_raster_at(doc: &Document, point: Point) -> Option<NodeId> {
    doc.nodes_in_draw_order()
        .into_iter()
        .rev()
        .find_map(|node| {
            let raster = traceable_raster(doc, node)?;
            let local = node.transform.to_kurbo().inverse() * point;
            (local.x >= 0.0
                && local.y >= 0.0
                && local.x < raster.image.width as f64
                && local.y < raster.image.height as f64)
                .then_some(node.id)
        })
}

/// Find the topmost raster touched by an axis-aligned trace region. Point hits
/// are tried first for intuitive overlap selection, followed by transformed
/// raster bounds so a user can drag around an image from the empty surround.
fn trace_raster_for_area(doc: &Document, start: Point, end: Point) -> Option<NodeId> {
    let x0 = start.x.min(end.x);
    let y0 = start.y.min(end.y);
    let x1 = start.x.max(end.x);
    let y1 = start.y.max(end.y);
    let probes = [
        start,
        end,
        Point::new((x0 + x1) * 0.5, (y0 + y1) * 0.5),
        Point::new(x0, y1),
        Point::new(x1, y0),
    ];
    for probe in probes {
        if let Some(id) = trace_raster_at(doc, probe) {
            return Some(id);
        }
    }

    doc.nodes_in_draw_order()
        .into_iter()
        .rev()
        .find_map(|node| {
            let raster = traceable_raster(doc, node)?;
            let w = raster.image.width as f64;
            let h = raster.image.height as f64;
            let corners = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
            let mut bx0 = f64::INFINITY;
            let mut by0 = f64::INFINITY;
            let mut bx1 = f64::NEG_INFINITY;
            let mut by1 = f64::NEG_INFINITY;
            for (x, y) in corners {
                let (tx, ty) = node.transform.apply(x, y);
                bx0 = bx0.min(tx);
                by0 = by0.min(ty);
                bx1 = bx1.max(tx);
                by1 = by1.max(ty);
            }
            (bx1 > x0 && bx0 < x1 && by1 > y0 && by0 < y1).then_some(node.id)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::raster::image::RasterImage;

    #[test]
    fn hit_test_prefers_topmost_raster() {
        let mut doc = Document::new("t", 20.0, 20.0);
        let layer = doc.layer_order[0];
        let bottom = SceneNode::new(
            "bottom",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                10,
                [255, 0, 0, 255],
            ))),
        );
        let top = SceneNode::new(
            "top",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                10,
                [0, 0, 255, 255],
            ))),
        );
        let top_id = top.id;
        doc.add_node(bottom, Some(layer));
        doc.add_node(top, Some(layer));
        assert_eq!(trace_raster_at(&doc, Point::new(5.0, 5.0)), Some(top_id));
    }

    #[test]
    fn area_hit_test_allows_dragging_from_outside_the_image() {
        let mut doc = Document::new("t", 30.0, 30.0);
        let layer = doc.layer_order[0];
        let node = SceneNode::new(
            "image",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                1,
                [30, 40, 50, 255],
            ))),
        )
        .with_transform(photonic_core::Transform::translate(10.0, 0.0));
        let id = node.id;
        doc.add_node(node, Some(layer));
        assert_eq!(
            trace_raster_for_area(&doc, Point::new(0.0, 0.0), Point::new(20.0, 2.0)),
            Some(id)
        );
    }

    #[test]
    fn area_hit_test_skips_hidden_nodes_and_layers() {
        let mut doc = Document::new("t", 30.0, 30.0);
        let layer = doc.layer_order[0];
        let mut node = SceneNode::new(
            "hidden node",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                1,
                [30, 40, 50, 255],
            ))),
        )
        .with_transform(photonic_core::Transform::translate(10.0, 0.0));
        node.visible = false;
        doc.add_node(node, Some(layer));
        assert_eq!(
            trace_raster_for_area(&doc, Point::new(0.0, 0.0), Point::new(20.0, 2.0)),
            None
        );

        let mut doc = Document::new("t", 30.0, 30.0);
        let layer = doc.layer_order[0];
        doc.layers.get_mut(&layer).expect("default layer").visible = false;
        let node = SceneNode::new(
            "hidden layer",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                1,
                [30, 40, 50, 255],
            ))),
        )
        .with_transform(photonic_core::Transform::translate(10.0, 0.0));
        doc.add_node(node, Some(layer));
        assert_eq!(
            trace_raster_for_area(&doc, Point::new(0.0, 0.0), Point::new(20.0, 2.0)),
            None
        );
    }

    #[test]
    fn area_hit_test_skips_singular_transform() {
        let mut doc = Document::new("t", 30.0, 30.0);
        let layer = doc.layer_order[0];
        let node = SceneNode::new(
            "singular",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                1,
                [30, 40, 50, 255],
            ))),
        )
        .with_transform(photonic_core::Transform::new(0.0, 0.0, 0.0, 1.0, 10.0, 0.0));
        doc.add_node(node, Some(layer));
        assert_eq!(
            trace_raster_for_area(&doc, Point::new(0.0, 0.0), Point::new(20.0, 2.0)),
            None
        );
    }

    #[test]
    fn area_hit_test_skips_hidden_top_raster_for_visible_overlap() {
        let mut doc = Document::new("t", 30.0, 30.0);
        let layer = doc.layer_order[0];
        let bottom = SceneNode::new(
            "visible bottom",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                1,
                [30, 40, 50, 255],
            ))),
        )
        .with_transform(photonic_core::Transform::translate(10.0, 0.0));
        let bottom_id = bottom.id;
        let mut top = SceneNode::new(
            "hidden top",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                1,
                [60, 70, 80, 255],
            ))),
        )
        .with_transform(photonic_core::Transform::translate(10.0, 0.0));
        top.visible = false;
        doc.add_node(bottom, Some(layer));
        doc.add_node(top, Some(layer));

        assert_eq!(
            trace_raster_for_area(&doc, Point::new(0.0, 0.0), Point::new(20.0, 2.0)),
            Some(bottom_id)
        );
    }

    #[test]
    fn area_trace_preview_refreshes_live_and_applies_as_one_undoable_group() {
        let mut doc = Document::new("t", 20.0, 20.0);
        let layer = doc.layer_order[0];
        let mut image = RasterImage::filled(10, 10, [255, 0, 0, 255]);
        for y in 0..10 {
            for x in 5..10 {
                image.set_pixel(x, y, [0, 0, 255, 255]);
            }
        }
        let raster = SceneNode::new(
            "source",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(image)),
        );
        let source_id = raster.id;
        doc.add_node(raster, Some(layer));
        let before = doc.nodes.len();
        let mut app = PhotonicApp::default();
        app.area_trace_colors = 2;
        app.area_trace_detail = 1.0;
        app.area_trace_ignore_white = false;
        app.area_trace_smoothing = 0.0;
        let mut history = CommandHistory::new(20);

        assert!(app.begin_area_trace_preview(
            &mut doc,
            source_id,
            Point::new(0.0, 0.0),
            Point::new(10.0, 10.0),
        ));
        assert_eq!(history.undo_depth(), 0, "preview must not touch history");
        assert_eq!(doc.nodes.len(), before + 3);
        let preview_root = app
            .area_trace_session
            .as_ref()
            .and_then(|session| session.preview_root)
            .expect("preview root");

        app.area_trace_smoothing = 4.0;
        app.refresh_area_trace_preview_if_needed(&mut doc);
        let refreshed_root = app
            .area_trace_session
            .as_ref()
            .and_then(|session| session.preview_root)
            .expect("refreshed preview root");
        assert_ne!(preview_root, refreshed_root);
        assert!(!doc.nodes.contains_key(&preview_root));
        assert_eq!(doc.nodes.len(), before + 3);
        assert_eq!(history.undo_depth(), 0);

        assert!(app.apply_area_trace_preview(&mut doc, &mut history));
        assert!(app.area_trace_session.is_none());
        let root = app.selected_id.expect("trace selected");
        let SceneNodeKind::Group(group) = &doc.nodes[&root].kind else {
            panic!("two colors should create a group");
        };
        assert_eq!(group.children.len(), 2);
        assert_eq!(doc.nodes.len(), before + 3);

        assert!(history.undo(&mut doc));
        assert_eq!(doc.nodes.len(), before);
        assert!(doc.nodes.contains_key(&source_id));
    }

    #[test]
    fn area_trace_cancel_removes_preview_without_history() {
        let mut doc = Document::new("t", 20.0, 20.0);
        let layer = doc.layer_order[0];
        let raster = SceneNode::new(
            "source",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                10,
                [20, 30, 40, 255],
            ))),
        );
        let source_id = raster.id;
        doc.add_node(raster, Some(layer));
        let before = doc.nodes.len();
        let mut app = PhotonicApp::default();
        app.area_trace_ignore_white = false;
        app.area_trace_smoothing = 0.0;
        let history = CommandHistory::new(20);

        assert!(app.begin_area_trace_preview(
            &mut doc,
            source_id,
            Point::new(0.0, 0.0),
            Point::new(10.0, 10.0),
        ));
        assert!(doc.nodes.len() > before);
        app.cancel_area_trace_preview(&mut doc, true);

        assert_eq!(doc.nodes.len(), before);
        assert_eq!(history.undo_depth(), 0);
        assert!(app.area_trace_session.is_none());
    }

    #[test]
    fn empty_preview_can_become_visible_from_live_setting_change() {
        let mut doc = Document::new("t", 20.0, 20.0);
        let layer = doc.layer_order[0];
        let raster = SceneNode::new(
            "white source",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                10,
                [255, 255, 255, 255],
            ))),
        );
        let source_id = raster.id;
        doc.add_node(raster, Some(layer));
        let mut app = PhotonicApp::default();
        app.area_trace_ignore_white = true;

        assert!(app.begin_area_trace_preview(
            &mut doc,
            source_id,
            Point::new(0.0, 0.0),
            Point::new(10.0, 10.0),
        ));
        assert!(app
            .area_trace_session
            .as_ref()
            .is_some_and(|session| session.preview_root.is_none()));

        app.area_trace_ignore_white = false;
        app.refresh_area_trace_preview_if_needed(&mut doc);
        assert!(app
            .area_trace_session
            .as_ref()
            .is_some_and(|session| session.preview_root.is_some()));
    }

    #[test]
    fn switching_documents_does_not_park_transient_preview_nodes() {
        let mut doc = Document::new("first", 20.0, 20.0);
        let layer = doc.layer_order[0];
        let raster = SceneNode::new(
            "source",
            layer,
            SceneNodeKind::Raster(photonic_core::node::RasterNode::new(RasterImage::filled(
                10,
                10,
                [20, 30, 40, 255],
            ))),
        );
        let source_id = raster.id;
        doc.add_node(raster, Some(layer));
        let committed_node_count = doc.nodes.len();
        let mut history = CommandHistory::new(20);
        let mut view = CanvasView::default();
        let mut app = PhotonicApp::default();
        app.area_trace_ignore_white = false;
        app.ensure_initial_tab(&doc, &history);
        assert!(app.begin_area_trace_preview(
            &mut doc,
            source_id,
            Point::new(0.0, 0.0),
            Point::new(10.0, 10.0),
        ));
        assert!(doc.nodes.len() > committed_node_count);

        app.open_in_new_tab(
            &mut doc,
            &mut history,
            &mut view,
            Document::new("second", 20.0, 20.0),
            CommandHistory::new(20),
            None,
        );

        assert!(app.area_trace_session.is_none());
        assert_eq!(app.tabs[0].document.nodes.len(), committed_node_count);
        assert!(app.tabs[0].document.nodes.contains_key(&source_id));
    }
}
