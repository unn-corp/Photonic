//! Document export utilities (SVG, etc.).

use crate::{
    layer::BlendMode,
    node::{NodeId, SceneNode, SceneNodeKind, TextAlign},
    style::{Fill, FillKind, Gradient, GradientKind, LineCap, LineJoin, Stroke, StrokeAlign},
    transform::Transform,
    Color, Document,
};
use std::collections::{HashMap, HashSet};

// ─── Export options ───────────────────────────────────────────────────────────

/// Options controlling SVG export output.
#[derive(Debug, Clone)]
pub struct SvgExportOptions {
    /// Emit slugified node/layer names as `id` attributes (default: `true`).
    pub semantic_ids: bool,
    /// Decimal places for SVG dimension and viewBox values, clamped 1–6 (default: `4`).
    pub precision: u8,
    /// Background fill. `None` (the default) exports a transparent SVG — no
    /// background rect is emitted. `Some(color)` emits a full-artboard rect of
    /// that color (e.g. white) behind the artwork.
    pub background: Option<Color>,
}

impl Default for SvgExportOptions {
    fn default() -> Self {
        Self {
            semantic_ids: true,
            precision: 4,
            background: None,
        }
    }
}

/// How a selection SVG frames its content in the `viewBox`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SvgNormalize {
    /// Tight union bbox of the selected nodes (legacy default).
    #[default]
    Tight,
    /// Center the content in a uniform square viewBox. `pad` is the padding as a
    /// fraction of the square side (e.g. `0.1` = 10% breathing room each way), so
    /// every icon in a set comes out at the same aspect ratio and scale.
    Square { pad: f64 },
}

/// Options controlling selection / icon SVG export.
#[derive(Debug, Clone)]
pub struct SvgSelectionOptions {
    /// Decimal places for coordinates and path data, clamped 1–6 (default: `4`).
    /// Fixes bloated 15-decimal path data on selection exports.
    pub precision: u8,
    /// Light optimization pass: trim trailing zeros (always on when set) — pairs
    /// with `precision` to keep icon SVGs compact.
    pub optimize: bool,
    /// viewBox framing (tight bbox vs. uniform centered square).
    pub normalize: SvgNormalize,
}

impl Default for SvgSelectionOptions {
    fn default() -> Self {
        Self {
            precision: 4,
            optimize: true,
            normalize: SvgNormalize::Tight,
        }
    }
}

// ─── Shared SVG emit context ──────────────────────────────────────────────────

/// Threaded through the node emitters: accumulates `<defs>` while deduplicating
/// structurally-identical paint definitions (one shared `<linearGradient>` per
/// unique paint instead of `grad-0`, `grad-1`, … clones) and carries the numeric
/// precision used for coordinates and path data.
struct SvgCtx {
    defs: String,
    counter: usize,
    /// Maps a paint's structural signature → the id already emitted for it.
    cache: HashMap<String, String>,
    /// Decimal places for def coordinates / gradient stops.
    coord_p: usize,
    /// `None` ⇒ emit full-precision path `d` via the cached string (full-document
    /// export, unchanged). `Some(p)` ⇒ re-emit path `d` rounded to `p` decimals.
    path_p: Option<usize>,
}

impl SvgCtx {
    fn new(coord_p: usize, path_p: Option<usize>) -> Self {
        Self {
            defs: String::new(),
            counter: 0,
            cache: HashMap::new(),
            coord_p,
            path_p,
        }
    }

    /// Intern a paint def by its structural signature `sig`. Returns the shared id
    /// (emitting the def via `make_def(id)` only the first time it is seen).
    fn intern(
        &mut self,
        prefix: &str,
        sig: String,
        make_def: impl FnOnce(&str) -> String,
    ) -> String {
        if let Some(id) = self.cache.get(&sig) {
            return id.clone();
        }
        let id = format!("{prefix}-{}", self.counter);
        self.counter += 1;
        let def = make_def(&id);
        self.defs.push_str(&def);
        self.cache.insert(sig, id);
        format!("{prefix}-{}", self.counter - 1)
    }
}

/// Format a float to `p` decimals, trimming trailing zeros and a bare `-0`.
fn fmt(v: f64, p: usize) -> String {
    let s = format!("{:.*}", p, v);
    if s.contains('.') {
        let t = s.trim_end_matches('0').trim_end_matches('.');
        if t.is_empty() || t == "-" || t == "-0" {
            "0".to_string()
        } else {
            t.to_string()
        }
    } else if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// Re-emit a path's geometry as an SVG `d` string with coordinates rounded to `p`
/// decimals (absolute commands). Used when a precision cap is requested so icon
/// exports don't carry 15-decimal path data.
fn path_d_rounded(path: &crate::path::PathData, p: usize) -> String {
    use kurbo::PathEl;
    let bez = path.to_bez_path();
    let mut out = String::new();
    for el in bez.elements() {
        match el {
            PathEl::MoveTo(pt) => out.push_str(&format!("M{} {}", fmt(pt.x, p), fmt(pt.y, p))),
            PathEl::LineTo(pt) => out.push_str(&format!("L{} {}", fmt(pt.x, p), fmt(pt.y, p))),
            PathEl::QuadTo(c, pt) => out.push_str(&format!(
                "Q{} {} {} {}",
                fmt(c.x, p),
                fmt(c.y, p),
                fmt(pt.x, p),
                fmt(pt.y, p)
            )),
            PathEl::CurveTo(c1, c2, pt) => out.push_str(&format!(
                "C{} {} {} {} {} {}",
                fmt(c1.x, p),
                fmt(c1.y, p),
                fmt(c2.x, p),
                fmt(c2.y, p),
                fmt(pt.x, p),
                fmt(pt.y, p)
            )),
            PathEl::ClosePath => out.push('Z'),
        }
    }
    out
}

// ─── Full-document export ─────────────────────────────────────────────────────

/// Export `doc` as an SVG string.
///
/// - Outputs `<!-- photonic-svg-v1 -->` as the first line for pipeline stability.
/// - Layers are emitted as `<g id="layer-name">` elements in draw order.
/// - When `opts.semantic_ids` is true, every node element receives an `id`
///   derived from its name (slugified, deduplicated with a `-2`/`-3` suffix).
/// - Gradients are collected into a `<defs>` block.
/// - Transforms use SVG `matrix(a,b,c,d,e,f)` syntax (identity is omitted).
pub fn export_svg(doc: &Document, opts: &SvgExportOptions) -> String {
    let p = opts.precision.clamp(1, 6) as usize;
    // Full-document export preserves full-precision path `d` (path_p = None) for
    // back-compatibility; only viewBox/coord values honor `precision`.
    let mut ctx = SvgCtx::new(p, None);
    let mut body = String::new();

    // Pre-build node ID map when semantic IDs are enabled.
    let id_map: Option<HashMap<NodeId, String>> = if opts.semantic_ids {
        let mut used: HashSet<String> = HashSet::new();
        let mut map: HashMap<NodeId, String> = HashMap::new();
        for layer_id in &doc.layer_order {
            if let Some(layer) = doc.layers.get(layer_id) {
                for node_id in &layer.node_ids {
                    if let Some(node) = doc.nodes.get(node_id) {
                        collect_node_ids(node, doc, &mut used, &mut map);
                    }
                }
            }
        }
        Some(map)
    } else {
        None
    };

    // Optional background rect. `None` => transparent SVG (no rect emitted).
    if let Some(bg) = opts.background {
        body.push_str(&format!(
            "  <rect width=\"{w:.p$}\" height=\"{h:.p$}\" fill=\"{fill}\"/>\n",
            w = doc.width,
            h = doc.height,
            p = p,
            fill = bg.to_hex(),
        ));
    }

    let mut used_layer_ids: HashSet<String> = HashSet::new();
    for layer_id in &doc.layer_order {
        let layer = match doc.layers.get(layer_id) {
            Some(l) if l.visible && l.print => l,
            _ => continue,
        };

        let layer_id_str = if opts.semantic_ids {
            unique_id(&slugify(&layer.name), &mut used_layer_ids)
        } else {
            format!("layer-{}", layer.id)
        };

        let mut attrs = format!(" id=\"{}\"", layer_id_str);
        if (layer.opacity - 1.0).abs() > 0.001 {
            attrs.push_str(&format!(" opacity=\"{:.4}\"", layer.opacity));
        }
        // Layer blend mode → CSS `mix-blend-mode` (the SVG keywords are 1:1 with
        // our BlendMode, so every mode round-trips). Normal is the default, omit.
        if layer.blend_mode != crate::layer::BlendMode::Normal {
            attrs.push_str(&format!(
                " style=\"mix-blend-mode:{}\"",
                layer.blend_mode.to_css()
            ));
        }
        body.push_str(&format!("  <g{}>\n", attrs));

        for node_id in &layer.node_ids {
            if let Some(node) = doc.nodes.get(node_id) {
                emit_node_inner(node, doc, &mut ctx, &mut body, 4, None, id_map.as_ref());
            }
        }

        body.push_str("  </g>\n");
    }

    let defs_block = if ctx.defs.is_empty() {
        String::new()
    } else {
        format!("  <defs>\n{}  </defs>\n", ctx.defs)
    };

    format!(
        "<!-- photonic-svg-v1 -->\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         width=\"{w:.p$}\" height=\"{h:.p$}\" viewBox=\"0 0 {w:.p$} {h:.p$}\">\n\
         {defs}{body}</svg>",
        w = doc.width,
        h = doc.height,
        p = p,
        defs = defs_block,
        body = body,
    )
}

// ─── Selection export ─────────────────────────────────────────────────────────

/// Export a subset of nodes as a self-contained SVG with a tight viewBox.
///
/// - `node_ids`: which nodes to include. Returns an empty SVG if none are found.
/// - Node `name` is slugified and used as the `id` attribute on each element.
/// - No artboard background rect is emitted.
/// - viewBox is the union of all selected nodes' world-space bounding boxes;
///   falls back to full document dimensions if no bounds can be computed.
pub fn export_nodes_as_svg(doc: &Document, node_ids: &[NodeId]) -> String {
    export_nodes_as_svg_opts(doc, node_ids, &SvgSelectionOptions::default())
}

/// Export a subset of nodes as SVG with explicit precision / normalization /
/// dedup options (see [`SvgSelectionOptions`]).
pub fn export_nodes_as_svg_opts(
    doc: &Document,
    node_ids: &[NodeId],
    opts: &SvgSelectionOptions,
) -> String {
    let cp = opts.precision.clamp(1, 6) as usize;
    let mut ctx = SvgCtx::new(cp, Some(cp));
    let mut body = String::new();
    let mut combined_bbox: Option<kurbo::Rect> = None;

    // Collect nodes in document order (layer order → z-order within layer).
    for layer_id in &doc.layer_order {
        let layer = match doc.layers.get(layer_id) {
            Some(l) if l.visible && l.print => l,
            _ => continue,
        };
        for node_id in &layer.node_ids {
            if !node_ids.contains(node_id) {
                continue;
            }
            if let Some(node) = doc.nodes.get(node_id) {
                if !node.visible {
                    continue;
                }
                if let Some(wb) = node_world_bbox(node, doc) {
                    combined_bbox = Some(match combined_bbox {
                        None => wb,
                        Some(prev) => prev.union(wb),
                    });
                }
                let slug = slugify(&node.name);
                emit_node_inner(node, doc, &mut ctx, &mut body, 2, Some(&slug), None);
            }
        }
    }

    let tight = match combined_bbox {
        Some(r) => (r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0),
        None => (0.0, 0.0, doc.width, doc.height),
    };

    // Frame the viewBox per the normalization mode. Content coordinates are never
    // moved — a square/normalized frame is achieved purely by widening the
    // viewBox and centering it on the content, so every icon in a set lands at a
    // uniform aspect ratio and scale without geometry rewrites.
    let (vx, vy, vw, vh) = match opts.normalize {
        SvgNormalize::Tight => tight,
        SvgNormalize::Square { pad } => {
            let (tx, ty, tw, th) = tight;
            let cx = tx + tw / 2.0;
            let cy = ty + th / 2.0;
            let base = tw.max(th).max(1e-6);
            let side = base * (1.0 + 2.0 * pad.max(0.0));
            (cx - side / 2.0, cy - side / 2.0, side, side)
        }
    };

    let defs_block = if ctx.defs.is_empty() {
        String::new()
    } else {
        format!("  <defs>\n{}  </defs>\n", ctx.defs)
    };

    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         width=\"{vw}\" height=\"{vh}\" viewBox=\"{vx} {vy} {vw} {vh}\">\n\
         {defs_block}{body}</svg>",
        vw = fmt(vw, cp),
        vh = fmt(vh, cp),
        vx = fmt(vx, cp),
        vy = fmt(vy, cp),
    )
}

/// Compute the world-space axis-aligned bounding box of a node by applying its
/// affine transform to its local bounding box.  Groups are handled recursively.
pub(crate) fn node_world_bbox(node: &SceneNode, doc: &Document) -> Option<kurbo::Rect> {
    let local = match &node.kind {
        SceneNodeKind::Path(p) => p.path_data.bounding_box()?,
        SceneNodeKind::Group(g) => {
            let mut combined: Option<kurbo::Rect> = None;
            for cid in &g.children {
                if let Some(child) = doc.nodes.get(cid) {
                    if let Some(cb) = node_world_bbox(child, doc) {
                        combined = Some(combined.map_or(cb, |prev| prev.union(cb)));
                    }
                }
            }
            combined?
        }
        // Text has no path outline, but its measured local bounds still define
        // a world-space extent for export, selection, and artboard ownership.
        SceneNodeKind::Text(_) => node.local_bounds()?,
        SceneNodeKind::Raster(r) => {
            if r.is_adjustment_layer() {
                return None;
            }
            kurbo::Rect::new(0.0, 0.0, r.image.width as f64, r.image.height as f64)
        }
    };
    Some(node.transform.to_kurbo().transform_rect_bbox(local))
}

/// Return a deduplicated slug: appends `-2`, `-3`, … when `base` is already taken.
fn unique_id(base: &str, used: &mut HashSet<String>) -> String {
    if !used.contains(base) {
        used.insert(base.to_string());
        return base.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{}-{}", base, n);
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        n += 1;
    }
}

/// Recursively populate `map` with slugified, deduplicated IDs for every node.
fn collect_node_ids(
    node: &SceneNode,
    doc: &Document,
    used: &mut HashSet<String>,
    map: &mut HashMap<NodeId, String>,
) {
    let slug = slugify(&node.name);
    let id = unique_id(&slug, used);
    map.insert(node.id, id);
    if let SceneNodeKind::Group(g) = &node.kind {
        for child_id in &g.children {
            if let Some(child) = doc.nodes.get(child_id) {
                collect_node_ids(child, doc, used, map);
            }
        }
    }
}

/// Convert a node name to a URL-safe `id` slug.
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = true; // start true to suppress leading dashes
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "node".to_string()
    } else {
        out
    }
}

// ─── Node emitters ────────────────────────────────────────────────────────────

fn emit_node_inner(
    node: &SceneNode,
    doc: &Document,
    ctx: &mut SvgCtx,
    body: &mut String,
    indent: usize,
    // Explicit ID override (used by selection export).
    id_override: Option<&str>,
    // Map of NodeId → unique slug (used by full-document export).
    id_map: Option<&HashMap<NodeId, String>>,
) {
    if !node.visible {
        return;
    }

    let pad = " ".repeat(indent);
    let id_attr = id_override
        .map(|s| format!(" id=\"{}\"", s))
        .or_else(|| {
            id_map
                .and_then(|m| m.get(&node.id))
                .map(|s| format!(" id=\"{}\"", s))
        })
        .unwrap_or_default();
    let transform = transform_attr(&node.transform);
    let opacity = if (node.opacity - 1.0).abs() > 0.001 {
        format!(" opacity=\"{:.4}\"", node.opacity)
    } else {
        String::new()
    };
    // Non-Normal blend modes round-trip via the CSS `mix-blend-mode` property.
    let blend = if node.blend_mode != BlendMode::Normal {
        format!(" style=\"mix-blend-mode:{}\"", node.blend_mode.to_css())
    } else {
        String::new()
    };

    let filter = filter_attrs(node, &mut ctx.defs);

    match &node.kind {
        SceneNodeKind::Path(p) => {
            let fill = fill_attrs(&p.fill, ctx);
            let stroke = stroke_attrs(&p.stroke, affine_scale(&node.transform), ctx);
            let fill_rule = if p.is_compound {
                " fill-rule=\"evenodd\""
            } else {
                ""
            };
            let d = match ctx.path_p {
                Some(prec) => path_d_rounded(&p.path_data, prec),
                None => p.path_data.as_svg().to_string(),
            };
            body.push_str(&format!(
                "{}<path{}{}{}{}{}{}{}{} d=\"{}\"/>\n",
                pad, id_attr, transform, opacity, blend, filter, fill, stroke, fill_rule, d,
            ));
        }
        SceneNodeKind::Group(g) => {
            // Live boolean (#25): emit the single resolved path styled by the
            // bottom-most path child, instead of the stacked children.
            let resolved = if g.live_boolean.is_some() {
                doc.resolve_live_boolean(node.id)
            } else {
                None
            };
            if let Some(resolved) = resolved {
                let (fill, stroke) = g
                    .children
                    .iter()
                    .filter_map(|c| doc.nodes.get(c))
                    .find_map(|c| match &c.kind {
                        SceneNodeKind::Path(p) => Some((
                            fill_attrs(&p.fill, ctx),
                            stroke_attrs(&p.stroke, affine_scale(&node.transform), ctx),
                        )),
                        _ => None,
                    })
                    .unwrap_or_default();
                let d = match ctx.path_p {
                    Some(prec) => path_d_rounded(&resolved, prec),
                    None => resolved.as_svg().to_string(),
                };
                body.push_str(&format!(
                    "{}<path{}{}{}{}{}{}{} d=\"{}\"/>\n",
                    pad, id_attr, transform, opacity, blend, filter, fill, stroke, d,
                ));
            } else {
                body.push_str(&format!(
                    "{}<g{}{}{}{}{}>\n",
                    pad, id_attr, transform, opacity, blend, filter
                ));
                for child_id in &g.children {
                    if let Some(child) = doc.nodes.get(child_id) {
                        emit_node_inner(child, doc, ctx, body, indent + 2, None, id_map);
                    }
                }
                body.push_str(&format!("{}</g>\n", pad));
            }
        }
        SceneNodeKind::Text(t) => {
            let fill = fill_attrs(&t.fill, ctx);
            let stroke = stroke_attrs(&t.stroke, affine_scale(&node.transform), ctx);
            let anchor = match t.align {
                TextAlign::Left => "start",
                TextAlign::Center => "middle",
                TextAlign::Right => "end",
            };
            let content = t
                .content
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            // Advanced character metrics: super/subscript reduces the rendered
            // font size and offsets the baseline; an explicit baseline shift adds
            // to that offset. SVG `baseline-shift` is positive-up, matching ours.
            let effective_font_size = t.font_size * t.script_position.size_scale();
            let shift_units =
                t.script_position.baseline_offset_em() * t.font_size + t.baseline_shift;
            let baseline_shift_attr = if shift_units.abs() > 1e-9 {
                format!(" baseline-shift=\"{shift_units}\"")
            } else {
                String::new()
            };
            body.push_str(&format!(
                "{}<text{}{}{}{} font-family=\"{}\" font-size=\"{}\" font-weight=\"{}\" \
                 text-anchor=\"{}\"{}{}{}>{}</text>\n",
                pad,
                id_attr,
                transform,
                opacity,
                blend,
                t.font_family,
                effective_font_size,
                t.font_weight,
                anchor,
                baseline_shift_attr,
                fill,
                stroke,
                content,
            ));
        }
        SceneNodeKind::Raster(r) => {
            // Non-destructive adjustment layers carry no pixels of their own —
            // they recolor the composite beneath them, which a flat SVG cannot
            // represent. Skip them rather than emit a bogus 1×1 placeholder
            // (the .photon format preserves them; PNG/JPEG bake them in).
            if r.is_adjustment_layer() {
                return;
            }
            // Embed the pixel data as a base64 PNG <image>. The node transform
            // positions/scales it; the image spans its native pixel size.
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(r.image.to_png());
            body.push_str(&format!(
                "{}<image{}{}{}{} width=\"{}\" height=\"{}\" \
                 href=\"data:image/png;base64,{}\"/>\n",
                pad, id_attr, transform, opacity, blend, r.image.width, r.image.height, b64,
            ));
        }
    }
}

fn transform_attr(t: &Transform) -> String {
    let [a, b, c, d, e, f] = t.matrix;
    if (a - 1.0).abs() < 1e-9
        && b.abs() < 1e-9
        && c.abs() < 1e-9
        && (d - 1.0).abs() < 1e-9
        && e.abs() < 1e-9
        && f.abs() < 1e-9
    {
        return String::new();
    }
    format!(" transform=\"matrix({a},{b},{c},{d},{e},{f})\"")
}

/// Emit an SVG `<filter>` for the node's live effects (drop shadow, object blur,
/// feather) into `defs` and return the ` filter="url(#…)"` attribute, or an
/// empty string when no effects are enabled. Effects chain in order: blur the
/// source first, then the drop shadow.
fn filter_attrs(node: &SceneNode, defs: &mut String) -> String {
    let ds = &node.drop_shadow;
    let ob = &node.object_blur;
    let ft = &node.feather;
    if !ds.enabled && !ob.enabled && !ft.enabled {
        return String::new();
    }
    let id = format!("fx{}", node.id.simple());
    let mut prims = String::new();
    // Object blur / feather both soften the graphic; object blur wins if both set.
    if ob.enabled {
        prims.push_str(&format!(
            "    <feGaussianBlur in=\"SourceGraphic\" stdDeviation=\"{:.3}\"/>\n",
            ob.radius
        ));
    } else if ft.enabled {
        prims.push_str(&format!(
            "    <feGaussianBlur in=\"SourceGraphic\" stdDeviation=\"{:.3}\"/>\n",
            ft.radius
        ));
    }
    if ds.enabled {
        let c = &ds.color;
        let hex = format!(
            "#{:02x}{:02x}{:02x}",
            (c.r * 255.0) as u8,
            (c.g * 255.0) as u8,
            (c.b * 255.0) as u8
        );
        prims.push_str(&format!(
            "    <feDropShadow dx=\"{:.3}\" dy=\"{:.3}\" stdDeviation=\"{:.3}\" \
             flood-color=\"{}\" flood-opacity=\"{:.3}\"/>\n",
            ds.dx,
            ds.dy,
            ds.blur,
            hex,
            (c.a * ds.opacity).clamp(0.0, 1.0),
        ));
    }
    // Generous region so blurs/shadows are not clipped.
    defs.push_str(&format!(
        "    <filter id=\"{}\" x=\"-50%\" y=\"-50%\" width=\"200%\" height=\"200%\">\n{}    </filter>\n",
        id, prims
    ));
    format!(" filter=\"url(#{})\"", id)
}

