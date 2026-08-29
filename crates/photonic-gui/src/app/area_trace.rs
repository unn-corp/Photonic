//! Interactive Area Trace tool: drag over a raster region and convert the
//! sampled pixels into one undoable editable vector (or color group).

use super::*;
use photonic_core::raster::trace::{trace_bitmap, TraceOptions};

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

        if viewport_kb(ui.ctx()) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.area_trace_start = None;
            self.area_trace_source = None;
            return;
        }

        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(current) = response.interact_pointer_pos() {
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
            let (sx, sy) = view.canvas_to_screen(start.x, start.y);
            let trace_rect = egui::Rect::from_two_pos(
                egui::pos2(sx as f32, sy as f32),
                egui::pos2(current.x, current.y),
            );
            let painter = ui.painter();
            painter.rect_filled(
                trace_rect,
                0.0,
                egui::Color32::from_rgba_unmultiplied(91, 124, 250, 28),
            );
            painter.rect_stroke(
                trace_rect,
                0.0,
                egui::Stroke::new(
                    1.5,
                    if self.area_trace_source.is_some() {
                        egui::Color32::from_rgb(111, 145, 255)
                    } else {
                        egui::Color32::from_rgb(240, 92, 92)
                    },
                ),
            );
            let size = format!("{:.0} × {:.0}", (cx - start.x).abs(), (cy - start.y).abs());
            painter.text(
                trace_rect.left_top() + egui::vec2(5.0, 5.0),
                egui::Align2::LEFT_TOP,
                size,
                egui::FontId::monospace(11.0),
                egui::Color32::WHITE,
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
                    if self.commit_area_trace(doc, history, source, start, end) {
                        *doc_modified = true;
                    }
                }
                (Some(_), Some(_), None) => {
                    self.file_status = Some("Area Trace: begin the drag on raster artwork".into());
                }
                _ => {}
            }
        }
    }

    fn commit_area_trace(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        source_id: NodeId,
        start: Point,
        end: Point,
    ) -> bool {
        let x0 = start.x.min(end.x);
        let y0 = start.y.min(end.y);
        let x1 = start.x.max(end.x);
        let y1 = start.y.max(end.y);
        if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
            self.file_status = Some("Area Trace: drag a larger region".into());
            return false;
        }

        let Some(source) = doc.nodes.get(&source_id).cloned() else {
            return false;
        };
        let SceneNodeKind::Raster(raster) = &source.kind else {
            return false;
        };
        let [a, b, c, d, _, _] = source.transform.matrix;
        let determinant = a * d - b * c;
        if determinant.abs() < 1e-12 || !determinant.is_finite() {
            self.file_status = Some("Area Trace: image transform is not invertible".into());
            return false;
        }
        let inverse = source.transform.to_kurbo().inverse();
        let inv = inverse.as_coeffs();
        let detail = self.area_trace_detail.clamp(0.1, 1.0) as f64;
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

        let mut samples = Vec::with_capacity(width as usize * height as usize);
        let mask_valid = raster.mask.as_ref().is_some_and(|mask| {
            mask.width == raster.image.width && mask.height == raster.image.height
        });
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
                    samples.push([0, 0, 0, 0]);
                    continue;
                }
                let px = local.x.floor() as u32;
                let py = local.y.floor() as u32;
                let mut rgba = raster.image.pixel(px, py);
                let mut alpha = rgba[3] as f32 * source.opacity.clamp(0.0, 1.0);
                if mask_valid {
                    alpha *= raster.mask.as_ref().unwrap().coverage(px, py);
                }
                rgba[3] = alpha.round().clamp(0.0, 255.0) as u8;
                samples.push(rgba);
            }
        }

        let shapes = trace_bitmap(
            &samples,
            width,
            height,
            [x0, y0, x1, y1],
            TraceOptions {
                colors: self.area_trace_colors as usize,
                alpha_threshold: 16,
                min_area: self.area_trace_min_area,
                smoothing: self.area_trace_smoothing as f64,
                ignore_white: self.area_trace_ignore_white,
            },
        );
        if shapes.is_empty() {
            self.file_status = Some(
                "Area Trace found no visible pixels (try disabling Ignore white or lowering Minimum area)"
                    .into(),
            );
            return false;
        }

        let target_layer = if !doc.is_layer_locked(&source.layer_id) {
            Some(source.layer_id)
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
        };
        let Some(target_layer) = target_layer else {
            self.file_status = Some("Area Trace: unlock a destination layer first".into());
            return false;
        };

        let generated_count = shapes.len();
        let mut children = Vec::with_capacity(generated_count);
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
                if generated_count == 1 {
                    "Area Trace".to_string()
                } else {
                    format!("Trace color {}", index + 1)
                },
                target_layer,
                SceneNodeKind::Path(path),
            ));
        }

        let (root_id, nodes) = if children.len() == 1 {
            let root_id = children[0].id;
            (root_id, children)
        } else {
            let child_ids = children.iter().map(|node| node.id).collect();
            let mut group = GroupNode::new();
            group.children = child_ids;
            let group_node = SceneNode::new(
                format!("Area Trace ({} colors)", children.len()),
                target_layer,
                SceneNodeKind::Group(group),
            );
            let root_id = group_node.id;
            children.push(group_node);
            (root_id, children)
        };
        history.execute(
            Command::AddSubtree {
                layer_id: target_layer,
                roots: vec![root_id],
                nodes,
            },
            doc,
        );
        doc.selection = Selection::single(root_id);
        self.selected_id = Some(root_id);
        self.file_status = Some(format!(
            "Area Trace created {generated_count} editable color shape{}",
            if generated_count == 1 { "" } else { "s" }
        ));
        true
    }
}

fn trace_raster_at(doc: &Document, point: Point) -> Option<NodeId> {
    doc.nodes_in_draw_order()
        .into_iter()
        .rev()
        .find_map(|node| {
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
            let SceneNodeKind::Raster(raster) = &node.kind else {
                return None;
            };
            if raster.is_adjustment_layer() {
                return None;
            }
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
                10,
                [30, 40, 50, 255],
            ))),
        )
        .with_transform(photonic_core::Transform::translate(10.0, 10.0));
        let id = node.id;
        doc.add_node(node, Some(layer));
        assert_eq!(
            trace_raster_for_area(&doc, Point::new(5.0, 5.0), Point::new(25.0, 25.0)),
            Some(id)
        );
    }

    #[test]
    fn area_trace_commit_is_one_undoable_vector_group() {
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

        assert!(app.commit_area_trace(
            &mut doc,
            &mut history,
            source_id,
            Point::new(0.0, 0.0),
            Point::new(10.0, 10.0),
        ));
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
}
