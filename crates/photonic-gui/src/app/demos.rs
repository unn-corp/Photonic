//! Built-in parametric demo artwork generators (charts, tilings) extracted
//! from app::mod. Each builds nodes into the document on request.
#![allow(clippy::too_many_arguments)]
use super::*;

pub(crate) fn gui_create_radar_chart_demo(
    cx: f64,
    cy: f64,
    doc: &mut Document,
    history: &mut CommandHistory,
    doc_modified: &mut bool,
) {
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind};

    let radius = 100.0_f64;
    let grid_rings = 4_usize;
    let n_axes = 5_usize;
    let series_data: &[(&str, &[f64], Color)] = &[
        (
            "Alpha",
            &[80.0, 60.0, 90.0, 50.0, 70.0],
            Color::from_hex("#4E79A7").unwrap_or(Color::new(0.31, 0.47, 0.65, 1.0)),
        ),
        (
            "Beta",
            &[50.0, 80.0, 40.0, 75.0, 55.0],
            Color::from_hex("#F28E2B").unwrap_or(Color::new(0.95, 0.56, 0.17, 1.0)),
        ),
    ];

    let axis_angle = |i: usize| -> f64 {
        -std::f64::consts::FRAC_PI_2 + (i as f64 / n_axes as f64) * std::f64::consts::TAU
    };

    let layer_id = doc.active_layer_id.unwrap_or(uuid::Uuid::nil());
    let mut child_ids: Vec<uuid::Uuid> = Vec::new();

    // Grid rings
    for ring in 1..=grid_rings {
        let r = radius * (ring as f64 / grid_rings as f64);
        let mut bez = BezPath::new();
        for i in 0..n_axes {
            let angle = axis_angle(i);
            let pt = Point::new(cx + r * angle.cos(), cy + r * angle.sin());
            if i == 0 {
                bez.move_to(pt);
            } else {
                bez.line_to(pt);
            }
        }
        bez.close_path();
        let mut pn = PathNode::new(PathData::from_bez_path(&bez));
        pn.fill = Fill {
            kind: FillKind::None,
            ..Default::default()
        };
        pn.stroke = Stroke::solid(Color::new(0.7, 0.7, 0.75, 1.0), 0.75);
        let node = SceneNode::new(
            format!("Grid Ring {ring}"),
            layer_id,
            SceneNodeKind::Path(pn),
        );
        child_ids.push(node.id);
        history.execute(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            doc,
        );
    }

    // Axis lines
    for i in 0..n_axes {
        let angle = axis_angle(i);
        let tip = Point::new(cx + radius * angle.cos(), cy + radius * angle.sin());
        let mut bez = BezPath::new();
        bez.move_to(Point::new(cx, cy));
        bez.line_to(tip);
        let mut pn = PathNode::new(PathData::from_bez_path(&bez));
        pn.fill = Fill {
            kind: FillKind::None,
            ..Default::default()
        };
        pn.stroke = Stroke::solid(Color::new(0.7, 0.7, 0.75, 1.0), 0.75);
        let node = SceneNode::new(format!("Axis {}", i + 1), layer_id, SceneNodeKind::Path(pn));
        child_ids.push(node.id);
        history.execute(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            doc,
        );
    }

    // Series polygons
    let axis_max = 100.0_f64; // both series scaled to 0–100
    for (name, values, color) in series_data {
        let mut bez = BezPath::new();
        for (ai, &val) in values.iter().enumerate() {
            let r = radius * (val / axis_max).clamp(0.0, 1.0);
            let angle = axis_angle(ai);
            let pt = Point::new(cx + r * angle.cos(), cy + r * angle.sin());
            if ai == 0 {
                bez.move_to(pt);
            } else {
                bez.line_to(pt);
            }
        }
        bez.close_path();
        let mut pn = PathNode::new(PathData::from_bez_path(&bez));
        pn.fill = Fill {
            kind: FillKind::Solid(Color::new(color.r, color.g, color.b, 0.2)),
            ..Default::default()
        };
        pn.stroke = Stroke::solid(*color, 1.5);
        let node = SceneNode::new(*name, layer_id, SceneNodeKind::Path(pn));
        child_ids.push(node.id);
        history.execute(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            doc,
        );
    }

    let group = SceneNode::new(
        "Radar Chart",
        layer_id,
        SceneNodeKind::Group(GroupNode::new()),
    );
    history.execute(
        Command::GroupNodes {
            group,
            layer_id,
            insert_index: 0,
            children: child_ids,
        },
        doc,
    );

    *doc_modified = true;
}