/// Wrap a paint-def id into a ` fill="url(#id)"` (or `stroke=`) attribute, adding
/// a `-opacity` attribute when the paint opacity is below 1.
fn paint_url_attr(prop: &str, id: &str, opacity: f32) -> String {
    if (opacity - 1.0).abs() < 0.001 {
        format!(" {prop}=\"url(#{id})\"")
    } else {
        format!(" {prop}=\"url(#{id})\" {prop}-opacity=\"{opacity:.4}\"")
    }
}

/// Emit stops for a linear/radial gradient with coordinate precision `p`.
fn gradient_stops(g: &Gradient, p: usize) -> String {
    g.stops
        .iter()
        .map(|s| {
            let hex = s.color.to_hex();
            if (s.color.a - 1.0).abs() < 0.001 {
                format!(
                    "      <stop offset=\"{}\" stop-color=\"{}\"/>\n",
                    fmt(s.offset as f64, p),
                    hex
                )
            } else {
                format!(
                    "      <stop offset=\"{}\" stop-color=\"{}\" stop-opacity=\"{:.4}\"/>\n",
                    fmt(s.offset as f64, p),
                    hex,
                    s.color.a
                )
            }
        })
        .collect()
}

/// Intern a linear/radial gradient def (deduped) and return its shared id.
fn gradient_ref(ctx: &mut SvgCtx, g: &Gradient) -> String {
    let p = ctx.coord_p;
    let stops = gradient_stops(g, p);
    match g.kind {
        GradientKind::Linear => {
            let (x1, y1, x2, y2) = if g.coords.len() >= 4 {
                (g.coords[0], g.coords[1], g.coords[2], g.coords[3])
            } else {
                (0.0, 0.0, 1.0, 0.0)
            };
            let (fx1, fy1, fx2, fy2) = (fmt(x1, p), fmt(y1, p), fmt(x2, p), fmt(y2, p));
            let sig = format!("lin|{fx1}|{fy1}|{fx2}|{fy2}|{stops}");
            ctx.intern("grad", sig, |id| {
                format!(
                    "    <linearGradient id=\"{id}\" x1=\"{fx1}\" y1=\"{fy1}\" x2=\"{fx2}\" \
                     y2=\"{fy2}\" gradientUnits=\"userSpaceOnUse\">\n{stops}    </linearGradient>\n",
                )
            })
        }
        GradientKind::Radial => {
            let (cx, cy, r) = if g.coords.len() >= 5 {
                (g.coords[0], g.coords[1], g.coords[4])
            } else {
                (0.5, 0.5, 0.5)
            };
            let (fcx, fcy, fr) = (fmt(cx, p), fmt(cy, p), fmt(r, p));
            let sig = format!("rad|{fcx}|{fcy}|{fr}|{stops}");
            ctx.intern("grad", sig, |id| {
                format!(
                    "    <radialGradient id=\"{id}\" cx=\"{fcx}\" cy=\"{fcy}\" r=\"{fr}\" \
                     gradientUnits=\"userSpaceOnUse\">\n{stops}    </radialGradient>\n",
                )
            })
        }
    }
}

fn fill_attrs(fill: &Fill, ctx: &mut SvgCtx) -> String {
    if !fill.enabled {
        return " fill=\"none\"".to_string();
    }
    let p = ctx.coord_p;
    match &fill.kind {
        FillKind::None => " fill=\"none\"".to_string(),
        FillKind::Solid(c) => solid_fill_attr(c, fill.opacity),
        FillKind::FluidGradient(fg) => {
            // Export as a radial gradient approximation: centroid center, first→last color.
            if fg.points.is_empty() {
                return " fill=\"none\"".to_string();
            }
            if fg.points.len() == 1 {
                return solid_fill_attr(&fg.points[0].color, fill.opacity);
            }
            let cx: f64 = fg.points.iter().map(|p| p.x).sum::<f64>() / fg.points.len() as f64;
            let cy: f64 = fg.points.iter().map(|p| p.y).sum::<f64>() / fg.points.len() as f64;
            let max_r: f64 = fg
                .points
                .iter()
                .map(|p| ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt())
                .fold(0.0_f64, f64::max)
                .max(1.0);
            let first = &fg.points[0];
            let last = &fg.points[fg.points.len() - 1];
            let (fcx, fcy, fr) = (fmt(cx, p), fmt(cy, p), fmt(max_r, p));
            let stops = format!(
                "      <stop offset=\"0\" stop-color=\"{}\"/>\n\
                 \x20     <stop offset=\"1\" stop-color=\"{}\"/>\n",
                first.color.to_hex(),
                last.color.to_hex()
            );
            let sig = format!("rad|{fcx}|{fcy}|{fr}|{stops}");
            let id = ctx.intern("grad", sig, |id| {
                format!(
                    "    <radialGradient id=\"{id}\" cx=\"{fcx}\" cy=\"{fcy}\" r=\"{fr}\" \
                     gradientUnits=\"userSpaceOnUse\">\n{stops}    </radialGradient>\n",
                )
            });
            paint_url_attr("fill", &id, fill.opacity)
        }
        FillKind::MeshGradient(mg) => {
            // Export as a linear-gradient approximation from the first to the
            // last cell colour along the grid diagonal (SVG has no mesh fill).
            if mg.cell_colors.is_empty() {
                return " fill=\"none\"".to_string();
            }
            if mg.cell_colors.len() == 1 {
                return solid_fill_attr(&mg.cell_colors[0], fill.opacity);
            }
            let first = mg.cell_colors[0];
            let last = *mg.cell_colors.last().unwrap();
            let (x1, y1, x2, y2) = (
                fmt(*mg.x_lines.first().unwrap_or(&0.0), p),
                fmt(*mg.y_lines.first().unwrap_or(&0.0), p),
                fmt(*mg.x_lines.last().unwrap_or(&1.0), p),
                fmt(*mg.y_lines.last().unwrap_or(&1.0), p),
            );
            let stops = format!(
                "      <stop offset=\"0\" stop-color=\"{}\"/>\n\
                 \x20     <stop offset=\"1\" stop-color=\"{}\"/>\n",
                first.to_hex(),
                last.to_hex()
            );
            let sig = format!("lin|{x1}|{y1}|{x2}|{y2}|{stops}");
            let id = ctx.intern("grad", sig, |id| {
                format!(
                    "    <linearGradient id=\"{id}\" x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" \
                     y2=\"{y2}\" gradientUnits=\"userSpaceOnUse\">\n{stops}    </linearGradient>\n",
                )
            });
            paint_url_attr("fill", &id, fill.opacity)
        }
        FillKind::Gradient(g) => {
            let id = gradient_ref(ctx, g);
            paint_url_attr("fill", &id, fill.opacity)
        }
        FillKind::Pattern(pat) => {
            let id = pattern_ref(ctx, pat);
            paint_url_attr("fill", &id, fill.opacity)
        }
    }
}

/// Intern a pattern def (deduped by tile bytes + transform) and return its id.
fn pattern_ref(ctx: &mut SvgCtx, p: &crate::style::PatternFill) -> String {
    {
        use base64::Engine;

        let tw = p.tile.width.max(1);
        let th = p.tile.height.max(1);
        // The pattern cell is the tile plus its inter-tile gutter.
        let cell_w = tw as f64 + p.spacing.max(0.0);
        let cell_h = th as f64 + p.spacing.max(0.0);

        let png = p.tile.to_png();
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);

        // patternTransform mirrors PatternFill's document-space transform:
        // translate(offset) → rotate(deg) → scale(s). SVG applies these
        // right-to-left, matching the inverse-transform sample order.
        let deg = p.rotation.to_degrees();
        let mut xform = String::new();
        if p.offset[0] != 0.0 || p.offset[1] != 0.0 {
            xform.push_str(&format!("translate({} {}) ", p.offset[0], p.offset[1]));
        }
        if deg.abs() > 1e-9 {
            xform.push_str(&format!("rotate({deg}) "));
        }
        if (p.scale - 1.0).abs() > 1e-9 {
            xform.push_str(&format!("scale({}) ", p.scale));
        }
        let xform = xform.trim_end();
        let xform_attr = if xform.is_empty() {
            String::new()
        } else {
            format!(" patternTransform=\"{xform}\"")
        };

        // Grid layout is exact; brick/hex staggers are approximated by the grid
        // cell here — on-canvas/headless remain the source of truth. Dedupe by the
        // full tile bytes + geometry so repeated tiles share one <pattern>.
        let sig = format!("pat|{cell_w}|{cell_h}|{xform_attr}|{b64}");
        ctx.intern("pat", sig, move |id| {
            format!(
                "    <pattern id=\"{id}\" patternUnits=\"userSpaceOnUse\" \
                 width=\"{cell_w}\" height=\"{cell_h}\"{xform_attr}>\n\
                 \x20     <image x=\"0\" y=\"0\" width=\"{tw}\" height=\"{th}\" \
                 href=\"data:image/png;base64,{b64}\"/>\n\
                 \x20 </pattern>\n",
            )
        })
    }
}

fn solid_fill_attr(c: &Color, fill_opacity: f32) -> String {
    let hex = c.to_hex();
    let opacity = c.a * fill_opacity;
    if (opacity - 1.0).abs() < 0.001 {
        format!(" fill=\"{hex}\"")
    } else {
        format!(" fill=\"{hex}\" fill-opacity=\"{opacity:.4}\"")
    }
}

/// Uniform scale factor of an affine transform, `sqrt(|det|)` — used to keep
/// stroke widths constant regardless of the object's scale (non-scaling stroke),
/// matching the renderer. `1.0` for unscaled/rotated/translated transforms.
fn affine_scale(t: &Transform) -> f64 {
    let [a, b, c, d, _, _] = t.matrix;
    (a * d - b * c).abs().sqrt().max(1e-6)
}

/// `obj_scale` is the emitting element's transform scale; the stroke width is
/// divided by it so that, once the element's `transform="matrix(...)"` scales it
/// back up, the stroke renders at its authored width in canvas units — a
/// non-scaling stroke, consistent with the live canvas and raster export.
fn stroke_attrs(stroke: &Stroke, obj_scale: f64, ctx: &mut SvgCtx) -> String {
    if !stroke.enabled || stroke.width <= 0.0 {
        return " stroke=\"none\"".to_string();
    }
    // Non-solid stroke paint (#201): a gradient/pattern stroke exports as
    // `stroke="url(#id)"`, reusing the same deduped paint defs as fills. Solid or
    // unsupported paints fall through to the flat `color` path below.
    let paint_ref: Option<String> = match &stroke.paint {
        Some(FillKind::Gradient(g)) => Some(gradient_ref(ctx, g)),
        Some(FillKind::Pattern(p)) => Some(pattern_ref(ctx, p)),
        _ => None,
    };
    let hex = match &stroke.paint {
        // A solid override paint recolors the stroke.
        Some(FillKind::Solid(c)) => c.to_hex(),
        _ => stroke.color.to_hex(),
    };
    let opacity = stroke.color.a * stroke.opacity;
    let cap = match stroke.line_cap {
        LineCap::Butt => "butt",
        LineCap::Round => "round",
        LineCap::Square => "square",
    };
    let join = match stroke.line_join {
        LineJoin::Miter => "miter",
        LineJoin::Round => "round",
        LineJoin::Bevel => "bevel",
    };

    let align_attr = match stroke.align {
        StrokeAlign::Center => "",
        StrokeAlign::Inside => " stroke-alignment=\"inner\"",
        StrokeAlign::Outside => " stroke-alignment=\"outer\"",
    };
    // Gradient/pattern paint wins over the flat color when present.
    let stroke_val = match &paint_ref {
        Some(id) => format!("url(#{id})"),
        None => hex,
    };
    let mut s = format!(
        " stroke=\"{stroke_val}\" stroke-width=\"{}\" stroke-linecap=\"{cap}\" stroke-linejoin=\"{join}\"{align_attr}",
        stroke.width / obj_scale,
    );
    if join == "miter" && (stroke.miter_limit - 4.0).abs() > 0.001 {
        s.push_str(&format!(" stroke-miterlimit=\"{}\"", stroke.miter_limit));
    }
    if (opacity - 1.0).abs() > 0.001 {
        s.push_str(&format!(" stroke-opacity=\"{opacity:.4}\""));
    }
    if !stroke.dash_array.is_empty() {
        let parts: Vec<String> = stroke.dash_array.iter().map(|d| d.to_string()).collect();
        s.push_str(&format!(" stroke-dasharray=\"{}\"", parts.join(",")));
        if stroke.dash_offset.abs() > 0.001 {
            s.push_str(&format!(" stroke-dashoffset=\"{}\"", stroke.dash_offset));
        }
    }
    s
}

// ─── PDF export (vector) ────────────────────────────────────────────────────

/// A colour resolved into the export colour model for the PDF content stream.
#[derive(Debug, Clone, Copy)]
pub enum PdfColor {
    Rgb([f32; 3]),
    Cmyk([f32; 4]),
}

/// Page geometry boxes, in PDF points (Y-up). T1.5/T1.8 expand media to include
/// bleed and set trim inside it; today all three equal the artboard.
#[derive(Debug, Clone, Copy)]
pub struct PageBoxes {
    pub media: [f32; 4],
    pub trim: [f32; 4],
    pub bleed: [f32; 4],
}

/// Options controlling vector PDF export.
#[derive(Debug, Clone, Default)]
pub struct PdfExportOptions {
    /// Paint a full-page background rectangle of this colour before the artwork.
    /// `None` (default) leaves the page background unpainted (white in viewers).
    pub background: Option<Color>,
    /// Convert text nodes to vector outlines (zero font deps). T0.2.
    pub outline_text: bool,
    /// Render trim + registration marks in the bleed/slug area. T1.6.
    pub marks: bool,
    /// Export colour model. Rgb (default) preserves current behaviour. T0.3.
    pub color_mode: crate::document::ColorMode,
    /// CMYK ICC profile path for conversion + OutputIntent. T0.3/T0.4.
    pub icc_profile: Option<std::path::PathBuf>,
}

/// Compute MediaBox/TrimBox/BleedBox in PDF points (T1.5/T1.8).
///
/// PDF points ≡ 72 dpi: `to_px(mm, Mm, 72.0)` converts mm → points.
///
/// Layout (all centred, Y-up, values in PDF points):
/// ```text
///   [0, 0, w+2*outer, h+2*outer]       ← MediaBox  (mark room + bleed + trim)
///   [mark_room, …, …+w+2*bleed, …]     ← BleedBox  (bleed outside trim)
///   [outer, outer, outer+w, outer+h]   ← TrimBox   (artboard, the "finished size")
/// ```
/// When `bleed_mm == 0` and `marks == false` all three collapse to `[0,0,w,h]`,
/// preserving the pre-bleed regression baseline exactly.
pub fn compute_page_boxes(doc: &Document, opts: &PdfExportOptions) -> PageBoxes {
    compute_page_boxes_dims(doc.width, doc.height, doc, opts)
}

/// Like [`compute_page_boxes`] but for an explicit page size in document px
/// (e.g. a single artboard's rectangle). DPI, bleed, and slug come from `doc`.
pub fn compute_page_boxes_dims(
    width_px: f64,
    height_px: f64,
    doc: &Document,
    opts: &PdfExportOptions,
) -> PageBoxes {
    use crate::units::{to_px, DocumentUnit::Mm};

    // Artwork is stored in px at the document DPI; PDF is in points (72 dpi).
    // Scale px → points so the page is the correct physical size at any DPI.
    let s = 72.0_f32 / doc.dpi as f32;
    let w = width_px as f32 * s;
    let h = height_px as f32 * s;

    // PDF points = 72 dpi; convert mm at that resolution.
    let pdf_dpi = 72.0_f64;
    let bleed_px = to_px(doc.bleed_mm, Mm, pdf_dpi) as f32;

    // Reserve room outside the bleed for marks (the slug band).
    // Use the document slug if non-zero; otherwise fall back to 5 mm so marks
    // always have somewhere to live when marks=true.
    let mark_room = if opts.marks {
        to_px(doc.slug_mm.max(5.0), Mm, pdf_dpi) as f32
    } else {
        0.0_f32
    };

    let outer = bleed_px + mark_room;

    // media  — page sheet including mark band
    let media = [0.0, 0.0, w + 2.0 * outer, h + 2.0 * outer];
    // bleed  — trim + bleed extension (mark_room offset from media origin)
    let bleed = [
        mark_room,
        mark_room,
        mark_room + w + 2.0 * bleed_px,
        mark_room + h + 2.0 * bleed_px,
    ];
    // trim   — the finished artboard size (outer offset from media origin)
    let trim = [outer, outer, outer + w, outer + h];

    PageBoxes { media, trim, bleed }
}

/// Resolve an RGB triple into the export colour model.
///
/// When `opts.color_mode == Cmyk` the colour is converted through a real ICC
/// profile (CoatedFOGRA39 by default, or `opts.icc_profile` when supplied).
/// The transform is cached process-wide so only the first call per profile pays
/// the parse cost.
fn convert_color(rgb: [f32; 3], opts: &PdfExportOptions) -> PdfColor {
    match opts.color_mode {
        crate::document::ColorMode::Cmyk => {
            let t = crate::color_cmyk::cached_transform(opts.icc_profile.as_deref());
            PdfColor::Cmyk(t.rgb_to_cmyk(rgb))
        }
        crate::document::ColorMode::Rgb => PdfColor::Rgb(rgb),
    }
}

/// Set the non-stroking (fill) colour on the content stream from a PdfColor.
fn set_fill_color(content: &mut pdf_writer::Content, c: PdfColor) {
    match c {
        PdfColor::Rgb([r, g, b]) => {
            content.set_fill_rgb(r, g, b);
        }
        PdfColor::Cmyk([cc, m, y, k]) => {
            content.set_fill_cmyk(cc, m, y, k);
        }
    }
}

/// Set the stroking colour on the content stream from a PdfColor.
fn set_stroke_color(content: &mut pdf_writer::Content, c: PdfColor) {
    match c {
        PdfColor::Rgb([r, g, b]) => {
            content.set_stroke_rgb(r, g, b);
        }
        PdfColor::Cmyk([cc, m, y, k]) => {
            content.set_stroke_cmyk(cc, m, y, k);
        }
    }
}

/// Emit a Text node as vector outlines. SEAM T0.2: when opts.outline_text, bridge
/// the photonic-render glyph outliner to emit_path_geometry. Today: no-op.
fn emit_text_pdf(
    _node: &SceneNode,
    _doc: &Document,
    _content: &mut pdf_writer::Content,
    _opts: &PdfExportOptions,
) {
}

