//! Native, deliberately bounded CSS-to-vector planning.
//!
//! This module does not depend on MCP.  It turns the portable part of the
//! CSS contract into a deterministic tree of editable box/ellipse paths; the
//! transport layer is responsible only for assigning ids and inserting it.

use crate::{Color, Fill, PathData, Stroke};
use serde::Serialize;
use std::collections::BTreeMap;

pub const CONTRACT_VERSION: u32 = 1;
pub const MAX_CSS_BYTES: usize = 256 * 1024;
pub const MAX_ELEMENTS: usize = 512;
/// Maximum nesting depth of the lowered virtual-element tree.
pub const MAX_DEPTH: usize = 64;
/// A virtual element emits one group and at most a background and border path.
pub const MAX_NODES: usize = MAX_ELEMENTS * 3;
/// Keep malformed or unsupported-input reporting bounded as well as lowering.
pub const MAX_DIAGNOSTICS: usize = 128;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct CssViewport {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct CssOrigin {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CssDiagnostic {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub selector: String,
    pub property: Option<String>,
    pub value: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CssVectorPath {
    pub name: String,
    pub path: PathData,
    pub fill: Fill,
    pub stroke: Stroke,
    pub opacity: f32,
}
#[derive(Debug, Clone)]
pub struct CssVectorGroup {
    pub name: String,
    pub children: Vec<CssVectorNode>,
    pub opacity: f32,
    pub provenance: String,
}
#[derive(Debug, Clone)]
pub enum CssVectorNode {
    Group(CssVectorGroup),
    Path(CssVectorPath),
}
#[derive(Debug, Clone)]
pub struct CssVectorPlan {
    pub roots: Vec<CssVectorNode>,
    pub diagnostics: Vec<CssDiagnostic>,
    pub bounds: (f64, f64, f64, f64),
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
struct Rule {
    selector: String,
    declarations: Vec<(String, String)>,
}

/// Compile supported CSS without a browser, DOM, SVG, or external resources.
/// Unsupported declarations are errors in strict mode and recorded omissions in
/// permissive mode.  The parser intentionally rejects constructs it cannot
/// lower faithfully instead of accepting and silently dropping them.
pub fn compile_css_vectors(
    css: &str,
    selector: Option<&str>,
    origin: CssOrigin,
    viewport: CssViewport,
    strict: bool,
) -> Result<CssVectorPlan, Vec<CssDiagnostic>> {
    let mut diagnostics = Vec::new();
    if css.len() > MAX_CSS_BYTES {
        return Err(vec![diag(
            "CSS_LIMIT",
            "CSS input exceeds 256 KiB",
            "",
            None,
            None,
        )]);
    }
    if !viewport.width.is_finite()
        || !viewport.height.is_finite()
        || viewport.width <= 0.0
        || viewport.height <= 0.0
    {
        return Err(vec![diag(
            "CSS_INVALID_VIEWPORT",
            "viewport dimensions must be finite and positive",
            "",
            None,
            None,
        )]);
    }
    let css = strip_comments(css);
    let rules = parse_rules(&css, &mut diagnostics);
    let rules = if rules.is_empty() && !css.trim().is_empty() {
        vec![Rule {
            selector: "CSS Object".into(),
            declarations: parse_declarations(&css, "CSS Object", &mut diagnostics),
        }]
    } else {
        rules
    };
    if rules.is_empty() {
        return Err(vec![diag(
            "CSS_EMPTY",
            "CSS contains no declarations",
            "",
            None,
            None,
        )]);
    }
    let mut elements: BTreeMap<String, Vec<&Rule>> = BTreeMap::new();
    // BTreeMap keeps cascade lookup deterministic, but CSS paint order is source
    // order, not lexical selector order. Keep that order separately.
    let mut element_order = BTreeMap::new();
    let mut element_sequence = Vec::new();
    for rule in &rules {
        for sel in rule
            .selector
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !supported_selector(sel) {
                record_diagnostic(
                    &mut diagnostics,
                    diag(
                        "CSS_UNSUPPORTED_SELECTOR",
                        "selector is not supported by the CSS-only object model",
                        sel,
                        None,
                        None,
                    ),
                );
                continue;
            }
            let key = sel.to_string();
            if !element_order.contains_key(&key) {
                let next_order = element_order.len();
                element_order.insert(key.clone(), next_order);
                if element_sequence.len() < MAX_ELEMENTS
                    && !key.contains('>')
                    && !key.contains("::")
                {
                    element_sequence.push(key.clone());
                }
            }
            elements.entry(key).or_default().push(rule);
        }
    }
    if elements.len() > MAX_ELEMENTS {
        record_diagnostic(
            &mut diagnostics,
            diag(
                "CSS_LIMIT",
                "CSS creates more than 512 virtual elements",
                "",
                None,
                None,
            ),
        );
        if strict {
            return Err(diagnostics);
        }
    }
    let selected: Vec<String> = match selector {
        Some(s) if elements.contains_key(s) => vec![s.to_string()],
        Some(s) => {
            record_diagnostic(
                &mut diagnostics,
                diag(
                    "CSS_UNKNOWN_SELECTOR",
                    "selector does not resolve to a virtual element",
                    s,
                    None,
                    None,
                ),
            );
            vec![]
        }
        None => element_sequence.clone(),
    };
    if selected.is_empty() {
        record_diagnostic(
            &mut diagnostics,
            diag(
                "CSS_NO_ROOT",
                "no virtual root could be selected",
                "",
                None,
                None,
            ),
        );
    }
    let child_index = index_direct_children(&elements, &element_order);
    let mut budget = LoweringBudget::new();
    let mut roots = Vec::new();
    for root in &selected {
        let Some(node) = build_element(
            root,
            &elements,
            &child_index,
            origin,
            viewport,
            &mut diagnostics,
            &mut budget,
            0,
            strict,
        )?
        else {
            break;
        };
        roots.push(CssVectorNode::Group(node));
    }
    if strict && diagnostics.iter().any(|d| d.severity == "error") {
        return Err(diagnostics);
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    visit_paths(&roots, &mut |p| {
        if let Some(b) = p.path.bounding_box() {
            min_x = min_x.min(b.x0);
            min_y = min_y.min(b.y0);
            max_x = max_x.max(b.x1);
            max_y = max_y.max(b.y1);
        }
    });
    let bounds = if min_x.is_finite() {
        (min_x, min_y, max_x - min_x, max_y - min_y)
    } else {
        (origin.x, origin.y, 0.0, 0.0)
    };
    let fingerprint = stable_fingerprint(&css, selector, origin, viewport);
    Ok(CssVectorPlan {
        roots,
        diagnostics,
        bounds,
        fingerprint,
    })
}

struct LoweringBudget {
    elements_remaining: usize,
    nodes_remaining: usize,
    depth_reported: bool,
    element_reported: bool,
    node_reported: bool,
}

impl LoweringBudget {
    fn new() -> Self {
        Self {
            elements_remaining: MAX_ELEMENTS,
            nodes_remaining: MAX_NODES,
            depth_reported: false,
            element_reported: false,
            node_reported: false,
        }
    }

    fn work_limit(&self) -> usize {
        self.elements_remaining.min(self.nodes_remaining)
    }

    fn report_depth(&mut self, diagnostics: &mut Vec<CssDiagnostic>, selector: &str) {
        if !self.depth_reported {
            record_diagnostic(
                diagnostics,
                diag(
                    "CSS_DEPTH_LIMIT",
                    &format!("CSS nesting exceeds {MAX_DEPTH} levels"),
                    selector,
                    None,
                    None,
                ),
            );
            self.depth_reported = true;
        }
    }

    fn report_element_limit(&mut self, diagnostics: &mut Vec<CssDiagnostic>, selector: &str) {
        if !self.element_reported {
            record_diagnostic(
                diagnostics,
                diag(
                    "CSS_LIMIT",
                    &format!("CSS lowering exceeds {MAX_ELEMENTS} virtual elements"),
                    selector,
                    None,
                    None,
                ),
            );
            self.element_reported = true;
        }
    }

    fn report_node_limit(&mut self, diagnostics: &mut Vec<CssDiagnostic>, selector: &str) {
        if !self.node_reported {
            record_diagnostic(
                diagnostics,
                diag(
                    "CSS_NODE_LIMIT",
                    &format!("CSS lowering exceeds {MAX_NODES} vector nodes"),
                    selector,
                    None,
                    None,
                ),
            );
            self.node_reported = true;
        }
    }
}

fn build_element(
    selector: &str,
    elements: &BTreeMap<String, Vec<&Rule>>,
    child_index: &BTreeMap<String, Vec<String>>,
    origin: CssOrigin,
    viewport: CssViewport,
    diagnostics: &mut Vec<CssDiagnostic>,
    budget: &mut LoweringBudget,
    depth: usize,
    strict: bool,
) -> Result<Option<CssVectorGroup>, Vec<CssDiagnostic>> {
    if depth >= MAX_DEPTH {
        budget.report_depth(diagnostics, selector);
        return if strict {
            Err(diagnostics.clone())
        } else {
            Ok(None)
        };
    }
    if budget.elements_remaining == 0 {
        budget.report_element_limit(diagnostics, selector);
        return if strict {
            Err(diagnostics.clone())
        } else {
            Ok(None)
        };
    }
    if budget.nodes_remaining == 0 {
        budget.report_node_limit(diagnostics, selector);
        return if strict {
            Err(diagnostics.clone())
        } else {
            Ok(None)
        };
    }
    budget.elements_remaining -= 1;
    budget.nodes_remaining -= 1;

    let mut props = BTreeMap::new();
    let mut background_declarations = Vec::new();
    if let Some(rules) = elements.get(selector) {
        for rule in rules {
            for (k, v) in &rule.declarations {
                props.insert(k.clone(), v.clone());
                if matches!(k.as_str(), "background" | "background-color") {
                    background_declarations.push((k.as_str(), v.as_str()));
                }
            }
        }
    }
    for (key, value) in &props {
        if !supported_property(key) {
            record_diagnostic(
                diagnostics,
                diag(
                    "CSS_UNSUPPORTED_PROPERTY",
                    &format!("{key} cannot be represented as editable paths"),
                    selector,
                    Some(key),
                    Some(value),
                ),
            );
        }
    }
    // Hidden CSS boxes must not become visible editable paths. Returning an
    // empty group also prevents descendants from being lowered: `display:none`
    // and `visibility:hidden` both suppress the element's rendered subtree.
    if css_element_hidden(&props) {
        return Ok(Some(CssVectorGroup {
            name: selector_name(selector),
            children: vec![],
            opacity: 1.0,
            provenance: selector.into(),
        }));
    }
    if let Some(value) = props.get("z-index") {
        let value = value.trim();
        if value != "auto" && value.parse::<i32>().is_err() {
            record_diagnostic(
                diagnostics,
                diag(
                    "CSS_INVALID_Z_INDEX",
                    "z-index must be an integer or auto",
                    selector,
                    Some("z-index"),
                    Some(value),
                ),
            );
        }
    }
    let width =
        length(props.get("width").map(String::as_str), viewport.width, 16.0).or_else(|| {
            length(props.get("right").map(String::as_str), viewport.width, 16.0).map(|r| {
                viewport.width
                    - r
                    - length(props.get("left").map(String::as_str), viewport.width, 16.0)
                        .unwrap_or(0.0)
            })
        });
    let height = length(
        props.get("height").map(String::as_str),
        viewport.height,
        16.0,
    )
    .or_else(|| {
        length(
            props.get("bottom").map(String::as_str),
            viewport.height,
            16.0,
        )
        .map(|b| {
            viewport.height
                - b
                - length(props.get("top").map(String::as_str), viewport.height, 16.0).unwrap_or(0.0)
        })
    });
    let (Some(width), Some(height)) = (width, height) else {
        record_diagnostic(
            diagnostics,
            diag(
                "CSS_UNRESOLVED_GEOMETRY",
                "width and height are required for a vectorizable box",
                selector,
                None,
                None,
            ),
        );
        return Ok(Some(CssVectorGroup {
            name: selector_name(selector),
            children: vec![],
            opacity: 1.0,
            provenance: selector.into(),
        }));
    };
    if !(width.is_finite()
        && height.is_finite()
        && width >= 0.0
        && height >= 0.0
        && width <= 1_000_000.0
        && height <= 1_000_000.0)
    {
        record_diagnostic(
            diagnostics,
            diag(
                "CSS_INVALID_GEOMETRY",
                "resolved dimensions are outside supported bounds",
                selector,
                None,
                None,
            ),
        );
    }
    let x = origin.x
        + length(
            props
                .get("left")
                .or_else(|| props.get("inset"))
                .map(String::as_str),
            viewport.width,
            16.0,
        )
        .unwrap_or(0.0);
    let y = origin.y
        + length(
            props
                .get("top")
                .or_else(|| props.get("inset"))
                .map(String::as_str),
            viewport.height,
            16.0,
        )
        .unwrap_or(0.0);
    let opacity = props
        .get("opacity")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    let radius = length(
        props.get("border-radius").map(String::as_str),
        width.min(height),
        16.0,
    )
    .unwrap_or(0.0)
    .min(width.min(height) / 2.0);
    let mut children = Vec::new();
    if let Some(fill) = background(&background_declarations, selector, diagnostics) {
        let path = if radius >= width.min(height) / 2.0 && (width - height).abs() < 1e-6 {
            PathData::ellipse(x + width / 2.0, y + height / 2.0, width / 2.0, height / 2.0)
        } else {
            PathData::rounded_rect(x, y, width, height, radius)
        };
        if budget.nodes_remaining == 0 {
            budget.report_node_limit(diagnostics, selector);
            if strict {
                return Err(diagnostics.clone());
            }
        } else {
            budget.nodes_remaining -= 1;
            children.push(CssVectorNode::Path(CssVectorPath {
                name: format!("{}/background", selector_name(selector)),
                path,
                fill,
                stroke: Stroke::none(),
                // Element opacity is applied by the containing group. Keeping
                // paths opaque avoids applying CSS opacity a second time after
                // scene-graph lowering (the renderer propagates group opacity to
                // every descendant path).
                opacity: 1.0,
            }));
        }
    }
    if let Some((color, border_width)) = border(&props, selector, diagnostics) {
        let path = PathData::rounded_rect(x, y, width, height, radius);
        if budget.nodes_remaining == 0 {
            budget.report_node_limit(diagnostics, selector);
            if strict {
                return Err(diagnostics.clone());
            }
        } else {
            budget.nodes_remaining -= 1;
            children.push(CssVectorNode::Path(CssVectorPath {
                name: format!("{}/border", selector_name(selector)),
                path,
                fill: Fill::none(),
                stroke: Stroke::solid(color, border_width),
                opacity: 1.0,
            }));
        }
    }
    let child_origin = CssOrigin { x, y };
    let child_viewport = CssViewport { width, height };
    let child_prefix = format!("{selector} > ");
    // Direct children and pseudo elements are ordered per the contract. The index
    // is built once so each lowered element does not rescan every CSS selector.
    for suffix in ["::before", ""] {
        if depth + 1 >= MAX_DEPTH {
            budget.report_depth(diagnostics, selector);
            if strict {
                return Err(diagnostics.clone());
            }
            break;
        }
        let work_limit = budget.work_limit();
        let direct_children: Vec<_> = child_index
            .get(selector)
            .into_iter()
            .flat_map(|children| children.iter())
            .filter(|child| {
                let Some(rest) = child.strip_prefix(&child_prefix) else {
                    return false;
                };
                if suffix.is_empty() {
                    !rest.contains("::")
                } else {
                    rest.ends_with(suffix)
                }
            })
            .take(work_limit)
            .cloned()
            .collect();
        for child in direct_children {
            let Some(child_node) = build_element(
                &child,
                elements,
                child_index,
                child_origin,
                child_viewport,
                diagnostics,
                budget,
                depth + 1,
                strict,
            )?
            else {
                break;
            };
            children.push(CssVectorNode::Group(child_node));
        }
    }
    let after_selector = format!("{selector}::after");
    if elements.contains_key(&after_selector) {
        if depth + 1 >= MAX_DEPTH {
            budget.report_depth(diagnostics, selector);
            if strict {
                return Err(diagnostics.clone());
            }
        } else if budget.work_limit() > 0 {
            if let Some(after_node) = build_element(
                &after_selector,
                elements,
                child_index,
                child_origin,
                child_viewport,
                diagnostics,
                budget,
                depth + 1,
                strict,
            )? {
                children.push(CssVectorNode::Group(after_node));
            }
        }
    }
    Ok(Some(CssVectorGroup {
        name: selector_name(selector),
        children,
        opacity,
        provenance: selector.into(),
    }))
}

fn index_direct_children(
    elements: &BTreeMap<String, Vec<&Rule>>,
    element_order: &BTreeMap<String, usize>,
) -> BTreeMap<String, Vec<String>> {
    let mut index: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for child in elements.keys() {
        let Some((parent, rest)) = child.rsplit_once(" > ") else {
            continue;
        };
        if rest.contains('>') || (!rest.ends_with("::before") && rest.contains("::")) {
            continue;
        }
        index
            .entry(parent.to_string())
            .or_default()
            .push(child.clone());
        let children = index.get_mut(parent).expect("parent index entry exists");
        if children.len() > MAX_ELEMENTS {
            let (worst, _) = children
                .iter()
                .enumerate()
                .max_by_key(|(_, child)| {
                    (
                        z_index(elements.get(*child)),
                        element_order.get(*child).copied().unwrap_or(usize::MAX),
                    )
                })
                .expect("children is non-empty");
            children.swap_remove(worst);
        }
    }
    for children in index.values_mut() {
        children.sort_by_key(|child| {
            (
                z_index(elements.get(child)),
                element_order.get(child).copied().unwrap_or(usize::MAX),
            )
        });
        children.truncate(MAX_ELEMENTS);
    }
    index
}

fn z_index(rules: Option<&Vec<&Rule>>) -> i32 {
    rules
        .into_iter()
        .flatten()
        .flat_map(|rule| rule.declarations.iter())
        .filter(|(property, _)| property == "z-index")
        .filter_map(|(_, value)| value.parse::<i32>().ok())
        .next_back()
        .unwrap_or(0)
}

fn supported_selector(s: &str) -> bool {
    !s.is_empty() && !s.contains(['+', '~', '[', ']', '*']) && !s.contains(':')
        || s.ends_with("::before")
        || s.ends_with("::after")
}
fn supported_property(k: &str) -> bool {
    matches!(
        k,
        "width"
            | "height"
            | "left"
            | "top"
            | "right"
            | "bottom"
            | "inset"
            | "background"
            | "background-color"
            | "border"
            | "border-color"
            | "border-width"
            | "border-radius"
            | "opacity"
            | "z-index"
            | "position"
            | "display"
            | "visibility"
            | "box-sizing"
    )
}

fn css_element_hidden(props: &BTreeMap<String, String>) -> bool {
    let first_token = |property: &str| {
        props
            .get(property)
            .and_then(|value| value.split_whitespace().next())
            .unwrap_or("")
    };
    first_token("display").eq_ignore_ascii_case("none")
        || matches!(
            first_token("visibility").to_ascii_lowercase().as_str(),
            "hidden" | "collapse"
        )
}
fn selector_name(s: &str) -> String {
    s.trim_matches('.')
        .trim_matches('#')
        .replace("::", "/")
        .replace(" > ", "/")
}
fn background(
    declarations: &[(&str, &str)],
    sel: &str,
    ds: &mut Vec<CssDiagnostic>,
) -> Option<Fill> {
    let (property, value) = declarations.last()?;
    match color(value) {
        Some(c) => Some(Fill::solid(c)),
        None => {
            record_diagnostic(
                ds,
                diag(
                    "CSS_UNSUPPORTED_PAINT",
                    "only solid background colors are currently vectorizable",
                    sel,
                    Some(property),
                    Some(value),
                ),
            );
            None
        }
    }
}
fn border(
    props: &BTreeMap<String, String>,
    sel: &str,
    ds: &mut Vec<CssDiagnostic>,
) -> Option<(Color, f64)> {
    if !props.keys().any(|property| {
        matches!(
            property.as_str(),
            "border" | "border-width" | "border-color"
        )
    }) {
        return None;
    }
    let shorthand = props.get("border");
    let shorthand_tokens = shorthand
        .map(|value| value.split_whitespace().collect::<Vec<_>>())
        .unwrap_or_default();
    let shorthand_widths: Vec<_> = shorthand_tokens
        .iter()
        .filter_map(|token| length(Some(token), 1.0, 16.0))
        .collect();
    if shorthand_widths.len() > 1 {
        ds.push(diag(
            "CSS_UNSUPPORTED_BORDER_WIDTH",
            "uniform border strokes do not support multiple width values",
            sel,
            Some("border"),
            shorthand.map(String::as_str),
        ));
    }
    let shorthand_width = if shorthand_widths.len() == 1 {
        Some(shorthand_widths[0])
    } else {
        None
    };
    let shorthand_color = shorthand_tokens.iter().find_map(|token| color(token));
    let width = props
        .get("border-width")
        .and_then(|value| {
            if value.split_whitespace().count() > 1 {
                ds.push(diag(
                    "CSS_UNSUPPORTED_BORDER_WIDTH",
                    "uniform border strokes do not support multiple width values",
                    sel,
                    Some("border-width"),
                    Some(value),
                ));
                None
            } else {
                length(Some(value), 1.0, 16.0)
            }
        })
        .or(shorthand_width)
        // In permissive mode, unsupported side-specific widths are ignored
        // after recording the diagnostic; a uniform shorthand may still
        // provide the width, otherwise the fallback is 1px.
        .unwrap_or(1.0);
    let c = props
        .get("border-color")
        .and_then(|value| color(value))
        .or(shorthand_color);
    if c.is_none() {
        let value = shorthand
            .map(String::as_str)
            .or_else(|| props.get("border-color").map(String::as_str));
        record_diagnostic(
            ds,
            diag(
                "CSS_UNSUPPORTED_BORDER",
                "border requires a supported solid color",
                sel,
                if shorthand.is_some() {
                    Some("border")
                } else {
                    Some("border-color")
                },
                value,
            ),
        );
    }
    c.map(|c| (c, width))
}
fn color(v: &str) -> Option<Color> {
    let v = v.trim();
    let expanded = if v.len() == 4 && v.starts_with('#') {
        format!(
            "#{}{}{}{}{}{}",
            &v[1..2],
            &v[1..2],
            &v[2..3],
            &v[2..3],
            &v[3..4],
            &v[3..4]
        )
    } else {
        v.to_string()
    };
    Color::from_hex(&expanded).or_else(|| match v.to_ascii_lowercase().as_str() {
        "white" => Some(Color::WHITE),
        "black" => Some(Color::BLACK),
        "transparent" => Some(Color::new(0.0, 0.0, 0.0, 0.0)),
        "red" => Color::from_hex("#ff0000"),
        "blue" => Color::from_hex("#0000ff"),
        "green" => Color::from_hex("#008000"),
        _ => None,
    })
}
fn length(v: Option<&str>, percent_base: f64, _em: f64) -> Option<f64> {
    let v = v?.trim();
    if v == "0" {
        return Some(0.0);
    }
    if let Some(p) = v.strip_suffix('%') {
        return p
            .trim()
            .parse::<f64>()
            .ok()
            .map(|n| n * percent_base / 100.0);
    }
    v.strip_suffix("px").unwrap_or(v).trim().parse::<f64>().ok()
}
fn parse_rules(css: &str, ds: &mut Vec<CssDiagnostic>) -> Vec<Rule> {
    let mut out = Vec::new();
    for part in css.split('}') {
        let Some((sel, body)) = part.split_once('{') else {
            continue;
        };
        if sel.trim().is_empty() {
            continue;
        };
        out.push(Rule {
            selector: sel.trim().into(),
            declarations: parse_declarations(body, sel.trim(), ds),
        });
    }
    out
}
fn parse_declarations(body: &str, sel: &str, ds: &mut Vec<CssDiagnostic>) -> Vec<(String, String)> {
    body.split(';')
        .filter_map(|d| {
            let (k, v) = d.split_once(':')?;
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            if k.is_empty() || v.is_empty() {
                record_diagnostic(
                    ds,
                    diag(
                        "CSS_MALFORMED_DECLARATION",
                        "declaration requires property and value",
                        sel,
                        Some(&k),
                        Some(&v),
                    ),
                );
                None
            } else {
                Some((k, v))
            }
        })
        .collect()
}
fn strip_comments(s: &str) -> String {
    let mut o = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        o.push_str(&rest[..i]);
        if let Some(j) = rest[i + 2..].find("*/") {
            rest = &rest[i + 2 + j + 2..]
        } else {
            return o;
        }
    }
    o.push_str(rest);
    o
}
fn diag(
    code: &str,
    message: &str,
    selector: &str,
    property: Option<&str>,
    value: Option<&str>,
) -> CssDiagnostic {
    CssDiagnostic {
        severity: "error".into(),
        code: code.into(),
        message: message.into(),
        selector: selector.into(),
        property: property.map(str::to_string),
        value: value.map(str::to_string),
    }
}

fn record_diagnostic(diagnostics: &mut Vec<CssDiagnostic>, diagnostic: CssDiagnostic) {
    if diagnostics.len() < MAX_DIAGNOSTICS {
        diagnostics.push(diagnostic);
    }
}
fn visit_paths(nodes: &[CssVectorNode], f: &mut impl FnMut(&CssVectorPath)) {
    for n in nodes {
        match n {
            CssVectorNode::Path(p) => f(p),
            CssVectorNode::Group(g) => visit_paths(&g.children, f),
        }
    }
}
fn stable_fingerprint(
    css: &str,
    selector: Option<&str>,
    origin: CssOrigin,
    viewport: CssViewport,
) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in format!(
        "{css}\0{:?}\0{:.9},{:.9},{:.9},{:.9}\0{}",
        selector, origin.x, origin.y, viewport.width, viewport.height, CONTRACT_VERSION
    )
    .bytes()
    {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3)
    }
    format!("cssv1-{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_count(nodes: &[CssVectorNode]) -> usize {
        nodes
            .iter()
            .map(|node| match node {
                CssVectorNode::Path(_) => 1,
                CssVectorNode::Group(group) => 1 + node_count(&group.children),
            })
            .sum()
    }

    fn group_count(nodes: &[CssVectorNode]) -> usize {
        nodes
            .iter()
            .map(|node| match node {
                CssVectorNode::Path(_) => 0,
                CssVectorNode::Group(group) => 1 + group_count(&group.children),
            })
            .sum()
    }

    #[test]
    fn lowers_box_and_border_deterministically() {
        let a=compile_css_vectors(".badge { width: 240px; height:96px; background:#6941c6; border:3px solid #fff; border-radius:24px; }",Some(".badge"),CssOrigin{x:100.,y:100.},CssViewport{width:512.,height:512.},true).unwrap();
        assert!((a.bounds.0 - 100.).abs() < 1e-9);
        assert!((a.bounds.1 - 100.).abs() < 1e-9);
        assert!((a.bounds.2 - 240.).abs() < 1e-9);
        assert!((a.bounds.3 - 96.).abs() < 1e-9);
        assert_eq!(a.fingerprint,compile_css_vectors(".badge { width: 240px; height:96px; background:#6941c6; border:3px solid #fff; border-radius:24px; }",Some(".badge"),CssOrigin{x:100.,y:100.},CssViewport{width:512.,height:512.},true).unwrap().fingerprint);
    }

    #[test]
    fn strict_mode_lowers_border_longhands() {
        let plan = compile_css_vectors(
            ".badge { width: 40px; height: 20px; border-width: 3px; border-color: #fff; }",
            Some(".badge"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        )
        .unwrap();
        let CssVectorNode::Group(group) = &plan.roots[0] else {
            panic!("root must be a group")
        };
        assert_eq!(group.children.len(), 1);
        assert!(matches!(group.children[0], CssVectorNode::Path(_)));
        assert!(plan.diagnostics.is_empty());
    }

    #[test]
    fn border_longhands_override_matching_shorthand_components() {
        let plan = compile_css_vectors(
            ".badge { width: 40px; height: 20px; border: 1px solid red; border-width: 4px; border-color: blue; }",
            Some(".badge"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        )
        .unwrap();
        let CssVectorNode::Group(group) = &plan.roots[0] else {
            panic!("root must be a group")
        };
        let CssVectorNode::Path(border) = &group.children[0] else {
            panic!("border must be a path")
        };
        assert_eq!(border.stroke.width, 4.0);
        assert_eq!(border.stroke.color, Color::from_hex("#0000ff").unwrap());
    }

    #[test]
    fn background_longhands_follow_css_source_order() {
        let background_then_color = compile_css_vectors(
            ".badge { width: 40px; height: 20px; background: red; background-color: blue; }",
            Some(".badge"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        )
        .unwrap();
        let color_then_background = compile_css_vectors(
            ".badge { width: 40px; height: 20px; background-color: blue; background: red; }",
            Some(".badge"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        )
        .unwrap();

        assert_eq!(
            lowered_background_fill(&background_then_color),
            Fill::solid(Color::BLUE)
        );
        assert_eq!(
            lowered_background_fill(&color_then_background),
            Fill::solid(Color::RED)
        );
        assert!(background_then_color.diagnostics.is_empty());
        assert!(color_then_background.diagnostics.is_empty());
    }

    #[test]
    fn solid_background_color_falls_back_from_unsupported_shorthand() {
        let plan = compile_css_vectors(
            ".badge { width: 40px; height: 20px; background: linear-gradient(red, blue); background-color: blue; }",
            Some(".badge"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        )
        .unwrap();

        assert_eq!(lowered_background_fill(&plan), Fill::solid(Color::BLUE));
        assert!(plan.diagnostics.is_empty());
    }

    fn lowered_background_fill(plan: &CssVectorPlan) -> Fill {
        let CssVectorNode::Group(group) = &plan.roots[0] else {
            panic!("root must be a group")
        };
        let CssVectorNode::Path(background) = &group.children[0] else {
            panic!("background must be a path")
        };
        background.fill.clone()
    }

    #[test]
    fn strict_mode_rejects_multi_value_border_width() {
        let result = compile_css_vectors(
            ".badge { width: 40px; height: 20px; border-width: 2px 4px; border-color: #fff; }",
            Some(".badge"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        );
        let Err(diagnostics) = result else {
            panic!("strict mode must reject side-specific border widths")
        };
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "CSS_UNSUPPORTED_BORDER_WIDTH"
                && diagnostic.property.as_deref() == Some("border-width")
                && diagnostic.value.as_deref() == Some("2px 4px")
        }));
    }

    #[test]
    fn permissive_mode_reports_and_falls_back_for_multi_value_border_width() {
        let plan = compile_css_vectors(
            ".badge { width: 40px; height: 20px; border-width: 2px 4px; border-color: #fff; }",
            Some(".badge"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            false,
        )
        .unwrap();
        let CssVectorNode::Group(group) = &plan.roots[0] else {
            panic!("root must be a group")
        };
        let CssVectorNode::Path(border) = &group.children[0] else {
            panic!("border must be a path")
        };
        assert_eq!(border.stroke.width, 1.0);
        assert!(plan.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "CSS_UNSUPPORTED_BORDER_WIDTH"
                && diagnostic.property.as_deref() == Some("border-width")
        }));
    }

    #[test]
    fn multi_value_border_shorthand_is_rejected_and_permissively_uses_one_pixel() {
        let css = ".badge { width: 40px; height: 20px; border: 2px 4px solid red; }";
        let result = compile_css_vectors(
            css,
            Some(".badge"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        );
        let Err(diagnostics) = result else {
            panic!("strict mode must reject multi-value border shorthand")
        };
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "CSS_UNSUPPORTED_BORDER_WIDTH"
                && diagnostic.property.as_deref() == Some("border")
        }));

        let plan = compile_css_vectors(
            css,
            Some(".badge"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            false,
        )
        .unwrap();
        let CssVectorNode::Group(group) = &plan.roots[0] else {
            panic!("root must be a group")
        };
        let CssVectorNode::Path(border) = &group.children[0] else {
            panic!("border must be a path")
        };
        assert_eq!(border.stroke.width, 1.0);
        assert!(plan.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "CSS_UNSUPPORTED_BORDER_WIDTH"
                && diagnostic.property.as_deref() == Some("border")
        }));
    }

    #[test]
    fn switch_tree_keeps_grandchildren_nested_relative_and_above_tracks() {
        let css = ".switch { width: 200px; height: 80px; left: 100px; top: 50px; }\
            .switch > .thumb { width: 30px; height: 30px; left: 150px; top: 20px; background: #fff; z-index: 2; border-radius: 50%; }\
            .switch > .track { width: 180px; height: 50px; left: 0px; top: 10px; background: #c89013; z-index: 1; border-radius: 25px; }\
            .switch > .track > .nested { width: 10px; height: 10px; left: 5px; top: 5px; background: #000; border-radius: 50%; }";
        let plan = compile_css_vectors(
            css,
            Some(".switch"),
            CssOrigin { x: 10.0, y: 20.0 },
            CssViewport {
                width: 500.0,
                height: 300.0,
            },
            true,
        )
        .unwrap();
        let CssVectorNode::Group(root) = &plan.roots[0] else {
            panic!("root must be a group")
        };
        // z-index wins over CSS source order: track is painted before thumb.
        let names: Vec<_> = root.children.iter().map(node_name).collect();
        assert_eq!(names, vec!["switch/.track", "switch/.thumb"]);
        let CssVectorNode::Group(track) = &root.children[0] else {
            panic!("track must be a group")
        };
        assert_eq!(
            track.children.len(),
            2,
            "nested child must not leak into root"
        );
        let CssVectorNode::Group(nested) = &track.children[1] else {
            panic!("nested node must remain grouped under track")
        };
        let CssVectorNode::Path(path) = &nested.children[0] else {
            panic!("nested background must be a path")
        };
        let bounds = path.path.bounding_box().unwrap();
        // Root origin (10,20) + root offset (100,50) + track (0,10) + nested (5,5).
        assert!((bounds.x0 - 115.0).abs() < 1e-9);
        assert!((bounds.y0 - 85.0).abs() < 1e-9);
    }

    fn node_name(node: &CssVectorNode) -> &str {
        match node {
            CssVectorNode::Group(group) => &group.name,
            CssVectorNode::Path(path) => &path.name,
        }
    }

    #[test]
    fn equal_z_index_uses_css_source_order_not_selector_order() {
        let plan = compile_css_vectors(
            ".root { width: 20px; height: 20px; }\
             .root > .zebra { width: 10px; height: 10px; background: red; }\
             .root > .alpha { width: 10px; height: 10px; background: blue; }",
            Some(".root"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        )
        .unwrap();
        let CssVectorNode::Group(root) = &plan.roots[0] else {
            panic!("root must be a group")
        };
        let names: Vec<_> = root.children.iter().map(node_name).collect();
        assert_eq!(names, vec!["root/.zebra", "root/.alpha"]);
    }

    #[test]
    fn strict_mode_rejects_non_integer_z_index() {
        let diagnostics = compile_css_vectors(
            ".root { width: 20px; height: 20px; z-index: 1.5; }",
            Some(".root"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        )
        .unwrap_err();
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CSS_INVALID_Z_INDEX"));
    }

    #[test]
    fn hidden_elements_do_not_lower_paths_or_descendants() {
        let css = ".root { width: 100px; height: 100px; background: red; }\
            .root > .display-hidden { width: 20px; height: 20px; display: none; background: blue; }\
            .root > .display-hidden > .nested { width: 10px; height: 10px; background: green; }\
            .root > .visibility-hidden { width: 20px; height: 20px; visibility: hidden; background: blue; }\
            .root > .visibility-hidden > .nested { width: 10px; height: 10px; background: green; }\
            .root > .shown { width: 20px; height: 20px; background: blue; }";
        let plan = compile_css_vectors(
            css,
            Some(".root"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        )
        .unwrap();
        let mut names = Vec::new();
        visit_paths(&plan.roots, &mut |path| names.push(path.name.clone()));
        assert_eq!(
            names,
            vec!["root/background", "root/.shown/background"],
            "hidden elements and their descendants must not lower to paths"
        );
    }

    #[test]
    fn strict_mode_rejects_element_limit_before_lowering() {
        let css: String = (0..=MAX_ELEMENTS)
            .map(|i| format!(".item-{i} {{ width: 1px; height: 1px; }}"))
            .collect();
        let diagnostics = compile_css_vectors(
            &css,
            None,
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            true,
        )
        .unwrap_err();
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "CSS_LIMIT"
                && diagnostic
                    .message
                    .contains("more than 512 virtual elements")
        }));
    }

    #[test]
    fn permissive_mode_caps_lowered_nodes_for_over_limit_input() {
        let mut css = String::from(".root { width: 100px; height: 100px; }");
        for i in 0..MAX_ELEMENTS {
            css.push_str(&format!(
                ".root > .child-{i} {{ width: 1px; height: 1px; background: red; }}"
            ));
        }
        let plan = compile_css_vectors(
            &css,
            Some(".root"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            false,
        )
        .unwrap();
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CSS_LIMIT"));
        assert!(group_count(&plan.roots) <= MAX_ELEMENTS);
        assert!(node_count(&plan.roots) <= MAX_NODES);
    }

    #[test]
    fn permissive_mode_reports_and_bounds_deep_nesting() {
        let mut css = String::from(".node-0 { width: 100px; height: 100px; }");
        let mut parent = String::from(".node-0");
        for i in 0..(MAX_DEPTH + 8) {
            let child = format!("{parent} > .node-{}", i + 1);
            css.push_str(&format!("{child} {{ width: 1px; height: 1px; }}"));
            parent = child;
        }
        let plan = compile_css_vectors(
            &css,
            Some(".node-0"),
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            false,
        )
        .unwrap();
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CSS_DEPTH_LIMIT"));
        assert!(group_count(&plan.roots) <= MAX_DEPTH);
    }

    #[test]
    fn diagnostics_are_capped_for_many_unsupported_selectors() {
        let css: String = (0..(MAX_DIAGNOSTICS + 32))
            .map(|i| format!("[data-item-{i}] {{ width: 1px; height: 1px; }}"))
            .collect();
        let plan = compile_css_vectors(
            &css,
            None,
            CssOrigin { x: 0.0, y: 0.0 },
            CssViewport {
                width: 100.0,
                height: 100.0,
            },
            false,
        )
        .unwrap();
        assert!(plan.diagnostics.len() <= MAX_DIAGNOSTICS);
    }
}
