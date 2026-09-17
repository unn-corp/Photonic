//! SVG file import — converts an SVG document into a Photonic [`Document`].

use crate::{
    layer::{BlendMode, LayerId},
    node::{GroupNode, PathNode, TextAlign, TextNode},
    style::{Gradient, GradientStop, LineCap, LineJoin},
    Color, Document, Fill, PathData, SceneNode, SceneNodeKind, Stroke, Transform,
};
use kurbo::{BezPath, PathEl};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use thiserror::Error;
use uuid::Uuid;

// ─── Error ────────────────────────────────────────────────────────────────────

/// The independently bounded resources used while importing an SVG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportLimit {
    InputBytes,
    Elements,
    Depth,
    TextBytes,
    CssBytes,
    PathPoints,
}

impl fmt::Display for ImportLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::InputBytes => "input bytes",
            Self::Elements => "elements",
            Self::Depth => "nesting depth",
            Self::TextBytes => "text bytes",
            Self::CssBytes => "CSS bytes",
            Self::PathPoints => "path points",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("XML parse error: {0}")]
    Xml(#[from] roxmltree::Error),
    #[error("The file does not appear to be an SVG")]
    NotSvg,
    #[error("SVG import {limit} limit exceeded (maximum {max})")]
    LimitExceeded { limit: ImportLimit, max: usize },
}

/// Maximum UTF-8 input accepted by [`import_svg`].
pub const MAX_SVG_INPUT_BYTES: usize = 512 * 1024;
/// Maximum number of XML elements traversed by the importer.
pub const MAX_SVG_ELEMENTS: usize = 4_096;
/// Maximum element nesting below the root `<svg>` element.
pub const MAX_SVG_DEPTH: usize = 64;
/// Maximum text content retained from imported `<text>` elements.
pub const MAX_SVG_TEXT_BYTES: usize = 256 * 1024;
/// Maximum CSS content retained from `<style>` and inline style attributes.
pub const MAX_SVG_CSS_BYTES: usize = 256 * 1024;
/// Maximum number of point-bearing path elements retained across the document.
pub const MAX_SVG_PATH_POINTS: usize = 100_000;

#[derive(Default)]
struct ImportState {
    elements: usize,
    text_bytes: usize,
    css_bytes: usize,
    path_points: usize,
}

impl ImportState {
    fn visit_element(&mut self, depth: usize) -> Result<(), ImportError> {
        if depth > MAX_SVG_DEPTH {
            return Err(ImportError::LimitExceeded {
                limit: ImportLimit::Depth,
                max: MAX_SVG_DEPTH,
            });
        }
        add_limited(
            &mut self.elements,
            1,
            MAX_SVG_ELEMENTS,
            ImportLimit::Elements,
        )
    }

    fn ensure_depth(&self, depth: usize) -> Result<(), ImportError> {
        if depth > MAX_SVG_DEPTH {
            Err(ImportError::LimitExceeded {
                limit: ImportLimit::Depth,
                max: MAX_SVG_DEPTH,
            })
        } else {
            Ok(())
        }
    }

    fn add_text_bytes(&mut self, amount: usize) -> Result<(), ImportError> {
        add_limited(
            &mut self.text_bytes,
            amount,
            MAX_SVG_TEXT_BYTES,
            ImportLimit::TextBytes,
        )
    }

    fn add_css_bytes(&mut self, amount: usize) -> Result<(), ImportError> {
        add_limited(
            &mut self.css_bytes,
            amount,
            MAX_SVG_CSS_BYTES,
            ImportLimit::CssBytes,
        )
    }

    fn ensure_path_points(&self, amount: usize) -> Result<(), ImportError> {
        if self.path_points.checked_add(amount).is_none()
            || self.path_points.saturating_add(amount) > MAX_SVG_PATH_POINTS
        {
            Err(ImportError::LimitExceeded {
                limit: ImportLimit::PathPoints,
                max: MAX_SVG_PATH_POINTS,
            })
        } else {
            Ok(())
        }
    }

    fn add_path_points(&mut self, amount: usize) -> Result<(), ImportError> {
        add_limited(
            &mut self.path_points,
            amount,
            MAX_SVG_PATH_POINTS,
            ImportLimit::PathPoints,
        )
    }
}

fn add_limited(
    value: &mut usize,
    amount: usize,
    max: usize,
    limit: ImportLimit,
) -> Result<(), ImportError> {
    let next = value
        .checked_add(amount)
        .ok_or(ImportError::LimitExceeded { limit, max })?;
    if next > max {
        return Err(ImportError::LimitExceeded { limit, max });
    }
    *value = next;
    Ok(())
}

/// Visit every element iteratively, enforcing the tree-wide element and depth
/// limits before any recursive scene-graph construction starts.
fn walk_elements<'a, 'input, F>(
    root: roxmltree::Node<'a, 'input>,
    state: &mut ImportState,
    mut visit: F,
) -> Result<(), ImportError>
where
    F: FnMut(roxmltree::Node<'a, 'input>, usize, &mut ImportState) -> Result<(), ImportError>,
{
    let mut pending = vec![(root, 0usize)];
    while let Some((node, depth)) = pending.pop() {
        if !node.is_element() {
            continue;
        }
        state.visit_element(depth)?;
        visit(node, depth, state)?;

        for child in node.children().filter(|child| child.is_element()).rev() {
            pending.push((child, depth.saturating_add(1)));
        }
    }
    Ok(())
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Parse an SVG string and return a Photonic [`Document`].
pub fn import_svg(svg_text: &str) -> Result<Document, ImportError> {
    if svg_text.len() > MAX_SVG_INPUT_BYTES {
        return Err(ImportError::LimitExceeded {
            limit: ImportLimit::InputBytes,
            max: MAX_SVG_INPUT_BYTES,
        });
    }

    // roxmltree deliberately rejects every DTD. SVG editors commonly emit the
    // standard SVG 1.1 external doctype even though the document does not rely
    // on it. Remove the declaration without resolving external resources.
    let svg_text = without_doctype(svg_text);
    let tree = roxmltree::Document::parse(&svg_text)?;
    let root = tree.root_element();

    if root.tag_name().name() != "svg" {
        return Err(ImportError::NotSvg);
    }

    let (doc_width, doc_height) = parse_viewport(&root);
    let mut doc = Document::new("Imported SVG", doc_width, doc_height);
    let layer_id = doc.active_layer_id.unwrap();
    let mut state = ImportState::default();

    // First pass: collect CSS rules and gradient defs from <defs> / <style>
    let ctx = build_context(&root, &mut state)?;

    let default_style = ComputedStyle::default();

    for child in root.children() {
        if !child.is_element() {
            continue;
        }
        if matches!(
            child.tag_name().name(),
            "defs" | "metadata" | "title" | "desc" | "style"
        ) {
            continue;
        }
        if let Some(node_id) = import_element(
            &child,
            &mut doc,
            layer_id,
            &default_style,
            &ctx,
            &mut state,
            1,
        )? {
            doc.layers
                .get_mut(&layer_id)
                .unwrap()
                .node_ids
                .push(node_id);
        }
    }

    Ok(doc)
}

fn without_doctype(svg: &str) -> Cow<'_, str> {
    let Some(start) = svg.find("<!DOCTYPE") else {
        return Cow::Borrowed(svg);
    };

    let bytes = svg.as_bytes();
    let mut quote = None;
    let mut subset_depth = 0usize;
    for (offset, byte) in bytes[start + "<!DOCTYPE".len()..]
        .iter()
        .copied()
        .enumerate()
    {
        match (quote, byte) {
            (Some(active), b) if b == active => quote = None,
            (Some(_), _) => {}
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'[') => subset_depth += 1,
            (None, b']') => subset_depth = subset_depth.saturating_sub(1),
            (None, b'>') if subset_depth == 0 => {
                let end = start + "<!DOCTYPE".len() + offset + 1;
                let mut sanitized = String::with_capacity(svg.len() - (end - start));
                sanitized.push_str(&svg[..start]);
                sanitized.push_str(&svg[end..]);
                return Cow::Owned(sanitized);
            }
            _ => {}
        }
    }

    Cow::Borrowed(svg)
}

