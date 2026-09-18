use crate::handlers::shared::spatial::world_aabb;
use crate::protocol::{
    AnalyzeCompositionArgs, DetectRhythmsArgs, MeasureDistancesArgs, ToolResult,
};
use crate::server::AppState;
use photonic_core::node::SceneNodeKind;
use photonic_core::style::FillKind;
use serde_json::json;

/// Analyze the composition of the current document and return advisory findings.
pub async fn analyze_composition(state: &AppState, args: AnalyzeCompositionArgs) -> ToolResult {
    tracing::debug!("tool: analyze_composition");
    let doc = state.document.lock().await;

    // Collect node bounds in world space
    struct NodeInfo {
        cx: f64,
        cy: f64,
        bx: f64,
        by: f64,
        bw: f64,
        bh: f64,
        fill_r: f32,
        fill_g: f32,
        fill_b: f32,
        has_solid_fill: bool,
    }

    let filter_ids: Option<std::collections::HashSet<uuid::Uuid>> = if args.node_ids.is_empty() {
        None
    } else {
        Some(
            args.node_ids
                .iter()
                .filter_map(|id| {
                    uuid::Uuid::parse_str(id)
                        .ok()
                        .or_else(|| doc.find_node_by_name(id).map(|n| n.id))
                })
                .collect(),
        )
    };

    let mut infos: Vec<NodeInfo> = Vec::new();
    let canvas_w = doc.width as f64;
    let canvas_h = doc.height as f64;

    for node in doc.nodes_in_draw_order() {
        if !node.visible {
            continue;
        }
        if let Some(ref ids) = filter_ids {
            if !ids.contains(&node.id) {
                continue;
            }
        }
        let (wx, wy) = node.transform.apply(0.0, 0.0);
        let (bx, by, bw, bh) = world_aabb(node)
            .map(|[x, y, width, height]| (x, y, width.max(1.0), height.max(1.0)))
            .unwrap_or((wx, wy, 1.0, 1.0));
        let (fill_r, fill_g, fill_b, has_solid) = match &node.kind {
            SceneNodeKind::Path(pn) => match &pn.fill.kind {
                FillKind::Solid(c) => (c.r, c.g, c.b, true),
                _ => (0.5, 0.5, 0.5, false),
            },
            SceneNodeKind::Text(tn) => match &tn.fill.kind {
                FillKind::Solid(c) => (c.r, c.g, c.b, true),
                _ => (0.0, 0.0, 0.0, true),
            },
            SceneNodeKind::Group(_) => (0.5, 0.5, 0.5, false),
            // raster: no vector fill
            SceneNodeKind::Raster(_) => (0.5, 0.5, 0.5, false),
        };
        infos.push(NodeInfo {
            cx: bx + bw / 2.0,
            cy: by + bh / 2.0,
            bx,
            by,
            bw,
            bh,
            fill_r,
            fill_g,
            fill_b,
            has_solid_fill: has_solid,
        });
    }

    let mut findings: Vec<serde_json::Value> = Vec::new();

    if infos.is_empty() {
        return ToolResult::text("No visible nodes to analyze.")
            .with_data(json!({ "node_count": 0, "findings": [] }));
    }

    let node_count = infos.len();

    // ── Balance: quadrant distribution ──────────────────────────────────────
    let mid_x = canvas_w / 2.0;
    let mid_y = canvas_h / 2.0;
    let (mut q_tl, mut q_tr, mut q_bl, mut q_br) = (0usize, 0usize, 0usize, 0usize);
    for n in &infos {
        match (n.cx < mid_x, n.cy < mid_y) {
            (true, true) => q_tl += 1,
            (false, true) => q_tr += 1,
            (true, false) => q_bl += 1,
            (false, false) => q_br += 1,
        }
    }
    let left = q_tl + q_bl;
    let right = q_tr + q_br;
    let top = q_tl + q_tr;
    let bottom = q_bl + q_br;
    let h_imbalance = if left + right > 0 {
        ((left as f64 - right as f64).abs() / (left + right) as f64 * 100.0) as u32
    } else {
        0
    };
    let v_imbalance = if top + bottom > 0 {
        ((top as f64 - bottom as f64).abs() / (top + bottom) as f64 * 100.0) as u32
    } else {
        0
    };
    if h_imbalance > 40 {
        let side = if left > right { "left" } else { "right" };
        findings.push(json!({
            "severity": "warning",
            "category": "balance",
            "description": format!(
                "Horizontal imbalance: {}% more objects on the {} side ({} left, {} right). Consider redistributing elements or adding counterweight.",
                h_imbalance, side, left, right
            )
        }));
    }
    if v_imbalance > 40 {
        let side = if top > bottom { "top" } else { "bottom" };
        findings.push(json!({
            "severity": "info",
            "category": "balance",
            "description": format!(
                "Vertical imbalance: {}% more objects near the {} ({} top half, {} bottom half).",
                v_imbalance, side, top, bottom
            )
        }));
    }
    if h_imbalance <= 20 && v_imbalance <= 20 {
        findings.push(json!({
            "severity": "ok",
            "category": "balance",
            "description": "Visual balance is good — objects are distributed evenly across quadrants."
        }));
    }

    // ── Density: canvas utilization ──────────────────────────────────────────
    let total_area: f64 = infos.iter().map(|n| n.bw * n.bh).sum();
    let canvas_area = (canvas_w * canvas_h).max(1.0);
    let density_pct = (total_area / canvas_area * 100.0).min(200.0);
    if density_pct < 5.0 {
        findings.push(json!({
            "severity": "info",
            "category": "density",
            "description": format!(
                "Canvas is very sparse ({:.1}% coverage). Objects occupy less than 5% of the canvas area.",
                density_pct
            )
        }));
    } else if density_pct > 120.0 {
        findings.push(json!({
            "severity": "warning",
            "category": "density",
            "description": format!(
                "Canvas may be overcrowded ({:.1}% combined bounding-box coverage). Some objects likely overlap significantly.",
                density_pct
            )
        }));
    }

    // ── Overlap detection ────────────────────────────────────────────────────
    let mut overlap_count = 0usize;
    for i in 0..infos.len() {
        for j in (i + 1)..infos.len() {
            let a = &infos[i];
            let b = &infos[j];
            let overlap = a.bx < b.bx + b.bw
                && a.bx + a.bw > b.bx
                && a.by < b.by + b.bh
                && a.by + a.bh > b.by;
            if overlap {
                overlap_count += 1;
            }
            if overlap_count >= 10 {
                break;
            }
        }
        if overlap_count >= 10 {
            break;
        }
    }
    if overlap_count > 0 {
        findings.push(json!({
            "severity": "info",
            "category": "overlap",
            "description": format!(
                "At least {} overlapping object pair(s) detected. This may be intentional (layering) or accidental — use distribute_no_overlap if unintended.",
                overlap_count
            )
        }));
    }

    // ── Color contrast ───────────────────────────────────────────────────────
    // Check pairs of solid-filled nodes for very similar colors
    let solid_nodes: Vec<_> = infos.iter().filter(|n| n.has_solid_fill).collect();
    let mut low_contrast_pairs = 0usize;
    'outer: for i in 0..solid_nodes.len() {
        for j in (i + 1)..solid_nodes.len() {
            let a = solid_nodes[i];
            let b = solid_nodes[j];
            let dr = (a.fill_r - b.fill_r).abs();
            let dg = (a.fill_g - b.fill_g).abs();
            let db = (a.fill_b - b.fill_b).abs();
            let delta = (dr * dr + dg * dg + db * db).sqrt();
            if delta < 0.1 {
                low_contrast_pairs += 1;
                if low_contrast_pairs >= 5 {
                    break 'outer;
                }
            }
        }
    }
    if low_contrast_pairs > 0 {
        findings.push(json!({
            "severity": "info",
            "category": "color_contrast",
            "description": format!(
                "{} pair(s) of objects with nearly identical fill colors detected. Objects may be hard to distinguish visually.",
                low_contrast_pairs
            )
        }));
    }

    // ── Unique colors (palette complexity) ──────────────────────────────────
    let unique_colors: std::collections::HashSet<(u8, u8, u8)> = solid_nodes
        .iter()
        .map(|n| {
            (
                (n.fill_r * 255.0) as u8,
                (n.fill_g * 255.0) as u8,
                (n.fill_b * 255.0) as u8,
            )
        })
        .collect();
    if unique_colors.len() > 12 {
        findings.push(json!({
            "severity": "info",
            "category": "color_palette",
            "description": format!(
                "{} unique fill colors in use. Consider reducing to a tighter palette (typically ≤ 5–7 colors) for visual cohesion.",
                unique_colors.len()
            )
        }));
    }

    // ── Off-canvas objects ───────────────────────────────────────────────────
    let off_canvas = infos
        .iter()
        .filter(|n| n.bx + n.bw < 0.0 || n.by + n.bh < 0.0 || n.bx > canvas_w || n.by > canvas_h)
        .count();
    if off_canvas > 0 {
        findings.push(json!({
            "severity": "warning",
            "category": "off_canvas",
            "description": format!(
                "{} object(s) are fully outside the canvas bounds and will not appear in exports.",
                off_canvas
            )
        }));
    }

    let summary = if findings.iter().any(|f| f["severity"] == "warning") {
        format!(
            "Analyzed {} node(s) — {} finding(s), some need attention.",
            node_count,
            findings.len()
        )
    } else {
        format!(
            "Analyzed {} node(s) — {} finding(s).",
            node_count,
            findings.len()
        )
    };

    ToolResult::text(summary).with_data(json!({
        "node_count": node_count,
        "quadrant_distribution": { "top_left": q_tl, "top_right": q_tr, "bottom_left": q_bl, "bottom_right": q_br },
        "canvas_coverage_pct": (density_pct * 10.0).round() / 10.0,
        "unique_fill_colors": unique_colors.len(),
        "findings": findings,
    }))
}