/// Create a sample 3-series stacked column chart for the GUI demo button.
pub(crate) fn gui_create_stacked_bar_chart_demo(
    x: f64,
    y: f64,
    doc: &mut Document,
    history: &mut CommandHistory,
    doc_modified: &mut bool,
) {
    use kurbo::Shape;
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind};

    let chart_w = 300.0_f64;
    let chart_h = 200.0_f64;
    let gap_frac = 0.2_f64;
    let series_data: &[(&str, &[f64], Color)] = &[
        (
            "Alpha",
            &[40.0, 55.0, 30.0, 65.0],
            Color::from_hex("#4E79A7").unwrap_or(Color::new(0.31, 0.47, 0.65, 1.0)),
        ),
        (
            "Beta",
            &[30.0, 25.0, 45.0, 20.0],
            Color::from_hex("#F28E2B").unwrap_or(Color::new(0.95, 0.56, 0.17, 1.0)),
        ),
        (
            "Gamma",
            &[20.0, 15.0, 20.0, 10.0],
            Color::from_hex("#E15759").unwrap_or(Color::new(0.88, 0.34, 0.35, 1.0)),
        ),
    ];
    let n_stacks = 4_usize;

    let max_total = (0..n_stacks)
        .map(|ci| series_data.iter().map(|(_, vals, _)| vals[ci]).sum::<f64>())
        .fold(0.0_f64, f64::max);
    if max_total <= 0.0 {
        return;
    }

    let bar_total = chart_w / n_stacks as f64;
    let bar_w = bar_total * (1.0 - gap_frac);
    let bar_gap = bar_total * gap_frac;

    let layer_id = doc.active_layer_id.unwrap_or(uuid::Uuid::nil());
    let mut child_ids: Vec<uuid::Uuid> = Vec::new();

    for ci in 0..n_stacks {
        let bx = x + (ci as f64 * bar_total) + bar_gap / 2.0;
        let mut cursor_y = y;
        for (sname, vals, color) in series_data {
            let val = vals[ci];
            if val <= 0.0 {
                continue;
            }
            let seg_h = (val / max_total) * chart_h;
            let rect = kurbo::Rect::new(bx, cursor_y - seg_h, bx + bar_w, cursor_y);
            let mut pn = PathNode::new(PathData::from_bez_path(&rect.to_path(0.0)));
            pn.fill = Fill {
                kind: FillKind::Solid(*color),
                ..Default::default()
            };
            pn.stroke = Stroke::none();
            let node = SceneNode::new(
                format!("{sname} / Bar {}", ci + 1),
                layer_id,
                SceneNodeKind::Path(pn),
            );
            child_ids.push(node.id);
            history.execute(
                Command::AddNode {
                    node,
                    layer_id: Some(layer_id),
                },
                doc,
            );
            cursor_y -= seg_h;
        }
    }

    let group = SceneNode::new(
        "Stacked Column Chart",
        layer_id,
        SceneNodeKind::Group(GroupNode::new()),
    );
    history.execute(
        Command::GroupNodes {
            group,
            layer_id,
            insert_index: 0,
            children: child_ids,
        },
        doc,
    );

    *doc_modified = true;
}

/// Create a parametric shape demo (Lissajous / Superellipse / Rose) at canvas center.
pub(crate) fn gui_create_parametric_shape_demo(
    shape_type: &str,
    cx: f64,
    cy: f64,
    doc: &mut Document,
    history: &mut CommandHistory,
    doc_modified: &mut bool,
) {
    use std::f64::consts::{PI, TAU};

    let radius = 100.0_f64;
    let n_pts = 360_usize;

    let (pts, label, fill_color, stroke_color): (Vec<(f64, f64)>, &str, Color, Color) =
        match shape_type {
            "lissajous" => {
                let freq_a = 3.0_f64;
                let freq_b = 2.0_f64;
                let delta = PI / 4.0_f64;
                let pts = (0..n_pts)
                    .map(|i| {
                        let t = i as f64 / n_pts as f64 * TAU;
                        (
                            radius * (freq_a * t + delta).sin(),
                            radius * (freq_b * t).sin(),
                        )
                    })
                    .collect();
                (
                    pts,
                    "Lissajous (3:2)",
                    Color::new(0.27, 0.51, 0.71, 0.63),
                    Color::new(0.12, 0.31, 0.55, 0.86),
                )
            }
            "superellipse" => {
                let n = 2.5_f64;
                let pts = (0..n_pts)
                    .map(|i| {
                        let t = i as f64 / n_pts as f64 * TAU;
                        let cos_t = t.cos();
                        let sin_t = t.sin();
                        let x = radius * cos_t.signum() * cos_t.abs().powf(2.0 / n);
                        let y = radius * sin_t.signum() * sin_t.abs().powf(2.0 / n);
                        (x, y)
                    })
                    .collect();
                (
                    pts,
                    "Superellipse (n=2.5)",
                    Color::new(0.78, 0.39, 0.24, 0.63),
                    Color::new(0.63, 0.24, 0.08, 0.86),
                )
            }
            _ => {
                // "rose" or default
                let k = 5.0_f64;
                let t_max = PI; // odd k -> integrate over PI for a closed rose
                let pts = (0..n_pts)
                    .map(|i| {
                        let t = i as f64 / n_pts as f64 * t_max;
                        let r = radius * (k * t).cos();
                        (r * t.cos(), r * t.sin())
                    })
                    .collect();
                (
                    pts,
                    "Rose Curve (k=5)",
                    Color::new(0.78, 0.24, 0.47, 0.63),
                    Color::new(0.63, 0.08, 0.31, 0.86),
                )
            }
        };

    if pts.is_empty() {
        return;
    }

    let mut bez = BezPath::new();
    for (i, (px, py)) in pts.iter().enumerate() {
        let pt = Point::new(cx + px, cy + py);
        if i == 0 {
            bez.move_to(pt);
        } else {
            bez.line_to(pt);
        }
    }
    bez.close_path();

    let mut pn = photonic_core::node::PathNode::new(photonic_core::PathData::from_bez_path(&bez));
    pn.fill = Fill::solid(fill_color);
    pn.stroke = Stroke::solid(stroke_color, 1.5);

    let layer_id = doc.active_layer_id.unwrap_or(uuid::Uuid::nil());

    let node = SceneNode::new(label, layer_id, SceneNodeKind::Path(pn));
    history.execute(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        doc,
    );

    *doc_modified = true;
}