// ─── Context: CSS rules + gradient defs ──────────────────────────────────────

/// Properties extracted from a CSS rule block (property name → value).
type CssProps = HashMap<String, String>;

struct SvgContext {
    /// `.class` and element-type CSS rules → property map
    css_rules: HashMap<String, CssProps>,
    /// Parsed gradient definitions, keyed by their `id`
    gradients: HashMap<String, ParsedGradient>,
    /// Document dimensions (needed for objectBoundingBox → userSpace conversion)
    doc_width: f64,
    doc_height: f64,
}

#[derive(Clone)]
struct ParsedGradient {
    kind: ParsedGradKind,
    stops: Vec<GradientStop>,
    units: GradientUnits,
    /// href/xlink:href pointing to another gradient whose stops to inherit
    href: Option<String>,
}

#[derive(Clone)]
enum ParsedGradKind {
    Linear { x1: f64, y1: f64, x2: f64, y2: f64 },
    Radial { cx: f64, cy: f64, r: f64 },
}

#[derive(Clone, Copy, PartialEq)]
enum GradientUnits {
    ObjectBoundingBox,
    UserSpaceOnUse,
}

fn build_context(
    root: &roxmltree::Node,
    state: &mut ImportState,
) -> Result<SvgContext, ImportError> {
    let (doc_width, doc_height) = parse_viewport(root);
    let mut css_rules: HashMap<String, CssProps> = HashMap::new();
    let mut gradients: HashMap<String, ParsedGradient> = HashMap::new();

    // Scan the entire tree, not just direct children of <svg>. Illustrator and
    // most other tools nest the <style> block and gradient defs inside <defs>,
    // and <style> can appear at any depth. The bounded iterative walk makes
    // CSS-class fills (`.cls-1 { fill: #... }`) and gradients resolve regardless
    // of nesting — the previous code only matched <style>/<defs> that were
    // direct children of the root <svg>, so class-based colors silently
    // imported as the default black fill.
    walk_elements(*root, state, |node, _depth, state| {
        match node.tag_name().name() {
            "style" => {
                let css = element_text(&node, state)?;
                if !css.trim().is_empty() {
                    parse_css_into(&css, &mut css_rules);
                }
            }
            "linearGradient" => {
                if let Some(id) = node.attribute("id") {
                    let units = parse_gradient_units(&node);
                    let x1 =
                        parse_percentage_or_number(node.attribute("x1").unwrap_or("0%"), units);
                    let y1 =
                        parse_percentage_or_number(node.attribute("y1").unwrap_or("0%"), units);
                    let x2 =
                        parse_percentage_or_number(node.attribute("x2").unwrap_or("100%"), units);
                    let y2 =
                        parse_percentage_or_number(node.attribute("y2").unwrap_or("0%"), units);
                    let stops = parse_gradient_stops(&node, state)?;
                    let href = node
                        .attribute("href")
                        .or_else(|| node.attribute("xlink:href"))
                        .filter(|h| h.starts_with('#'))
                        .map(|h| h[1..].to_string());
                    gradients.insert(
                        id.to_string(),
                        ParsedGradient {
                            kind: ParsedGradKind::Linear { x1, y1, x2, y2 },
                            stops,
                            units,
                            href,
                        },
                    );
                }
            }
            "radialGradient" => {
                if let Some(id) = node.attribute("id") {
                    let units = parse_gradient_units(&node);
                    let cx =
                        parse_percentage_or_number(node.attribute("cx").unwrap_or("50%"), units);
                    let cy =
                        parse_percentage_or_number(node.attribute("cy").unwrap_or("50%"), units);
                    let r = parse_percentage_or_number(node.attribute("r").unwrap_or("50%"), units);
                    let stops = parse_gradient_stops(&node, state)?;
                    let href = node
                        .attribute("href")
                        .or_else(|| node.attribute("xlink:href"))
                        .filter(|h| h.starts_with('#'))
                        .map(|h| h[1..].to_string());
                    gradients.insert(
                        id.to_string(),
                        ParsedGradient {
                            kind: ParsedGradKind::Radial { cx, cy, r },
                            stops,
                            units,
                            href,
                        },
                    );
                }
            }
            _ => {}
        }
        Ok(())
    })?;

    // Resolve href stop inheritance (one level deep is sufficient for most SVGs)
    let ids: Vec<String> = gradients.keys().cloned().collect();
    for id in ids {
        let href_id = gradients[&id].href.clone();
        if let Some(ref_id) = href_id {
            if gradients[&id].stops.is_empty() {
                if let Some(stops) = gradients.get(&ref_id).map(|g| g.stops.clone()) {
                    if let Some(g) = gradients.get_mut(&id) {
                        g.stops = stops;
                    }
                }
            }
        }
    }

    Ok(SvgContext {
        css_rules,
        gradients,
        doc_width,
        doc_height,
    })
}

// ─── CSS parsing ──────────────────────────────────────────────────────────────

/// Concatenate all text (including CDATA) found under an element. `<style>`
/// content is usually a single text node, but it may be wrapped in CDATA or
/// split into several segments — this gathers all of it.
fn element_text(node: &roxmltree::Node, state: &mut ImportState) -> Result<String, ImportError> {
    collect_descendant_text(*node, state, ImportLimit::CssBytes)
}

