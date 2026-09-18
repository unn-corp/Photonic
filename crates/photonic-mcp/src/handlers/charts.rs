use crate::handlers::nodes::catmull_rom_to_bezier;
use crate::protocol::{
    CreateBarChartArgs, CreateLineChartArgs, CreatePieChartArgs, CreateRadarChartArgs,
    CreateScatterPlotArgs, CreateStackedBarChartArgs, ToolResult,
};
use crate::server::AppState;
use photonic_core::{
    history::Command,
    node::{PathNode, SceneNode, SceneNodeKind},
    path::PathData,
};

pub async fn create_scatter_plot(state: &AppState, args: CreateScatterPlotArgs) -> ToolResult {
    tracing::debug!("tool: create_scatter_plot");
    use kurbo::Shape;
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind, Stroke};

    if args.points.is_empty() {
        return ToolResult::error("points must not be empty");
    }

    let plot_w = args.width.unwrap_or(300.0);
    let plot_h = args.height.unwrap_or(300.0);
    let dot_r = args.dot_radius.unwrap_or(4.0);
    let color = args.color.as_deref().unwrap_or("#4E79A7");
    let dot_color = Color::from_hex(color).unwrap_or(Color::new(0.3, 0.47, 0.65, 1.0));

    // Find data bounds.
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &[px, py] in &args.points {
        min_x = min_x.min(px);
        max_x = max_x.max(px);
        min_y = min_y.min(py);
        max_y = max_y.max(py);
    }
    let range_x = (max_x - min_x).max(1e-9);
    let range_y = (max_y - min_y).max(1e-9);

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    // Build all dots as a single compound path.
    let mut bez = kurbo::BezPath::new();
    for &[px, py] in &args.points {
        let cx = args.x + ((px - min_x) / range_x) * plot_w;
        let cy = args.y - ((py - min_y) / range_y) * plot_h;
        let circle = kurbo::Circle::new((cx, cy), dot_r);
        for el in circle.to_path(0.1).elements() {
            bez.push(*el);
        }
    }

    let mut pn = PathNode::new(PathData::from_bez_path(&bez));
    pn.fill = Fill {
        kind: FillKind::Solid(dot_color),
        ..Default::default()
    };
    pn.stroke = Stroke::none();

    let node = SceneNode::new("Scatter Plot", layer_id, SceneNodeKind::Path(pn));
    let node_id = node.id;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: Some(layer_id),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created scatter plot at ({},{}) — {} points",
        args.x,
        args.y,
        args.points.len()
    ))
    .with_data(serde_json::json!({ "node_id": node_id, "point_count": args.points.len() }))
}