/// Render trim and registration marks into the content stream (T1.6).
///
/// All coordinates are in absolute PDF page space (Y-up, origin = media bottom-left).
/// Called *after* `content.restore_state()` so the artwork CTM has been popped.
///
/// Marks drawn (when `opts.marks` and sufficient mark_room):
/// - **Crop/trim marks**: two hairlines at each of the 4 trim corners, extending
///   outward from the trim edge with a small gap so they do not cross into the live
///   area.  Colour: registration (CMYK 1,1,1,1), 0.25 pt line width.
/// - **Registration targets**: a crosshair centred on each of the 4 trim sides,
///   placed in the middle of the mark band outside the bleed.  A circle (4-arc
///   bezier approximation) surrounds the crosshair.  Same colour/weight.
///   NOTE: the circle is approximated by cubic beziers (k ≈ 0.5523); no arc
///   primitive is available in the PDF content-stream operator set.
fn emit_marks(content: &mut pdf_writer::Content, boxes: &PageBoxes, opts: &PdfExportOptions) {
    if !opts.marks {
        return;
    }

    // Derive geometry.
    let [tx0, ty0, tx1, ty1] = boxes.trim; // trim box in page space
    let [bx0, by0, bx1, by1] = boxes.bleed;

    // mark_room is the band between bleed and media edges.
    let mark_room_x = bx0; // == media_x0 + mark_room (= mark_room since media_x0=0)
    let mark_room_y = by0; // same for Y
    if mark_room_x <= 0.0 || mark_room_y <= 0.0 {
        // No room for marks (bleed_mm=0 with marks=true would be unusual, skip).
        return;
    }

    // Gap between trim edge and start of the crop mark hairline (3 pt).
    let gap = 3.0_f32;
    // Crop mark length in points (≈ 12 pt or up to 60% of mark_room).
    let mark_len = (mark_room_x * 0.6).clamp(6.0_f32, 12.0_f32);

    // Registration colour: CMYK all-ink (prints in all channels simultaneously).
    content.set_stroke_cmyk(1.0, 1.0, 1.0, 1.0);
    content.set_line_width(0.25);

    // ── Crop / trim marks ─────────────────────────────────────────────────────
    // Each corner emits two right-angle hairlines that bracket the trim corner
    // from outside the live area.  The marks start at `gap` beyond the trim edge
    // and extend a further `mark_len` outward.

    // Helper: emit one crop hairline from (x0,y0) to (x1,y1).
    let line = |content: &mut pdf_writer::Content, x0: f32, y0: f32, x1: f32, y1: f32| {
        content.move_to(x0, y0);
        content.line_to(x1, y1);
        content.stroke();
    };

    // Bottom-left corner (trim corner: tx0, ty0)
    // Horizontal hairline leftward from trim-left edge
    line(content, tx0 - gap, ty0, tx0 - gap - mark_len, ty0);
    // Vertical hairline downward from trim-bottom edge
    line(content, tx0, ty0 - gap, tx0, ty0 - gap - mark_len);

    // Bottom-right corner (trim corner: tx1, ty0)
    line(content, tx1 + gap, ty0, tx1 + gap + mark_len, ty0);
    line(content, tx1, ty0 - gap, tx1, ty0 - gap - mark_len);

    // Top-right corner (trim corner: tx1, ty1)
    line(content, tx1 + gap, ty1, tx1 + gap + mark_len, ty1);
    line(content, tx1, ty1 + gap, tx1, ty1 + gap + mark_len);

    // Top-left corner (trim corner: tx0, ty1)
    line(content, tx0 - gap, ty1, tx0 - gap - mark_len, ty1);
    line(content, tx0, ty1 + gap, tx0, ty1 + gap + mark_len);

    // ── Registration targets ──────────────────────────────────────────────────
    // Centred on each of the 4 trim sides, in the mid-point of the mark band
    // (between bleed edge and media edge).  A crosshair + circle.

    // Radius for the registration circle (half the mark band width, capped at 6 pt).
    let r = (mark_room_x * 0.4).clamp(3.0_f32, 6.0_f32);

    // Cubic bezier approximation constant for a quarter circle.
    let k = 0.5523_f32;

    let reg_target = |content: &mut pdf_writer::Content, cx: f32, cy: f32| {
        // Crosshair (two lines through the centre).
        content.move_to(cx - r, cy);
        content.line_to(cx + r, cy);
        content.stroke();
        content.move_to(cx, cy - r);
        content.line_to(cx, cy + r);
        content.stroke();

        // Circle approximated by 4 cubic bezier arcs (counter-clockwise from right).
        // Start at (cx+r, cy).
        content.move_to(cx + r, cy);
        // Q1: right → top
        content.cubic_to(cx + r, cy + k * r, cx + k * r, cy + r, cx, cy + r);
        // Q2: top → left
        content.cubic_to(cx - k * r, cy + r, cx - r, cy + k * r, cx - r, cy);
        // Q3: left → bottom
        content.cubic_to(cx - r, cy - k * r, cx - k * r, cy - r, cx, cy - r);
        // Q4: bottom → right
        content.cubic_to(cx + k * r, cy - r, cx + r, cy - k * r, cx + r, cy);
        content.close_path();
        content.stroke();
    };

    // Centre of the left mark band (between bleed-left and media-left).
    let cx_left = bx0 * 0.5;
    // Centre of the right mark band (between bleed-right and media-right).
    let cx_right = bx1 + (boxes.media[2] - bx1) * 0.5;
    // Centre of the bottom mark band.
    let cy_bot = by0 * 0.5;
    // Centre of the top mark band.
    let cy_top = by1 + (boxes.media[3] - by1) * 0.5;
    // Trim mid-points for the cross-axis coordinates.
    let trim_mid_x = (tx0 + tx1) * 0.5;
    let trim_mid_y = (ty0 + ty1) * 0.5;

    reg_target(content, cx_left, trim_mid_y);
    reg_target(content, cx_right, trim_mid_y);
    reg_target(content, trim_mid_x, cy_bot);
    reg_target(content, trim_mid_x, cy_top);
}

/// A single output page: the document-space rectangle it frames, in doc px.
#[derive(Debug, Clone, Copy)]
pub struct PageRegion {
    /// Top-left of the region in document coordinates.
    pub origin_x: f64,
    pub origin_y: f64,
    /// Region size in document px (physical size = size / dpi × 72 pt).
    pub width_px: f64,
    pub height_px: f64,
    /// Clip artwork to the region (+ bleed). The whole-document single page sets
    /// `false` so its output is byte-for-byte the legacy exporter.
    pub clip: bool,
}

impl PageRegion {
    /// The whole document as one unclipped page (legacy single-page export).
    pub fn whole(doc: &Document) -> Self {
        Self {
            origin_x: 0.0,
            origin_y: 0.0,
            width_px: doc.width,
            height_px: doc.height,
            clip: false,
        }
    }

    /// A clipped page framing a single artboard's rectangle.
    pub fn artboard(a: &crate::document::Artboard) -> Self {
        Self {
            origin_x: a.x,
            origin_y: a.y,
            width_px: a.width,
            height_px: a.height,
            clip: true,
        }
    }
}

/// Export `doc` as a single-page vector PDF (the whole canvas).
///
/// Scope: filled/stroked vector paths, gradient fills (axial/radial shadings),
/// placed raster images (image XObjects), node/group affine transforms, group
/// nesting, and per-layer opacity + blend mode (via ExtGState). Physical page
/// size honours `doc.dpi` (px → pt by 72/dpi). CMYK mode emits a print-ready
/// PDF/X-1a:2001 file. For per-artboard / multi-page output use
/// [`export_pdf_regions`]. Like SVG, PDF layer groups are non-isolated, so blend
/// reads the page backdrop.
pub fn export_pdf(doc: &Document, opts: &PdfExportOptions) -> Vec<u8> {
    export_pdf_regions(doc, opts, &[PageRegion::whole(doc)])
}

/// Export `doc` as a multi-page vector PDF — one page per [`PageRegion`]. This
/// backs per-artboard / multi-page export: each region becomes a page clipped to
/// its rectangle + bleed, with its own Media/Trim/Bleed boxes and marks. Layer
/// opacity/blend ExtGStates are document-level, so they are computed once and the
/// objects are shared across every page. An empty `regions` slice falls back to
/// the whole document (identical to [`export_pdf`]).
pub fn export_pdf_regions(
    doc: &Document,
    opts: &PdfExportOptions,
    regions: &[PageRegion],
) -> Vec<u8> {
    use pdf_writer::{Finish, Name, Pdf, Rect, Ref, TextStr};

    let whole = [PageRegion::whole(doc)];
    let regions: &[PageRegion] = if regions.is_empty() { &whole } else { regions };

    // Layer ExtGStates are document-level → computed once, objects shared.
    let gstates = collect_layer_gstates(doc);

    // Build each page's boxes + content stream (and its deferred resources) up front.
    let pages: Vec<(PageBoxes, Vec<u8>, PdfResources)> = regions
        .iter()
        .map(|r| {
            let boxes = compute_page_boxes_dims(r.width_px, r.height_px, doc, opts);
            let (stream, res) = build_page_content(doc, opts, *r, &boxes);
            (boxes, stream, res)
        })
        .collect();

    // ── Ref allocation (catalog=1, page-tree=2, then page/content pairs, then
    // the shared ExtGStates, then ICC + Info). For a single whole-doc page this
    // reproduces the legacy 1–6 numbering exactly.
    let catalog_id = Ref::new(1);
    let page_tree_id = Ref::new(2);
    let mut next = 3_i32;
    let mut page_refs: Vec<(Ref, Ref)> = Vec::with_capacity(pages.len());
    for _ in 0..pages.len() {
        let page_id = Ref::new(next);
        let content_id = Ref::new(next + 1);
        next += 2;
        page_refs.push((page_id, content_id));
    }
    let gs_refs: Vec<Ref> = (0..gstates.len())
        .map(|_| {
            let r = Ref::new(next);
            next += 1;
            r
        })
        .collect();
    let icc_ref = Ref::new(next);
    next += 1;
    let info_ref = Ref::new(next);
    next += 1;

    // Per-page resource object refs: images `(color, optional smask)` and
    // shadings `(shading, stitching-fn, per-segment exponential-fns)`. Allocated
    // after the fixed objects so docs with no images/gradients keep legacy
    // numbering.
    struct PageObjRefs {
        images: Vec<(Ref, Option<Ref>)>,
        shadings: Vec<(Ref, Ref, Vec<Ref>)>,
        /// One ExtGState object per deduped per-object (fill, stroke) alpha pair.
        gstates: Vec<Ref>,
    }
    let alloc = |n_slots: &mut i32| {
        let r = Ref::new(*n_slots);
        *n_slots += 1;
        r
    };
    let obj_refs: Vec<PageObjRefs> = pages
        .iter()
        .map(|(_, _, res)| {
            let images = res
                .images
                .iter()
                .map(|img| {
                    let color = alloc(&mut next);
                    let smask = img.alpha.as_ref().map(|_| alloc(&mut next));
                    (color, smask)
                })
                .collect();
            let shadings = res
                .shadings
                .iter()
                .map(|sh| {
                    let shading = alloc(&mut next);
                    let stitch = alloc(&mut next);
                    let exps: Vec<Ref> = (0..sh.stops.len().saturating_sub(1))
                        .map(|_| alloc(&mut next))
                        .collect();
                    (shading, stitch, exps)
                })
                .collect();
            let gstates = res.gstates.iter().map(|_| alloc(&mut next)).collect();
            PageObjRefs {
                images,
                shadings,
                gstates,
            }
        })
        .collect();

    let mut pdf = Pdf::new();

    // PDF/X-1a: a CMYK export is emitted as a print-ready PDF/X-1a:2001 file —
    // PDF 1.3, an embedded DeviceCMYK OutputIntent, and GTS_PDFX Info metadata.
    let x1a = opts.color_mode == crate::document::ColorMode::Cmyk;
    if x1a {
        pdf.set_version(1, 3);
    }

    {
        let mut cat = pdf.catalog(catalog_id);
        cat.pages(page_tree_id);
        if x1a {
            let mut intents = cat.output_intents();
            intents
                .push()
                .subtype(pdf_writer::types::OutputIntentSubtype::PDFX)
                .output_condition_identifier(TextStr("CoatedFOGRA39"))
                .output_condition(TextStr("Coated FOGRA39 (ISO 12647-2:2004)"))
                .registry_name(TextStr("http://www.color.org"))
                .info(TextStr("Coated FOGRA39 (ISO 12647-2:2004)"))
                .dest_output_profile(icc_ref);
            intents.finish();
        }
        cat.finish();
    }

    // Page tree.
    {
        let kids: Vec<Ref> = page_refs.iter().map(|(p, _)| *p).collect();
        pdf.pages(page_tree_id).kids(kids).count(pages.len() as i32);
    }

    // Each page object + its content stream.
    for (((boxes, stream, _), (page_id, content_id)), page_objs) in
        pages.iter().zip(page_refs.iter()).zip(obj_refs.iter())
    {
        {
            let mut page = pdf.page(*page_id);
            page.parent(page_tree_id)
                .media_box(Rect::new(
                    boxes.media[0],
                    boxes.media[1],
                    boxes.media[2],
                    boxes.media[3],
                ))
                .trim_box(Rect::new(
                    boxes.trim[0],
                    boxes.trim[1],
                    boxes.trim[2],
                    boxes.trim[3],
                ))
                .bleed_box(Rect::new(
                    boxes.bleed[0],
                    boxes.bleed[1],
                    boxes.bleed[2],
                    boxes.bleed[3],
                ))
                .contents(*content_id);
            {
                let mut res = page.resources();
                if !gs_refs.is_empty() || !page_objs.gstates.is_empty() {
                    let mut egs = res.ext_g_states();
                    for (i, r) in gs_refs.iter().enumerate() {
                        egs.pair(Name(format!("gs{i}").as_bytes()), *r);
                    }
                    for (i, r) in page_objs.gstates.iter().enumerate() {
                        egs.pair(Name(format!("ga{i}").as_bytes()), *r);
                    }
                    egs.finish();
                }
                if !page_objs.images.is_empty() {
                    let mut xobjs = res.x_objects();
                    for (i, (color, _)) in page_objs.images.iter().enumerate() {
                        xobjs.pair(Name(format!("Im{i}").as_bytes()), *color);
                    }
                    xobjs.finish();
                }
                if !page_objs.shadings.is_empty() {
                    let mut sh = res.shadings();
                    for (i, (shading, _, _)) in page_objs.shadings.iter().enumerate() {
                        sh.pair(Name(format!("Sh{i}").as_bytes()), *shading);
                    }
                    sh.finish();
                }
                res.finish();
            }
            page.finish();
        }
        pdf.stream(*content_id, stream);
    }

    // Image XObjects + gradient shading objects, per page.
    for ((_, _, res), page_objs) in pages.iter().zip(obj_refs.iter()) {
        for (img, (color_ref, smask_ref)) in res.images.iter().zip(page_objs.images.iter()) {
            {
                let mut xobj = pdf.image_xobject(*color_ref, &img.color);
                xobj.filter(img.color_filter);
                xobj.width(img.width as i32).height(img.height as i32);
                if img.is_cmyk {
                    xobj.color_space().device_cmyk();
                } else {
                    xobj.color_space().device_rgb();
                }
                xobj.bits_per_component(8);
                if let Some(sm) = smask_ref {
                    xobj.s_mask(*sm);
                }
                xobj.finish();
            }
            // SMask samples are Flate-compressed (see build_pdf_image).
            if let (Some(alpha), Some(sm)) = (img.alpha.as_ref(), smask_ref) {
                let mut mask = pdf.image_xobject(*sm, alpha);
                mask.filter(pdf_writer::Filter::FlateDecode);
                mask.width(img.width as i32).height(img.height as i32);
                mask.color_space().device_gray();
                mask.bits_per_component(8);
                mask.finish();
            }
        }
        for (sh, (shading_ref, stitch_ref, exp_refs)) in
            res.shadings.iter().zip(page_objs.shadings.iter())
        {
            // Per-segment exponential (linear) interpolation functions.
            for (seg, exp_ref) in exp_refs.iter().enumerate() {
                let c0 = sh.stops[seg].1.clone();
                let c1 = sh.stops[seg + 1].1.clone();
                pdf.exponential_function(*exp_ref)
                    .domain([0.0, 1.0])
                    .c0(c0)
                    .c1(c1)
                    .n(1.0)
                    .finish();
            }
            // Stitching function across all segments.
            {
                let interior: Vec<f32> = sh.stops[1..sh.stops.len() - 1]
                    .iter()
                    .map(|s| s.0)
                    .collect();
                let encode: Vec<f32> = exp_refs.iter().flat_map(|_| [0.0_f32, 1.0]).collect();
                pdf.stitching_function(*stitch_ref)
                    .domain([0.0, 1.0])
                    .functions(exp_refs.iter().copied())
                    .bounds(interior)
                    .encode(encode)
                    .finish();
            }
            // The axial/radial shading itself.
            {
                use pdf_writer::types::FunctionShadingType;
                let mut shading = pdf.function_shading(*shading_ref);
                shading.shading_type(if sh.radial {
                    FunctionShadingType::Radial
                } else {
                    FunctionShadingType::Axial
                });
                if sh.is_cmyk {
                    shading.color_space().device_cmyk();
                } else {
                    shading.color_space().device_rgb();
                }
                shading
                    .insert(Name(b"Coords"))
                    .array()
                    .items(sh.coords.iter().copied());
                shading
                    .insert(Name(b"Domain"))
                    .array()
                    .items([0.0_f32, 1.0]);
                shading.insert(Name(b"Extend")).array().items([true, true]);
                shading.function(*stitch_ref);
                shading.finish();
            }
        }
        // Per-object fill/stroke alpha ExtGStates (`ca`/`CA`) for this page.
        for (&(fill_alpha, stroke_alpha), r) in res.gstates.iter().zip(page_objs.gstates.iter()) {
            pdf.ext_graphics(*r)
                .non_stroking_alpha(fill_alpha)
                .stroking_alpha(stroke_alpha)
                .finish();
        }
    }

    // Shared ExtGState objects: layer alpha (fill + stroke) and blend mode.
    for ((opacity, blend), r) in gstates.iter().zip(gs_refs.iter()) {
        pdf.ext_graphics(*r)
            .non_stroking_alpha(*opacity)
            .stroking_alpha(*opacity)
            .blend_mode(*blend)
            .finish();
    }

    // CMYK OutputIntent profile stream + PDF/X-1a Info metadata.
    if x1a {
        let icc_bytes = x1a_icc_bytes(opts);
        pdf.icc_profile(icc_ref, &icc_bytes).n(4).finish();
        pdf.document_info(info_ref)
            .title(TextStr("Photonic print export"))
            .creator(TextStr("Photonic"))
            .producer(TextStr("Photonic"))
            .pair(Name(b"GTS_PDFXVersion"), TextStr("PDF/X-1a:2001"))
            .pair(Name(b"GTS_PDFXConformance"), TextStr("PDF/X-1a:2001"));
    }
    pdf.finish()
}

/// Collect the per-layer ExtGState (alpha, blend) list in draw order — one entry
/// per visible/printing layer that needs a non-trivial graphics state. Shared by
/// every page; the order matches the `gsN` counter in [`build_page_content`] so
/// the indices line up.
fn collect_layer_gstates(doc: &Document) -> Vec<(f32, pdf_writer::types::BlendMode)> {
    let mut states = Vec::new();
    for layer_id in &doc.layer_order {
        let layer = match doc.layers.get(layer_id) {
            Some(l) if l.visible && l.print => l,
            _ => continue,
        };
        if layer.opacity < 1.0 || layer.blend_mode != crate::layer::BlendMode::Normal {
            states.push((
                layer.opacity.clamp(0.0, 1.0),
                pdf_blend_mode(layer.blend_mode),
            ));
        }
    }
    states
}

/// Build one page's content stream for `region` (see [`export_pdf_regions`]).
///
/// PDF is Y-up, Photonic Y-down. The CTM maps document coordinates so the
/// region's top-left `(origin_x, origin_y)` lands at the trim box's top-left and
/// Y is flipped:
///   page_x = s·doc_x + (trim_x0 − s·origin_x)
///   page_y = −s·doc_y + (trim_y1 + s·origin_y)
/// where `s = 72/dpi`. For the whole document (origin 0,0) this collapses to the
/// legacy `[s,0,0,−s,trim_x0,trim_y1]`.
fn build_page_content(
    doc: &Document,
    opts: &PdfExportOptions,
    region: PageRegion,
    boxes: &PageBoxes,
) -> (Vec<u8>, PdfResources) {
    use pdf_writer::{Content, Name};

    let mut res = PdfResources::default();
    let mut content = Content::new();

    let s = 72.0_f32 / doc.dpi as f32;
    let trim_x0 = boxes.trim[0];
    let trim_y1 = boxes.trim[3]; // top of trim in Y-up page space
    let e = trim_x0 - s * region.origin_x as f32;
    let f = trim_y1 + s * region.origin_y as f32;

    // Bleed in document px — the CTM scale converts it to point-space bleed.
    let bleed_px =
        crate::units::to_px(doc.bleed_mm, crate::units::DocumentUnit::Mm, doc.dpi) as f32;

    // Per-artboard export must include EXACTLY the content overlapping this
    // region. On a clipped (per-artboard) page, a node whose world bbox lies
    // wholly outside the region rect (+ bleed) contributes nothing but a hidden,
    // clipped-away XObject — so skip emitting it entirely, assigning each node to
    // the artboard region it actually occupies. Nodes with an unknown bbox (e.g.
    // text) are always kept and left to the clip, preserving prior behavior.
    let region_rect = region.clip.then(|| {
        let b = bleed_px as f64;
        kurbo::Rect::new(
            region.origin_x - b,
            region.origin_y - b,
            region.origin_x + region.width_px + b,
            region.origin_y + region.height_px + b,
        )
    });
    let overlaps_region = |node: &SceneNode| -> bool {
        let Some(rect) = region_rect else { return true };
        match node_world_bbox(node, doc) {
            Some(bb) => bb.x1 > rect.x0 && bb.x0 < rect.x1 && bb.y1 > rect.y0 && bb.y0 < rect.y1,
            None => true,
        }
    };

    // Save state so marks can be drawn afterward in absolute page space.
    content.save_state();
    content.transform([s, 0.0, 0.0, -s, e, f]);

    let ox = region.origin_x as f32;
    let oy = region.origin_y as f32;
    let w = region.width_px as f32;
    let h = region.height_px as f32;

    // Clip artwork to the region + bleed (per-artboard export). The whole-doc
    // page sets clip=false and emits no clip op, preserving legacy output.
    if region.clip {
        content.rect(
            ox - bleed_px,
            oy - bleed_px,
            w + 2.0 * bleed_px,
            h + 2.0 * bleed_px,
        );
        content.clip_nonzero();
        content.end_path();
    }

    // Bleed-aware background: fill past the trim edge into the bleed zone so no
    // white sliver remains after cutting.
    if let Some(bg) = opts.background {
        set_fill_color(&mut content, convert_color([bg.r, bg.g, bg.b], opts));
        content.move_to(ox - bleed_px, oy - bleed_px);
        content.line_to(ox + w + bleed_px, oy - bleed_px);
        content.line_to(ox + w + bleed_px, oy + h + bleed_px);
        content.line_to(ox - bleed_px, oy + h + bleed_px);
        content.close_path();
        content.fill_nonzero();
    }

    // Layers, each wrapped in its shared `/gsN` ExtGState when it needs one.
    let mut gs_counter = 0usize;
    for layer_id in &doc.layer_order {
        let layer = match doc.layers.get(layer_id) {
            Some(l) if l.visible && l.print => l,
            _ => continue,
        };
        let needs_gs = layer.opacity < 1.0 || layer.blend_mode != crate::layer::BlendMode::Normal;
        if needs_gs {
            let name = format!("gs{gs_counter}");
            gs_counter += 1;
            content.save_state();
            content.set_parameters(Name(name.as_bytes()));
        }
        for node_id in &layer.node_ids {
            if let Some(node) = doc.nodes.get(node_id) {
                if !overlaps_region(node) {
                    continue;
                }
                emit_node_pdf(node, doc, &mut content, opts, &mut res, 1.0);
            }
        }
        if needs_gs {
            content.restore_state();
        }
    }

    // Pop the artwork CTM so marks are drawn in absolute page space.
    content.restore_state();
    emit_marks(&mut content, boxes, opts);
    (content.finish(), res)
}

/// Resolve the CMYK ICC profile bytes for the PDF/X OutputIntent: the caller's
/// `icc_profile` if it loads, otherwise the bundled FOGRA39 default (same profile
/// the CMYK conversion uses, so the OutputIntent matches the separated colours).
fn x1a_icc_bytes(opts: &PdfExportOptions) -> Vec<u8> {
    if let Some(p) = &opts.icc_profile {
        if let Ok(bytes) = std::fs::read(p) {
            return bytes;
        }
    }
    crate::color_cmyk::DEFAULT_CMYK_ICC.to_vec()
}