fn parse_css_into(css: &str, out: &mut HashMap<String, CssProps>) {
    // Strip block comments
    let mut text = css.to_string();
    while let (Some(start), Some(end)) = (text.find("/*"), text.find("*/")) {
        if start < end {
            text.replace_range(start..end + 2, " ");
        } else {
            break;
        }
    }

    // Split on '}' to get each rule block
    for rule_block in text.split('}') {
        let rule_block = rule_block.trim();
        if rule_block.is_empty() {
            continue;
        }
        let brace = match rule_block.find('{') {
            Some(i) => i,
            None => continue,
        };
        let selectors_str = rule_block[..brace].trim();
        let decls_str = rule_block[brace + 1..].trim();

        // Parse declarations once, share across selectors
        let mut props = CssProps::new();
        for decl in decls_str.split(';') {
            let decl = decl.trim();
            if decl.is_empty() {
                continue;
            }
            let mut kv = decl.splitn(2, ':');
            if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
                props.insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }

        // Apply props to each comma-separated selector
        for sel in selectors_str.split(',') {
            let sel = sel.trim().to_string();
            if sel.is_empty() {
                continue;
            }
            out.entry(sel).or_default().extend(props.clone());
        }
    }
}

// ─── Gradient stop parsing ────────────────────────────────────────────────────

fn parse_gradient_stops(
    node: &roxmltree::Node,
    state: &mut ImportState,
) -> Result<Vec<GradientStop>, ImportError> {
    let mut stops = Vec::new();
    for child in node.children() {
        if !child.is_element() || child.tag_name().name() != "stop" {
            continue;
        }
        if let Some(style_attr) = child.attribute("style") {
            state.add_css_bytes(style_attr.len())?;
        }
        let offset = child
            .attribute("offset")
            .map(parse_stop_offset)
            .unwrap_or(0.0);

        // stop-color and stop-opacity can be in either style attr or presentation attr
        let get_stop = |prop: &str| -> Option<String> {
            if let Some(style_attr) = child.attribute("style") {
                for decl in style_attr.split(';') {
                    let mut parts = decl.splitn(2, ':');
                    if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                        if k.trim().eq_ignore_ascii_case(prop) {
                            return Some(v.trim().to_string());
                        }
                    }
                }
            }
            child.attribute(prop).map(str::to_string)
        };

        let color_str = get_stop("stop-color").unwrap_or_else(|| "black".to_string());
        let mut color = parse_color(&color_str).unwrap_or(Color::BLACK);
        let opacity: f32 = get_stop("stop-opacity")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(1.0);
        color.a = opacity.clamp(0.0, 1.0);

        stops.push(GradientStop::new(offset, color));
    }
    Ok(stops)
}

fn parse_stop_offset(s: &str) -> f32 {
    let s = s.trim();
    if s.ends_with('%') {
        s[..s.len() - 1].parse::<f32>().unwrap_or(0.0) / 100.0
    } else {
        s.parse().unwrap_or(0.0)
    }
}

fn parse_gradient_units(node: &roxmltree::Node) -> GradientUnits {
    match node
        .attribute("gradientUnits")
        .unwrap_or("objectBoundingBox")
    {
        "userSpaceOnUse" => GradientUnits::UserSpaceOnUse,
        _ => GradientUnits::ObjectBoundingBox,
    }
}

/// Parse a gradient coordinate value.
/// - `objectBoundingBox` (default): values are 0..1 fractions
/// - `userSpaceOnUse`: values are literal user-space coords
fn parse_percentage_or_number(s: &str, _units: GradientUnits) -> f64 {
    // Coordinates are stored as fractions [0..1] for OBB or user-space values for USoU.
    // The caller decides how to interpret them via build_gradient.
    let s = s.trim();
    if s.ends_with('%') {
        s[..s.len() - 1].parse::<f64>().unwrap_or(0.0) / 100.0
    } else {
        s.parse().unwrap_or(0.0)
    }
}

// ─── Internal style types ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum SvgPaint {
    None,
    Color(Color),
    /// Reference to a gradient in the defs
    GradientRef(String),
}

#[derive(Debug, Clone)]
struct ComputedStyle {
    fill: SvgPaint,
    fill_opacity: f32,
    stroke: SvgPaint,
    stroke_opacity: f32,
    stroke_width: f64,
    stroke_linecap: LineCap,
    stroke_linejoin: LineJoin,
    stroke_miterlimit: f64,
    stroke_dasharray: Vec<f64>,
    stroke_dashoffset: f64,
    /// Element's own opacity (not composed from parents — the renderer stacks these)
    opacity: f32,
    display: bool,
    visibility: bool,
    font_family: String,
    font_size: f64,
    font_weight: u16,
    text_anchor: TextAlign,
    /// CSS `mix-blend-mode`. Not inherited — reset per element in `resolve_style`.
    blend_mode: BlendMode,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        Self {
            fill: SvgPaint::Color(Color::BLACK),
            fill_opacity: 1.0,
            stroke: SvgPaint::None,
            stroke_opacity: 1.0,
            stroke_width: 1.0,
            stroke_linecap: LineCap::Butt,
            stroke_linejoin: LineJoin::Miter,
            stroke_miterlimit: 4.0,
            stroke_dasharray: vec![],
            stroke_dashoffset: 0.0,
            opacity: 1.0,
            display: true,
            visibility: true,
            font_family: "sans-serif".to_string(),
            font_size: 16.0,
            font_weight: 400,
            text_anchor: TextAlign::Left,
            blend_mode: BlendMode::Normal,
        }
    }
}

// ─── Recursive element import ─────────────────────────────────────────────────