/// Detect visual rhythms (spacing, size, rotation patterns) in the document.
pub async fn detect_rhythms(state: &AppState, args: DetectRhythmsArgs) -> ToolResult {
    tracing::debug!("tool: detect_rhythms");
    let doc = state.document.lock().await;
    let min_count = args.min_count.unwrap_or(3).max(2);

    let filter_ids: Option<std::collections::HashSet<uuid::Uuid>> = if args.node_ids.is_empty() {
        None
    } else {
        Some(
            args.node_ids
                .iter()
                .filter_map(|id| {
                    uuid::Uuid::parse_str(id)
                        .ok()
                        .or_else(|| doc.find_node_by_name(id).map(|n| n.id))
                })
                .collect(),
        )
    };

    struct NodeMetrics {
        cx: f64,
        cy: f64,
        w: f64,
        area: f64,
        rotation_deg: f64,
    }

    let mut metrics: Vec<NodeMetrics> = Vec::new();

    for node in doc.nodes_in_draw_order() {
        if !node.visible {
            continue;
        }
        if let Some(ref ids) = filter_ids {
            if !ids.contains(&node.id) {
                continue;
            }
        }
        // Skip groups for cleaner analysis
        if matches!(node.kind, SceneNodeKind::Group(_)) {
            continue;
        }

        let (bx, by, bw, bh) = world_aabb(node)
            .map(|[x, y, width, height]| (x, y, width.max(0.001), height.max(0.001)))
            .unwrap_or_else(|| {
                let (wx, wy) = node.transform.apply(0.0, 0.0);
                (wx, wy, 1.0, 1.0)
            });

        // Extract rotation from affine matrix [a, b, c, d, tx, ty]: angle = atan2(b, a)
        let rotation_deg = {
            let r = node.transform.matrix[1]
                .atan2(node.transform.matrix[0])
                .to_degrees()
                % 360.0;
            if r < 0.0 {
                r + 360.0
            } else {
                r
            }
        };

        metrics.push(NodeMetrics {
            cx: bx + bw / 2.0,
            cy: by + bh / 2.0,
            w: bw,
            area: bw * bh,
            rotation_deg,
        });
    }

    if metrics.len() < min_count {
        return ToolResult::text(format!(
            "Only {} visible leaf node(s) found — need at least {} to detect rhythms.",
            metrics.len(),
            min_count
        ))
        .with_data(json!({ "node_count": metrics.len(), "patterns": [] }));
    }

    let mut patterns: Vec<serde_json::Value> = Vec::new();
    let tolerance = 4.0_f64; // px tolerance for spacing/size grouping

    // ── Horizontal spacing rhythm ─────────────────────────────────────────────
    {
        let mut xs: Vec<f64> = metrics.iter().map(|m| m.cx).collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut gaps: Vec<f64> = xs.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // Find dominant gap interval
        let mut best_interval = 0.0_f64;
        let mut best_count = 0usize;
        for &gap in &gaps {
            if gap < 1.0 {
                continue;
            }
            let count = gaps
                .iter()
                .filter(|&&g| (g - gap).abs() < tolerance)
                .count();
            if count > best_count {
                best_count = count;
                best_interval = gap;
            }
        }
        if best_count >= min_count - 1 {
            patterns.push(json!({
                "type": "horizontal_spacing",
                "interval_px": (best_interval * 10.0).round() / 10.0,
                "count": best_count + 1,
                "description": format!(
                    "{} objects are spaced ~{:.0}px apart horizontally. Extend the pattern or enforce uniform spacing.",
                    best_count + 1, best_interval
                )
            }));
        }
    }

    // ── Vertical spacing rhythm ───────────────────────────────────────────────
    {
        let mut ys: Vec<f64> = metrics.iter().map(|m| m.cy).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut gaps: Vec<f64> = ys.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let mut best_interval = 0.0_f64;
        let mut best_count = 0usize;
        for &gap in &gaps {
            if gap < 1.0 {
                continue;
            }
            let count = gaps
                .iter()
                .filter(|&&g| (g - gap).abs() < tolerance)
                .count();
            if count > best_count {
                best_count = count;
                best_interval = gap;
            }
        }
        if best_count >= min_count - 1 {
            patterns.push(json!({
                "type": "vertical_spacing",
                "interval_px": (best_interval * 10.0).round() / 10.0,
                "count": best_count + 1,
                "description": format!(
                    "{} objects are spaced ~{:.0}px apart vertically. Extend the pattern or enforce uniform spacing.",
                    best_count + 1, best_interval
                )
            }));
        }
    }

    // ── Width rhythm ─────────────────────────────────────────────────────────
    {
        let mut widths: Vec<f64> = metrics.iter().map(|m| m.w).collect();
        widths.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let mut best_w = 0.0_f64;
        let mut best_count = 0usize;
        for &w in &widths {
            if w < 1.0 {
                continue;
            }
            let count = widths
                .iter()
                .filter(|&&x| (x - w).abs() < tolerance)
                .count();
            if count > best_count {
                best_count = count;
                best_w = w;
            }
        }
        if best_count >= min_count {
            patterns.push(json!({
                "type": "uniform_width",
                "width_px": (best_w * 10.0).round() / 10.0,
                "count": best_count,
                "description": format!(
                    "{} objects share a width of ~{:.0}px. Consider whether the remaining objects should match.",
                    best_count, best_w
                )
            }));
        }
    }

    // ── Size scaling rhythm (geometric progression) ───────────────────────────
    {
        let mut areas: Vec<f64> = metrics.iter().map(|m| m.area).collect();
        areas.sort_by(|a, b| a.partial_cmp(b).unwrap());

        if areas.len() >= min_count {
            // Look for geometric ratio between consecutive areas
            let ratios: Vec<f64> = areas
                .windows(2)
                .filter(|w| w[0] > 0.0)
                .map(|w| w[1] / w[0])
                .collect();

            let mut best_ratio = 1.0_f64;
            let mut best_count = 0usize;
            let ratio_tol = 0.15;
            for &r in &ratios {
                if (r - 1.0).abs() < 0.05 {
                    continue;
                } // skip near-equal
                let count = ratios
                    .iter()
                    .filter(|&&x| (x - r).abs() < ratio_tol)
                    .count();
                if count > best_count {
                    best_count = count;
                    best_ratio = r;
                }
            }
            if best_count >= min_count - 1 && (best_ratio - 1.0).abs() > 0.1 {
                let trend = if best_ratio > 1.0 {
                    "increasing"
                } else {
                    "decreasing"
                };
                patterns.push(json!({
                    "type": "size_progression",
                    "ratio": (best_ratio * 100.0).round() / 100.0,
                    "count": best_count + 1,
                    "description": format!(
                        "{} objects have {} sizes with a ~{:.0}% scale factor per step. Extend or enforce this progression.",
                        best_count + 1, trend, ((best_ratio - 1.0).abs() * 100.0).round()
                    )
                }));
            }
        }
    }

    // ── Rotation rhythm ───────────────────────────────────────────────────────
    {
        let mut rots: Vec<f64> = metrics.iter().map(|m| m.rotation_deg).collect();
        rots.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let rot_tol = 3.0_f64;

        let mut rot_gaps: Vec<f64> = rots.windows(2).map(|w| w[1] - w[0]).collect();
        rot_gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let mut best_interval = 0.0_f64;
        let mut best_count = 0usize;
        for &gap in &rot_gaps {
            if gap < 1.0 {
                continue;
            }
            let count = rot_gaps
                .iter()
                .filter(|&&g| (g - gap).abs() < rot_tol)
                .count();
            if count > best_count {
                best_count = count;
                best_interval = gap;
            }
        }
        if best_count >= min_count - 1 && best_interval >= 5.0 {
            let symmetry_n = (360.0 / best_interval).round() as u32;
            let sym_note = if (2..=12).contains(&symmetry_n) {
                format!(" ({}× rotational symmetry)", symmetry_n)
            } else {
                String::new()
            };
            patterns.push(json!({
                "type": "rotation_rhythm",
                "interval_deg": (best_interval * 10.0).round() / 10.0,
                "count": best_count + 1,
                "description": format!(
                    "{} objects are rotated ~{:.0}° apart{sym_note}. Add missing rotations or flatten to a full symmetry group.",
                    best_count + 1, best_interval
                )
            }));
        }
    }

    let summary = if patterns.is_empty() {
        format!(
            "Analyzed {} node(s) — no repeating rhythms detected.",
            metrics.len()
        )
    } else {
        format!(
            "Analyzed {} node(s) — {} rhythm pattern(s) detected.",
            metrics.len(),
            patterns.len()
        )
    };

    ToolResult::text(summary).with_data(json!({
        "node_count": metrics.len(),
        "patterns": patterns,
    }))
}