/// Map a Photonic blend mode to the `pdf-writer` blend mode (the 16 PDF standard
/// separable + non-separable modes are 1:1 with ours).
fn pdf_blend_mode(m: crate::layer::BlendMode) -> pdf_writer::types::BlendMode {
    use crate::layer::BlendMode as B;
    use pdf_writer::types::BlendMode as P;
    match m {
        B::Normal => P::Normal,
        B::Multiply => P::Multiply,
        B::Screen => P::Screen,
        B::Overlay => P::Overlay,
        B::Darken => P::Darken,
        B::Lighten => P::Lighten,
        B::ColorDodge => P::ColorDodge,
        B::ColorBurn => P::ColorBurn,
        B::HardLight => P::HardLight,
        B::SoftLight => P::SoftLight,
        B::Difference => P::Difference,
        B::Exclusion => P::Exclusion,
        B::Hue => P::Hue,
        B::Saturation => P::Saturation,
        B::Color => P::Color,
        B::Luminosity => P::Luminosity,
        // Photoshop extras have no PDF standard blend mode — fall back to Normal.
        B::LinearDodge
        | B::LinearBurn
        | B::Subtract
        | B::Divide
        | B::VividLight
        | B::LinearLight
        | B::PinLight
        | B::HardMix
        | B::DarkerColor
        | B::LighterColor => P::Normal,
    }
}

// Recursively emit a node's geometry into the PDF content stream, applying its
// affine transform within a save/restore so siblings are unaffected.
// ─── Deferred page resources (images + gradient shadings) ────────────────────

/// A placed raster prepared for PDF embedding as an image XObject. Pixel data is
/// already downsampled to a sane effective DPI and **compressed** (see
/// [`build_pdf_image`]); `width`/`height` are the post-downsample sample dims.
struct PdfImage {
    width: u32,
    height: u32,
    /// Encoded colour stream (3/px DeviceRGB or 4/px DeviceCMYK before encoding).
    color: Vec<u8>,
    /// Stream filter for `color` (`FlateDecode` for lossless, `DctDecode` for
    /// JPEG-compressed photographic RGB).
    color_filter: pdf_writer::Filter,
    is_cmyk: bool,
    /// Optional DeviceGray soft mask, Flate-compressed (RGB export only; CMYK/
    /// X-1a is pre-flattened opaque so the output carries no transparency).
    alpha: Option<Vec<u8>>,
}

/// Cap on a placed raster's effective resolution. Anything sampled finer than
/// this at its on-page size is downsampled — 300 DPI is press-quality and the
/// eye can't resolve more at normal viewing distance, so this is the "no visible
/// quality loss" point the export bug (11.8 MB → a fraction) targets.
const MAX_IMAGE_DPI: f64 = 300.0;

/// zlib/Flate-compress `data` for a PDF stream `/Filter /FlateDecode`.
fn flate_compress(data: &[u8]) -> Vec<u8> {
    use miniz_oxide::deflate::compress_to_vec_zlib;
    // Level 7: strong ratio without the price of level 10; images are already
    // downsampled so this runs on modest buffers.
    compress_to_vec_zlib(data, 7)
}

/// True when `rgb` samples look photographic (lots of distinct colours / smooth
/// gradients) rather than flat vector art / screenshots with hard edges. Photos
/// tolerate DCT/JPEG with no visible loss and compress far better with it; flat
/// art gets ringing artifacts, so it stays on lossless Flate. Samples a stride
/// for speed on large buffers.
fn looks_photographic(rgb: &[u8]) -> bool {
    let px = rgb.len() / 3;
    if px < 256 {
        return false; // tiny images: not worth DCT, keep them crisp
    }
    let mut seen = std::collections::HashSet::new();
    let step = (px / 4096).max(1);
    let mut i = 0;
    while i < px {
        let o = i * 3;
        // Quantise to 5 bits/channel so anti-aliasing noise doesn't inflate the
        // count, but true photo variety still does.
        let key = ((rgb[o] >> 3) as u32) << 10
            | ((rgb[o + 1] >> 3) as u32) << 5
            | (rgb[o + 2] >> 3) as u32;
        seen.insert(key);
        if seen.len() > 1024 {
            return true;
        }
        i += step;
    }
    false
}

/// DCT/JPEG-encode an interleaved RGB buffer at `q` quality (0–100). Returns
/// `None` if the encoder fails (caller falls back to Flate).
fn jpeg_compress_rgb(rgb: &[u8], width: u32, height: u32, q: u8) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
    enc.encode(rgb, width, height, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

/// Downsample an RGBA8 buffer to `nw`×`nh` with a triangle (bilinear) filter.
/// Returns the original buffer untouched when no shrink is needed.
fn downsample_rgba(px: &[u8], w: u32, h: u32, nw: u32, nh: u32) -> (Vec<u8>, u32, u32) {
    if nw >= w || nh >= h || nw == 0 || nh == 0 {
        return (px.to_vec(), w, h);
    }
    let Some(src) = image::RgbaImage::from_raw(w, h, px.to_vec()) else {
        return (px.to_vec(), w, h);
    };
    let dst = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Triangle);
    (dst.into_raw(), nw, nh)
}

/// A linear/radial gradient prepared as a PDF axial/radial shading.
struct PdfShading {
    radial: bool,
    /// Axial `[x0 y0 x1 y1]` or radial `[x0 y0 r0 x1 y1 r1]`, in node-local space.
    coords: Vec<f32>,
    is_cmyk: bool,
    /// `(offset, colour-components)` in the page colour model, offset ascending.
    stops: Vec<(f32, Vec<f32>)>,
}

/// Resources collected while a page's content stream is built; written as PDF
/// objects (and named in the page `/Resources`) by [`export_pdf_regions`].
#[derive(Default)]
struct PdfResources {
    images: Vec<PdfImage>,
    shadings: Vec<PdfShading>,
    /// Per-object soft-alpha ExtGStates: `(non_stroking_alpha, stroking_alpha)`
    /// for fill and stroke opacity. Named `gaN` in the page `/Resources` to keep
    /// them distinct from the document-level layer `gsN` states.
    gstates: Vec<(f32, f32)>,
}

impl PdfResources {
    fn add_image(&mut self, img: PdfImage) -> usize {
        self.images.push(img);
        self.images.len() - 1
    }
    fn add_shading(&mut self, sh: PdfShading) -> usize {
        self.shadings.push(sh);
        self.shadings.len() - 1
    }
    /// Add (or reuse) a per-object alpha ExtGState carrying fill (`ca`) and stroke
    /// (`CA`) opacity. Deduped so a whole grid of faint strokes shares one object.
    fn add_gstate(&mut self, fill_alpha: f32, stroke_alpha: f32) -> usize {
        if let Some(i) = self.gstates.iter().position(|&(ca, ca_s)| {
            (ca - fill_alpha).abs() < 1e-4 && (ca_s - stroke_alpha).abs() < 1e-4
        }) {
            return i;
        }
        self.gstates.push((fill_alpha, stroke_alpha));
        self.gstates.len() - 1
    }
}

/// Effective fill opacity for the PDF non-stroking alpha — the solid colour's own
/// alpha times the fill opacity, matching the SVG exporter's `c.a * fill_opacity`.
/// Non-solid paints carry per-stop alpha elsewhere, so only the fill opacity applies.
fn pdf_fill_alpha(fill: &Fill) -> f32 {
    if !fill.enabled {
        return 1.0;
    }
    match &fill.kind {
        FillKind::Solid(c) => c.a * fill.opacity,
        FillKind::None => 1.0,
        _ => fill.opacity,
    }
}

/// One opaque(-ish) paint of the artwork drawn beneath a placed raster, projected
/// into that raster's own pixel grid (native, pre-downsample). Fills and strokes
/// both reduce to a *fillable* outline so a single scanline rasteriser renders
/// them; `alpha` carries node/fill/stroke/colour opacity for correct compositing.
struct BackdropOp {
    /// Fillable outline in native image-pixel coordinates.
    outline: kurbo::BezPath,
    rgb: [f32; 3],
    alpha: f32,
}

/// The real artwork drawn *beneath* a placed raster, expressed in the raster's own
/// pixel grid, so the CMYK/X-1a flatten can composite the transparent image over
/// genuine underlying content (bg + grid + slashes + …) instead of a single solid
/// colour. This is how Illustrator flattens: transparent regions resolve to the
/// art behind them, not a hard-coded box.
///
/// [`Self::rasterize`] renders the scene at any target resolution, so it stays
/// aligned with a raster that gets downsampled to the DPI cap before encoding.
struct BackdropScene {
    /// Page background / white — the colour of any pixel no op covers.
    fallback: [f32; 3],
    /// Covering artwork in native image-pixel space, in draw order (painter's).
    ops: Vec<BackdropOp>,
    /// Native raster dims the ops were projected into (`rasterize` scales from here).
    img_w: u32,
    img_h: u32,
}

impl BackdropScene {
    /// A single flat colour with no underlying geometry — the fallback route for
    /// callers/tests that only need a uniform flatten backdrop.
    fn uniform(rgb: [f32; 3]) -> Self {
        Self {
            fallback: rgb,
            ops: Vec::new(),
            img_w: 0,
            img_h: 0,
        }
    }

    /// Render the backdrop into a `w`×`h` RGB grid (row-major, one `[f32; 3]`/px).
    /// Ops are projected from native image-pixel space by the downsample scale so
    /// the grid aligns pixel-for-pixel with the (possibly downsampled) raster.
    fn rasterize(&self, w: u32, h: u32) -> Vec<[f32; 3]> {
        let mut buf = vec![self.fallback; (w as usize) * (h as usize)];
        if self.ops.is_empty() || w == 0 || h == 0 {
            return buf;
        }
        let sx = w as f64 / self.img_w.max(1) as f64;
        let sy = h as f64 / self.img_h.max(1) as f64;
        let scale = kurbo::Affine::scale_non_uniform(sx, sy);
        for op in &self.ops {
            let path = scale * op.outline.clone();
            fill_bezpath_into(&path, op.rgb, op.alpha, w, h, &mut buf);
        }
        buf
    }
}