fn import_element(
    node: &roxmltree::Node,
    doc: &mut Document,
    layer_id: LayerId,
    parent_style: &ComputedStyle,
    ctx: &SvgContext,
    state: &mut ImportState,
    depth: usize,
) -> Result<Option<Uuid>, ImportError> {
    state.ensure_depth(depth)?;
    let tag = node.tag_name().name();

    let style = resolve_style(node, parent_style, ctx, state)?;

    if !style.display || !style.visibility {
        return Ok(None);
    }

    let local_transform = node
        .attribute("transform")
        .map(parse_transform_attr)
        .unwrap_or(Transform::IDENTITY);

    match tag {
        "g" => {
            let mut children = Vec::new();
            for child in node.children() {
                if !child.is_element() {
                    continue;
                }
                if matches!(
                    child.tag_name().name(),
                    "defs" | "metadata" | "title" | "desc"
                ) {
                    continue;
                }
                if let Some(child_id) = import_element(
                    &child,
                    doc,
                    layer_id,
                    &style,
                    ctx,
                    state,
                    depth.saturating_add(1),
                )? {
                    children.push(child_id);
                }
            }
            if children.is_empty() {
                return Ok(None);
            }
            let id = Uuid::new_v4();
            let name = node.attribute("id").unwrap_or("Group").to_string();
            doc.nodes.insert(
                id,
                SceneNode {
                    id,
                    name,
                    layer_id,
                    kind: SceneNodeKind::Group(GroupNode {
                        children,
                        clip_children: false,
                        clip_node_id: None,
                        blend_spine_id: None,
                        live_boolean: None,
                    }),
                    transform: local_transform,
                    opacity: style.opacity,
                    visible: true,
                    locked: false,
                    blend_mode: style.blend_mode,
                    tags: vec![],
                    prompt_history: vec![],
                    outer_glow: Default::default(),
                    inner_glow: Default::default(),
                    gaussian_glow: Default::default(),
                    drop_shadow: Default::default(),
                    object_blur: Default::default(),
                    feather: Default::default(),
                    effects: Vec::new(),
                    export_spec: None,
                    symbol_ref: None,
                    symbol_fill_override: None,
                    symbol_stroke_override: None,
                },
            );
            Ok(Some(id))
        }

        "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" => {
            let Some(d) = element_to_path_d(node, state)? else {
                return Ok(None);
            };
            let Some(path_data) = bounded_path_data(&d, state)? else {
                return Ok(None);
            };

            // For objectBoundingBox gradients we need the shape's bounds
            let bbox = path_data.bounding_box();
            let fill = resolve_fill(&style, ctx, bbox);
            let stroke = resolve_stroke(&style, ctx, bbox);

            let id = Uuid::new_v4();
            let name = node.attribute("id").unwrap_or(tag).to_string();
            doc.nodes.insert(
                id,
                SceneNode {
                    id,
                    name,
                    layer_id,
                    kind: SceneNodeKind::Path(PathNode {
                        path_data,
                        fill,
                        stroke,
                        is_compound: false,
                    }),
                    transform: local_transform,
                    opacity: style.opacity,
                    visible: true,
                    locked: false,
                    blend_mode: style.blend_mode,
                    tags: vec![],
                    prompt_history: vec![],
                    outer_glow: Default::default(),
                    inner_glow: Default::default(),
                    gaussian_glow: Default::default(),
                    drop_shadow: Default::default(),
                    object_blur: Default::default(),
                    feather: Default::default(),
                    effects: Vec::new(),
                    export_spec: None,
                    symbol_ref: None,
                    symbol_fill_override: None,
                    symbol_stroke_override: None,
                },
            );
            Ok(Some(id))
        }

        "text" => {
            let content = collect_text_content(node, state)?;
            if content.trim().is_empty() {
                return Ok(None);
            }
            let x = parse_length(node.attribute("x").unwrap_or("0"));
            let y = parse_length(node.attribute("y").unwrap_or("0"));
            let combined_transform = if x != 0.0 || y != 0.0 {
                Transform::translate(x, y).then(&local_transform)
            } else {
                local_transform
            };

            let id = Uuid::new_v4();
            let name = node.attribute("id").unwrap_or("Text").to_string();
            doc.nodes.insert(
                id,
                SceneNode {
                    id,
                    name,
                    layer_id,
                    kind: SceneNodeKind::Text(TextNode {
                        content: content.trim().to_string(),
                        font_family: style.font_family.clone(),
                        font_size: style.font_size,
                        font_weight: style.font_weight,
                        fill: resolve_fill(&style, ctx, None),
                        stroke: resolve_stroke(&style, ctx, None),
                        align: style.text_anchor,
                        line_height: 1.2,
                        letter_spacing: 0.0,
                        path_spine_id: None,
                        path_offset: 0.0,
                        vertical: false,
                        area_path_id: None,
                        variable_binding: None,
                        font_style: crate::node::FontStyle::Normal,
                        next_frame: None,
                        prev_frame: None,
                        opentype_features: Vec::new(),
                        text_decoration: String::new(),
                        paragraph_spacing_before: 0.0,
                        paragraph_spacing_after: 0.0,
                        text_indent: 0.0,
                        tab_stops: Vec::new(),
                        baseline_shift: 0.0,
                        script_position: crate::node::ScriptPosition::Normal,
                    }),
                    transform: combined_transform,
                    opacity: style.opacity,
                    visible: true,
                    locked: false,
                    blend_mode: style.blend_mode,
                    tags: vec![],
                    prompt_history: vec![],
                    outer_glow: Default::default(),
                    inner_glow: Default::default(),
                    gaussian_glow: Default::default(),
                    drop_shadow: Default::default(),
                    object_blur: Default::default(),
                    feather: Default::default(),
                    effects: Vec::new(),
                    export_spec: None,
                    symbol_ref: None,
                    symbol_fill_override: None,
                    symbol_stroke_override: None,
                },
            );
            Ok(Some(id))
        }

        _ => Ok(None),
    }
}

// ─── Style resolution ─────────────────────────────────────────────────────────

/// Resolve the computed style for `node`, inheriting from `parent`.
/// Priority (highest to lowest): inline `style` attr → CSS classes → presentation attrs → inherited.
fn resolve_style(
    node: &roxmltree::Node,
    parent: &ComputedStyle,
    ctx: &SvgContext,
    state: &mut ImportState,
) -> Result<ComputedStyle, ImportError> {
    let mut s = parent.clone();
    // Each node has its own opacity and blend mode unless it declares one
    // (neither is inherited from the parent group).
    s.opacity = 1.0;
    s.blend_mode = BlendMode::Normal;

    // Build a merged property map from (lowest to highest priority):
    // 1. Matching element-type rules (e.g. `path { ... }`)
    // 2. Matching class rules (e.g. `.foo { ... }`)
    // 3. Presentation attributes (e.g. `fill="red"`)
    // 4. Inline style attribute (e.g. `style="fill:red"`)

    let mut merged: HashMap<String, String> = HashMap::new();

    // 1. Element-type CSS rule
    let tag = node.tag_name().name();
    if let Some(props) = ctx.css_rules.get(tag) {
        for (k, v) in props {
            merged.insert(k.clone(), v.clone());
        }
    }

    // 2. CSS class rules (may be multiple classes)
    if let Some(class_attr) = node.attribute("class") {
        for class_name in class_attr.split_whitespace() {
            let selector = format!(".{class_name}");
            if let Some(props) = ctx.css_rules.get(selector.as_str()) {
                for (k, v) in props {
                    merged.insert(k.clone(), v.clone());
                }
            }
        }
    }

    // 3. Presentation attributes
    for attr in node.attributes() {
        let name = attr.name().to_lowercase();
        // Only known SVG presentation attributes
        if is_presentation_attr(&name) {
            merged
                .entry(name)
                .or_insert_with(|| attr.value().to_string());
        }
    }

    // 4. Inline style — highest priority, overrides everything
    if let Some(style_str) = node.attribute("style") {
        state.add_css_bytes(style_str.len())?;
        for decl in style_str.split(';') {
            let decl = decl.trim();
            if decl.is_empty() {
                continue;
            }
            let mut kv = decl.splitn(2, ':');
            if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
                let k = k.trim().to_lowercase();
                merged.insert(k, v.trim().to_string());
            }
        }
    }

    // Apply the merged map to our style struct
    apply_props(&merged, &mut s);
    Ok(s)
}

fn is_presentation_attr(name: &str) -> bool {
    matches!(
        name,
        "fill"
            | "fill-opacity"
            | "fill-rule"
            | "stroke"
            | "stroke-opacity"
            | "stroke-width"
            | "stroke-linecap"
            | "stroke-linejoin"
            | "stroke-miterlimit"
            | "stroke-dasharray"
            | "stroke-dashoffset"
            | "opacity"
            | "display"
            | "visibility"
            | "font-family"
            | "font-size"
            | "font-weight"
            | "text-anchor"
            | "color"
    )
}