pub async fn create_line_chart(state: &AppState, args: CreateLineChartArgs) -> ToolResult {
    tracing::debug!("tool: create_line_chart");
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind, Stroke};

    if args.series.is_empty() || args.series.iter().all(|s| s.is_empty()) {
        return ToolResult::error("At least one non-empty data series required");
    }

    let chart_w = args.width.unwrap_or(300.0);
    let chart_h = args.height.unwrap_or(200.0);
    let stroke_w = args.stroke_width.unwrap_or(2.0);
    let x = args.x;
    let y = args.y;

    // Find global min/max across all series.
    let mut all_max = f64::NEG_INFINITY;
    let mut all_min = f64::INFINITY;
    let mut max_len = 0usize;
    for series in &args.series {
        for &v in series {
            all_max = all_max.max(v);
            all_min = all_min.min(v);
        }
        max_len = max_len.max(series.len());
    }
    if max_len < 2 {
        return ToolResult::error("Each series needs at least 2 data points");
    }
    let range = (all_max - all_min).max(1e-9);

    let default_colors = ["#4E79A7", "#F28E2B", "#E15759", "#76B7B2", "#59A14F"];
    let colors: Vec<Color> = if args.colors.is_empty() {
        default_colors
            .iter()
            .filter_map(|h| Color::from_hex(h))
            .collect()
    } else {
        args.colors
            .iter()
            .filter_map(|h| Color::from_hex(h))
            .collect()
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut child_ids = Vec::new();

    for (si, series) in args.series.iter().enumerate() {
        if series.len() < 2 {
            continue;
        }
        let color = colors[si % colors.len()];

        // Convert data points to canvas coordinates.
        let pts: Vec<kurbo::Point> = series
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let px = x + (i as f64 / (series.len() - 1) as f64) * chart_w;
                let py = y - ((v - all_min) / range) * chart_h;
                kurbo::Point::new(px, py)
            })
            .collect();

        let line_path = if args.smooth && pts.len() >= 3 {
            // Catmull-Rom smooth.
            catmull_rom_to_bezier(&pts, false)
        } else {
            let mut bez = kurbo::BezPath::new();
            for (i, &p) in pts.iter().enumerate() {
                if i == 0 {
                    bez.move_to(p);
                } else {
                    bez.line_to(p);
                }
            }
            bez
        };

        if args.fill_area {
            // Create filled area: line path + close to baseline.
            let mut area = line_path.clone();
            area.line_to((pts.last().unwrap().x, y));
            area.line_to((pts[0].x, y));
            area.close_path();

            let mut pn = PathNode::new(PathData::from_bez_path(&area));
            pn.fill = Fill {
                kind: FillKind::Solid(Color::new(color.r, color.g, color.b, 0.2)),
                ..Default::default()
            };
            pn.stroke = Stroke::none();
            let node = SceneNode::new(
                format!("Series {} Area", si + 1),
                layer_id,
                SceneNodeKind::Path(pn),
            );
            child_ids.push(node.id);
            history.execute_discrete(
                Command::AddNode {
                    node,
                    layer_id: Some(layer_id),
                },
                &mut doc,
            );
        }

        // Stroke line.
        let mut pn = PathNode::new(PathData::from_bez_path(&line_path));
        pn.fill = Fill::none();
        pn.stroke = Stroke {
            color,
            width: stroke_w,
            enabled: true,
            ..Default::default()
        };
        let node = SceneNode::new(
            format!("Series {}", si + 1),
            layer_id,
            SceneNodeKind::Path(pn),
        );
        child_ids.push(node.id);
        history.execute_discrete(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
    }

    let group = SceneNode::new(
        "Line Chart",
        layer_id,
        SceneNodeKind::Group(photonic_core::node::GroupNode::new()),
    );
    let group_id = group.id;
    history.execute_discrete(
        Command::GroupNodes {
            group,
            layer_id,
            insert_index: 0,
            children: child_ids.clone(),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created line chart at ({x},{y}) — {} series, {} max points",
        args.series.len(),
        max_len
    ))
    .with_data(serde_json::json!({ "group_id": group_id, "series_count": args.series.len() }))
}