/// Measure edge-to-edge gaps, center-to-center distances, and alignment between nodes.
pub async fn measure_distances(state: &AppState, args: MeasureDistancesArgs) -> ToolResult {
    tracing::debug!("tool: measure_distances");
    if args.node_ids.len() < 2 {
        return ToolResult::error("At least 2 node_ids are required for distance measurement.");
    }

    let doc = state.document.lock().await;

    struct NodeBox {
        name: String,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
    }

    let mut boxes: Vec<NodeBox> = Vec::new();
    for id_str in &args.node_ids {
        let uid = uuid::Uuid::parse_str(id_str)
            .ok()
            .or_else(|| doc.find_node_by_name(id_str).map(|n| n.id));
        let node = uid.and_then(|uid| doc.nodes.get(&uid));
        if let Some(node) = node {
            let (bx, by, bw, bh) = if let Some([x, y, width, height]) = world_aabb(node) {
                (x, y, width, height)
            } else {
                let (wx, wy) = node.transform.apply(0.0, 0.0);
                (wx, wy, 0.0_f64, 0.0_f64)
            };
            boxes.push(NodeBox {
                name: if node.name.is_empty() {
                    id_str.clone()
                } else {
                    node.name.clone()
                },
                x0: bx,
                y0: by,
                x1: bx + bw,
                y1: by + bh,
            });
        } else {
            return ToolResult::error(format!("Node '{}' not found.", id_str));
        }
    }

    let mut measurements: Vec<serde_json::Value> = Vec::new();

    // Measure every pair (i, i+1) in the provided order, plus all combinations if ≤ 6 nodes
    let n = boxes.len();
    let pairs: Vec<(usize, usize)> = if n <= 6 {
        let mut p = Vec::new();
        for i in 0..n {
            for j in (i + 1)..n {
                p.push((i, j));
            }
        }
        p
    } else {
        (0..n - 1).map(|i| (i, i + 1)).collect()
    };

    for (i, j) in pairs {
        let a = &boxes[i];
        let b = &boxes[j];

        // Center-to-center
        let acx = (a.x0 + a.x1) / 2.0;
        let acy = (a.y0 + a.y1) / 2.0;
        let bcx = (b.x0 + b.x1) / 2.0;
        let bcy = (b.y0 + b.y1) / 2.0;
        let center_dist = ((bcx - acx).powi(2) + (bcy - acy).powi(2)).sqrt();

        // Edge-to-edge horizontal gap
        let h_gap = if a.x1 <= b.x0 {
            b.x0 - a.x1 // a is left of b
        } else if b.x1 <= a.x0 {
            b.x1 - a.x0 // b is left of a (negative means overlap)
        } else {
            // Overlapping horizontally
            let overlap = a.x1.min(b.x1) - a.x0.max(b.x0);
            -overlap
        };

        // Edge-to-edge vertical gap
        let v_gap = if a.y1 <= b.y0 {
            b.y0 - a.y1
        } else if b.y1 <= a.y0 {
            b.y1 - a.y0
        } else {
            let overlap = a.y1.min(b.y1) - a.y0.max(b.y0);
            -overlap
        };

        // Alignment offsets
        let h_align_offset = (acy - bcy).abs(); // how misaligned vertically (for horizontal layout)
        let v_align_offset = (acx - bcx).abs(); // how misaligned horizontally (for vertical layout)

        measurements.push(json!({
            "from": a.name,
            "to": b.name,
            "center_to_center_px": (center_dist * 10.0).round() / 10.0,
            "horizontal_gap_px": (h_gap * 10.0).round() / 10.0,
            "vertical_gap_px": (v_gap * 10.0).round() / 10.0,
            "horizontal_alignment_offset_px": (h_align_offset * 10.0).round() / 10.0,
            "vertical_alignment_offset_px": (v_align_offset * 10.0).round() / 10.0,
            "overlapping": h_gap < 0.0 && v_gap < 0.0,
        }));
    }

    ToolResult::text(format!("Measured {} pair(s).", measurements.len()))
        .with_data(json!({ "measurements": measurements }))
}