fn apply_props(props: &HashMap<String, String>, s: &mut ComputedStyle) {
    if let Some(v) = props.get("mix-blend-mode") {
        if let Some(mode) = BlendMode::from_css(v) {
            s.blend_mode = mode;
        }
    }
    if let Some(v) = props.get("display") {
        if v.trim() == "none" {
            s.display = false;
        }
    }
    if let Some(v) = props.get("visibility") {
        let v = v.trim();
        if v == "hidden" || v == "collapse" {
            s.visibility = false;
        } else if v == "visible" {
            s.visibility = true;
        }
    }
    if let Some(v) = props.get("fill") {
        s.fill = parse_paint(v);
    }
    if let Some(v) = props.get("fill-opacity") {
        let v = v.trim();
        if let Ok(op) = v.trim_end_matches('%').parse::<f32>() {
            s.fill_opacity = if v.ends_with('%') { op / 100.0 } else { op }.clamp(0.0, 1.0);
        }
    }
    if let Some(v) = props.get("stroke") {
        s.stroke = parse_paint(v);
    }
    if let Some(v) = props.get("stroke-opacity") {
        let v = v.trim();
        if let Ok(op) = v.trim_end_matches('%').parse::<f32>() {
            s.stroke_opacity = if v.ends_with('%') { op / 100.0 } else { op }.clamp(0.0, 1.0);
        }
    }
    if let Some(v) = props.get("stroke-width") {
        s.stroke_width = parse_length(v.trim());
    }
    if let Some(v) = props.get("stroke-linecap") {
        s.stroke_linecap = match v.trim() {
            "round" => LineCap::Round,
            "square" => LineCap::Square,
            _ => LineCap::Butt,
        };
    }
    if let Some(v) = props.get("stroke-linejoin") {
        s.stroke_linejoin = match v.trim() {
            "round" => LineJoin::Round,
            "bevel" => LineJoin::Bevel,
            _ => LineJoin::Miter,
        };
    }
    if let Some(v) = props.get("stroke-miterlimit") {
        if let Ok(ml) = v.trim().parse::<f64>() {
            s.stroke_miterlimit = ml;
        }
    }
    if let Some(v) = props.get("stroke-dasharray") {
        let v = v.trim();
        if v == "none" {
            s.stroke_dasharray = vec![];
        } else {
            s.stroke_dasharray = v
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|p| !p.is_empty())
                .filter_map(|p| p.parse().ok())
                .collect();
        }
    }
    if let Some(v) = props.get("stroke-dashoffset") {
        s.stroke_dashoffset = parse_length(v.trim());
    }
    if let Some(v) = props.get("opacity") {
        let v = v.trim();
        if let Ok(op) = v.trim_end_matches('%').parse::<f32>() {
            s.opacity = if v.ends_with('%') { op / 100.0 } else { op }.clamp(0.0, 1.0);
        }
    }
    if let Some(v) = props.get("font-family") {
        s.font_family = v.trim().trim_matches('\'').trim_matches('"').to_string();
    }
    if let Some(v) = props.get("font-size") {
        let fs = parse_length(v.trim());
        if fs > 0.0 {
            s.font_size = fs;
        }
    }
    if let Some(v) = props.get("font-weight") {
        s.font_weight = match v.trim() {
            "bold" | "bolder" => 700,
            "lighter" | "normal" => 400,
            other => other.parse().unwrap_or(400),
        };
    }
    if let Some(v) = props.get("text-anchor") {
        s.text_anchor = match v.trim() {
            "middle" => TextAlign::Center,
            "end" => TextAlign::Right,
            _ => TextAlign::Left,
        };
    }
}

// ─── Paint / gradient resolution ─────────────────────────────────────────────

fn parse_paint(s: &str) -> SvgPaint {
    let s = s.trim();
    if s == "none" {
        return SvgPaint::None;
    }
    if s == "transparent" {
        return SvgPaint::Color(Color::TRANSPARENT);
    }
    if let Some(rest) = s.strip_prefix("url(#") {
        let id = rest
            .trim_end_matches(')')
            .trim_end_matches('"')
            .trim_end_matches('\'');
        return SvgPaint::GradientRef(id.to_string());
    }
    if s.starts_with("url(") {
        return SvgPaint::None;
    }
    parse_color(s)
        .map(SvgPaint::Color)
        .unwrap_or(SvgPaint::None)
}

/// Resolve `SvgPaint::GradientRef` to a Photonic `Fill`, using the shape's
/// bounding box to convert `objectBoundingBox` coordinates to user space.
fn resolve_fill(style: &ComputedStyle, ctx: &SvgContext, bbox: Option<kurbo::Rect>) -> Fill {
    match &style.fill {
        SvgPaint::None => Fill::none(),
        SvgPaint::Color(c) => {
            let mut fill = Fill::solid(*c);
            fill.opacity = style.fill_opacity;
            fill
        }
        SvgPaint::GradientRef(id) => {
            if let Some(grad) = ctx.gradients.get(id) {
                if let Some(photon_grad) = build_gradient(grad, bbox, ctx.doc_width, ctx.doc_height)
                {
                    let mut fill = Fill::gradient(photon_grad);
                    fill.opacity = style.fill_opacity;
                    return fill;
                }
            }
            Fill::none()
        }
    }
}

fn resolve_stroke(style: &ComputedStyle, ctx: &SvgContext, _bbox: Option<kurbo::Rect>) -> Stroke {
    match &style.stroke {
        SvgPaint::None => Stroke::none(),
        SvgPaint::Color(c) => {
            let mut stroke = Stroke::solid(*c, style.stroke_width);
            stroke.opacity = style.stroke_opacity;
            stroke.line_cap = style.stroke_linecap;
            stroke.line_join = style.stroke_linejoin;
            stroke.miter_limit = style.stroke_miterlimit;
            stroke.dash_array = style.stroke_dasharray.clone();
            stroke.dash_offset = style.stroke_dashoffset;
            stroke
        }
        SvgPaint::GradientRef(id) => {
            // Gradient strokes: use first stop color as a fallback solid stroke
            if let Some(grad) = ctx.gradients.get(id) {
                if let Some(stop) = grad.stops.first() {
                    let mut stroke = Stroke::solid(stop.color, style.stroke_width);
                    stroke.opacity = style.stroke_opacity;
                    return stroke;
                }
            }
            Stroke::none()
        }
    }
}