/// Scanline-fill `path` (nonzero winding) into `buf` (`w`×`h`, row-major RGB),
/// alpha-blending `rgb` at `alpha` over whatever is already there. One sample at
/// each pixel centre — no anti-aliasing, since the flatten only needs the
/// underlying colours, not edge quality.
fn fill_bezpath_into(
    path: &kurbo::BezPath,
    rgb: [f32; 3],
    alpha: f32,
    w: u32,
    h: u32,
    buf: &mut [[f32; 3]],
) {
    use kurbo::PathEl;
    if alpha <= 0.0 {
        return;
    }
    // Flatten to closed polygon rings (each subpath is implicitly closed for fill).
    let mut rings: Vec<Vec<kurbo::Point>> = Vec::new();
    let mut cur: Vec<kurbo::Point> = Vec::new();
    kurbo::flatten(path.elements().iter().copied(), 0.25, |el| match el {
        PathEl::MoveTo(p) => {
            if cur.len() > 1 {
                rings.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            cur.push(p);
        }
        PathEl::LineTo(p) => cur.push(p),
        PathEl::ClosePath => {
            if cur.len() > 1 {
                rings.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
        _ => {}
    });
    if cur.len() > 1 {
        rings.push(cur);
    }

    // Edges: (y_top, y_bot, x_at_y_top, dx/dy, winding-direction).
    struct Edge {
        y_top: f64,
        y_bot: f64,
        x_top: f64,
        dxdy: f64,
        dir: i32,
    }
    let mut edges: Vec<Edge> = Vec::new();
    for ring in &rings {
        for i in 0..ring.len() {
            let a = ring[i];
            let b = ring[(i + 1) % ring.len()];
            if a.y == b.y {
                continue; // horizontal edges contribute no crossings
            }
            let (p0, p1, dir) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
            edges.push(Edge {
                y_top: p0.y,
                y_bot: p1.y,
                x_top: p0.x,
                dxdy: (p1.x - p0.x) / (p1.y - p0.y),
                dir,
            });
        }
    }
    if edges.is_empty() {
        return;
    }

    let a = alpha.clamp(0.0, 1.0);
    let one_minus = 1.0 - a;
    for py in 0..h {
        let yc = py as f64 + 0.5;
        // Crossings of scanline yc with each edge (half-open [y_top, y_bot)).
        let mut xs: Vec<(f64, i32)> = Vec::new();
        for e in &edges {
            if yc >= e.y_top && yc < e.y_bot {
                xs.push((e.x_top + (yc - e.y_top) * e.dxdy, e.dir));
            }
        }
        if xs.len() < 2 {
            continue;
        }
        xs.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut wind = 0i32;
        for pair in 0..xs.len() - 1 {
            wind += xs[pair].1;
            if wind == 0 {
                continue; // outside the shape between these two crossings
            }
            let (xa, xb) = (xs[pair].0, xs[pair + 1].0);
            // Pixel centres px+0.5 in [xa, xb): px in [xa-0.5, xb-0.5).
            let start = ((xa - 0.5).ceil()).max(0.0) as i64;
            let end = ((xb - 0.5).ceil()).min(w as f64) as i64;
            for px in start..end {
                let idx = py as usize * w as usize + px as usize;
                let d = buf[idx];
                buf[idx] = [
                    rgb[0] * a + d[0] * one_minus,
                    rgb[1] * a + d[1] * one_minus,
                    rgb[2] * a + d[2] * one_minus,
                ];
            }
        }
    }
}

/// Build the [`BackdropScene`] beneath a placed `raster`, for the CMYK/X-1a flatten
/// (which can't keep alpha). Walks the document in draw order, projecting every
/// path drawn **before** the raster into the raster's pixel grid — so the flatten
/// composites the transparent image over the *actual* content behind it (the card,
/// the grid, the slashes), not a single solid colour. Falls back to the page
/// background (then white) for any pixel no artwork covers.
fn backdrop_scene_for(
    doc: &Document,
    opts: &PdfExportOptions,
    raster: &SceneNode,
) -> BackdropScene {
    let fallback = opts
        .background
        .map(|c| [c.r, c.g, c.b])
        .unwrap_or([1.0, 1.0, 1.0]);
    let world = raster.transform.to_kurbo();
    // Map world (doc-px) → this raster's native pixel grid. The image unit square
    // is drawn onto node-local (0,0)-(w,h) (see `emit_node_pdf`), so node-local
    // coords *are* image-pixel coords; inverse of the raster's world transform
    // takes any underlying node's world geometry into that grid.
    if world.determinant().abs() < 1e-12 {
        return BackdropScene::uniform(fallback);
    }
    let inv = world.inverse();

    let mut scene = BackdropScene {
        fallback,
        ops: Vec::new(),
        img_w: raster_native_dims(raster).map(|d| d.0).unwrap_or(0),
        img_h: raster_native_dims(raster).map(|d| d.1).unwrap_or(0),
    };
    if scene.img_w == 0 || scene.img_h == 0 {
        return scene;
    }

    'scan: for layer_id in &doc.layer_order {
        let layer = match doc.layers.get(layer_id) {
            Some(l) if l.visible && l.print => l,
            _ => continue,
        };
        for nid in &layer.node_ids {
            if *nid == raster.id {
                break 'scan; // reached the image — later nodes are above it
            }
            if let Some(node) = doc.nodes.get(nid) {
                collect_backdrop_ops(node, doc, kurbo::Affine::IDENTITY, inv, &mut scene.ops);
            }
        }
    }
    scene
}

/// Native pixel dims of a placed (non-adjustment) raster, else `None`.
fn raster_native_dims(node: &SceneNode) -> Option<(u32, u32)> {
    match &node.kind {
        SceneNodeKind::Raster(r) if !r.is_adjustment_layer() => {
            Some((r.image.width, r.image.height))
        }
        _ => None,
    }
}

/// Recursively project `node`'s fills/strokes into the backdrop grid via
/// `inv * parent_world * node.transform`, appending to `ops` in draw order (fill
/// beneath stroke, earlier nodes beneath later ones). Groups recurse; rasters and
/// text below the image are ignored (they fall through to the scene fallback).
fn collect_backdrop_ops(
    node: &SceneNode,
    doc: &Document,
    parent_world: kurbo::Affine,
    inv: kurbo::Affine,
    ops: &mut Vec<BackdropOp>,
) {
    if !node.visible || node.opacity <= 0.0 {
        return;
    }
    let world = parent_world * node.transform.to_kurbo();
    match &node.kind {
        SceneNodeKind::Path(p) => {
            let to_grid = inv * world;
            // Fill first (drawn underneath its own stroke).
            if let Some(rgb) = fill_rgb(&p.fill) {
                let ca = match &p.fill.kind {
                    FillKind::Solid(c) => c.a,
                    _ => 1.0,
                };
                let alpha = node.opacity * p.fill.opacity * ca;
                if alpha > 0.0 {
                    ops.push(BackdropOp {
                        outline: to_grid * p.path_data.to_bez_path(),
                        rgb,
                        alpha,
                    });
                }
            }
            // Stroke: expand to a fillable outline in node-local space, then project.
            if p.stroke.enabled && p.stroke.width > 0.0 {
                let alpha = node.opacity * p.stroke.opacity * p.stroke.color.a;
                if alpha > 0.0 {
                    let style = kurbo::Stroke::new(p.stroke.width);
                    let outline = kurbo::stroke(
                        p.path_data.to_bez_path(),
                        &style,
                        &kurbo::StrokeOpts::default(),
                        0.1,
                    );
                    ops.push(BackdropOp {
                        outline: to_grid * outline,
                        rgb: [p.stroke.color.r, p.stroke.color.g, p.stroke.color.b],
                        alpha,
                    });
                }
            }
        }
        SceneNodeKind::Group(g) => {
            for child_id in &g.children {
                if let Some(child) = doc.nodes.get(child_id) {
                    collect_backdrop_ops(child, doc, world, inv, ops);
                }
            }
        }
        // Raster/text backdrops aren't rasterised here — a transparent pixel over
        // them resolves to the scene fallback (page bg). Full generality stays on
        // the RGB/SMask route.
        _ => {}
    }
}

/// Composite an RGB `paint` over an opaque `backdrop` at `alpha`, yielding an
/// opaque tone. The CMYK/X-1a flatten uses this so a faint fill/stroke emits at
/// full opacity (PDF/X forbids constant alpha < 1) yet still reads as the correct
/// light shade — 6% white over a dark card becomes a faint dark-grey, not white.
fn flatten_over(paint: [f32; 3], backdrop: [f32; 3], alpha: f32) -> [f32; 3] {
    let a = alpha.clamp(0.0, 1.0);
    let one_minus = 1.0 - a;
    [
        paint[0] * a + backdrop[0] * one_minus,
        paint[1] * a + backdrop[1] * one_minus,
        paint[2] * a + backdrop[2] * one_minus,
    ]
}

/// Resolve an opaque RGB backdrop beneath `target`, for the CMYK/X-1a vector
/// opacity flatten. Walks the document in painter order and returns the topmost
/// (last-drawn) opaque solid fill whose world bbox covers `target`'s centre,
/// falling back to the page background (then white) where nothing opaque sits
/// beneath. This mirrors the raster flatten's backdrop resolution
/// ([`backdrop_scene_for`]) but collapses to a single representative colour, which
/// is all a uniform fill/stroke composite needs.
fn backdrop_rgb_for(doc: &Document, opts: &PdfExportOptions, target: &SceneNode) -> [f32; 3] {
    let fallback = opts
        .background
        .map(|c| [c.r, c.g, c.b])
        .unwrap_or([1.0, 1.0, 1.0]);
    // Pre-target opaque fills in draw order, with their world bboxes.
    let mut candidates: Vec<(kurbo::Rect, [f32; 3])> = Vec::new();
    let mut target_center: Option<(f64, f64)> = None;
    'outer: for layer_id in &doc.layer_order {
        let layer = match doc.layers.get(layer_id) {
            Some(l) if l.visible && l.print => l,
            _ => continue,
        };
        for nid in &layer.node_ids {
            if let Some(node) = doc.nodes.get(nid) {
                if scan_backdrop_fills(
                    node,
                    doc,
                    kurbo::Affine::IDENTITY,
                    target.id,
                    &mut candidates,
                    &mut target_center,
                ) {
                    break 'outer;
                }
            }
        }
    }
    let Some((cx, cy)) = target_center else {
        return fallback;
    };
    // Topmost (last-drawn) opaque fill whose bbox covers the target centre wins.
    for (bb, rgb) in candidates.iter().rev() {
        if cx >= bb.x0 && cx <= bb.x1 && cy >= bb.y0 && cy <= bb.y1 {
            return *rgb;
        }
    }
    fallback
}

/// DFS helper for [`backdrop_rgb_for`]. Accumulates the world transform, appends
/// every pre-`target` opaque solid fill (world bbox + colour) to `candidates` in
/// draw order, and records `target`'s world-bbox centre when it is reached.
/// Returns `true` once `target` is encountered so the walk can stop.
fn scan_backdrop_fills(
    node: &SceneNode,
    doc: &Document,
    parent_world: kurbo::Affine,
    target_id: NodeId,
    candidates: &mut Vec<(kurbo::Rect, [f32; 3])>,
    target_center: &mut Option<(f64, f64)>,
) -> bool {
    let world = parent_world * node.transform.to_kurbo();
    if node.id == target_id {
        if let Some(bb) = local_bbox(node, doc) {
            let wbb = world.transform_rect_bbox(bb);
            *target_center = Some(((wbb.x0 + wbb.x1) * 0.5, (wbb.y0 + wbb.y1) * 0.5));
        }
        return true;
    }
    if !node.visible || node.opacity <= 0.0 {
        return false;
    }
    match &node.kind {
        SceneNodeKind::Path(p) => {
            if p.fill.enabled {
                if let FillKind::Solid(c) = &p.fill.kind {
                    let a = c.a * p.fill.opacity * node.opacity;
                    if a >= 0.999 {
                        if let Some(bb) = p.path_data.bounding_box() {
                            candidates.push((world.transform_rect_bbox(bb), [c.r, c.g, c.b]));
                        }
                    }
                }
            }
        }
        SceneNodeKind::Group(g) => {
            for cid in &g.children {
                if let Some(child) = doc.nodes.get(cid) {
                    if scan_backdrop_fills(child, doc, world, target_id, candidates, target_center)
                    {
                        return true;
                    }
                }
            }
        }
        _ => {}
    }
    false
}

/// Node-local bounding box (pre-transform) of a `target` node, for backdrop
/// centre resolution. Groups union their children; text/adjustment nodes have no
/// geometric box here.
fn local_bbox(node: &SceneNode, doc: &Document) -> Option<kurbo::Rect> {
    match &node.kind {
        SceneNodeKind::Path(p) => p.path_data.bounding_box(),
        SceneNodeKind::Group(g) => {
            let mut combined: Option<kurbo::Rect> = None;
            for cid in &g.children {
                if let Some(child) = doc.nodes.get(cid) {
                    if let Some(cb) = node_world_bbox(child, doc) {
                        combined = Some(combined.map_or(cb, |prev| prev.union(cb)));
                    }
                }
            }
            combined
        }
        SceneNodeKind::Raster(r) if !r.is_adjustment_layer() => Some(kurbo::Rect::new(
            0.0,
            0.0,
            r.image.width as f64,
            r.image.height as f64,
        )),
        _ => None,
    }
}

/// Convert a placed RGBA raster to a [`PdfImage`] in the export colour model,
/// **downsampled** to at most [`MAX_IMAGE_DPI`] at its placed size and
/// **compressed** so a placed photo doesn't bloat the PDF (bug: two rasters →
/// 11.8 MB uncompressed).
///
/// `placed_scale` is the accumulated affine scale from the page root to this
/// node (doc-px per image-px); `doc_dpi` is the document resolution. The image's
/// effective DPI is `doc_dpi / placed_scale`, so anything finer than the cap is
/// shrunk before encoding. Transparency is handled per colour model:
/// RGB export keeps an 8-bit soft mask for true alpha (bug 1); CMYK/X-1a can't
/// carry transparency, so it pre-composites the alpha over `backdrop` — the
/// actual artwork behind the image (e.g. the dark card), NOT a hard-coded white,
/// so a transparent avatar blends into the card instead of punching a white hole.
fn build_pdf_image(
    img: &crate::raster::RasterImage,
    opts: &PdfExportOptions,
    placed_scale: f64,
    doc_dpi: f64,
    backdrop: &BackdropScene,
) -> PdfImage {
    // ── Downsample to the effective-DPI cap ──────────────────────────────────
    let effective_dpi = doc_dpi / placed_scale.max(1e-6);
    let factor = (MAX_IMAGE_DPI / effective_dpi).min(1.0);
    let (px, width, height) = if factor < 1.0 {
        let nw = ((img.width as f64 * factor).round() as u32).max(1);
        let nh = ((img.height as f64 * factor).round() as u32).max(1);
        downsample_rgba(&img.pixels, img.width, img.height, nw, nh)
    } else {
        (img.pixels.clone(), img.width, img.height)
    };

    let n = (width as usize) * (height as usize);
    let is_cmyk = opts.color_mode == crate::document::ColorMode::Cmyk;
    if is_cmyk {
        // Rasterise the real artwork beneath the image at the (downsampled) sample
        // grid, so each transparent pixel flattens over its ACTUAL backdrop (bg +
        // grid + slashes), not one solid colour — the Illustrator flatten model.
        let bd = backdrop.rasterize(width, height);
        let mut color = Vec::with_capacity(n * 4);
        for i in 0..n {
            let r = px[i * 4] as f32 / 255.0;
            let g = px[i * 4 + 1] as f32 / 255.0;
            let b = px[i * 4 + 2] as f32 / 255.0;
            let a = px[i * 4 + 3] as f32 / 255.0;
            let bg = bd[i];
            // Composite over the real backdrop to drop alpha (X-1a output is
            // opaque). Transparent pixels resolve to the artwork behind the image
            // (the card/grid), not white — so no white/black box over dark art.
            let over = |c: f32, bg: f32| c * a + (1.0 - a) * bg;
            let cmyk = match convert_color([over(r, bg[0]), over(g, bg[1]), over(b, bg[2])], opts) {
                PdfColor::Cmyk(v) => v,
                PdfColor::Rgb(v) => [0.0, 0.0, 0.0, 1.0 - (v[0] + v[1] + v[2]) / 3.0],
            };
            color.push((cmyk[0] * 255.0).round().clamp(0.0, 255.0) as u8);
            color.push((cmyk[1] * 255.0).round().clamp(0.0, 255.0) as u8);
            color.push((cmyk[2] * 255.0).round().clamp(0.0, 255.0) as u8);
            color.push((cmyk[3] * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        // Lossless Flate: DCT on DeviceCMYK carries an Adobe-inversion caveat and
        // X-1a bans lossy-with-transparency edge cases — keep the print path safe.
        PdfImage {
            width,
            height,
            color: flate_compress(&color),
            color_filter: pdf_writer::Filter::FlateDecode,
            is_cmyk: true,
            alpha: None,
        }
    } else {
        let mut color = Vec::with_capacity(n * 3);
        let mut alpha = Vec::with_capacity(n);
        let mut any_alpha = false;
        for i in 0..n {
            color.push(px[i * 4]);
            color.push(px[i * 4 + 1]);
            color.push(px[i * 4 + 2]);
            let a = px[i * 4 + 3];
            if a != 255 {
                any_alpha = true;
            }
            alpha.push(a);
        }
        // DCT/JPEG for opaque photographic RGB (big win, no visible loss); Flate
        // otherwise — images with alpha (logos/cutouts, kept crisp + JPEG can't
        // carry the mask) and flat vector art (JPEG rings on hard edges).
        let (color_enc, color_filter) = if !any_alpha && looks_photographic(&color) {
            match jpeg_compress_rgb(&color, width, height, 85) {
                Some(j) => (j, pdf_writer::Filter::DctDecode),
                None => (flate_compress(&color), pdf_writer::Filter::FlateDecode),
            }
        } else {
            (flate_compress(&color), pdf_writer::Filter::FlateDecode)
        };
        PdfImage {
            width,
            height,
            color: color_enc,
            color_filter,
            is_cmyk: false,
            alpha: any_alpha.then(|| flate_compress(&alpha)),
        }
    }
}

/// Build a [`PdfShading`] from a linear/radial gradient, or `None` for other
/// gradient kinds (which fall back to a solid approximation via [`fill_rgb`]).
/// `flatten`, when `Some((backdrop, k))`, composites each stop's colour over
/// `backdrop` at `stop.alpha * k` before colour conversion — the CMYK/PDF-X path
/// resolving per-stop (and fill/node) opacity into opaque stops so no constant
/// alpha is emitted. `None` (RGB export) leaves the stop colours untouched.
fn build_pdf_shading(
    g: &Gradient,
    opts: &PdfExportOptions,
    flatten: Option<([f32; 3], f32)>,
) -> Option<PdfShading> {
    let is_cmyk = opts.color_mode == crate::document::ColorMode::Cmyk;
    let comps = |col: &Color| -> Vec<f32> {
        let rgb = match flatten {
            Some((bd, k)) => flatten_over([col.r, col.g, col.b], bd, (col.a * k).clamp(0.0, 1.0)),
            None => [col.r, col.g, col.b],
        };
        match convert_color(rgb, opts) {
            PdfColor::Rgb(v) => v.to_vec(),
            PdfColor::Cmyk(v) => v.to_vec(),
        }
    };
    let mut stops: Vec<(f32, Vec<f32>)> = g
        .stops
        .iter()
        .map(|s| (s.offset.clamp(0.0, 1.0), comps(&s.color)))
        .collect();
    if stops.len() < 2 {
        return None;
    }
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Force strictly-increasing offsets so the stitching-function Bounds are valid.
    for i in 1..stops.len() {
        if stops[i].0 <= stops[i - 1].0 {
            stops[i].0 = (stops[i - 1].0 + 1e-4).min(1.0);
        }
    }
    match g.kind {
        GradientKind::Linear => {
            if g.coords.len() < 4 {
                return None;
            }
            let coords = vec![
                g.coords[0] as f32,
                g.coords[1] as f32,
                g.coords[2] as f32,
                g.coords[3] as f32,
            ];
            Some(PdfShading {
                radial: false,
                coords,
                is_cmyk,
                stops,
            })
        }
        GradientKind::Radial => {
            if g.coords.len() < 5 {
                return None;
            }
            let (cx, cy, r) = (g.coords[0] as f32, g.coords[1] as f32, g.coords[4] as f32);
            let coords = vec![cx, cy, 0.0, cx, cy, r];
            Some(PdfShading {
                radial: true,
                coords,
                is_cmyk,
                stops,
            })
        }
    }
}

fn emit_node_pdf(
    node: &SceneNode,
    doc: &Document,
    content: &mut pdf_writer::Content,
    opts: &PdfExportOptions,
    res: &mut PdfResources,
    // Accumulated affine scale from the page root to this node (doc-px per
    // node-local unit). Threaded so placed rasters know their on-page size and can
    // downsample to a sane effective DPI. The root call passes 1.0.
    parent_scale: f64,
) {
    if !node.visible {
        return;
    }
    let [a, b, c, d, e, f] = node.transform.matrix;
    let node_scale = parent_scale * affine_scale(&node.transform);
    content.save_state();
    content.transform([a as f32, b as f32, c as f32, d as f32, e as f32, f as f32]);

    match &node.kind {
        SceneNodeKind::Path(p) => {
            let stroke = if p.stroke.enabled && p.stroke.width > 0.0 {
                Some(&p.stroke)
            } else {
                None
            };
            // Per-object soft alpha: fill + stroke opacity (and node opacity) must
            // be honoured on export, or a faint grid stroked at 6% white prints at
            // full strength — heavy, near-solid lines. The live canvas and raster
            // export already multiply these in; the PDF path did not. Emit an
            // ExtGState carrying `ca` (fill) / `CA` (stroke) alpha around the draw.
            let node_op = node.opacity.clamp(0.0, 1.0);
            let fill_alpha = (pdf_fill_alpha(&p.fill) * node_op).clamp(0.0, 1.0);
            let stroke_alpha =
                (stroke.map(|s| s.color.a * s.opacity).unwrap_or(1.0) * node_op).clamp(0.0, 1.0);
            let is_cmyk = opts.color_mode == crate::document::ColorMode::Cmyk;
            let faint = fill_alpha < 0.999 || stroke_alpha < 0.999;
            // RGB export keeps true constant alpha via a per-object `/ca`/`/CA`
            // ExtGState. CMYK/PDF-X FORBIDS constant alpha < 1, so instead FLATTEN:
            // composite each faint paint over the opaque backdrop beneath the node
            // and emit at full opacity (no `/ca`/`/CA`). Resolve the backdrop once.
            let backdrop = if is_cmyk && faint {
                Some(backdrop_rgb_for(doc, opts, node))
            } else {
                None
            };
            let alpha_gs = if !is_cmyk && faint {
                let idx = res.add_gstate(fill_alpha, stroke_alpha);
                content.save_state();
                content.set_parameters(pdf_writer::Name(format!("ga{idx}").as_bytes()));
                true
            } else {
                false
            };
            // Gradient fill → a real PDF axial/radial shading (#4): clip to the
            // path, then paint the shading over the clip region.
            let shading = if p.fill.enabled {
                if let FillKind::Gradient(g) = &p.fill.kind {
                    // Resolve object-bounding-box gradients into the path's local
                    // user space before building the shading. The shading is painted
                    // in node-local space (post-transform, clipped to the local path
                    // geometry), so a raw `objectBoundingBox` gradient — whose coords
                    // live in `0..1` — would give an axis one unit long. Under the
                    // shading's `Extend [true true]` that collapses the whole object
                    // to a single stop (the reported "gradient exports as solid").
                    // CMYK/PDF-X: flatten per-stop alpha (and fill/node opacity)
                    // over the backdrop so the shading emits opaque stops with no
                    // constant alpha. RGB keeps the shading colours untouched.
                    let grad_flatten = backdrop.map(|bd| (bd, (p.fill.opacity * node_op)));
                    if g.units.is_object_box() {
                        match p.path_data.bounding_box() {
                            Some(bb) => build_pdf_shading(
                                &g.resolved_for_bbox(bb.x0, bb.y0, bb.width(), bb.height()),
                                opts,
                                grad_flatten,
                            ),
                            None => build_pdf_shading(g, opts, grad_flatten),
                        }
                    } else {
                        build_pdf_shading(g, opts, grad_flatten)
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(sh) = shading {
                let idx = res.add_shading(sh);
                emit_path_geometry(&p.path_data, content);
                content.save_state();
                content.clip_nonzero();
                content.end_path();
                content.shading(pdf_writer::Name(format!("Sh{idx}").as_bytes()));
                content.restore_state();
                // Stroke on top of the gradient fill, if present.
                if let Some(s) = stroke {
                    emit_path_geometry(&p.path_data, content);
                    let src = [s.color.r, s.color.g, s.color.b];
                    let rgb = match backdrop {
                        Some(bd) => flatten_over(src, bd, stroke_alpha),
                        None => src,
                    };
                    set_stroke_color(content, convert_color(rgb, opts));
                    let obj_scale = (a * d - b * c).abs().sqrt().max(1e-6);
                    content.set_line_width((s.width / obj_scale) as f32);
                    content.stroke();
                }
            } else {
                emit_path_geometry(&p.path_data, content);
                let fill = fill_rgb(&p.fill);
                if let Some(src) = fill {
                    let rgb = match backdrop {
                        Some(bd) => flatten_over(src, bd, fill_alpha),
                        None => src,
                    };
                    set_fill_color(content, convert_color(rgb, opts));
                }
                if let Some(s) = stroke {
                    let src = [s.color.r, s.color.g, s.color.b];
                    let rgb = match backdrop {
                        Some(bd) => flatten_over(src, bd, stroke_alpha),
                        None => src,
                    };
                    set_stroke_color(content, convert_color(rgb, opts));
                    // Non-scaling stroke: the `cm` transform above scales subsequent
                    // line widths, so divide by the transform scale to keep the
                    // authored width (matches the canvas and other exporters).
                    let obj_scale = (a * d - b * c).abs().sqrt().max(1e-6);
                    content.set_line_width((s.width / obj_scale) as f32);
                }
                match (fill.is_some(), stroke.is_some()) {
                    (true, true) => {
                        content.fill_nonzero_and_stroke();
                    }
                    (true, false) => {
                        content.fill_nonzero();
                    }
                    (false, true) => {
                        content.stroke();
                    }
                    // No paint — discard the path so it does not linger in the stream.
                    (false, false) => {
                        content.end_path();
                    }
                }
            }
            if alpha_gs {
                content.restore_state();
            }
        }
        SceneNodeKind::Group(g) => {
            for child_id in &g.children {
                if let Some(child) = doc.nodes.get(child_id) {
                    emit_node_pdf(child, doc, content, opts, res, node_scale);
                }
            }
        }
        // Route text nodes through the seam (no-op today; SEAM T0.2 fills this).
        SceneNodeKind::Text(_) => {
            emit_text_pdf(node, doc, content, opts);
        }
        // Placed rasters embed as image XObjects (#3). Adjustment layers carry no
        // pixels of their own (they recolour the composite) and are skipped.
        SceneNodeKind::Raster(r) => {
            if !r.is_adjustment_layer() && r.image.width > 0 && r.image.height > 0 {
                // CMYK/X-1a flatten composites over the real backdrop (RGB keeps
                // its SMask, so the backdrop is unused there).
                let backdrop = backdrop_scene_for(doc, opts, node);
                let idx = res.add_image(build_pdf_image(
                    &r.image, opts, node_scale, doc.dpi, &backdrop,
                ));
                let w = r.image.width as f32;
                let h = r.image.height as f32;
                content.save_state();
                // Map the image unit square onto the node-local (0,0)-(w,h) rect
                // (Y-down): sample row 0 (top) → doc y=0.
                content.transform([w, 0.0, 0.0, -h, 0.0, h]);
                content.x_object(pdf_writer::Name(format!("Im{idx}").as_bytes()));
                content.restore_state();
            }
        }
    }

    content.restore_state();
}

/// Emit a `PathData`'s segments as PDF path-construction operators. Quadratic
/// segments are elevated to cubics (PDF has no quadratic operator).
fn emit_path_geometry(path: &crate::path::PathData, content: &mut pdf_writer::Content) {
    use kurbo::PathEl;
    let bez = path.to_bez_path();
    let mut cur = (0.0_f64, 0.0_f64);
    for el in bez.elements() {
        match el {
            PathEl::MoveTo(p) => {
                content.move_to(p.x as f32, p.y as f32);
                cur = (p.x, p.y);
            }
            PathEl::LineTo(p) => {
                content.line_to(p.x as f32, p.y as f32);
                cur = (p.x, p.y);
            }
            PathEl::QuadTo(c1, p) => {
                // Quadratic → cubic control-point elevation.
                let c1x = cur.0 + 2.0 / 3.0 * (c1.x - cur.0);
                let c1y = cur.1 + 2.0 / 3.0 * (c1.y - cur.1);
                let c2x = p.x + 2.0 / 3.0 * (c1.x - p.x);
                let c2y = p.y + 2.0 / 3.0 * (c1.y - p.y);
                content.cubic_to(
                    c1x as f32, c1y as f32, c2x as f32, c2y as f32, p.x as f32, p.y as f32,
                );
                cur = (p.x, p.y);
            }
            PathEl::CurveTo(c1, c2, p) => {
                content.cubic_to(
                    c1.x as f32,
                    c1.y as f32,
                    c2.x as f32,
                    c2.y as f32,
                    p.x as f32,
                    p.y as f32,
                );
                cur = (p.x, p.y);
            }
            PathEl::ClosePath => {
                content.close_path();
            }
        }
    }
}

/// Resolve a fill to a representative solid RGB, or `None` when the fill is
/// disabled / `None`. Gradient fills are approximated by their first stop.
fn fill_rgb(fill: &Fill) -> Option<[f32; 3]> {
    if !fill.enabled {
        return None;
    }
    let c = match &fill.kind {
        FillKind::None => return None,
        FillKind::Solid(c) => *c,
        FillKind::Gradient(g) => g.stops.first().map(|s| s.color)?,
        FillKind::FluidGradient(g) => g.points.first().map(|p| p.color)?,
        FillKind::MeshGradient(g) => g.cell_colors.first().copied()?,
        // Pattern fills can't tile in this representative-RGB path; approximate by
        // the tile's own centre pixel — always inside the tile (never the
        // inter-tile gutter, unlike sampling at the document origin) — composited
        // over white so a semi-transparent sample doesn't read as black on paper.
        FillKind::Pattern(p) => {
            let s = p
                .tile
                .sample_bilinear(p.tile.width as f32 * 0.5, p.tile.height as f32 * 0.5);
            let a = s[3] as f32 / 255.0;
            let over = |c: u8| (c as f32 / 255.0) * a + (1.0 - a);
            return Some([over(s[0]), over(s[1]), over(s[2])]);
        }
    };
    Some([c.r, c.g, c.b])
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-artboard export must include EXACTLY the content overlapping each
    /// artboard's rectangle. A raster placed over artboard 2 must appear (as an
    /// image XObject) in artboard 2's page, and must NOT be embedded at all in
    /// artboard 1's page — not even as a hidden, clipped-away XObject.
    #[test]
    fn per_artboard_export_includes_only_overlapping_content() {
        use crate::document::Artboard;
        use crate::node::RasterNode;
        use crate::raster::RasterImage;
        use crate::transform::Transform;

        // A1 at the origin, A2 to the right — non-overlapping rectangles.
        let mut doc = Document::new("t", 200.0, 200.0);
        doc.artboards = vec![
            Artboard::new("A1", 0.0, 0.0, 200.0, 200.0),
            Artboard::new("A2", 300.0, 0.0, 200.0, 200.0),
        ];
        let layer = doc.active_layer_id.unwrap();

        // A 32×32 raster whose top-left sits at (320, 40) — squarely inside A2,
        // wholly outside A1.
        let img = RasterImage::filled(32, 32, [255, 0, 0, 255]);
        let mut node = SceneNode::new("img", layer, SceneNodeKind::Raster(RasterNode::new(img)));
        node.transform = Transform::translate(320.0, 40.0);
        doc.add_node(node, None);

        let opts = PdfExportOptions::default();
        let a1 = export_pdf_regions(&doc, &opts, &[PageRegion::artboard(&doc.artboards[0])]);
        let a2 = export_pdf_regions(&doc, &opts, &[PageRegion::artboard(&doc.artboards[1])]);

        // An image XObject is a stream carrying `/Subtype /Image`.
        let has_image = |pdf: &[u8]| {
            pdf.windows(b"/Subtype /Image".len())
                .any(|w| w == b"/Subtype /Image")
        };
        assert!(has_image(&a2), "A2 (overlapping) must embed the image");
        assert!(
            !has_image(&a1),
            "A1 (non-overlapping) must NOT embed the image, even clipped"
        );
    }

    /// ROUND 2 / Issue C — the per-artboard CMYK export path flattens a
    /// transparent raster over the ACTUAL backdrop (the dark card beneath it), not
    /// white. Drives `export_pdf_regions` with a clipped artboard region, then
    /// decodes the embedded CMYK image's transparent-corner sample and asserts it
    /// resolved to the dark card (high K), not white (all-zero CMYK). Also asserts
    /// the stream is Flate-compressed with no SMask (X-1a-safe).
    #[test]
    fn pdf_artboard_cmyk_raster_flattens_over_backdrop_not_white() {
        use crate::document::{Artboard, ColorMode};
        use crate::node::{PathNode, RasterNode};
        use crate::path::PathData;
        use crate::raster::RasterImage;
        use crate::style::{Fill, FillKind};
        use crate::transform::Transform;

        let mut doc = Document::new("card", 1050.0, 600.0);
        doc.dpi = 300.0;
        doc.color_mode = ColorMode::Cmyk;
        doc.artboards = vec![Artboard::new("Card Back", 0.0, 0.0, 1050.0, 600.0)];
        let layer = doc.active_layer_id.unwrap();

        // Dark card #0b0b12 covering the artboard.
        let mut card = PathNode::new(PathData::rect(0.0, 0.0, 1050.0, 600.0));
        card.fill = Fill {
            kind: FillKind::Solid(Color::new(0.043, 0.043, 0.070, 1.0)),
            opacity: 1.0,
            enabled: true,
        };
        doc.add_node(
            SceneNode::new("card", layer, SceneNodeKind::Path(card)),
            None,
        );

        // Avatar over the card: transparent corner (a==0), opaque interior.
        let native = 200u32;
        let mut px = vec![0u8; (native * native * 4) as usize];
        for y in 0..native {
            for x in 0..native {
                let i = ((y * native + x) * 4) as usize;
                let opaque = x > 60 && x < 140 && y > 60 && y < 140;
                px[i..i + 4].copy_from_slice(&[240, 230, 220, if opaque { 255 } else { 0 }]);
            }
        }
        let img = RasterImage::from_rgba(native, native, px).unwrap();
        let mut avatar =
            SceneNode::new("avatar", layer, SceneNodeKind::Raster(RasterNode::new(img)));
        avatar.transform = Transform::new(1.5, 0.0, 0.0, 1.5, 400.0, 200.0);
        let avatar_id = avatar.id;
        doc.add_node(avatar, None);

        let opts = PdfExportOptions {
            color_mode: ColorMode::Cmyk,
            marks: true,
            ..Default::default()
        };
        let region = PageRegion::artboard(&doc.artboards[0]);
        let bytes = export_pdf_regions(&doc, &opts, &[region]);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(
            text.contains("/FlateDecode"),
            "CMYK raster must be Flate-compressed"
        );
        assert!(
            !text.contains("/SMask"),
            "CMYK/X-1a must carry no transparency"
        );

        // The backdrop resolution + composite: the transparent corner must resolve
        // to the dark card, not white.
        let scene = backdrop_scene_for(&doc, &opts, doc.nodes.get(&avatar_id).unwrap());
        let bg = scene.rasterize(native, native)[0]; // corner pixel
        assert!(
            bg[0] < 0.2 && bg[1] < 0.2 && bg[2] < 0.2,
            "backdrop should resolve to the dark card, got {bg:?}"
        );
        let mut corner = vec![0u8; (native * native * 4) as usize]; // all transparent
        for i in 0..(native * native) as usize {
            corner[i * 4..i * 4 + 4].copy_from_slice(&[240, 230, 220, 0]);
        }
        let cimg = RasterImage::from_rgba(native, native, corner).unwrap();
        let pi = build_pdf_image(&cimg, &opts, 1.5, 300.0, &scene);
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&pi.color).unwrap();
        // Not white: white flattens to CMYK 0,0,0,0. The dark card has substantial K.
        assert!(
            raw[3] > 120,
            "transparent area must flatten to the dark card (high K), got CMYK {:?} (white box regression)",
            &raw[..4]
        );
    }

    /// ACCEPT (issue: transparent PNGs not transparent on export). A transparent
    /// raster over a dark card + a thin grid must NOT flatten to a single solid
    /// box on the CMYK/X-1a path — its transparent regions must resolve to the
    /// ACTUAL art behind it (bg *and* grid), and the RGB path must keep true
    /// per-pixel alpha via an `/SMask`. Illustrator's flatten model.
    #[test]
    fn transparent_raster_flattens_over_real_backdrop_grid_not_solid_box() {
        use crate::document::{Artboard, ColorMode};
        use crate::node::{PathNode, RasterNode};
        use crate::path::PathData;
        use crate::raster::RasterImage;
        use crate::style::{Fill, FillKind, Stroke};
        use crate::transform::Transform;

        let build = || {
            let mut doc = Document::new("card", 1050.0, 600.0);
            doc.dpi = 300.0;
            doc.artboards = vec![Artboard::new("Card Back", 0.0, 0.0, 1050.0, 600.0)];
            let layer = doc.active_layer_id.unwrap();

            // Dark background rect covering the artboard (#0b0b12).
            let mut bg = PathNode::new(PathData::rect(0.0, 0.0, 1050.0, 600.0));
            bg.fill = Fill {
                kind: FillKind::Solid(Color::new(0.043, 0.043, 0.070, 1.0)),
                opacity: 1.0,
                enabled: true,
            };
            doc.add_node(SceneNode::new("bg", layer, SceneNodeKind::Path(bg)), None);

            // A thin light-grey grid (strokes, no fill) crossing the image region
            // — vertical + horizontal lines through world x∈[400,700], y∈[200,500].
            let grid_svg = "M450 0 L450 600 M550 0 L550 600 M650 0 L650 600 \
                            M0 250 L1050 250 M0 350 L1050 350 M0 450 L1050 450";
            let mut grid = PathNode::new(PathData::from_svg(grid_svg).unwrap());
            grid.fill = Fill {
                kind: FillKind::None,
                opacity: 1.0,
                enabled: false,
            };
            grid.stroke = Stroke::solid(Color::new(0.60, 0.60, 0.62, 1.0), 6.0);
            doc.add_node(
                SceneNode::new("grid", layer, SceneNodeKind::Path(grid)),
                None,
            );

            // Fully-transparent PNG placed over the card + grid: every pixel a==0,
            // so the flattened output is *entirely* the backdrop. If the flatten
            // used one solid colour the whole image would be uniform; over the real
            // backdrop it must show the grid against the dark card.
            let native = 200u32;
            let mut px = vec![0u8; (native * native * 4) as usize];
            for i in 0..(native * native) as usize {
                px[i * 4..i * 4 + 4].copy_from_slice(&[240, 230, 220, 0]);
            }
            let img = RasterImage::from_rgba(native, native, px).unwrap();
            let mut avatar =
                SceneNode::new("avatar", layer, SceneNodeKind::Raster(RasterNode::new(img)));
            avatar.transform = Transform::new(1.5, 0.0, 0.0, 1.5, 400.0, 200.0);
            let id = avatar.id;
            doc.add_node(avatar, None);
            (doc, id, native)
        };

        // ── CMYK / X-1a flatten: transparent regions show the real backdrop ──────
        let (doc, avatar_id, native) = build();
        let cmyk_opts = PdfExportOptions {
            color_mode: ColorMode::Cmyk,
            ..Default::default()
        };
        let cmyk = String::from_utf8_lossy(&export_pdf_regions(
            &doc,
            &cmyk_opts,
            &[PageRegion::artboard(&doc.artboards[0])],
        ))
        .into_owned();
        assert!(
            cmyk.contains("/DeviceCMYK"),
            "CMYK raster should be DeviceCMYK"
        );
        assert!(
            !cmyk.contains("/SMask"),
            "CMYK/X-1a export must not carry transparency"
        );

        // The rasterised backdrop must be *non-uniform* — dark card AND lighter grid.
        let scene = backdrop_scene_for(&doc, &cmyk_opts, doc.nodes.get(&avatar_id).unwrap());
        let bd = scene.rasterize(native, native);
        let lum = |c: [f32; 3]| c[0] + c[1] + c[2];
        let min = bd.iter().copied().map(lum).fold(f32::MAX, f32::min);
        let max = bd.iter().copied().map(lum).fold(f32::MIN, f32::max);
        assert!(
            max - min > 0.6,
            "backdrop must be real art (dark card + grid), not a solid box: \
             luminance span {min}..{max}"
        );

        // The flattened CMYK samples must vary too: high K over the dark card, lower
        // K over the grid lines — i.e. the grid genuinely shows through.
        let pi = build_pdf_image(
            &doc.nodes
                .get(&avatar_id)
                .and_then(|n| match &n.kind {
                    SceneNodeKind::Raster(r) => Some(r.image.clone()),
                    _ => None,
                })
                .unwrap(),
            &cmyk_opts,
            1.5,
            300.0,
            &scene,
        );
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&pi.color).unwrap();
        let ks: Vec<u8> = raw.chunks_exact(4).map(|p| p[3]).collect();
        let kmin = *ks.iter().min().unwrap();
        let kmax = *ks.iter().max().unwrap();
        assert!(
            kmax > 200,
            "dark card must flatten to high K, got max {kmax}"
        );
        assert!(
            kmin < 160,
            "grid lines must show through as lower K, got min {kmin}"
        );

        // ── RGB path: true per-pixel alpha preserved via an /SMask ───────────────
        let (rgb_doc, _, _) = build();
        let rgb = String::from_utf8_lossy(&export_pdf_regions(
            &rgb_doc,
            &PdfExportOptions::default(),
            &[PageRegion::artboard(&rgb_doc.artboards[0])],
        ))
        .into_owned();
        assert!(
            rgb.contains("/SMask"),
            "RGB export must preserve alpha via an /SMask"
        );
        assert!(
            rgb.contains("/DeviceGray"),
            "the soft mask is a DeviceGray image"
        );
    }

    /// SVG export must be transparent by default — no opaque background rect
    /// baked behind the artwork (regression for white-background exports).
    #[test]
    fn svg_export_is_transparent_by_default() {
        let doc = Document::new("t", 100.0, 100.0);
        let svg = export_svg(&doc, &SvgExportOptions::default());
        assert!(
            !svg.contains("<rect"),
            "default SVG export should emit no background rect:\n{svg}"
        );
    }

    /// When a background color is requested, a full-artboard rect is emitted.
    #[test]
    fn svg_export_emits_background_rect_when_requested() {
        let doc = Document::new("t", 100.0, 100.0);
        let opts = SvgExportOptions {
            background: Some(Color::WHITE),
            ..Default::default()
        };
        let svg = export_svg(&doc, &opts);
        assert!(svg.contains("<rect"), "expected a background rect:\n{svg}");
        assert!(
            svg.to_lowercase().contains("#ffffff"),
            "expected white background fill:\n{svg}"
        );
    }

    /// Non-scaling stroke: a node scaled 2× must export a stroke-width halved,
    /// so that the element's `matrix(...)` scales it back to the authored width
    /// in canvas units (constant width regardless of object size).
    #[test]
    fn svg_export_stroke_width_is_non_scaling() {
        use crate::node::{PathNode, SceneNode, SceneNodeKind};
        use crate::path::PathData;
        use crate::transform::Transform;

        let mut doc = Document::new("t", 200.0, 200.0);
        let layer_id = doc.layer_order[0];
        let pn = PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))
            .with_stroke(Stroke::solid(Color::BLACK, 4.0));
        let mut node = SceneNode::new("r", layer_id, SceneNodeKind::Path(pn));
        // Uniform 2× scale about the origin.
        node.transform = Transform::new(2.0, 0.0, 0.0, 2.0, 0.0, 0.0);
        let nid = node.id;
        doc.nodes.insert(nid, node);
        doc.layers.get_mut(&layer_id).unwrap().node_ids.push(nid);

        let svg = export_svg(&doc, &SvgExportOptions::default());
        assert!(
            svg.contains("stroke-width=\"2\""),
            "2× scaled node should export stroke-width 2 (4 / 2):\n{svg}"
        );
    }

    #[test]
    fn affine_scale_is_rotation_invariant() {
        use crate::transform::Transform;
        // Pure rotation → scale 1 (stroke unaffected).
        let rot = Transform::new(0.6, 0.8, -0.8, 0.6, 0.0, 0.0);
        assert!((affine_scale(&rot) - 1.0).abs() < 1e-9);
        // Uniform 3× → scale 3.
        let s = Transform::new(3.0, 0.0, 0.0, 3.0, 5.0, 7.0);
        assert!((affine_scale(&s) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn blend_mode_css_names_round_trip() {
        use crate::layer::BlendMode::*;
        for mode in [
            Normal, Multiply, Screen, Overlay, Darken, Lighten, ColorDodge, ColorBurn, HardLight,
            SoftLight, Difference, Exclusion, Hue, Saturation, Color, Luminosity,
        ] {
            assert_eq!(BlendMode::from_css(mode.to_css()), Some(mode));
        }
        assert_eq!(BlendMode::from_css("not-a-mode"), None);
    }

    #[test]
    fn blend_mode_survives_svg_round_trip() {
        use crate::node::PathNode;
        use crate::path::PathData;

        let mut doc = Document::new("t", 100.0, 100.0);
        let mut node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))),
        );
        node.blend_mode = BlendMode::Multiply;
        doc.add_node(node, None);

        let svg = export_svg(&doc, &SvgExportOptions::default());
        assert!(
            svg.contains("mix-blend-mode:multiply"),
            "export should emit the CSS blend mode:\n{svg}"
        );

        let reimported = crate::import::import_svg(&svg).expect("re-import");
        let modes: Vec<_> = reimported.nodes.values().map(|n| n.blend_mode).collect();
        assert!(
            modes.contains(&BlendMode::Multiply),
            "blend mode lost on re-import; modes = {modes:?}"
        );
    }

    /// P0: a layer's opacity and blend mode are emitted on its wrapper `<g>`
    /// (opacity attribute + `mix-blend-mode` style), so exported SVG composites
    /// the layer as a unit — matching the renderer's per-layer compositing.
    #[test]
    fn layer_opacity_and_blend_emit_on_group() {
        use crate::node::PathNode;
        use crate::path::PathData;

        let mut doc = Document::new("t", 100.0, 100.0);
        doc.add_node(
            SceneNode::new(
                "r",
                doc.active_layer_id.unwrap(),
                SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))),
            ),
            None,
        );
        let lid = doc.active_layer_id.unwrap();
        {
            let layer = doc.layers.get_mut(&lid).unwrap();
            layer.opacity = 0.5;
            layer.blend_mode = BlendMode::Screen;
        }
        let svg = export_svg(&doc, &SvgExportOptions::default());
        assert!(
            svg.contains("opacity=\"0.5"),
            "layer opacity on <g>:\n{svg}"
        );
        assert!(
            svg.contains("mix-blend-mode:screen"),
            "layer blend mode on <g>:\n{svg}"
        );
    }

    #[test]
    fn pdf_export_is_a_valid_single_page_pdf() {
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::Fill;

        let mut doc = Document::new("t", 200.0, 150.0);
        let mut rect = PathNode::new(PathData::rect(10.0, 10.0, 80.0, 60.0));
        rect.fill = Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0));
        let node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(rect),
        );
        doc.add_node(node, None);

        let bytes = export_pdf(&doc, &PdfExportOptions::default());
        let text = String::from_utf8_lossy(&bytes);
        assert!(bytes.starts_with(b"%PDF-1"), "missing PDF header");
        assert!(text.contains("%%EOF"), "missing EOF marker");
        assert!(text.contains("/Type /Page"), "missing page object");
        assert!(text.contains("MediaBox"), "missing MediaBox");
        // Red fill colour + a fill-path operator in the (uncompressed) stream.
        assert!(
            text.contains("1 0 0 rg"),
            "missing red fill operator:\n{text}"
        );
        assert!(
            text.contains(" m\n") || text.contains(" m "),
            "missing path move op"
        );
    }

    /// P0: a layer with a blend mode + opacity emits a `/gs0` ExtGState carrying
    /// the PDF blend mode and alpha, and the layer's content is wrapped with it.
    #[test]
    fn pdf_export_layer_blend_and_opacity_emit_extgstate() {
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::Fill;

        let mut doc = Document::new("t", 100.0, 100.0);
        let mut rect = PathNode::new(PathData::rect(0.0, 0.0, 50.0, 50.0));
        rect.fill = Fill::solid(Color::new(0.0, 0.0, 1.0, 1.0));
        doc.add_node(
            SceneNode::new("r", doc.active_layer_id.unwrap(), SceneNodeKind::Path(rect)),
            None,
        );
        let lid = doc.active_layer_id.unwrap();
        {
            let l = doc.layers.get_mut(&lid).unwrap();
            l.opacity = 0.5;
            l.blend_mode = crate::layer::BlendMode::Multiply;
        }
        let bytes = export_pdf(&doc, &PdfExportOptions::default());
        let text = String::from_utf8_lossy(&bytes);
        assert!(bytes.starts_with(b"%PDF-1"), "missing PDF header");
        assert!(
            text.contains("/BM /Multiply"),
            "missing blend mode:\n{text}"
        );
        assert!(
            text.contains("/gs0 gs"),
            "content not wrapped with the ExtGState"
        );
        assert!(
            text.contains("/ExtGState") && text.contains("/ca 0.5"),
            "missing ExtGState alpha:\n{text}"
        );
    }

    /// Regression: a thin, faint grid stroke (width 1, white, opacity 0.06) must
    /// export at its authored width AND opacity. Before the fix the PDF path set
    /// the stroke colour + width but never emitted the stroke alpha, so a 6%-white
    /// grid printed as full-strength, near-solid lines. Assert (a) the emitted
    /// stroke width equals the document width (`1 w`, non-scaling at identity
    /// transform) and (b) a per-object ExtGState carries the 0.06 stroke alpha,
    /// referenced by the path before it strokes.
    #[test]
    fn pdf_export_thin_faint_stroke_keeps_width_and_opacity() {
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::{Fill, Stroke};

        let mut doc = Document::new("t", 200.0, 150.0);
        let mut rect = PathNode::new(PathData::rect(10.0, 10.0, 180.0, 130.0));
        rect.fill = Fill::none();
        let mut stroke = Stroke::solid(Color::new(1.0, 1.0, 1.0, 1.0), 1.0);
        stroke.opacity = 0.06;
        rect.stroke = stroke;
        doc.add_node(
            SceneNode::new(
                "grid",
                doc.active_layer_id.unwrap(),
                SceneNodeKind::Path(rect),
            ),
            None,
        );

        let bytes = export_pdf(&doc, &PdfExportOptions::default());
        let text = String::from_utf8_lossy(&bytes);

        // (a) Stroke width matches the document width (1.0), not thickened.
        assert!(
            text.contains("1 w\n") || text.contains("1 w "),
            "stroke width must export as the authored 1 (non-scaling):\n{text}"
        );
        // White stroke colour set, then the path is stroked.
        assert!(
            text.contains("1 1 1 RG"),
            "missing white stroke colour:\n{text}"
        );

        // (b) A per-object ExtGState (`gaN`) is referenced before the stroke, and
        // carries the faint 0.06 stroke alpha (`/CA`) — NOT 1.0.
        assert!(
            text.contains("/ga0 gs"),
            "path not wrapped with a per-object alpha ExtGState:\n{text}"
        );
        assert!(
            text.contains("/CA 0.06"),
            "stroke alpha must export at 0.06, not full opacity:\n{text}"
        );
        // Guard against the alpha being silently full-strength.
        assert!(
            !text.contains("/CA 1\n") && !text.contains("/CA 1 "),
            "stroke alpha must not be 1.0:\n{text}"
        );
    }

    /// Regression: the CMYK / PDF-X-1a export path must NOT emit constant alpha
    /// (`/ca`/`/CA` < 1) for a faint fill/stroke — PDF/X-1a forbids live
    /// transparency. Instead the faint paint is FLATTENED: composited over the
    /// opaque backdrop beneath it into an opaque tone. A dark card + a path stroked
    /// 6% white must export with (a) no constant-alpha ExtGState at all, and (b) a
    /// CMYK stroke colour that is the faint dark-grey composite, NOT pure white
    /// (`0 0 0 0 K`). RGB export (covered above) still emits true `/CA`.
    #[test]
    fn pdf_export_cmyk_flattens_faint_stroke_no_constant_alpha() {
        use crate::document::ColorMode;
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::{Fill, FillKind, Stroke};

        let mut doc = Document::new("t", 200.0, 150.0);
        doc.color_mode = ColorMode::Cmyk;
        let layer = doc.active_layer_id.unwrap();

        // Opaque dark backdrop covering the page.
        let mut bg = PathNode::new(PathData::rect(0.0, 0.0, 200.0, 150.0));
        bg.fill = Fill {
            kind: FillKind::Solid(Color::new(0.05, 0.05, 0.05, 1.0)),
            opacity: 1.0,
            enabled: true,
        };
        doc.add_node(SceneNode::new("bg", layer, SceneNodeKind::Path(bg)), None);

        // A path stroked white at 6% opacity, over the dark backdrop.
        let mut grid = PathNode::new(PathData::rect(10.0, 10.0, 180.0, 130.0));
        grid.fill = Fill::none();
        let mut stroke = Stroke::solid(Color::new(1.0, 1.0, 1.0, 1.0), 1.0);
        stroke.opacity = 0.06;
        grid.stroke = stroke;
        doc.add_node(
            SceneNode::new("grid", layer, SceneNodeKind::Path(grid)),
            None,
        );

        let opts = PdfExportOptions {
            color_mode: ColorMode::Cmyk,
            ..Default::default()
        };
        let bytes = export_pdf(&doc, &opts);
        let text = String::from_utf8_lossy(&bytes);

        // (a) No constant alpha anywhere — no `/ca` or `/CA` keys emitted.
        assert!(
            !text.contains("/ca") && !text.contains("/CA"),
            "CMYK/X-1a export must not emit any constant alpha (/ca or /CA):\n{text}"
        );

        // (b) The stroke composited to a faint dark-grey — a CMYK `K` stroke op
        // that is NOT pure white (`0 0 0 0 K`) and carries real ink (K > 0).
        let stroke_k = text
            .lines()
            .find_map(|l| l.trim().strip_suffix(" K"))
            .map(|s| s.to_string());
        let stroke_k = stroke_k.expect("expected a CMYK stroke-colour op (`… K`)");
        let comps: Vec<f32> = stroke_k
            .split_whitespace()
            .filter_map(|t| t.parse::<f32>().ok())
            .collect();
        assert_eq!(
            comps.len(),
            4,
            "stroke `K` op must have 4 CMYK components: {stroke_k:?}"
        );
        assert!(
            comps != vec![0.0, 0.0, 0.0, 0.0],
            "faint white stroke must composite to a dark tone, not pure white: {stroke_k:?}"
        );
        assert!(
            comps[3] > 0.1,
            "composited faint stroke should carry substantial K ink: {stroke_k:?}"
        );
    }

    #[test]
    fn pdf_export_empty_document_is_valid() {
        let doc = Document::new("t", 100.0, 100.0);
        let bytes = export_pdf(&doc, &PdfExportOptions::default());
        assert!(bytes.starts_with(b"%PDF-1"));
        assert!(String::from_utf8_lossy(&bytes).contains("%%EOF"));
    }

    #[test]
    fn pdf_export_red_rect_produces_mediabox_and_nonempty() {
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::Fill;

        let mut doc = Document::new("t", 100.0, 100.0);
        let mut rect = PathNode::new(PathData::rect(0.0, 0.0, 100.0, 100.0));
        rect.fill = Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0));
        let node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(rect),
        );
        doc.add_node(node, None);
        let bytes = export_pdf(&doc, &PdfExportOptions::default());
        assert!(!bytes.is_empty(), "PDF bytes must not be empty");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("MediaBox"), "PDF must contain MediaBox");
    }

    #[test]
    fn pdf_export_business_card_mediabox_72dpi() {
        use crate::document::PrintPreset;

        // US business card at 72 dpi → 252×144 px → 252×144 pt MediaBox
        let doc = Document::from_preset(PrintPreset::BUSINESS_CARD_US, 72.0);
        let bytes = export_pdf(&doc, &PdfExportOptions::default());
        let text = String::from_utf8_lossy(&bytes);
        // MediaBox must be present and the document must be non-empty.
        assert!(!bytes.is_empty());
        assert!(text.contains("MediaBox"), "PDF must contain MediaBox");
        // Width = 252.0, height = 144.0 — these appear in the MediaBox entry.
        assert!(
            text.contains("252") || text.contains("252."),
            "MediaBox should reference width 252:\n{}",
            &text[..text.len().min(500)]
        );
    }

    /// Superseded by [`pdf_gradient_exports_axial_shading_not_solid`]: a gradient
    /// fill now exports as a real PDF axial shading, no longer flattened to its
    /// first stop. This guards against regressing back to the solid approximation.
    #[test]
    fn pdf_gradient_no_longer_flattens_to_first_stop() {
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::{Fill, FillKind, Gradient, GradientStop};

        let mut doc = Document::new("t", 100.0, 100.0);
        let grad = Gradient::linear(
            0.0,
            0.0,
            100.0,
            0.0,
            vec![
                GradientStop::new(0.0, Color::new(0.0, 1.0, 0.0, 1.0)),
                GradientStop::new(1.0, Color::new(0.0, 0.0, 1.0, 1.0)),
            ],
        );
        let mut p = PathNode::new(PathData::rect(0.0, 0.0, 50.0, 50.0));
        p.fill = Fill {
            kind: FillKind::Gradient(grad),
            opacity: 1.0,
            enabled: true,
        };
        let node = SceneNode::new("g", doc.active_layer_id.unwrap(), SceneNodeKind::Path(p));
        doc.add_node(node, None);

        let text =
            String::from_utf8_lossy(&export_pdf(&doc, &PdfExportOptions::default())).into_owned();
        assert!(
            text.contains("/ShadingType 2"),
            "gradient must export as a shading"
        );
        // The old first-stop-solid behaviour must be gone.
        assert!(
            !text.contains("0 1 0 rg"),
            "gradient must not flatten to a first-stop solid fill:\n{text}"
        );
    }

    #[test]
    fn live_effects_export_svg_filters() {
        use crate::node::PathNode;
        use crate::path::PathData;

        let mut doc = Document::new("t", 100.0, 100.0);
        let mut node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))),
        );
        node.drop_shadow.enabled = true;
        node.drop_shadow.dx = 5.0;
        node.drop_shadow.dy = 6.0;
        node.drop_shadow.blur = 3.0;
        node.object_blur.enabled = true;
        node.object_blur.radius = 2.5;
        doc.add_node(node, None);

        let svg = export_svg(&doc, &SvgExportOptions::default());
        assert!(svg.contains("<filter"), "expected a filter def:\n{svg}");
        assert!(
            svg.contains("<feDropShadow"),
            "expected feDropShadow:\n{svg}"
        );
        assert!(svg.contains("dx=\"5.000\""), "expected shadow dx:\n{svg}");
        assert!(
            svg.contains("<feGaussianBlur"),
            "expected feGaussianBlur:\n{svg}"
        );
        assert!(
            svg.contains("filter=\"url(#fx"),
            "path should reference the filter:\n{svg}"
        );
    }

    /// #205: two paths sharing a byte-identical gradient must emit ONE
    /// `<linearGradient>` def, referenced twice — not `grad-0` + `grad-1` clones.
    #[test]
    fn identical_gradients_are_deduped() {
        use crate::node::{PathNode, SceneNode, SceneNodeKind};
        use crate::path::PathData;
        use crate::style::{Fill, Gradient, GradientStop};

        let mut doc = Document::new("t", 100.0, 100.0);
        let layer_id = doc.layer_order[0];
        let grad = || {
            Gradient::linear(
                0.0,
                0.0,
                100.0,
                0.0,
                vec![
                    GradientStop::new(0.0, Color::new(0.0, 0.0, 1.0, 1.0)),
                    GradientStop::new(1.0, Color::new(0.5, 0.0, 0.5, 1.0)),
                ],
            )
        };
        for name in ["a", "b"] {
            let mut pn = PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0));
            pn.fill = Fill::gradient(grad());
            let node = SceneNode::new(name, layer_id, SceneNodeKind::Path(pn));
            let nid = node.id;
            doc.nodes.insert(nid, node);
            doc.layers.get_mut(&layer_id).unwrap().node_ids.push(nid);
        }
        let svg = export_svg(&doc, &SvgExportOptions::default());
        let def_count = svg.matches("<linearGradient").count();
        assert_eq!(
            def_count, 1,
            "identical gradients should dedupe to 1 def:\n{svg}"
        );
        assert_eq!(
            svg.matches("url(#grad-0)").count(),
            2,
            "both paths should reference the single shared gradient:\n{svg}"
        );
    }

    /// #201: a gradient stroke paint must export as `stroke="url(#…)"`, and when a
    /// fill and stroke share the SAME gradient, both reference ONE deduped def.
    #[test]
    fn gradient_stroke_exports_url_and_dedupes_with_fill() {
        use crate::node::{PathNode, SceneNode, SceneNodeKind};
        use crate::path::PathData;
        use crate::style::{Fill, FillKind, Gradient, GradientStop, Stroke};

        let mut doc = Document::new("t", 100.0, 100.0);
        let layer_id = doc.layer_order[0];
        let grad = || {
            Gradient::linear(
                0.0,
                0.0,
                100.0,
                0.0,
                vec![
                    GradientStop::new(0.0, Color::new(0.18, 0.34, 0.81, 1.0)),
                    GradientStop::new(1.0, Color::new(0.49, 0.23, 0.93, 1.0)),
                ],
            )
        };
        let mut pn = PathNode::new(PathData::rect(0.0, 0.0, 40.0, 40.0))
            .with_stroke(Stroke::solid(Color::BLACK, 6.0));
        pn.fill = Fill::gradient(grad());
        // Same gradient on the stroke paint.
        pn.stroke.paint = Some(FillKind::Gradient(grad()));
        let node = SceneNode::new("icon", layer_id, SceneNodeKind::Path(pn));
        let nid = node.id;
        doc.nodes.insert(nid, node);
        doc.layers.get_mut(&layer_id).unwrap().node_ids.push(nid);

        let svg = export_svg(&doc, &SvgExportOptions::default());
        assert!(
            svg.contains("stroke=\"url(#grad-0)\""),
            "gradient stroke should export as url ref:\n{svg}"
        );
        assert!(
            svg.contains("fill=\"url(#grad-0)\""),
            "fill should share the same deduped gradient id:\n{svg}"
        );
        assert_eq!(
            svg.matches("<linearGradient").count(),
            1,
            "fill+stroke sharing one gradient must emit a single def:\n{svg}"
        );
    }

    /// #206: selection export must round path `d` to the requested precision
    /// instead of emitting 15-decimal coordinates.
    #[test]
    fn selection_export_respects_precision() {
        use crate::node::{PathNode, SceneNode, SceneNodeKind};
        use crate::path::PathData;

        let mut doc = Document::new("t", 100.0, 100.0);
        let layer_id = doc.layer_order[0];
        // A path with an ugly high-precision coordinate.
        let pd = PathData::from_svg("M0.123456789 0.987654321 L10 10 Z").unwrap();
        let node = SceneNode::new("icon", layer_id, SceneNodeKind::Path(PathNode::new(pd)));
        let nid = node.id;
        doc.nodes.insert(nid, node);
        doc.layers.get_mut(&layer_id).unwrap().node_ids.push(nid);

        let opts = SvgSelectionOptions {
            precision: 3,
            ..Default::default()
        };
        let svg = export_nodes_as_svg_opts(&doc, &[nid], &opts);
        assert!(
            svg.contains("M0.123 0.988"),
            "path d should be rounded to 3 decimals:\n{svg}"
        );
        assert!(
            !svg.contains("0.123456"),
            "no 15-decimal coordinates should survive:\n{svg}"
        );
    }

    /// #203: `Square` normalization must produce a 1:1 (square) viewBox centered on
    /// the content, so a wide icon and a tall icon frame identically.
    #[test]
    fn selection_export_square_normalize_is_uniform() {
        use crate::node::{PathNode, SceneNode, SceneNodeKind};
        use crate::path::PathData;

        let mut doc = Document::new("t", 400.0, 400.0);
        let layer_id = doc.layer_order[0];
        // Wide rectangle: 80×20.
        let pd = PathData::rect(10.0, 40.0, 80.0, 20.0);
        let node = SceneNode::new("wide", layer_id, SceneNodeKind::Path(PathNode::new(pd)));
        let nid = node.id;
        doc.nodes.insert(nid, node);
        doc.layers.get_mut(&layer_id).unwrap().node_ids.push(nid);

        let opts = SvgSelectionOptions {
            normalize: SvgNormalize::Square { pad: 0.0 },
            ..Default::default()
        };
        let svg = export_nodes_as_svg_opts(&doc, &[nid], &opts);
        // viewBox side should be max(80,20) = 80 in both dimensions.
        assert!(
            svg.contains("width=\"80\" height=\"80\""),
            "square normalize should yield an 80×80 viewBox:\n{svg}"
        );
    }

    #[test]
    fn no_filter_emitted_without_effects() {
        use crate::node::PathNode;
        use crate::path::PathData;

        let mut doc = Document::new("t", 100.0, 100.0);
        let node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))),
        );
        doc.add_node(node, None);
        let svg = export_svg(&doc, &SvgExportOptions::default());
        assert!(!svg.contains("<filter"), "no effects → no filter:\n{svg}");
    }

    // ── T0.3 CMYK colour-mode tests ────────────────────────────────────────────

    /// A 100×100 doc with one red-filled rect exported in CMYK mode must contain
    /// a CMYK fill operator (` k`) and must NOT contain an RGB fill operator (` rg`).
    #[test]
    fn pdf_cmyk_mode_emits_k_operator_not_rg() {
        use crate::document::ColorMode;
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::Fill;

        let mut doc = Document::new("t", 100.0, 100.0);
        let mut rect = PathNode::new(PathData::rect(10.0, 10.0, 80.0, 80.0));
        rect.fill = Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0)); // red
        let node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(rect),
        );
        doc.add_node(node, None);

        let opts = PdfExportOptions {
            color_mode: ColorMode::Cmyk,
            ..Default::default()
        };
        let bytes = export_pdf(&doc, &opts);
        let text = String::from_utf8_lossy(&bytes);

        assert!(
            text.contains(" k\n") || text.contains(" k "),
            "CMYK export must contain a ` k` fill operator;\n{text}"
        );
        assert!(
            !text.contains(" rg\n") && !text.contains(" rg "),
            "CMYK export must NOT contain an ` rg` RGB fill operator;\n{text}"
        );
    }

    /// Write the CMYK PDF to /tmp for offline qpdf inspection (accept criterion).
    /// This test is always skipped in CI (guarded by env CMYK_WRITE_ACCEPT=1).
    #[test]
    fn pdf_cmyk_accept_write_to_tmp() {
        if std::env::var("CMYK_WRITE_ACCEPT").as_deref() != Ok("1") {
            return;
        }
        use crate::document::ColorMode;
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::Fill;

        let mut doc = Document::new("accept", 100.0, 100.0);
        let mut rect = PathNode::new(PathData::rect(10.0, 10.0, 80.0, 80.0));
        rect.fill = Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0));
        let node = SceneNode::new("r", doc.active_layer_id.unwrap(), SceneNodeKind::Path(rect));
        doc.add_node(node, None);
        let opts = PdfExportOptions {
            color_mode: ColorMode::Cmyk,
            ..Default::default()
        };
        let bytes = export_pdf(&doc, &opts);
        let path = std::env::temp_dir().join("cmyk_accept.pdf");
        std::fs::write(&path, &bytes).unwrap();
        println!("Written {} ({} bytes)", path.display(), bytes.len());
    }

    /// The same doc in default (RGB) mode must contain ` rg` and must NOT
    /// contain a CMYK ` k` fill operator.
    #[test]
    fn pdf_rgb_mode_emits_rg_operator_not_k() {
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::Fill;

        let mut doc = Document::new("t", 100.0, 100.0);
        let mut rect = PathNode::new(PathData::rect(10.0, 10.0, 80.0, 80.0));
        rect.fill = Fill::solid(Color::new(1.0, 0.0, 0.0, 1.0)); // red
        let node = SceneNode::new(
            "rect",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Path(rect),
        );
        doc.add_node(node, None);

        let bytes = export_pdf(&doc, &PdfExportOptions::default());
        let text = String::from_utf8_lossy(&bytes);

        // pdf-writer writes the content stream uncompressed, so rg must appear.
        assert!(
            text.contains(" rg\n") || text.contains(" rg "),
            "RGB export must contain an ` rg` fill operator;\n{text}"
        );
        // No CMYK operator should appear.
        assert!(
            !text.contains(" k\n") && !text.contains(" k "),
            "RGB export must NOT contain a CMYK ` k` fill operator;\n{text}"
        );
    }

    // ── T1.5/T1.6/T1.8 — page-box geometry and marks ─────────────────────────

    /// Regression: bleed=0, marks=false → all three boxes collapse to [0,0,w,h].
    /// Behaviour must be identical to pre-bleed baseline.
    #[test]
    fn page_boxes_no_bleed_no_marks_equals_artboard() {
        let doc = Document::new("t", 200.0, 150.0);
        let opts = PdfExportOptions::default(); // marks=false, bleed=0 (doc default)
        let boxes = compute_page_boxes(&doc, &opts);
        let expected = [0.0_f32, 0.0, 200.0, 150.0];
        assert_eq!(boxes.media, expected, "media should equal artboard");
        assert_eq!(boxes.trim, expected, "trim  should equal artboard");
        assert_eq!(boxes.bleed, expected, "bleed should equal artboard");
    }

    /// T1.5: a 3 mm bleed (marks=false) produces Trim ⊂ Bleed ⊂ Media with
    /// correct numeric sizes.  At 72 dpi: bleed_px = 3 * 72 / 25.4 ≈ 8.504 pt.
    #[test]
    fn page_boxes_bleed_containment_and_size() {
        use crate::units::{to_px, DocumentUnit::Mm};

        let mut doc = Document::new("t", 252.0, 144.0); // business-card-ish
        doc.bleed_mm = 3.0;
        let opts = PdfExportOptions {
            marks: false,
            ..Default::default()
        };
        let boxes = compute_page_boxes(&doc, &opts);

        let bleed_pt = to_px(3.0, Mm, 72.0) as f32;
        let w = 252.0_f32;
        let h = 144.0_f32;

        // Trim == artboard (outer = bleed_px when marks=false, no mark_room)
        let outer = bleed_pt;
        let expected_trim = [outer, outer, outer + w, outer + h];
        let expected_bleed = [0.0, 0.0, w + 2.0 * bleed_pt, h + 2.0 * bleed_pt];
        let expected_media = [0.0, 0.0, w + 2.0 * bleed_pt, h + 2.0 * bleed_pt];

        // Containment: trim x0/y0 ≥ bleed x0/y0, trim x1/y1 ≤ bleed x1/y1
        assert!(
            boxes.trim[0] >= boxes.bleed[0] && boxes.trim[1] >= boxes.bleed[1],
            "trim origin must be inside bleed: trim={:?}, bleed={:?}",
            boxes.trim,
            boxes.bleed
        );
        assert!(
            boxes.trim[2] <= boxes.bleed[2] && boxes.trim[3] <= boxes.bleed[3],
            "trim far edge must be inside bleed"
        );
        assert!(
            boxes.bleed[0] >= boxes.media[0] && boxes.bleed[1] >= boxes.media[1],
            "bleed origin must be inside media"
        );
        assert!(
            boxes.bleed[2] <= boxes.media[2] && boxes.bleed[3] <= boxes.media[3],
            "bleed far edge must be inside media"
        );

        // Numeric sizes (tolerance 0.01 pt for float arithmetic).
        let tol = 0.01_f32;
        assert!((boxes.trim[0] - expected_trim[0]).abs() < tol, "trim[0]");
        assert!((boxes.trim[1] - expected_trim[1]).abs() < tol, "trim[1]");
        assert!((boxes.trim[2] - expected_trim[2]).abs() < tol, "trim[2]");
        assert!((boxes.trim[3] - expected_trim[3]).abs() < tol, "trim[3]");

        // When marks=false: media == bleed (no extra band).
        assert!(
            (boxes.media[2] - expected_media[2]).abs() < tol,
            "media width"
        );
        assert!(
            (boxes.media[3] - expected_media[3]).abs() < tol,
            "media height"
        );
        let _ = (expected_bleed, expected_media); // suppress unused warnings
    }

    /// T1.6: exporting with marks=true produces a longer content stream than
    /// marks=false because extra stroke operators are emitted for the marks.
    #[test]
    fn pdf_export_marks_true_emits_more_stroke_ops() {
        let mut doc = Document::new("t", 252.0, 144.0);
        doc.bleed_mm = 3.0;

        let opts_no_marks = PdfExportOptions {
            marks: false,
            ..Default::default()
        };
        let opts_marks = PdfExportOptions {
            marks: true,
            ..Default::default()
        };

        let bytes_no = export_pdf(&doc, &opts_no_marks);
        let bytes_yes = export_pdf(&doc, &opts_marks);

        assert!(
            bytes_yes.len() > bytes_no.len(),
            "marks=true PDF ({} bytes) should be larger than marks=false ({} bytes)",
            bytes_yes.len(),
            bytes_no.len()
        );

        // The marks stream must contain stroke operators (`S` in PDF).
        let text_yes = String::from_utf8_lossy(&bytes_yes);
        assert!(
            text_yes.contains(" S\n") || text_yes.contains(" S ") || text_yes.contains("\nS\n"),
            "marks=true PDF should contain stroke (S) operators"
        );
    }

    /// T1.8: when bleed_mm > 0 the exported PDF still starts with %PDF and the
    /// content-stream transform uses the trim-offset CTM, not the bare [1,0,0,-1,0,h].
    #[test]
    fn pdf_export_with_bleed_is_valid_and_uses_trim_offset_transform() {
        let mut doc = Document::new("t", 252.0, 144.0);
        doc.bleed_mm = 3.0;
        let opts = PdfExportOptions::default();
        let bytes = export_pdf(&doc, &opts);
        assert!(bytes.starts_with(b"%PDF-1"), "must start with PDF header");
        // When bleed is 0, trim_y1 = h = 144.  With bleed=3mm≈8.5pt, trim_y1
        // is approximately 144 + 8.5 = 152.5 pt (trim starts at outer≈8.5).
        let text = String::from_utf8_lossy(&bytes);
        // The old bare-h transform [1 0 0 -1 0 144] must NOT appear; the new
        // bleed-offset trim_y1 value (≈152.5) should appear instead.
        assert!(
            !text.contains("0 144 cm"),
            "old bare-h transform must not appear with bleed set"
        );
    }

    // ── P0#2 — per-artboard / multi-page PDF export ──────────────────────────

    /// A two-region export must produce a 2-page PDF: `/Count 2`, two `/Type
    /// /Page` objects, and two distinct MediaBoxes.
    #[test]
    fn export_pdf_regions_emits_one_page_per_region() {
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::Fill;

        let mut doc = Document::new("t", 400.0, 200.0);
        let mut rect = PathNode::new(PathData::rect(0.0, 0.0, 400.0, 200.0));
        rect.fill = Fill::solid(Color::new(0.0, 0.0, 1.0, 1.0));
        doc.add_node(
            SceneNode::new("r", doc.active_layer_id.unwrap(), SceneNodeKind::Path(rect)),
            None,
        );

        // Two side-by-side 200×200 regions carved out of the 400×200 canvas.
        let regions = [
            PageRegion {
                origin_x: 0.0,
                origin_y: 0.0,
                width_px: 200.0,
                height_px: 200.0,
                clip: true,
            },
            PageRegion {
                origin_x: 200.0,
                origin_y: 0.0,
                width_px: 200.0,
                height_px: 200.0,
                clip: true,
            },
        ];
        let bytes = export_pdf_regions(&doc, &PdfExportOptions::default(), &regions);
        let text = String::from_utf8_lossy(&bytes);
        assert!(bytes.starts_with(b"%PDF-1"), "must be a PDF");
        assert_eq!(
            text.matches("/Type /Page\n").count()
                + text.matches("/Type /Page ").count()
                + text.matches("/Type /Page/").count(),
            2,
            "expected exactly 2 page objects:\n{text}"
        );
        assert!(
            text.contains("/Count 2"),
            "page tree /Count must be 2:\n{text}"
        );
        // Clip operator (`W n`) must appear for a clipped region.
        assert!(
            text.contains(" W\n") || text.contains(" W ") || text.contains("\nW\n"),
            "clipped region should emit a clip (W) operator"
        );
    }

    /// ACCEPT (P0#2): a 3.5×2 in card authored at 300 dpi (1050×600 px) exported
    /// per-artboard must land a 252×144 pt TrimBox — the finished card size — no
    /// redesign needed.
    #[test]
    fn per_artboard_pdf_trim_is_physical_size_at_300dpi() {
        let mut doc = Document::new("card", 1050.0, 600.0);
        doc.dpi = 300.0;
        doc.bleed_mm = 3.0;
        let ab = doc.artboards[0].clone();
        // The default artboard spans the canvas; assert it's 1050×600.
        assert!((ab.width - 1050.0).abs() < 1e-6 && (ab.height - 600.0).abs() < 1e-6);

        let region = PageRegion::artboard(&ab);
        let boxes = compute_page_boxes_dims(
            region.width_px,
            region.height_px,
            &doc,
            &PdfExportOptions::default(),
        );
        let approx = |a: f32, b: f32| (a - b).abs() < 0.05;
        let trim_w = boxes.trim[2] - boxes.trim[0];
        let trim_h = boxes.trim[3] - boxes.trim[1];
        assert!(
            approx(trim_w, 252.0) && approx(trim_h, 144.0),
            "300-dpi 1050×600 card must trim to 252×144 pt, got {trim_w}×{trim_h}"
        );

        // End-to-end: the multi-page exporter produces a valid PDF for it.
        let bytes = export_pdf_regions(&doc, &PdfExportOptions::default(), &[region]);
        assert!(bytes.starts_with(b"%PDF-1"));
    }

    /// The whole-document single-page path must still route through the region
    /// exporter and stay a valid single page (`/Count 1`).
    #[test]
    fn export_pdf_single_page_still_count_one() {
        let doc = Document::new("t", 100.0, 100.0);
        let bytes = export_pdf(&doc, &PdfExportOptions::default());
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("/Count 1"),
            "single page must have /Count 1:\n{text}"
        );
    }

    // ── P1#4 — gradient shading in PDF ───────────────────────────────────────

    /// A blue→purple linear gradient fill must export as a real PDF axial shading
    /// (ShadingType 2, a stitching + exponential function, and an `sh` paint op) —
    /// not a flattened first-stop solid.
    #[test]
    fn pdf_gradient_exports_axial_shading_not_solid() {
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::{Fill, FillKind, Gradient, GradientStop};

        let mut doc = Document::new("t", 100.0, 100.0);
        let grad = Gradient::linear(
            0.0,
            0.0,
            100.0,
            0.0,
            vec![
                GradientStop::new(0.0, Color::new(0.1, 0.2, 0.9, 1.0)),
                GradientStop::new(1.0, Color::new(0.5, 0.1, 0.8, 1.0)),
            ],
        );
        let mut p = PathNode::new(PathData::rect(0.0, 0.0, 100.0, 100.0));
        p.fill = Fill {
            kind: FillKind::Gradient(grad),
            opacity: 1.0,
            enabled: true,
        };
        doc.add_node(
            SceneNode::new("g", doc.active_layer_id.unwrap(), SceneNodeKind::Path(p)),
            None,
        );

        let text =
            String::from_utf8_lossy(&export_pdf(&doc, &PdfExportOptions::default())).into_owned();
        assert!(
            text.contains("/ShadingType 2"),
            "expected axial shading:\n{text}"
        );
        assert!(
            text.contains("/FunctionType 3"),
            "expected a stitching function"
        );
        assert!(
            text.contains("/FunctionType 2"),
            "expected exponential sub-function(s)"
        );
        assert!(text.contains("/Coords"), "shading must carry Coords");
        // `sh` paint operator (clip + shade), and the path is clipped (`W`).
        assert!(
            text.contains(" sh\n") || text.contains(" sh ") || text.contains("\nsh\n"),
            "expected an sh (shade) operator"
        );
        // Must NOT collapse to a first-stop solid RGB fill.
        assert!(
            !text.contains(" rg\nf"),
            "gradient should not export as a solid fill"
        );
    }

    /// Pull the numeric `/Coords [ … ]` array out of an exported PDF (the shading
    /// dict is emitted uncompressed, so it is plain text).
    #[cfg(test)]
    fn extract_shading_coords(pdf: &str) -> Vec<f32> {
        let start = pdf.find("/Coords").expect("shading must carry /Coords");
        let open = pdf[start..].find('[').expect("Coords array open") + start + 1;
        let close = pdf[open..].find(']').expect("Coords array close") + open;
        pdf[open..close]
            .split_whitespace()
            .filter_map(|t| t.parse::<f32>().ok())
            .collect()
    }

    /// ACCEPT (gradient export): a rect with an `objectBoundingBox` blue→purple
    /// gradient must export a real axial shading whose axis SPANS the rect — not a
    /// `0..1` axis that `Extend` collapses to a single solid stop. Verified for
    /// both RGB (DeviceRGB) and CMYK/PDF-X (DeviceCMYK) exports.
    #[test]
    fn pdf_object_bbox_gradient_axis_spans_rect_not_solid() {
        use crate::document::ColorMode;
        use crate::node::PathNode;
        use crate::path::PathData;
        use crate::style::{Fill, FillKind, Gradient, GradientStop, GradientUnits};

        // #2f56cf → #5b21b6
        let c0 = Color::from_hex("#2f56cf").unwrap();
        let c1 = Color::from_hex("#5b21b6").unwrap();

        let make_doc = || {
            let mut doc = Document::new("t", 300.0, 200.0);
            // Left→right gradient in object-bounding-box space: [0,0.5]→[1,0.5].
            let grad = Gradient::linear(
                0.0,
                0.5,
                1.0,
                0.5,
                vec![GradientStop::new(0.0, c0), GradientStop::new(1.0, c1)],
            )
            .with_units(GradientUnits::ObjectBoundingBox);
            let mut p = PathNode::new(PathData::rect(0.0, 0.0, 300.0, 200.0));
            p.fill = Fill {
                kind: FillKind::Gradient(grad),
                opacity: 1.0,
                enabled: true,
            };
            doc.add_node(
                SceneNode::new("g", doc.active_layer_id.unwrap(), SceneNodeKind::Path(p)),
                None,
            );
            doc
        };

        // ── RGB export ──
        let rgb = String::from_utf8_lossy(&export_pdf(&make_doc(), &PdfExportOptions::default()))
            .into_owned();
        assert!(
            rgb.contains("/ShadingType 2"),
            "expected an axial shading:\n{rgb}"
        );
        assert!(
            rgb.contains("/DeviceRGB"),
            "RGB export uses DeviceRGB stops"
        );
        let coords = extract_shading_coords(&rgb);
        assert_eq!(coords.len(), 4, "axial shading has 4 coords: {coords:?}");
        // The axis must span the object (x: 0 → 300), NOT the buggy 0 → 1.
        let axis_len = (coords[2] - coords[0]).hypot(coords[3] - coords[1]);
        assert!(
            axis_len > 100.0,
            "gradient axis must span the rect (got length {axis_len} from {coords:?}); \
             a ~1-unit axis means the object-bbox coords were not resolved and the \
             gradient collapses to a solid stop under Extend"
        );

        // ── CMYK / PDF-X export ──
        let cmyk_opts = PdfExportOptions {
            color_mode: ColorMode::Cmyk,
            ..Default::default()
        };
        let cmyk = String::from_utf8_lossy(&export_pdf(&make_doc(), &cmyk_opts)).into_owned();
        assert!(
            cmyk.contains("/ShadingType 2"),
            "CMYK export also emits a real shading"
        );
        assert!(
            cmyk.contains("/DeviceCMYK"),
            "CMYK export uses DeviceCMYK stops"
        );
        let cmyk_coords = extract_shading_coords(&cmyk);
        let cmyk_axis = (cmyk_coords[2] - cmyk_coords[0]).hypot(cmyk_coords[3] - cmyk_coords[1]);
        assert!(
            cmyk_axis > 100.0,
            "CMYK gradient axis must span the rect: {cmyk_coords:?}"
        );
    }

    // ── P1#3 — placed raster embeds as an image XObject ──────────────────────

    /// A document with a placed PNG (raster node) must export a PDF containing an
    /// image XObject and a Do paint operator — the image is present, not dropped.
    #[test]
    fn pdf_embeds_placed_raster_as_image_xobject() {
        use crate::node::RasterNode;
        use crate::raster::RasterImage;

        let mut doc = Document::new("t", 64.0, 64.0);
        // A 4×4 opaque red RGBA image.
        let pixels: Vec<u8> = (0..16).flat_map(|_| [255u8, 0, 0, 255]).collect();
        let img = RasterImage::from_rgba(4, 4, pixels).unwrap();
        let node = SceneNode::new(
            "photo",
            doc.active_layer_id.unwrap(),
            SceneNodeKind::Raster(RasterNode::new(img)),
        );
        doc.add_node(node, None);

        let text =
            String::from_utf8_lossy(&export_pdf(&doc, &PdfExportOptions::default())).into_owned();
        assert!(
            text.contains("/Subtype /Image"),
            "expected an image XObject:\n{}",
            &text[..text.len().min(800)]
        );
        assert!(
            text.contains("/Width 4") && text.contains("/Height 4"),
            "image dimensions"
        );
        assert!(text.contains("/DeviceRGB"), "RGB image colour space");
        assert!(
            text.contains("/Im0 Do") || text.contains("Do\n"),
            "expected a Do (paint XObject) operator"
        );
    }

    /// A placed raster with alpha (RGB export) must carry an SMask; a CMYK export
    /// must pre-flatten it to an opaque DeviceCMYK image (no SMask — X-1a forbids
    /// transparency).
    #[test]
    fn pdf_raster_alpha_smask_rgb_and_flattened_cmyk() {
        use crate::document::ColorMode;
        use crate::node::RasterNode;
        use crate::raster::RasterImage;

        let make_doc = || {
            let mut doc = Document::new("t", 64.0, 64.0);
            // Semi-transparent green.
            let pixels: Vec<u8> = (0..16).flat_map(|_| [0u8, 200, 0, 128]).collect();
            let img = RasterImage::from_rgba(4, 4, pixels).unwrap();
            doc.add_node(
                SceneNode::new(
                    "photo",
                    doc.active_layer_id.unwrap(),
                    SceneNodeKind::Raster(RasterNode::new(img)),
                ),
                None,
            );
            doc
        };

        let rgb = String::from_utf8_lossy(&export_pdf(&make_doc(), &PdfExportOptions::default()))
            .into_owned();
        assert!(
            rgb.contains("/SMask"),
            "RGB raster with alpha should carry an SMask"
        );
        assert!(rgb.contains("/DeviceGray"), "SMask is a DeviceGray image");

        let cmyk_opts = PdfExportOptions {
            color_mode: ColorMode::Cmyk,
            ..Default::default()
        };
        let cmyk = String::from_utf8_lossy(&export_pdf(&make_doc(), &cmyk_opts)).into_owned();
        assert!(
            cmyk.contains("/DeviceCMYK"),
            "CMYK raster should be DeviceCMYK"
        );
        assert!(
            !cmyk.contains("/SMask"),
            "CMYK/X-1a export must not carry transparency"
        );
    }

    /// ACCEPT (bug 1): a transparent PNG over a filled rect must export so the rect
    /// shows through — the exporter emits a soft mask and a transparent pixel maps
    /// to SMask 0 (fully transparent), NOT an opaque white fill covering the card.
    #[test]
    fn pdf_transparent_raster_shows_through_not_white() {
        use crate::node::{PathNode, RasterNode};
        use crate::path::PathData;
        use crate::raster::RasterImage;
        use crate::style::{Fill, FillKind};

        let mut doc = Document::new("t", 8.0, 8.0);
        let layer = doc.active_layer_id.unwrap();

        // Dark background rect covering the page.
        let mut bg = PathNode::new(PathData::rect(0.0, 0.0, 8.0, 8.0));
        bg.fill = Fill {
            kind: FillKind::Solid(Color::new(0.05, 0.05, 0.08, 1.0)),
            opacity: 1.0,
            enabled: true,
        };
        doc.add_node(SceneNode::new("card", layer, SceneNodeKind::Path(bg)), None);

        // A 4×4 RGBA image whose corner is fully transparent (0,0,0,0) and centre
        // is opaque white — the "avatar over a dark card" repro.
        let mut pixels = vec![0u8; 4 * 4 * 4];
        for i in 0..16 {
            let opaque = i == 5; // one interior pixel fully opaque white
            let p = &mut pixels[i * 4..i * 4 + 4];
            p.copy_from_slice(&[255, 255, 255, if opaque { 255 } else { 0 }]);
        }
        let img = RasterImage::from_rgba(4, 4, pixels.clone()).unwrap();
        doc.add_node(
            SceneNode::new(
                "avatar",
                layer,
                SceneNodeKind::Raster(RasterNode::new(img.clone())),
            ),
            None,
        );

        // Structural: the exported PDF carries a soft mask (not a white fill).
        let text =
            String::from_utf8_lossy(&export_pdf(&doc, &PdfExportOptions::default())).into_owned();
        assert!(
            text.contains("/SMask"),
            "transparent raster must export a soft mask"
        );
        assert!(
            text.contains("/DeviceGray"),
            "the soft mask is a DeviceGray image"
        );

        // The soft-mask samples themselves: the transparent corner is 0, an opaque
        // pixel is 255. (Had the exporter filled transparent white, there'd be no
        // mask and the corner colour would be opaque.)
        let pi = build_pdf_image(
            &img,
            &PdfExportOptions::default(),
            1.0,
            72.0,
            &BackdropScene::uniform([1.0, 1.0, 1.0]),
        );
        let alpha = pi.alpha.as_ref().expect("alpha soft mask must be present");
        let gray = miniz_oxide::inflate::decompress_to_vec_zlib(alpha)
            .expect("SMask stream is valid zlib");
        assert_eq!(gray.len(), 16, "one gray sample per pixel");
        assert_eq!(
            gray[0], 0,
            "transparent corner → SMask 0 (shows the card through)"
        );
        assert_eq!(gray[5], 255, "opaque pixel → SMask 255");
    }

    /// ACCEPT (bug 3): a card with two placed rasters must export at a small
    /// fraction of the raw pixel size — downsampled to the DPI cap and compressed
    /// (DCT for the photographic RGB), no longer the ~11.8 MB uncompressed dump.
    #[test]
    fn pdf_embedded_rasters_are_downsampled_and_compressed() {
        use crate::node::RasterNode;
        use crate::raster::RasterImage;
        use crate::transform::Transform;

        // 2×3 in card at 300 dpi; two 1200×1200 photographic rasters placed at
        // half size (→ 600 dpi effective, above the 300 cap → downsampled).
        let mut doc = Document::new("card", 600.0, 900.0);
        doc.dpi = 300.0;
        let layer = doc.active_layer_id.unwrap();

        let native = 1200u32;
        let make_photo = || {
            let mut px = Vec::with_capacity((native * native * 4) as usize);
            for y in 0..native {
                for x in 0..native {
                    // Three de-correlated smooth ramps → >1024 distinct colours, so
                    // `looks_photographic` picks the DCT path.
                    px.extend_from_slice(&[
                        (x.wrapping_mul(2) & 0xff) as u8,
                        (y.wrapping_mul(2) & 0xff) as u8,
                        ((x + y).wrapping_mul(2) & 0xff) as u8,
                        255,
                    ]);
                }
            }
            RasterImage::from_rgba(native, native, px).unwrap()
        };

        for ty in [0.0_f64, 300.0] {
            let mut node = SceneNode::new(
                "photo",
                layer,
                SceneNodeKind::Raster(RasterNode::new(make_photo())),
            );
            // Scale 0.5 (half the native pixels on the page) and stack vertically.
            node.transform = Transform::new(0.5, 0.0, 0.0, 0.5, 0.0, ty);
            doc.add_node(node, None);
        }

        let bytes = export_pdf(&doc, &PdfExportOptions::default());
        // Raw uncompressed color for two 1200² rasters ≈ 8.2 MB (the old dump).
        let raw = 2 * (native as usize) * (native as usize) * 3;
        assert!(
            bytes.len() < raw / 8,
            "expected a small fraction of the {raw}-byte raw size, got {}",
            bytes.len()
        );
        // Absolute sanity: comfortably under 1 MB for this card.
        assert!(
            bytes.len() < 1_000_000,
            "two-raster card should export well under 1 MB, got {} bytes",
            bytes.len()
        );
        // The downsample cap: XObject width is 600 (1200 × 0.5), not the native 1200.
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(
            text.contains("/Width 600"),
            "raster should be downsampled to 600 px wide"
        );
        assert!(
            text.contains("/DCTDecode"),
            "photographic RGB should compress with DCT/JPEG"
        );
    }

    /// ACCEPT (P0#2 + P1): write a 2-artboard CMYK PDF/X-1a card batch (with a
    /// gradient fill + a placed raster) to the temp dir for offline preflight.
    /// Gated by `ARTBOARD_ACCEPT=1` so CI stays fast.
    ///   ARTBOARD_ACCEPT=1 cargo test -p photonic-core --lib per_artboard_cmyk_accept -- --nocapture
    #[test]
    fn per_artboard_cmyk_accept_write_to_tmp() {
        if std::env::var("ARTBOARD_ACCEPT").as_deref() != Ok("1") {
            return;
        }
        use crate::document::{Artboard, ColorMode};
        use crate::node::{PathNode, RasterNode};
        use crate::path::PathData;
        use crate::raster::RasterImage;
        use crate::style::{Fill, FillKind, Gradient, GradientStop};

        // 3.5×2 in card at 300 dpi ⇒ 1050×600 px, plus a second (back) artboard.
        let mut doc = Document::new("cards", 2100.0, 600.0);
        doc.dpi = 300.0;
        doc.bleed_mm = 3.0;
        doc.color_mode = ColorMode::Cmyk;
        doc.artboards = vec![
            Artboard::new("front", 0.0, 0.0, 1050.0, 600.0),
            Artboard::new("back", 1050.0, 0.0, 1050.0, 600.0),
        ];
        doc.active_artboard = Some(doc.artboards[0].id);
        let layer = doc.active_layer_id.unwrap();

        // Gradient panel on the front.
        let grad = Gradient::linear(
            0.0,
            0.0,
            1050.0,
            0.0,
            vec![
                GradientStop::new(0.0, Color::new(0.05, 0.12, 0.30, 1.0)),
                GradientStop::new(1.0, Color::new(0.35, 0.10, 0.55, 1.0)),
            ],
        );
        let mut panel = PathNode::new(PathData::rect(0.0, 0.0, 1050.0, 600.0));
        panel.fill = Fill {
            kind: FillKind::Gradient(grad),
            opacity: 1.0,
            enabled: true,
        };
        doc.add_node(
            SceneNode::new("panel", layer, SceneNodeKind::Path(panel)),
            None,
        );

        // A placed raster (opaque) on the front.
        let pixels: Vec<u8> = (0..(64 * 64))
            .flat_map(|i| {
                let v = (i % 255) as u8;
                [v, 200, 255 - v, 255]
            })
            .collect();
        let img = RasterImage::from_rgba(64, 64, pixels).unwrap();
        let mut rnode = SceneNode::new("logo", layer, SceneNodeKind::Raster(RasterNode::new(img)));
        rnode.transform = crate::transform::Transform::new(4.0, 0.0, 0.0, 4.0, 60.0, 60.0);
        doc.add_node(rnode, None);

        let opts = PdfExportOptions {
            background: Some(Color::new(0.05, 0.12, 0.30, 1.0)),
            outline_text: false,
            marks: true,
            color_mode: ColorMode::Cmyk,
            icc_profile: None,
        };
        let regions: Vec<PageRegion> = doc.artboards.iter().map(PageRegion::artboard).collect();
        let bytes = export_pdf_regions(&doc, &opts, &regions);
        let path = std::env::temp_dir().join("photonic_artboard_batch.pdf");
        std::fs::write(&path, &bytes).unwrap();
        println!(
            "Wrote {} ({} bytes, {} pages)",
            path.display(),
            bytes.len(),
            regions.len()
        );
    }

    /// ACCEPT evidence: write business-card PDFs (with and without marks) to
    /// /tmp/photonic_accept_*.pdf and print computed box values.
    /// Run with: cargo test -p photonic-core --lib accept_evidence -- --nocapture
    #[test]
    fn accept_evidence_page_boxes_and_marks() {
        use crate::units::{to_px, DocumentUnit::Mm};

        let mut doc = Document::new("accept", 252.0, 144.0);
        doc.bleed_mm = 3.0;

        let opts_no = PdfExportOptions {
            marks: false,
            ..Default::default()
        };
        let opts_marks = PdfExportOptions {
            marks: true,
            ..Default::default()
        };

        let boxes_no = compute_page_boxes(&doc, &opts_no);
        let boxes_marks = compute_page_boxes(&doc, &opts_marks);

        let bleed_pt = to_px(3.0, Mm, 72.0) as f32;
        println!("bleed_mm=3.0  → bleed_pt = {bleed_pt:.4}");
        println!("--- marks=false ---");
        println!("  MediaBox = {:?}", boxes_no.media);
        println!("  BleedBox = {:?}", boxes_no.bleed);
        println!("  TrimBox  = {:?}", boxes_no.trim);
        println!("--- marks=true ---");
        println!("  MediaBox = {:?}", boxes_marks.media);
        println!("  BleedBox = {:?}", boxes_marks.bleed);
        println!("  TrimBox  = {:?}", boxes_marks.trim);

        // Containment
        let m = &boxes_marks;
        assert!(m.trim[0] >= m.bleed[0] && m.trim[0] >= 0.0, "T⊂B⊂M x0");
        assert!(m.bleed[0] >= m.media[0], "B⊂M x0");
        assert!(m.trim[2] <= m.bleed[2], "T⊂B x1");
        assert!(m.bleed[2] <= m.media[2], "B⊂M x1");

        // Write PDFs to the OS temp dir (cross-platform; a hardcoded /tmp path
        // does not exist on the Windows CI runner and fails the test there).
        let bytes_no = export_pdf(&doc, &opts_no);
        let bytes_marks = export_pdf(&doc, &opts_marks);
        let dir = std::env::temp_dir();
        let path_no = dir.join("photonic_accept_no_marks.pdf");
        let path_marks = dir.join("photonic_accept_marks.pdf");
        std::fs::write(&path_no, &bytes_no).unwrap();
        std::fs::write(&path_marks, &bytes_marks).unwrap();
        println!("Wrote: {} ({} bytes)", path_no.display(), bytes_no.len());
        println!(
            "Wrote: {} ({} bytes)",
            path_marks.display(),
            bytes_marks.len()
        );
        assert!(bytes_marks.len() > bytes_no.len(), "marks adds bytes");
    }
}