pub async fn create_bar_chart(state: &AppState, args: CreateBarChartArgs) -> ToolResult {
    tracing::debug!("tool: create_bar_chart");
    use kurbo::Shape;
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind, Stroke};

    if args.values.is_empty() {
        return ToolResult::error("values must not be empty");
    }

    let chart_w = args.width.unwrap_or(300.0);
    let chart_h = args.height.unwrap_or(200.0);
    let gap_frac = args.gap.unwrap_or(0.2).clamp(0.0, 0.9);
    let n = args.values.len();
    let max_val = args
        .values
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    if max_val <= 0.0 {
        return ToolResult::error("At least one value must be positive");
    }

    let default_colors = [
        "#4E79A7", "#F28E2B", "#E15759", "#76B7B2", "#59A14F", "#EDC948", "#B07AA1", "#FF9DA7",
        "#9C755F", "#BAB0AC",
    ];
    let colors: Vec<Color> = if args.colors.is_empty() {
        default_colors
            .iter()
            .filter_map(|h| Color::from_hex(h))
            .collect()
    } else {
        args.colors
            .iter()
            .filter_map(|h| Color::from_hex(h))
            .collect()
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut child_ids = Vec::new();

    if args.horizontal {
        let bar_total = chart_h / n as f64;
        let bar_h = bar_total * (1.0 - gap_frac);
        let bar_gap = bar_total * gap_frac;

        for (i, &val) in args.values.iter().enumerate() {
            let bar_w = (val / max_val) * chart_w;
            let bx = args.x;
            let by = args.y - chart_h + (i as f64 * bar_total) + bar_gap / 2.0;

            let rect = kurbo::Rect::new(bx, by, bx + bar_w, by + bar_h);
            let mut pn = PathNode::new(PathData::from_bez_path(&rect.to_path(0.0)));
            pn.fill = Fill {
                kind: FillKind::Solid(colors[i % colors.len()]),
                ..Default::default()
            };
            pn.stroke = Stroke::none();

            let label = args
                .labels
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("Bar {}", i + 1));
            let node = SceneNode::new(&label, layer_id, SceneNodeKind::Path(pn));
            child_ids.push(node.id);
            history.execute_discrete(
                Command::AddNode {
                    node,
                    layer_id: Some(layer_id),
                },
                &mut doc,
            );
        }
    } else {
        let bar_total = chart_w / n as f64;
        let bar_w = bar_total * (1.0 - gap_frac);
        let bar_gap = bar_total * gap_frac;

        for (i, &val) in args.values.iter().enumerate() {
            let bar_h = (val / max_val) * chart_h;
            let bx = args.x + (i as f64 * bar_total) + bar_gap / 2.0;
            let by = args.y - bar_h;

            let rect = kurbo::Rect::new(bx, by, bx + bar_w, args.y);
            let mut pn = PathNode::new(PathData::from_bez_path(&rect.to_path(0.0)));
            pn.fill = Fill {
                kind: FillKind::Solid(colors[i % colors.len()]),
                ..Default::default()
            };
            pn.stroke = Stroke::none();

            let label = args
                .labels
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("Bar {}", i + 1));
            let node = SceneNode::new(&label, layer_id, SceneNodeKind::Path(pn));
            child_ids.push(node.id);
            history.execute_discrete(
                Command::AddNode {
                    node,
                    layer_id: Some(layer_id),
                },
                &mut doc,
            );
        }
    }

    let group = SceneNode::new(
        "Bar Chart",
        layer_id,
        SceneNodeKind::Group(photonic_core::node::GroupNode::new()),
    );
    let group_id = group.id;
    history.execute_discrete(
        Command::GroupNodes {
            group,
            layer_id,
            insert_index: 0,
            children: child_ids.clone(),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created {} bar chart at ({},{}) — {} bars",
        if args.horizontal {
            "horizontal"
        } else {
            "vertical"
        },
        args.x,
        args.y,
        n
    ))
    .with_data(serde_json::json!({ "group_id": group_id, "bars": n }))
}