/// Convert a parsed SVG gradient into a Photonic [`Gradient`].
fn build_gradient(
    grad: &ParsedGradient,
    bbox: Option<kurbo::Rect>,
    doc_width: f64,
    doc_height: f64,
) -> Option<Gradient> {
    if grad.stops.is_empty() {
        return None;
    }

    let to_user = |frac_x: f64, frac_y: f64| -> (f64, f64) {
        match (grad.units, bbox) {
            (GradientUnits::ObjectBoundingBox, Some(bb)) => (
                bb.x0 + frac_x * (bb.x1 - bb.x0),
                bb.y0 + frac_y * (bb.y1 - bb.y0),
            ),
            (GradientUnits::ObjectBoundingBox, None) => {
                // No bbox — scale to document size as best-effort
                (frac_x * doc_width, frac_y * doc_height)
            }
            (GradientUnits::UserSpaceOnUse, _) => (frac_x, frac_y),
        }
    };

    let stops = grad.stops.clone();
    match grad.kind {
        ParsedGradKind::Linear { x1, y1, x2, y2 } => {
            let (ux1, uy1) = to_user(x1, y1);
            let (ux2, uy2) = to_user(x2, y2);
            Some(Gradient::linear(ux1, uy1, ux2, uy2, stops))
        }
        ParsedGradKind::Radial { cx, cy, r } => {
            let (ucx, ucy) = to_user(cx, cy);
            // Radius: scale by the average of bbox dimensions for OBB
            let ur = match (grad.units, bbox) {
                (GradientUnits::ObjectBoundingBox, Some(bb)) => {
                    r * ((bb.x1 - bb.x0) + (bb.y1 - bb.y0)) * 0.5
                }
                (GradientUnits::ObjectBoundingBox, None) => r * (doc_width + doc_height) * 0.5,
                (GradientUnits::UserSpaceOnUse, _) => r,
            };
            Some(Gradient::radial(ucx, ucy, ur, stops))
        }
    }
}

// ─── Color parsing ────────────────────────────────────────────────────────────

fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    if s.starts_with('#') {
        let hex = &s[1..];
        return match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()? as f32 / 255.0;
                let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()? as f32 / 255.0;
                let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()? as f32 / 255.0;
                Some(Color::rgb(r, g, b))
            }
            4 => {
                let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()? as f32 / 255.0;
                let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()? as f32 / 255.0;
                let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()? as f32 / 255.0;
                let a = u8::from_str_radix(&hex[3..4].repeat(2), 16).ok()? as f32 / 255.0;
                Some(Color::new(r, g, b, a))
            }
            6 => Color::from_hex(hex),
            8 => Color::from_hex(hex),
            _ => None,
        };
    }

    if s.starts_with("rgb(") || s.starts_with("rgba(") {
        let inner = s
            .trim_start_matches("rgba(")
            .trim_start_matches("rgb(")
            .trim_end_matches(')');
        let parts: Vec<&str> = inner
            .split(|c: char| c == ',' || (c.is_whitespace() && !inner.contains(',')))
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        let parse_ch = |p: &str| -> f32 {
            if p.ends_with('%') {
                p.trim_end_matches('%').parse::<f32>().unwrap_or(0.0) / 100.0
            } else {
                p.parse::<f32>().unwrap_or(0.0) / 255.0
            }
        };
        let r = parts.first().map(|p| parse_ch(p)).unwrap_or(0.0);
        let g = parts.get(1).map(|p| parse_ch(p)).unwrap_or(0.0);
        let b = parts.get(2).map(|p| parse_ch(p)).unwrap_or(0.0);
        let a = parts
            .get(3)
            .map(|p| {
                if p.ends_with('%') {
                    p.trim_end_matches('%').parse::<f32>().unwrap_or(100.0) / 100.0
                } else {
                    p.parse::<f32>().unwrap_or(1.0)
                }
            })
            .unwrap_or(1.0);
        return Some(Color::new(
            r.clamp(0.0, 1.0),
            g.clamp(0.0, 1.0),
            b.clamp(0.0, 1.0),
            a.clamp(0.0, 1.0),
        ));
    }

    // Named CSS colors
    Some(match s {
        "black" => Color::BLACK,
        "white" => Color::WHITE,
        "red" => Color::rgb(1.0, 0.0, 0.0),
        "green" => Color::rgb(0.0, 0.502, 0.0),
        "lime" => Color::rgb(0.0, 1.0, 0.0),
        "blue" => Color::rgb(0.0, 0.0, 1.0),
        "yellow" => Color::rgb(1.0, 1.0, 0.0),
        "cyan" | "aqua" => Color::rgb(0.0, 1.0, 1.0),
        "magenta" | "fuchsia" => Color::rgb(1.0, 0.0, 1.0),
        "orange" => Color::rgb(1.0, 0.647, 0.0),
        "pink" => Color::rgb(1.0, 0.753, 0.796),
        "purple" => Color::rgb(0.502, 0.0, 0.502),
        "gray" | "grey" => Color::rgb(0.502, 0.502, 0.502),
        "silver" => Color::rgb(0.753, 0.753, 0.753),
        "maroon" => Color::rgb(0.502, 0.0, 0.0),
        "navy" => Color::rgb(0.0, 0.0, 0.502),
        "olive" => Color::rgb(0.502, 0.502, 0.0),
        "teal" => Color::rgb(0.0, 0.502, 0.502),
        "darkgray" | "darkgrey" => Color::rgb(0.663, 0.663, 0.663),
        "lightgray" | "lightgrey" => Color::rgb(0.827, 0.827, 0.827),
        "brown" => Color::rgb(0.647, 0.165, 0.165),
        "coral" => Color::rgb(1.0, 0.498, 0.314),
        "gold" => Color::rgb(1.0, 0.843, 0.0),
        "indigo" => Color::rgb(0.294, 0.0, 0.51),
        "violet" => Color::rgb(0.933, 0.51, 0.933),
        "tomato" => Color::rgb(1.0, 0.388, 0.278),
        "transparent" => Color::TRANSPARENT,
        _ => return None,
    })
}

// ─── Shape → SVG path data conversion ────────────────────────────────────────