/// Generate a demo Truchet tiling at the given position.
pub(crate) fn gui_create_truchet_tiling_demo(
    style: &str,
    x: f64,
    y: f64,
    size: f64,
    doc: &mut Document,
    history: &mut CommandHistory,
    doc_modified: &mut bool,
) {
    let ts = 32.0_f64;
    let cols = (size / ts).floor() as usize;
    let rows = cols;
    if cols == 0 || rows == 0 {
        return;
    }

    let tile_color = Color::new(0.10, 0.10, 0.18, 1.0);
    let sw = 2.0_f64;

    // Simple LCG for reproducible demo pattern.
    let mut rng: u64 = 42u64
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut next_bool = move || -> bool {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (rng >> 33) & 1 == 0
    };

    let layer_id = doc.active_layer_id.unwrap_or(uuid::Uuid::nil());
    let mut child_ids: Vec<photonic_core::node::NodeId> = Vec::new();

    for row in 0..rows {
        for col in 0..cols {
            let tx = x + col as f64 * ts;
            let ty = y + row as f64 * ts;
            let flip = next_bool();

            let mut bez = BezPath::new();

            match style {
                "triangles" => {
                    if flip {
                        bez.move_to(Point::new(tx, ty));
                        bez.line_to(Point::new(tx + ts, ty));
                        bez.line_to(Point::new(tx, ty + ts));
                    } else {
                        bez.move_to(Point::new(tx + ts, ty));
                        bez.line_to(Point::new(tx + ts, ty + ts));
                        bez.line_to(Point::new(tx, ty + ts));
                    }
                    bez.close_path();
                }
                _ => {
                    // "arcs"
                    let mid = ts / 2.0;
                    let k = mid * 0.5523;
                    if flip {
                        bez.move_to(Point::new(tx + mid, ty));
                        bez.curve_to(
                            Point::new(tx + mid - k, ty),
                            Point::new(tx, ty + mid - k),
                            Point::new(tx, ty + mid),
                        );
                        bez.move_to(Point::new(tx + mid, ty + ts));
                        bez.curve_to(
                            Point::new(tx + mid + k, ty + ts),
                            Point::new(tx + ts, ty + mid + k),
                            Point::new(tx + ts, ty + mid),
                        );
                    } else {
                        bez.move_to(Point::new(tx + mid, ty));
                        bez.curve_to(
                            Point::new(tx + mid + k, ty),
                            Point::new(tx + ts, ty + mid - k),
                            Point::new(tx + ts, ty + mid),
                        );
                        bez.move_to(Point::new(tx + mid, ty + ts));
                        bez.curve_to(
                            Point::new(tx + mid - k, ty + ts),
                            Point::new(tx, ty + mid + k),
                            Point::new(tx, ty + mid),
                        );
                    }
                }
            }

            let mut pn =
                photonic_core::node::PathNode::new(photonic_core::PathData::from_bez_path(&bez));
            if style == "triangles" {
                pn.fill = Fill::solid(tile_color);
                pn.stroke = Stroke::none();
            } else {
                pn.fill = Fill::none();
                pn.stroke = Stroke::solid(tile_color, sw);
            }

            let node = SceneNode::new(format!("t{row}_{col}"), layer_id, SceneNodeKind::Path(pn));
            let nid = node.id;
            history.execute(
                Command::AddNode {
                    node,
                    layer_id: Some(layer_id),
                },
                doc,
            );
            child_ids.push(nid);
        }
    }

    let label = format!("Truchet {style} {cols}×{rows}");
    let group = SceneNode::new(&label, layer_id, SceneNodeKind::Group(GroupNode::new()));
    history.execute(
        Command::GroupNodes {
            group,
            layer_id,
            insert_index: 0,
            children: child_ids,
        },
        doc,
    );

    *doc_modified = true;
}