pub async fn create_stacked_bar_chart(
    state: &AppState,
    args: CreateStackedBarChartArgs,
) -> ToolResult {
    tracing::debug!("tool: create_stacked_bar_chart");
    use kurbo::Shape;
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind, Stroke};

    if args.series.is_empty() {
        return ToolResult::error("series must not be empty");
    }
    let n_stacks = args.series[0].len();
    if n_stacks == 0 {
        return ToolResult::error("each series must have at least one value");
    }
    for (i, s) in args.series.iter().enumerate() {
        if s.len() != n_stacks {
            return ToolResult::error(format!(
                "all series must have the same length; series 0 has {} values but series {} has {}",
                n_stacks,
                i,
                s.len()
            ));
        }
    }

    let chart_w = args.width.unwrap_or(300.0);
    let chart_h = args.height.unwrap_or(200.0);
    let gap_frac = args.gap.unwrap_or(0.2).clamp(0.0, 0.9);

    // Max stack total for normalization.
    let max_total = (0..n_stacks)
        .map(|ci| args.series.iter().map(|s| s[ci]).sum::<f64>())
        .fold(0.0_f64, f64::max);
    if max_total <= 0.0 {
        return ToolResult::error("at least one value must be positive");
    }

    let default_colors = [
        "#4E79A7", "#F28E2B", "#E15759", "#76B7B2", "#59A14F", "#EDC948", "#B07AA1", "#FF9DA7",
        "#9C755F", "#BAB0AC",
    ];
    let parsed_user: Vec<Color> = args
        .colors
        .iter()
        .filter_map(|h| Color::from_hex(h))
        .collect();
    let colors: Vec<Color> = if parsed_user.is_empty() {
        default_colors
            .iter()
            .filter_map(|h| Color::from_hex(h))
            .collect()
    } else {
        parsed_user
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut child_ids = Vec::new();

    if args.horizontal {
        let bar_total = chart_h / n_stacks as f64;
        let bar_h = bar_total * (1.0 - gap_frac);
        let bar_gap = bar_total * gap_frac;

        for ci in 0..n_stacks {
            let by = args.y - chart_h + (ci as f64 * bar_total) + bar_gap / 2.0;
            let mut cursor_x = args.x;
            for (si, series) in args.series.iter().enumerate() {
                let val = series[ci];
                if val <= 0.0 {
                    cursor_x += 0.0;
                    continue;
                }
                let seg_w = (val / max_total) * chart_w;
                let rect = kurbo::Rect::new(cursor_x, by, cursor_x + seg_w, by + bar_h);
                let mut pn = PathNode::new(PathData::from_bez_path(&rect.to_path(0.0)));
                pn.fill = Fill {
                    kind: FillKind::Solid(colors[si % colors.len()]),
                    ..Default::default()
                };
                pn.stroke = Stroke::none();
                let sname = args
                    .series_names
                    .get(si)
                    .cloned()
                    .unwrap_or_else(|| format!("Series {}", si + 1));
                let lname = args
                    .labels
                    .get(ci)
                    .cloned()
                    .unwrap_or_else(|| format!("Bar {}", ci + 1));
                let node = SceneNode::new(
                    format!("{sname} / {lname}"),
                    layer_id,
                    SceneNodeKind::Path(pn),
                );
                child_ids.push(node.id);
                history.execute_discrete(
                    Command::AddNode {
                        node,
                        layer_id: Some(layer_id),
                    },
                    &mut doc,
                );
                cursor_x += seg_w;
            }
        }
    } else {
        let bar_total = chart_w / n_stacks as f64;
        let bar_w = bar_total * (1.0 - gap_frac);
        let bar_gap = bar_total * gap_frac;

        for ci in 0..n_stacks {
            let bx = args.x + (ci as f64 * bar_total) + bar_gap / 2.0;
            let mut cursor_y = args.y; // top of stack grows upward
            for (si, series) in args.series.iter().enumerate() {
                let val = series[ci];
                if val <= 0.0 {
                    continue;
                }
                let seg_h = (val / max_total) * chart_h;
                let rect = kurbo::Rect::new(bx, cursor_y - seg_h, bx + bar_w, cursor_y);
                let mut pn = PathNode::new(PathData::from_bez_path(&rect.to_path(0.0)));
                pn.fill = Fill {
                    kind: FillKind::Solid(colors[si % colors.len()]),
                    ..Default::default()
                };
                pn.stroke = Stroke::none();
                let sname = args
                    .series_names
                    .get(si)
                    .cloned()
                    .unwrap_or_else(|| format!("Series {}", si + 1));
                let lname = args
                    .labels
                    .get(ci)
                    .cloned()
                    .unwrap_or_else(|| format!("Bar {}", ci + 1));
                let node = SceneNode::new(
                    format!("{sname} / {lname}"),
                    layer_id,
                    SceneNodeKind::Path(pn),
                );
                child_ids.push(node.id);
                history.execute_discrete(
                    Command::AddNode {
                        node,
                        layer_id: Some(layer_id),
                    },
                    &mut doc,
                );
                cursor_y -= seg_h;
            }
        }
    }

    let label = format!(
        "Stacked {} Chart",
        if args.horizontal { "Bar" } else { "Column" }
    );
    let group = SceneNode::new(
        &label,
        layer_id,
        SceneNodeKind::Group(photonic_core::node::GroupNode::new()),
    );
    let group_id = group.id;
    history.execute_discrete(
        Command::GroupNodes {
            group,
            layer_id,
            insert_index: 0,
            children: child_ids.clone(),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created stacked {} chart at ({},{}) — {} stacks, {} series",
        if args.horizontal { "bar" } else { "column" },
        args.x,
        args.y,
        n_stacks,
        args.series.len()
    ))
    .with_data(serde_json::json!({
        "group_id": group_id,
        "stacks": n_stacks,
        "series": args.series.len(),
    }))
}