fn element_to_path_d(
    node: &roxmltree::Node,
    state: &mut ImportState,
) -> Result<Option<String>, ImportError> {
    match node.tag_name().name() {
        "path" => Ok(node.attribute("d").map(str::to_string)),

        "rect" => {
            let x = parse_length(node.attribute("x").unwrap_or("0"));
            let y = parse_length(node.attribute("y").unwrap_or("0"));
            let w = parse_length(node.attribute("width").unwrap_or("0"));
            let h = parse_length(node.attribute("height").unwrap_or("0"));
            if w <= 0.0 || h <= 0.0 {
                return Ok(None);
            }
            let rx = node
                .attribute("rx")
                .filter(|v| *v != "auto")
                .map(parse_length);
            let ry = node
                .attribute("ry")
                .filter(|v| *v != "auto")
                .map(parse_length);
            let (rx, ry) = match (rx, ry) {
                (Some(r), None) | (None, Some(r)) => (r, r),
                (Some(rx), Some(ry)) => (rx, ry),
                (None, None) => (0.0, 0.0),
            };
            Ok(Some(rect_to_path_d(
                x,
                y,
                w,
                h,
                rx.min(w / 2.0),
                ry.min(h / 2.0),
            )))
        }

        "circle" => {
            let cx = parse_length(node.attribute("cx").unwrap_or("0"));
            let cy = parse_length(node.attribute("cy").unwrap_or("0"));
            let r = parse_length(node.attribute("r").unwrap_or("0"));
            if r <= 0.0 {
                return Ok(None);
            }
            Ok(Some(ellipse_to_path_d(cx, cy, r, r)))
        }

        "ellipse" => {
            let cx = parse_length(node.attribute("cx").unwrap_or("0"));
            let cy = parse_length(node.attribute("cy").unwrap_or("0"));
            let rx = parse_length(node.attribute("rx").unwrap_or("0"));
            let ry = parse_length(node.attribute("ry").unwrap_or("0"));
            if rx <= 0.0 || ry <= 0.0 {
                return Ok(None);
            }
            Ok(Some(ellipse_to_path_d(cx, cy, rx, ry)))
        }

        "line" => {
            let x1 = parse_length(node.attribute("x1").unwrap_or("0"));
            let y1 = parse_length(node.attribute("y1").unwrap_or("0"));
            let x2 = parse_length(node.attribute("x2").unwrap_or("0"));
            let y2 = parse_length(node.attribute("y2").unwrap_or("0"));
            Ok(Some(format!("M {x1} {y1} L {x2} {y2}")))
        }

        "polyline" => match node.attribute("points") {
            Some(points) => polyline_to_path_d(points, false, state),
            None => Ok(None),
        },
        "polygon" => match node.attribute("points") {
            Some(points) => polyline_to_path_d(points, true, state),
            None => Ok(None),
        },

        _ => Ok(None),
    }
}

fn bounded_path_data(d: &str, state: &mut ImportState) -> Result<Option<PathData>, ImportError> {
    let parsed = match BezPath::from_svg(d) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    let point_count = parsed
        .elements()
        .iter()
        .filter(|element| !matches!(element, PathEl::ClosePath))
        .count();
    state.add_path_points(point_count)?;
    Ok(PathData::from_svg(d).ok())
}

fn rect_to_path_d(x: f64, y: f64, w: f64, h: f64, rx: f64, ry: f64) -> String {
    if rx <= 0.0 && ry <= 0.0 {
        format!("M {x} {y} H {} V {} H {x} Z", x + w, y + h)
    } else {
        format!(
            "M {x1} {y} H {x2} A {rx} {ry} 0 0 1 {x3} {y1} \
             V {y2} A {rx} {ry} 0 0 1 {x2} {y3} \
             H {x1} A {rx} {ry} 0 0 1 {x} {y2} \
             V {y1} A {rx} {ry} 0 0 1 {x1} {y} Z",
            x1 = x + rx,
            x2 = x + w - rx,
            x3 = x + w,
            y1 = y + ry,
            y2 = y + h - ry,
            y3 = y + h,
        )
    }
}

fn ellipse_to_path_d(cx: f64, cy: f64, rx: f64, ry: f64) -> String {
    format!(
        "M {} {cy} A {rx} {ry} 0 0 1 {} {cy} A {rx} {ry} 0 0 1 {} {cy} Z",
        cx - rx,
        cx + rx,
        cx - rx,
    )
}

fn polyline_to_path_d(
    points_attr: &str,
    close: bool,
    state: &mut ImportState,
) -> Result<Option<String>, ImportError> {
    // Check the numeric point count before building the coordinate vector so a
    // wide polyline cannot force an oversized temporary allocation.
    let coordinate_count = points_attr
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<f64>().ok())
        .count();
    if coordinate_count < 4 || coordinate_count % 2 != 0 {
        return Ok(None);
    }
    state.ensure_path_points(coordinate_count / 2)?;

    let coords: Vec<f64> = points_attr
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();

    if coords.len() < 4 || coords.len() % 2 != 0 {
        return Ok(None);
    }

    let mut d = format!("M {} {}", coords[0], coords[1]);
    for i in (2..coords.len()).step_by(2) {
        d.push_str(&format!(" L {} {}", coords[i], coords[i + 1]));
    }
    if close {
        d.push_str(" Z");
    }
    Ok(Some(d))
}

// ─── Text content collection ──────────────────────────────────────────────────

fn collect_text_content(
    node: &roxmltree::Node,
    state: &mut ImportState,
) -> Result<String, ImportError> {
    collect_descendant_text(*node, state, ImportLimit::TextBytes)
}

fn collect_descendant_text(
    node: roxmltree::Node<'_, '_>,
    state: &mut ImportState,
    limit: ImportLimit,
) -> Result<String, ImportError> {
    let mut out = String::new();
    let mut pending: Vec<roxmltree::Node<'_, '_>> = node.children().rev().collect();
    while let Some(child) = pending.pop() {
        if child.is_text() {
            if let Some(text) = child.text() {
                match limit {
                    ImportLimit::TextBytes => state.add_text_bytes(text.len())?,
                    ImportLimit::CssBytes => state.add_css_bytes(text.len())?,
                    _ => unreachable!("text collection uses a text or CSS limit"),
                }
                out.push_str(text);
            }
        } else {
            pending.extend(child.children().rev());
        }
    }
    Ok(out)
}

// ─── Transform attribute parsing ─────────────────────────────────────────────

fn parse_transform_attr(s: &str) -> Transform {
    let mut result = Transform::IDENTITY;
    let mut remaining = s.trim();
    loop {
        remaining = remaining.trim_start();
        if remaining.is_empty() {
            break;
        }
        let paren = match remaining.find('(') {
            Some(i) => i,
            None => break,
        };
        let name = remaining[..paren].trim();
        remaining = &remaining[paren + 1..];

        let end_paren = match remaining.find(')') {
            Some(i) => i,
            None => break,
        };
        let args_str = &remaining[..end_paren];
        remaining = &remaining[end_paren + 1..];

        let args: Vec<f64> = args_str
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();

        let t = match name {
            "matrix" if args.len() == 6 => {
                Transform::new(args[0], args[1], args[2], args[3], args[4], args[5])
            }
            "translate" => {
                let tx = args.first().copied().unwrap_or(0.0);
                let ty = args.get(1).copied().unwrap_or(0.0);
                Transform::translate(tx, ty)
            }
            "scale" => {
                let sx = args.first().copied().unwrap_or(1.0);
                let sy = args.get(1).copied().unwrap_or(sx);
                Transform::scale(sx, sy)
            }
            "rotate" => {
                let angle = args.first().copied().unwrap_or(0.0).to_radians();
                let cx = args.get(1).copied().unwrap_or(0.0);
                let cy = args.get(2).copied().unwrap_or(0.0);
                if cx == 0.0 && cy == 0.0 {
                    Transform::rotate(angle)
                } else {
                    Transform::rotate_around(angle, cx, cy)
                }
            }
            "skewX" => {
                let angle = args.first().copied().unwrap_or(0.0).to_radians();
                Transform::new(1.0, 0.0, angle.tan(), 1.0, 0.0, 0.0)
            }
            "skewY" => {
                let angle = args.first().copied().unwrap_or(0.0).to_radians();
                Transform::new(1.0, angle.tan(), 0.0, 1.0, 0.0, 0.0)
            }
            _ => Transform::IDENTITY,
        };

        result = result.then(&t);
    }
    result
}