pub async fn create_pie_chart(state: &AppState, args: CreatePieChartArgs) -> ToolResult {
    tracing::debug!("tool: create_pie_chart");
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind, Stroke};

    if args.values.is_empty() {
        return ToolResult::error("values must not be empty");
    }

    let total: f64 = args.values.iter().sum();
    if total <= 0.0 {
        return ToolResult::error("Sum of values must be positive");
    }

    let radius = args.radius.unwrap_or(80.0);
    let inner_r = args.inner_radius.unwrap_or(0.0).max(0.0);
    let cx = args.cx;
    let cy = args.cy;

    // Default palette if none provided.
    let default_colors = [
        "#4E79A7", "#F28E2B", "#E15759", "#76B7B2", "#59A14F", "#EDC948", "#B07AA1", "#FF9DA7",
        "#9C755F", "#BAB0AC",
    ];

    let colors: Vec<Color> = if args.colors.is_empty() {
        default_colors
            .iter()
            .filter_map(|h| Color::from_hex(h))
            .collect()
    } else {
        args.colors
            .iter()
            .filter_map(|h| Color::from_hex(h))
            .collect()
    };

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut child_ids = Vec::new();
    let mut start_angle = -std::f64::consts::FRAC_PI_2; // Start from top (12 o'clock).
    let n_segs = 32;

    for (i, &val) in args.values.iter().enumerate() {
        let sweep = (val / total) * std::f64::consts::TAU;
        let end_angle = start_angle + sweep;
        let color = colors[i % colors.len()];

        let mut bez = kurbo::BezPath::new();

        if inner_r > 0.0 {
            // Donut slice: outer arc → line to inner → inner arc reversed → close.
            for j in 0..=n_segs {
                let t = j as f64 / n_segs as f64;
                let a = start_angle + sweep * t;
                let pt = kurbo::Point::new(cx + radius * a.cos(), cy + radius * a.sin());
                if j == 0 {
                    bez.move_to(pt);
                } else {
                    bez.line_to(pt);
                }
            }
            for j in (0..=n_segs).rev() {
                let t = j as f64 / n_segs as f64;
                let a = start_angle + sweep * t;
                let pt = kurbo::Point::new(cx + inner_r * a.cos(), cy + inner_r * a.sin());
                bez.line_to(pt);
            }
        } else {
            // Solid pie: center → arc → close.
            bez.move_to((cx, cy));
            for j in 0..=n_segs {
                let t = j as f64 / n_segs as f64;
                let a = start_angle + sweep * t;
                bez.line_to((cx + radius * a.cos(), cy + radius * a.sin()));
            }
        }
        bez.close_path();

        let mut pn = PathNode::new(PathData::from_bez_path(&bez));
        pn.fill = Fill {
            kind: FillKind::Solid(color),
            ..Default::default()
        };
        pn.stroke = Stroke {
            color: Color::WHITE,
            width: 1.0,
            enabled: true,
            ..Default::default()
        };

        let label = args
            .labels
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("Slice {}", i + 1));
        let node = SceneNode::new(&label, layer_id, SceneNodeKind::Path(pn));
        let nid = node.id;
        child_ids.push(nid);
        history.execute_discrete(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );

        start_angle = end_angle;
    }

    // Group all slices.
    let group = SceneNode::new(
        "Pie Chart",
        layer_id,
        SceneNodeKind::Group(photonic_core::node::GroupNode::new()),
    );
    let group_id = group.id;
    history.execute_discrete(
        Command::GroupNodes {
            group,
            layer_id,
            insert_index: 0,
            children: child_ids.clone(),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created pie chart at ({cx},{cy}) — {} slices, r={radius}",
        args.values.len()
    ))
    .with_data(serde_json::json!({
        "group_id": group_id,
        "slices": args.values.len(),
    }))
}