// ─── Dimension / length parsing ───────────────────────────────────────────────

fn parse_viewport(root: &roxmltree::Node) -> (f64, f64) {
    if let Some(vb) = root.attribute("viewBox") {
        let nums: Vec<f64> = vb
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        if nums.len() >= 4 && nums[2] > 0.0 && nums[3] > 0.0 {
            return (nums[2], nums[3]);
        }
    }
    let w = root.attribute("width").map(parse_length).unwrap_or(800.0);
    let h = root.attribute("height").map(parse_length).unwrap_or(600.0);
    (w.max(1.0), h.max(1.0))
}

/// Parse an SVG length value to logical pixels (96 dpi).
pub(crate) fn parse_length(s: &str) -> f64 {
    let s = s.trim();
    if s.ends_with("px") {
        s[..s.len() - 2].parse().unwrap_or(0.0)
    } else if s.ends_with("pt") {
        s[..s.len() - 2].parse::<f64>().unwrap_or(0.0) * (96.0 / 72.0)
    } else if s.ends_with("mm") {
        s[..s.len() - 2].parse::<f64>().unwrap_or(0.0) * (96.0 / 25.4)
    } else if s.ends_with("cm") {
        s[..s.len() - 2].parse::<f64>().unwrap_or(0.0) * (96.0 / 2.54)
    } else if s.ends_with("in") {
        s[..s.len() - 2].parse::<f64>().unwrap_or(0.0) * 96.0
    } else if s.ends_with("rem") {
        s[..s.len() - 3].parse::<f64>().unwrap_or(0.0) * 16.0
    } else if s.ends_with("em") {
        s[..s.len() - 2].parse::<f64>().unwrap_or(0.0) * 16.0
    } else {
        s.parse().unwrap_or(0.0)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FillKind;

    /// Collect every solid fill color found among a document's nodes.
    fn solid_fills(doc: &Document) -> Vec<Color> {
        doc.nodes
            .values()
            .filter_map(|n| match &n.kind {
                SceneNodeKind::Path(p) => match p.fill.kind {
                    FillKind::Solid(c) => Some(c),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn approx(c: Color, hex: &str) -> bool {
        let expected = Color::from_hex(hex).unwrap();
        (c.r - expected.r).abs() < 1e-3
            && (c.g - expected.g).abs() < 1e-3
            && (c.b - expected.b).abs() < 1e-3
    }

    /// Regression test: Illustrator nests the `<style>` block (which defines
    /// `.cls-N { fill: #... }`) inside `<defs>`. The importer must resolve those
    /// class-based fills, not fall back to the default black fill.
    #[test]
    fn css_class_fill_nested_in_defs_is_resolved() {
        let svg = r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
  <defs>
    <style>
      .cls-1 { fill: #3d5ba5; }
      .cls-2 { fill: #5d4096; }
    </style>
  </defs>
  <g>
    <path class="cls-1" d="M0,0 L10,0 L10,10 Z"/>
    <polygon class="cls-2" points="20,20 30,20 30,30"/>
  </g>
</svg>"#;

        let doc = import_svg(svg).expect("import should succeed");
        let fills = solid_fills(&doc);
        assert_eq!(fills.len(), 2, "expected two filled shapes");

        // Neither shape should have fallen back to the default black fill.
        assert!(
            !fills.iter().any(|c| approx(*c, "000000")),
            "a shape imported as black — CSS class fill was not resolved: {fills:?}"
        );
        assert!(
            fills.iter().any(|c| approx(*c, "3d5ba5")),
            "missing .cls-1 fill #3d5ba5: {fills:?}"
        );
        assert!(
            fills.iter().any(|c| approx(*c, "5d4096")),
            "missing .cls-2 fill #5d4096: {fills:?}"
        );
    }

    #[test]
    fn standard_svg_doctype_is_ignored_without_external_resolution() {
        let svg = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
  <path d="M0 0 L10 0 L10 10 Z"/>
</svg>"#;

        let doc = import_svg(svg).expect("standard external SVG doctype should be accepted");
        assert_eq!(doc.nodes.len(), 1);
    }

    fn assert_limit(svg: &str, expected: ImportLimit) {
        let error = import_svg(svg).expect_err("adversarial SVG should be rejected");
        assert!(
            matches!(error, ImportError::LimitExceeded { limit, .. } if limit == expected),
            "expected {expected:?} limit, got {error:?}"
        );
    }

    #[test]
    fn oversized_input_is_rejected_before_xml_parsing() {
        let svg = "x".repeat(MAX_SVG_INPUT_BYTES + 1);
        assert_limit(&svg, ImportLimit::InputBytes);
    }

    #[test]
    fn deeply_nested_groups_hit_the_depth_limit() {
        let mut svg =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">"#);
        for _ in 0..=MAX_SVG_DEPTH {
            svg.push_str("<g>");
        }
        svg.push_str(r#"<path d="M0 0 L1 0 L1 1 Z"/>"#);
        for _ in 0..=MAX_SVG_DEPTH {
            svg.push_str("</g>");
        }
        svg.push_str("</svg>");

        assert_limit(&svg, ImportLimit::Depth);
    }

    #[test]
    fn wide_element_tree_hits_the_element_limit() {
        let mut svg =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">"#);
        for _ in 0..MAX_SVG_ELEMENTS {
            svg.push_str(r#"<path d="M0 0 L1 0 L1 1 Z"/>"#);
        }
        svg.push_str("</svg>");

        assert_limit(&svg, ImportLimit::Elements);
    }

    #[test]
    fn oversized_text_and_css_are_rejected() {
        let text = "x".repeat(MAX_SVG_TEXT_BYTES + 1);
        let text_svg =
            format!(r#"<svg xmlns="http://www.w3.org/2000/svg"><text>{text}</text></svg>"#);
        assert_limit(&text_svg, ImportLimit::TextBytes);

        let css = "x".repeat(MAX_SVG_CSS_BYTES + 1);
        let css_svg =
            format!(r#"<svg xmlns="http://www.w3.org/2000/svg"><style>{css}</style></svg>"#);
        assert_limit(&css_svg, ImportLimit::CssBytes);
    }

    #[test]
    fn excessive_path_points_are_rejected() {
        let mut path = String::from("M0 0");
        for _ in 0..=MAX_SVG_PATH_POINTS {
            path.push_str("l0 0");
        }
        let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg"><path d="{path}"/></svg>"#);

        assert!(
            svg.len() <= MAX_SVG_INPUT_BYTES,
            "path fixture must exercise the path limit, not the input limit"
        );
        assert_limit(&svg, ImportLimit::PathPoints);
    }
}