pub async fn create_radar_chart(state: &AppState, args: CreateRadarChartArgs) -> ToolResult {
    tracing::debug!("tool: create_radar_chart");
    use photonic_core::color::Color;
    use photonic_core::style::{Fill, FillKind, Stroke};

    if args.series.is_empty() {
        return ToolResult::error("series must not be empty");
    }
    let n_axes = args.series[0].len();
    if n_axes < 3 {
        return ToolResult::error("each series must have at least 3 values (axes)");
    }
    for (i, s) in args.series.iter().enumerate() {
        if s.len() != n_axes {
            return ToolResult::error(format!(
                "all series must have the same length; series 0 has {} values but series {} has {}",
                n_axes,
                i,
                s.len()
            ));
        }
    }

    let radius = args.radius.unwrap_or(100.0);
    let grid_rings = args.grid_rings.unwrap_or(4).max(1);
    let stroke_w = args.stroke_width.unwrap_or(1.5);
    let cx = args.cx;
    let cy = args.cy;

    let default_colors = [
        "#4E79A7", "#F28E2B", "#E15759", "#76B7B2", "#59A14F", "#EDC948", "#B07AA1", "#FF9DA7",
        "#9C755F", "#BAB0AC",
    ];
    let parsed_user: Vec<Color> = args
        .colors
        .iter()
        .filter_map(|h| Color::from_hex(h))
        .collect();
    let colors: Vec<Color> = if parsed_user.is_empty() {
        default_colors
            .iter()
            .filter_map(|h| Color::from_hex(h))
            .collect()
    } else {
        parsed_user
    };

    // Compute axis angles: evenly distributed, starting at top (−π/2).
    let axis_angle = |i: usize| -> f64 {
        -std::f64::consts::FRAC_PI_2 + (i as f64 / n_axes as f64) * std::f64::consts::TAU
    };

    // Max value across all series per axis for normalization.
    let axis_max: Vec<f64> = (0..n_axes)
        .map(|ai| args.series.iter().map(|s| s[ai]).fold(0.0_f64, f64::max))
        .collect();

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;

    let layer_id = args
        .layer_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .or(doc.active_layer_id)
        .unwrap_or(uuid::Uuid::nil());

    let mut child_ids: Vec<uuid::Uuid> = Vec::new();

    // ── Grid rings ──────────────────────────────────────────────────────────
    for ring in 1..=grid_rings {
        let r = radius * (ring as f64 / grid_rings as f64);
        let mut bez = kurbo::BezPath::new();
        for i in 0..n_axes {
            let angle = axis_angle(i);
            let pt = kurbo::Point::new(cx + r * angle.cos(), cy + r * angle.sin());
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
        history.execute_discrete(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
    }

    // ── Axis lines ──────────────────────────────────────────────────────────
    for i in 0..n_axes {
        let angle = axis_angle(i);
        let tip = kurbo::Point::new(cx + radius * angle.cos(), cy + radius * angle.sin());
        let mut bez = kurbo::BezPath::new();
        bez.move_to(kurbo::Point::new(cx, cy));
        bez.line_to(tip);
        let mut pn = PathNode::new(PathData::from_bez_path(&bez));
        pn.fill = Fill {
            kind: FillKind::None,
            ..Default::default()
        };
        pn.stroke = Stroke::solid(Color::new(0.7, 0.7, 0.75, 1.0), 0.75);
        let label = args
            .labels
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("Axis {}", i + 1));
        let node = SceneNode::new(format!("Axis {label}"), layer_id, SceneNodeKind::Path(pn));
        child_ids.push(node.id);
        history.execute_discrete(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
    }

    // ── Data series polygons ────────────────────────────────────────────────
    for (si, series) in args.series.iter().enumerate() {
        let color = colors[si % colors.len()];
        let mut bez = kurbo::BezPath::new();
        for (ai, &val) in series.iter().enumerate() {
            let max = if axis_max[ai] > 0.0 {
                axis_max[ai]
            } else {
                1.0
            };
            let r = radius * (val / max).clamp(0.0, 1.0);
            let angle = axis_angle(ai);
            let pt = kurbo::Point::new(cx + r * angle.cos(), cy + r * angle.sin());
            if ai == 0 {
                bez.move_to(pt);
            } else {
                bez.line_to(pt);
            }
        }
        bez.close_path();
        let mut pn = PathNode::new(PathData::from_bez_path(&bez));
        if args.fill_area {
            pn.fill = Fill {
                kind: FillKind::Solid(Color::new(color.r, color.g, color.b, 0.2)),
                ..Default::default()
            };
        } else {
            pn.fill = Fill {
                kind: FillKind::None,
                ..Default::default()
            };
        }
        pn.stroke = Stroke::solid(color, stroke_w);
        let series_name = args
            .series_names
            .get(si)
            .cloned()
            .unwrap_or_else(|| format!("Series {}", si + 1));
        let node = SceneNode::new(&series_name, layer_id, SceneNodeKind::Path(pn));
        child_ids.push(node.id);
        history.execute_discrete(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            &mut doc,
        );
    }

    let group = SceneNode::new(
        "Radar Chart",
        layer_id,
        SceneNodeKind::Group(photonic_core::node::GroupNode::new()),
    );
    let group_id = group.id;
    history.execute_discrete(
        Command::GroupNodes {
            group,
            layer_id,
            insert_index: 0,
            children: child_ids.clone(),
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Created radar chart at ({cx},{cy}) — {} axes, {} series, r={radius}",
        n_axes,
        args.series.len()
    ))
    .with_data(serde_json::json!({
        "group_id": group_id,
        "axes": n_axes,
        "series": args.series.len(),
    }))
}
