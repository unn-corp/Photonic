//! MCP handlers for raster (pixel) image editing — the Photoshop-grade surface.
//!
//! Placement (`place_image`, `create_raster_layer`), destructive edits
//! (`apply_adjustment`, `apply_filter`, `brush_stroke`, `bucket_fill`,
//! `gradient_fill`, `transform_image`), non-destructive layer masks
//! (`set_layer_mask`, `clear_layer_mask`), and inspection (`get_raster_info`).
//!
//! Every edit routes through `Command::UpdateNode` so it lands in the undo
//! history exactly like any other node mutation.

use crate::protocol::ToolResult;
use crate::server::AppState;
use base64::Engine;
use photonic_core::{
    history::Command,
    node::{NodeId, RasterNode, SceneNode, SceneNodeKind},
    raster::{
        adjust::AdjustmentSpec, advanced, brush, filter, geometry, image::RasterImage, mask::Mask,
        repair, validate_raster_dimensions, warp,
    },
};
use serde::Deserialize;
use serde_json::{json, Value};

// ─── Shared param helpers ───────────────────────────────────────────────────────

fn pf(v: &Value, key: &str, default: f32) -> f32 {
    v.get(key)
        .and_then(|x| x.as_f64())
        .map(|x| x as f32)
        .unwrap_or(default)
}
fn pu(v: &Value, key: &str, default: u32) -> u32 {
    v.get(key)
        .and_then(|x| x.as_u64())
        .map(|x| x as u32)
        .unwrap_or(default)
}
fn pbool(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(default)
}
fn pi(v: &Value, key: &str, default: i64) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(default)
}
fn parr3(v: &Value, key: &str, default: [f32; 3]) -> [f32; 3] {
    match v.get(key).and_then(|x| x.as_array()) {
        Some(a) if a.len() >= 3 => [
            a[0].as_f64().unwrap_or(default[0] as f64) as f32,
            a[1].as_f64().unwrap_or(default[1] as f64) as f32,
            a[2].as_f64().unwrap_or(default[2] as f64) as f32,
        ],
        _ => default,
    }
}

/// Parse a color given either as a hex string (`#rgb`, `#rrggbb`, `#rrggbbaa`)
/// or a JSON array `[r,g,b]` / `[r,g,b,a]` (0–255). Falls back to `default`.
fn parse_color_value(v: &Value, default: [u8; 4]) -> [u8; 4] {
    match v {
        Value::String(s) => parse_hex(s).unwrap_or(default),
        Value::Array(a) if a.len() >= 3 => {
            let g = |i: usize, d: u8| {
                a.get(i)
                    .and_then(|x| x.as_u64())
                    .map(|x| x as u8)
                    .unwrap_or(d)
            };
            [
                g(0, default[0]),
                g(1, default[1]),
                g(2, default[2]),
                g(3, 255),
            ]
        }
        _ => default,
    }
}

fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().trim_start_matches('#');
    let parse2 = |h: &str| u8::from_str_radix(h, 16).ok();
    match s.len() {
        3 => {
            let r = parse2(&s[0..1].repeat(2))?;
            let g = parse2(&s[1..2].repeat(2))?;
            let b = parse2(&s[2..3].repeat(2))?;
            Some([r, g, b, 255])
        }
        6 => Some([parse2(&s[0..2])?, parse2(&s[2..4])?, parse2(&s[4..6])?, 255]),
        8 => Some([
            parse2(&s[0..2])?,
            parse2(&s[2..4])?,
            parse2(&s[4..6])?,
            parse2(&s[6..8])?,
        ]),
        _ => None,
    }
}

/// Resolve a node id given as a UUID string or a node name.
fn resolve_node_id(doc: &photonic_core::Document, s: &str) -> Option<NodeId> {
    uuid::Uuid::parse_str(s)
        .ok()
        .filter(|id| doc.nodes.contains_key(id))
        .or_else(|| doc.find_node_by_name(s).map(|n| n.id))
}

// ─── Selection spec ─────────────────────────────────────────────────────────────

/// A declarative selection used to confine an edit (the MCP equivalent of making
/// a marquee/lasso/wand selection before applying an adjustment).
#[derive(Debug, Clone, Deserialize)]
pub struct SelectionSpec {
    /// "rect" | "ellipse" | "polygon" | "wand" | "color_range" | "whole"
    pub kind: String,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub w: f64,
    #[serde(default)]
    pub h: f64,
    #[serde(default)]
    pub points: Vec<[f64; 2]>,
    #[serde(default)]
    pub tolerance: f32,
    #[serde(default)]
    pub color: Option<Value>,
    #[serde(default)]
    pub feather: f32,
    #[serde(default)]
    pub invert: bool,
    #[serde(default)]
    pub grow: u32,
    #[serde(default)]
    pub contract: u32,
}

fn build_selection(img: &RasterImage, spec: &SelectionSpec) -> Option<Mask> {
    let mut mask = match spec.kind.as_str() {
        "whole" => return None, // no constraint
        "rect" => Mask::rect(
            img.width,
            img.height,
            spec.x as i64,
            spec.y as i64,
            spec.w as i64,
            spec.h as i64,
        ),
        "ellipse" => Mask::ellipse(img.width, img.height, spec.x, spec.y, spec.w, spec.h),
        "polygon" => {
            let pts: Vec<(f64, f64)> = spec.points.iter().map(|p| (p[0], p[1])).collect();
            Mask::polygon(img.width, img.height, &pts)
        }
        "wand" => Mask::magic_wand(img, spec.x as u32, spec.y as u32, spec.tolerance),
        "color_range" => {
            let target = spec
                .color
                .as_ref()
                .map(|c| parse_color_value(c, [0, 0, 0, 255]))
                .unwrap_or([0, 0, 0, 255]);
            Mask::color_range(img, target, spec.tolerance)
        }
        _ => return None,
    };
    if spec.grow > 0 {
        mask.grow(spec.grow);
    }
    if spec.contract > 0 {
        mask.contract(spec.contract);
    }
    if spec.feather > 0.0 {
        mask.feather(spec.feather);
    }
    if spec.invert {
        mask.invert();
    }
    Some(mask)
}

/// Fetch a clone of a raster node by id/name, or an error result.
fn get_raster(
    doc: &photonic_core::Document,
    id_or_name: &str,
) -> Result<(NodeId, SceneNode), String> {
    let nid = resolve_node_id(doc, id_or_name)
        .ok_or_else(|| format!("Node '{}' not found", id_or_name))?;
    let node = doc.nodes.get(&nid).cloned().ok_or("node vanished")?;
    if !matches!(node.kind, SceneNodeKind::Raster(_)) {
        return Err(format!("Node '{}' is not a raster node", id_or_name));
    }
    Ok((nid, node))
}

// ─── place_image / create_raster_layer ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PlaceImageArgs {
    /// Path to an image file on disk (PNG/JPEG/WebP/…).
    #[serde(default)]
    pub path: Option<String>,
    /// Or base64-encoded image bytes.
    #[serde(default)]
    pub data_base64: Option<String>,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub layer_id: Option<NodeId>,
}

pub async fn place_image(state: &AppState, args: PlaceImageArgs) -> ToolResult {
    let bytes = if let Some(path) = &args.path {
        match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("Failed to read '{}': {}", path, e)),
        }
    } else if let Some(b64) = &args.data_base64 {
        match base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("Invalid base64: {}", e)),
        }
    } else {
        return ToolResult::error("place_image requires `path` or `data_base64`");
    };

    let image = match RasterImage::from_encoded(&bytes) {
        Ok(i) => i,
        Err(e) => return ToolResult::error(format!("Failed to decode image: {}", e)),
    };
    let (w, h) = (image.width, image.height);

    let mut raster = RasterNode::new(image);
    if let Some(p) = &args.path {
        raster.source_uri = Some(p.clone());
    }

    let name = args.name.unwrap_or_else(|| "Image".to_string());
    let mut node = SceneNode::new(&name, uuid::Uuid::nil(), SceneNodeKind::Raster(raster));
    node.transform = photonic_core::Transform::translate(args.x, args.y);
    let node_id = node.id;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: args.layer_id,
        },
        &mut doc,
    );

    ToolResult::text(format!(
        "Placed image '{}' ({}×{}, id: {})",
        name, w, h, node_id
    ))
    .with_data(json!({ "node_id": node_id, "width": w, "height": h }))
}

#[derive(Debug, Deserialize)]
pub struct CreateRasterLayerArgs {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    /// Optional fill color (hex or [r,g,b,a]); default fully transparent.
    #[serde(default)]
    pub fill: Option<Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub layer_id: Option<NodeId>,
}

pub async fn create_raster_layer(state: &AppState, args: CreateRasterLayerArgs) -> ToolResult {
    if let Err(error) = validate_raster_dimensions(args.width, args.height) {
        return ToolResult::error(error);
    }
    let image_result = match &args.fill {
        Some(c) => {
            RasterImage::try_filled(args.width, args.height, parse_color_value(c, [0, 0, 0, 0]))
        }
        None => RasterImage::try_new(args.width, args.height),
    };
    let image = match image_result {
        Ok(image) => image,
        Err(error) => return ToolResult::error(format!("Could not create raster layer: {error}")),
    };
    let name = args.name.unwrap_or_else(|| "Raster Layer".to_string());
    let mut node = SceneNode::new(
        &name,
        uuid::Uuid::nil(),
        SceneNodeKind::Raster(RasterNode::new(image)),
    );
    node.transform = photonic_core::Transform::translate(args.x, args.y);
    let node_id = node.id;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: args.layer_id,
        },
        &mut doc,
    );
    ToolResult::text(format!(
        "Created raster layer '{}' ({}×{}, id: {})",
        name, args.width, args.height, node_id
    ))
    .with_data(json!({ "node_id": node_id, "width": args.width, "height": args.height }))
}

// ─── apply_adjustment ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ApplyAdjustmentArgs {
    pub node_id: String,
    /// Adjustment name (e.g. "brightness_contrast", "levels", "curves", …).
    pub adjustment: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub selection: Option<SelectionSpec>,
}

pub async fn apply_adjustment(state: &AppState, args: ApplyAdjustmentArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    let img = &mut rn.image;
    let sel = args
        .selection
        .as_ref()
        .and_then(|s| build_selection(img, s));
    let sel = sel.as_ref();

    let spec = match build_adjustment_spec(&args.adjustment, &args.params) {
        Ok(s) => s,
        Err(e) => return ToolResult::error(e),
    };
    spec.apply(img, sel);

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!("Applied {} to {}", args.adjustment, nid))
        .with_data(json!({ "node_id": nid, "adjustment": args.adjustment }))
}

/// Build a serializable [`AdjustmentSpec`] from an MCP adjustment name + params.
/// Shared by `apply_adjustment` (destructive) and `create_adjustment_layer`
/// (non-destructive), so both speak the exact same parameter language.
fn build_adjustment_spec(name: &str, p: &Value) -> Result<AdjustmentSpec, String> {
    Ok(match name {
        "brightness_contrast" => AdjustmentSpec::BrightnessContrast {
            brightness: pf(p, "brightness", 0.0),
            contrast: pf(p, "contrast", 0.0),
        },
        "levels" => AdjustmentSpec::Levels {
            in_black: pf(p, "in_black", 0.0),
            in_white: pf(p, "in_white", 1.0),
            gamma: pf(p, "gamma", 1.0),
            out_black: pf(p, "out_black", 0.0),
            out_white: pf(p, "out_white", 1.0),
        },
        "curves" => {
            let parse_pts = |key: &str| -> Vec<(f32, f32)> {
                p.get(key)
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|pt| pt.as_array())
                            .filter(|pt| pt.len() >= 2)
                            .map(|pt| {
                                (
                                    pt[0].as_f64().unwrap_or(0.0) as f32,
                                    pt[1].as_f64().unwrap_or(0.0) as f32,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            // `points` is the composite (RGB) curve; `red`/`green`/`blue` are
            // optional per-channel curves. Empty = identity for that channel.
            AdjustmentSpec::Curves {
                rgb: parse_pts("points"),
                red: parse_pts("red"),
                green: parse_pts("green"),
                blue: parse_pts("blue"),
            }
        }
        "exposure" => AdjustmentSpec::Exposure {
            stops: pf(p, "stops", 0.0),
        },
        "hue_saturation" => AdjustmentSpec::HueSaturation {
            hue: pf(p, "hue", 0.0),
            saturation: pf(p, "saturation", 0.0),
            lightness: pf(p, "lightness", 0.0),
        },
        "color_balance" => AdjustmentSpec::ColorBalance {
            shadows: parr3(p, "shadows", [0.0; 3]),
            midtones: parr3(p, "midtones", [0.0; 3]),
            highlights: parr3(p, "highlights", [0.0; 3]),
            preserve_luminosity: pbool(p, "preserve_luminosity", true),
        },
        "vibrance" => AdjustmentSpec::Vibrance {
            amount: pf(p, "amount", 0.0),
        },
        "desaturate" => AdjustmentSpec::Desaturate,
        "black_and_white" => AdjustmentSpec::BlackAndWhite {
            weights: parr3(p, "weights", [0.299, 0.587, 0.114]),
        },
        "invert" => AdjustmentSpec::Invert,
        "posterize" => AdjustmentSpec::Posterize {
            levels: pu(p, "levels", 4),
        },
        "threshold" => AdjustmentSpec::Threshold {
            level: pf(p, "level", 0.5),
        },
        "photo_filter" => AdjustmentSpec::PhotoFilter {
            color: parr3(p, "color", [1.0, 0.5, 0.0]),
            density: pf(p, "density", 0.25),
            preserve_luminosity: pbool(p, "preserve_luminosity", true),
        },
        "channel_mixer" => AdjustmentSpec::ChannelMixer {
            red: parr3(p, "red", [1.0, 0.0, 0.0]),
            green: parr3(p, "green", [0.0, 1.0, 0.0]),
            blue: parr3(p, "blue", [0.0, 0.0, 1.0]),
        },
        "gradient_map" => {
            let stops: Vec<(f32, [u8; 3])> = p
                .get("stops")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| {
                            let pos = s.get("pos").and_then(|x| x.as_f64())? as f32;
                            let c = parse_color_value(s.get("color")?, [0, 0, 0, 255]);
                            Some((pos, [c[0], c[1], c[2]]))
                        })
                        .collect()
                })
                .unwrap_or_else(|| vec![(0.0, [0, 0, 0]), (1.0, [255, 255, 255])]);
            AdjustmentSpec::GradientMap { stops }
        }
        "selective_color" => AdjustmentSpec::SelectiveColor {
            target: parr3(p, "target", [1.0, 0.0, 0.0]),
            adjust: parr3(p, "adjust", [0.0; 3]),
            range: pf(p, "range", 0.3),
        },
        "shadows_highlights" => AdjustmentSpec::ShadowsHighlights {
            shadows: pf(p, "shadows", 0.3),
            highlights: pf(p, "highlights", 0.3),
            radius: pf(p, "radius", 30.0),
        },
        "gamma" => AdjustmentSpec::Gamma {
            gamma: pf(p, "gamma", 1.0),
        },
        "auto_contrast" => AdjustmentSpec::AutoContrast,
        "auto_levels" => AdjustmentSpec::AutoLevels,
        other => return Err(format!("Unknown adjustment '{}'", other)),
    })
}

#[derive(Debug, Deserialize)]
pub struct CreateAdjustmentLayerArgs {
    /// Adjustment name (same vocabulary as `apply_adjustment`).
    pub adjustment: String,
    #[serde(default)]
    pub params: Value,
    /// Layer opacity = adjustment strength (0..1, default 1).
    #[serde(default)]
    pub opacity: Option<f32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub layer_id: Option<NodeId>,
}

pub async fn create_adjustment_layer(
    state: &AppState,
    args: CreateAdjustmentLayerArgs,
) -> ToolResult {
    let spec = match build_adjustment_spec(&args.adjustment, &args.params) {
        Ok(s) => s,
        Err(e) => return ToolResult::error(e),
    };
    let name = args
        .name
        .unwrap_or_else(|| format!("{} (adjustment)", args.adjustment));
    let mut node = SceneNode::new(
        &name,
        uuid::Uuid::nil(),
        SceneNodeKind::Raster(RasterNode::adjustment_layer(spec)),
    );
    if let Some(o) = args.opacity {
        node.opacity = o.clamp(0.0, 1.0);
    }
    let node_id = node.id;

    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    history.execute_discrete(
        Command::AddNode {
            node,
            layer_id: args.layer_id,
        },
        &mut doc,
    );
    ToolResult::text(format!(
        "Created non-destructive adjustment layer '{}' (id: {})",
        name, node_id
    ))
    .with_data(json!({ "node_id": node_id, "adjustment": args.adjustment }))
}

// ─── apply_filter ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ApplyFilterArgs {
    pub node_id: String,
    pub filter: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub selection: Option<SelectionSpec>,
}

pub async fn apply_filter(state: &AppState, args: ApplyFilterArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    let img = &mut rn.image;
    let sel = args
        .selection
        .as_ref()
        .and_then(|s| build_selection(img, s));
    let sel = sel.as_ref();
    let p = &args.params;

    match args.filter.as_str() {
        "gaussian_blur" => filter::gaussian_blur(img, pf(p, "radius", 2.0), sel),
        "box_blur" => filter::box_blur(img, pu(p, "radius", 2), sel),
        "motion_blur" => filter::motion_blur(img, pf(p, "angle", 0.0), pu(p, "distance", 10), sel),
        "sharpen" => filter::sharpen(img, pf(p, "amount", 1.0), sel),
        "unsharp_mask" => filter::unsharp_mask(
            img,
            pf(p, "radius", 2.0),
            pf(p, "amount", 1.0),
            pu(p, "threshold", 0).min(255) as u8,
            sel,
        ),
        "median" => filter::median(img, pu(p, "radius", 1), sel),
        "add_noise" => filter::add_noise(
            img,
            pf(p, "amount", 0.1),
            pbool(p, "monochrome", false),
            pu(p, "seed", 1) as u64,
            sel,
        ),
        "emboss" => filter::emboss(img, sel),
        "find_edges" => filter::find_edges(img, sel),
        "mosaic" => filter::mosaic(img, pu(p, "block", 8), sel),
        "high_pass" => filter::high_pass(img, pf(p, "radius", 2.0), sel),
        // ── Advanced filters ─────────────────────────────────────────────────
        "surface_blur" => {
            advanced::surface_blur(img, pu(p, "radius", 5), pf(p, "threshold", 0.1), sel)
        }
        "lens_blur" => advanced::lens_blur(img, pf(p, "radius", 6.0), sel),
        "smart_sharpen" => advanced::smart_sharpen(
            img,
            pf(p, "amount", 1.0),
            pf(p, "radius", 2.0),
            pu(p, "threshold", 0).min(255) as u8,
            sel,
        ),
        "reduce_noise" => advanced::reduce_noise(img, pf(p, "strength", 0.5), sel),
        "clarity" => advanced::clarity(img, pf(p, "amount", 0.3), sel),
        "vignette" => advanced::vignette(img, pf(p, "amount", -0.4), pf(p, "feather", 0.5), sel),
        "chromatic_aberration" => advanced::chromatic_aberration(img, pf(p, "amount", 2.0), sel),
        other => return ToolResult::error(format!("Unknown filter '{}'", other)),
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!("Applied {} to {}", args.filter, nid))
        .with_data(json!({ "node_id": nid, "filter": args.filter }))
}

// ─── brush_stroke / bucket_fill / gradient_fill ─────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BrushStrokeArgs {
    pub node_id: String,
    /// Polyline of [x,y] points in the image's local pixel space.
    pub points: Vec<[f64; 2]>,
    #[serde(default)]
    pub color: Option<Value>,
    #[serde(default)]
    pub radius: Option<f32>,
    #[serde(default)]
    pub hardness: Option<f32>,
    #[serde(default)]
    pub flow: Option<f32>,
    #[serde(default)]
    pub opacity: Option<f32>,
    /// "paint" (default) | "erase"
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub selection: Option<SelectionSpec>,
}

pub async fn brush_stroke(state: &AppState, args: BrushStrokeArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    if args.points.is_empty() {
        return ToolResult::error("brush_stroke requires at least one point");
    }
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    let img = &mut rn.image;
    let sel = args
        .selection
        .as_ref()
        .and_then(|s| build_selection(img, s));
    let sel = sel.as_ref();

    let color = args
        .color
        .as_ref()
        .map(|c| parse_color_value(c, [0, 0, 0, 255]))
        .unwrap_or([0, 0, 0, 255]);
    let mut b = brush::Brush::new(args.radius.unwrap_or(10.0), color);
    if let Some(h) = args.hardness {
        b.hardness = h.clamp(0.0, 1.0);
    }
    if let Some(f) = args.flow {
        b.flow = f.clamp(0.0, 1.0);
    }
    if let Some(o) = args.opacity {
        b.opacity = o.clamp(0.0, 1.0);
    }
    let pts: Vec<(f32, f32)> = args
        .points
        .iter()
        .map(|p| (p[0] as f32, p[1] as f32))
        .collect();

    match args.mode.as_deref() {
        Some("erase") => brush::erase(img, &pts, &b, sel),
        _ => brush::stroke(img, &pts, &b, sel),
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!("Painted stroke ({} points) on {}", pts.len(), nid))
        .with_data(json!({ "node_id": nid }))
}

#[derive(Debug, Deserialize)]
pub struct BucketFillArgs {
    pub node_id: String,
    pub x: u32,
    pub y: u32,
    pub color: Value,
    #[serde(default)]
    pub tolerance: f32,
}

pub async fn bucket_fill(state: &AppState, args: BucketFillArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    let color = parse_color_value(&args.color, [0, 0, 0, 255]);
    brush::bucket_fill(&mut rn.image, args.x, args.y, color, args.tolerance);

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!(
        "Filled region at ({},{}) on {}",
        args.x, args.y, nid
    ))
    .with_data(json!({ "node_id": nid }))
}

#[derive(Debug, Deserialize)]
pub struct GradientFillArgs {
    pub node_id: String,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub color0: Value,
    pub color1: Value,
    #[serde(default)]
    pub selection: Option<SelectionSpec>,
}

pub async fn gradient_fill(state: &AppState, args: GradientFillArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    let img = &mut rn.image;
    let sel = args
        .selection
        .as_ref()
        .and_then(|s| build_selection(img, s));
    let c0 = parse_color_value(&args.color0, [0, 0, 0, 255]);
    let c1 = parse_color_value(&args.color1, [255, 255, 255, 255]);
    brush::gradient_fill(
        img,
        args.x0,
        args.y0,
        args.x1,
        args.y1,
        c0,
        c1,
        sel.as_ref(),
    );

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!("Filled gradient on {}", nid)).with_data(json!({ "node_id": nid }))
}

// ─── transform_image ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TransformImageArgs {
    pub node_id: String,
    /// "crop" | "resize" | "resize_canvas" | "rotate90" | "rotate180" |
    /// "rotate270" | "flip_h" | "flip_v" | "rotate"
    pub op: String,
    #[serde(default)]
    pub params: Value,
}

pub async fn transform_image(state: &AppState, args: TransformImageArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    let img = &rn.image;
    let p = &args.params;

    let new_img = match args.op.as_str() {
        "crop" => geometry::crop(
            img,
            pi(p, "x", 0),
            pi(p, "y", 0),
            pu(p, "width", img.width),
            pu(p, "height", img.height),
        ),
        "resize" => {
            let filter = match p.get("filter").and_then(|x| x.as_str()) {
                Some("nearest") => geometry::Resample::Nearest,
                Some("bilinear") => geometry::Resample::Bilinear,
                _ => geometry::Resample::Lanczos3,
            };
            geometry::resize(
                img,
                pu(p, "width", img.width),
                pu(p, "height", img.height),
                filter,
            )
        }
        "resize_canvas" => geometry::resize_canvas(
            img,
            pu(p, "width", img.width),
            pu(p, "height", img.height),
            pi(p, "offset_x", 0),
            pi(p, "offset_y", 0),
        ),
        "rotate90" => geometry::rotate90(img),
        "rotate180" => geometry::rotate180(img),
        "rotate270" => geometry::rotate270(img),
        "flip_h" => geometry::flip_h(img),
        "flip_v" => geometry::flip_v(img),
        "rotate" => geometry::rotate_arbitrary(img, pf(p, "angle", 0.0)),
        other => return ToolResult::error(format!("Unknown transform op '{}'", other)),
    };
    let (w, h) = (new_img.width, new_img.height);
    rn.image = new_img;
    // A resized buffer invalidates a same-size layer mask.
    if rn
        .mask
        .as_ref()
        .map(|m| m.width != w || m.height != h)
        .unwrap_or(false)
    {
        rn.mask = None;
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!("{} → {}×{} on {}", args.op, w, h, nid))
        .with_data(json!({ "node_id": nid, "width": w, "height": h }))
}

// ─── layer mask ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SetLayerMaskArgs {
    pub node_id: String,
    pub selection: SelectionSpec,
}

pub async fn set_layer_mask(state: &AppState, args: SetLayerMaskArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut new_node = old.clone();
    // An adjustment layer covers the whole canvas, so its mask is built in
    // document space; a pixel layer's mask matches its own image dimensions.
    let (mw, mh) = {
        let SceneNodeKind::Raster(rn) = &new_node.kind else {
            return ToolResult::error("not a raster node");
        };
        if rn.is_adjustment_layer() {
            (doc.width.max(1.0) as u32, doc.height.max(1.0) as u32)
        } else {
            (rn.image.width, rn.image.height)
        }
    };
    let dims_img = RasterImage::new(mw, mh);
    // For a layer mask, "whole" means fully revealed.
    let mask = build_selection(&dims_img, &args.selection).unwrap_or_else(|| Mask::full(mw, mh));
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    rn.mask = Some(mask);

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!("Set layer mask on {}", nid)).with_data(json!({ "node_id": nid }))
}

#[derive(Debug, Deserialize)]
pub struct ClearLayerMaskArgs {
    pub node_id: String,
}

pub async fn clear_layer_mask(state: &AppState, args: ClearLayerMaskArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    rn.mask = None;

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!("Cleared layer mask on {}", nid)).with_data(json!({ "node_id": nid }))
}

// ─── remove_background ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RemoveBackgroundArgs {
    pub node_id: String,
}

/// Detect the subject with the local matting model (photonic-matte, U²-Net-p
/// via ONNX Runtime — offline after a one-time ~5 MB download) and apply the
/// result as a non-destructive foreground layer mask.
pub async fn remove_background(state: &AppState, args: RemoveBackgroundArgs) -> ToolResult {
    // Snapshot the pixels, then release the doc lock for the (slow) inference.
    let (nid, img) = {
        let doc = state.document.lock().await;
        let (nid, node) = match get_raster(&doc, &args.node_id) {
            Ok(v) => v,
            Err(e) => return ToolResult::error(e),
        };
        let SceneNodeKind::Raster(rn) = &node.kind else {
            return ToolResult::error("not a raster node");
        };
        if rn.is_adjustment_layer() {
            return ToolResult::error("cannot remove background on an adjustment layer");
        }
        (nid, rn.image.clone())
    };

    let matte =
        match tokio::task::spawn_blocking(move || photonic_matte::remove_background(&img)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => return ToolResult::error(format!("background removal failed: {e:#}")),
            Err(e) => return ToolResult::error(format!("background removal panicked: {e}")),
        };

    // Re-read the node — it may have changed while inference ran; the re-read
    // state is the correct `old` for undo, and a resized image is rejected by
    // the dimension check below.
    let mut doc = state.document.lock().await;
    let Some(old) = doc.nodes.get(&nid).cloned() else {
        return ToolResult::error("node was deleted during background removal");
    };
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("node is no longer a raster node");
    };
    if rn.image.width != matte.width || rn.image.height != matte.height {
        return ToolResult::error("layer pixels changed during background removal; retry");
    }
    rn.set_foreground_matte(matte);

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!(
        "Removed background on {} (applied as a non-destructive layer mask)",
        nid
    ))
    .with_data(json!({ "node_id": nid }))
}

// ─── get_raster_info (read-only) ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GetRasterInfoArgs {
    pub node_id: String,
}

pub async fn get_raster_info(state: &AppState, args: GetRasterInfoArgs) -> ToolResult {
    let doc = state.document.lock().await;
    let (nid, node) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let SceneNodeKind::Raster(rn) = &node.kind else {
        return ToolResult::error("not a raster node");
    };
    let img = &rn.image;

    // 16-bucket luma histogram.
    let mut hist = [0u32; 16];
    for px in img.pixels.chunks_exact(4) {
        let l = photonic_core::raster::luma([
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
        ]);
        let b = ((l * 16.0) as usize).min(15);
        hist[b] += 1;
    }

    ToolResult::text(format!(
        "{}×{} raster, mask: {}",
        img.width,
        img.height,
        rn.mask.is_some()
    ))
    .with_data(json!({
        "node_id": nid,
        "width": img.width,
        "height": img.height,
        "has_mask": rn.mask.is_some(),
        "source_uri": rn.source_uri,
        "luma_histogram": hist,
    }))
}

// ─── retouch (healing / content-aware / red-eye) ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RetouchArgs {
    pub node_id: String,
    /// "healing_brush" | "spot_healing" | "content_aware_fill" | "red_eye" | "dust_and_scratches"
    pub op: String,
    #[serde(default)]
    pub params: Value,
    /// Required for content_aware_fill (the region to fill); optional elsewhere.
    #[serde(default)]
    pub selection: Option<SelectionSpec>,
}

pub async fn retouch(state: &AppState, args: RetouchArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    let img = &mut rn.image;
    let p = &args.params;
    let sel = args
        .selection
        .as_ref()
        .and_then(|s| build_selection(img, s));

    match args.op.as_str() {
        "healing_brush" => repair::healing_brush(
            img,
            pf(p, "cx", 0.0),
            pf(p, "cy", 0.0),
            pf(p, "radius", 10.0),
            pi(p, "src_dx", 0),
            pi(p, "src_dy", 0),
        ),
        "spot_healing" => repair::spot_healing(
            img,
            pf(p, "cx", 0.0),
            pf(p, "cy", 0.0),
            pf(p, "radius", 10.0),
        ),
        "content_aware_fill" => {
            let Some(mask) = &sel else {
                return ToolResult::error("content_aware_fill requires a `selection`");
            };
            repair::content_aware_fill(img, mask);
        }
        "red_eye" => repair::red_eye(
            img,
            pf(p, "cx", 0.0),
            pf(p, "cy", 0.0),
            pf(p, "radius", 10.0),
        ),
        "dust_and_scratches" => repair::dust_and_scratches(
            img,
            pu(p, "radius", 2),
            pu(p, "threshold", 16).min(255) as u8,
            sel.as_ref(),
        ),
        other => return ToolResult::error(format!("Unknown retouch op '{}'", other)),
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!("{} on {}", args.op, nid)).with_data(json!({ "node_id": nid }))
}

// ─── liquify / distort ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LiquifyArgs {
    pub node_id: String,
    /// "push" | "twirl" | "pucker" | "bloat" | "pinch" | "spherize" | "ripple" | "perspective"
    pub op: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub selection: Option<SelectionSpec>,
}

pub async fn liquify(state: &AppState, args: LiquifyArgs) -> ToolResult {
    let mut doc = state.document.lock().await;
    let (nid, old) = match get_raster(&doc, &args.node_id) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(e),
    };
    let mut new_node = old.clone();
    let SceneNodeKind::Raster(rn) = &mut new_node.kind else {
        return ToolResult::error("not a raster node");
    };
    let img = &mut rn.image;
    let p = &args.params;
    let sel = args
        .selection
        .as_ref()
        .and_then(|s| build_selection(img, s));
    let sel = sel.as_ref();

    match args.op.as_str() {
        "push" => warp::liquify_push(
            img,
            pf(p, "cx", 0.0),
            pf(p, "cy", 0.0),
            pf(p, "dx", 0.0),
            pf(p, "dy", 0.0),
            pf(p, "radius", 50.0),
            pf(p, "strength", 0.5),
            sel,
        ),
        "twirl" => warp::liquify_twirl(
            img,
            pf(p, "cx", 0.0),
            pf(p, "cy", 0.0),
            pf(p, "radius", 50.0),
            pf(p, "angle", 45.0),
            sel,
        ),
        "pucker" => warp::liquify_pucker(
            img,
            pf(p, "cx", 0.0),
            pf(p, "cy", 0.0),
            pf(p, "radius", 50.0),
            pf(p, "amount", 0.5),
            sel,
        ),
        "bloat" => warp::liquify_pucker(
            img,
            pf(p, "cx", 0.0),
            pf(p, "cy", 0.0),
            pf(p, "radius", 50.0),
            -pf(p, "amount", 0.5),
            sel,
        ),
        "pinch" => warp::pinch(img, pf(p, "amount", 0.5), sel),
        "spherize" => warp::spherize(img, pf(p, "amount", 0.5), sel),
        "ripple" => warp::ripple(img, pf(p, "amplitude", 5.0), pf(p, "wavelength", 30.0), sel),
        "perspective" => {
            let corners = p.get("corners").and_then(|x| x.as_array());
            let Some(c) = corners.filter(|a| a.len() == 4) else {
                return ToolResult::error(
                    "perspective requires `corners`: [[x,y]×4] (TL,TR,BR,BL)",
                );
            };
            let mut dst = [(0.0f32, 0.0f32); 4];
            for (i, pt) in c.iter().enumerate() {
                let arr = pt.as_array();
                let (Some(a), true) = (arr, arr.map(|a| a.len() >= 2).unwrap_or(false)) else {
                    return ToolResult::error("each corner must be [x,y]");
                };
                dst[i] = (
                    a[0].as_f64().unwrap_or(0.0) as f32,
                    a[1].as_f64().unwrap_or(0.0) as f32,
                );
            }
            *img = warp::perspective(img, dst);
        }
        other => return ToolResult::error(format!("Unknown liquify op '{}'", other)),
    }

    let mut history = state.history.lock().await;
    history.execute_discrete(Command::UpdateNode { old, new: new_node }, &mut doc);
    ToolResult::text(format!("{} on {}", args.op, nid)).with_data(json!({ "node_id": nid }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::McpServerConfig;
    use photonic_core::{Artboard, AuditLog, Document};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn test_state(doc: Document) -> AppState {
        let (tx, _rx) = std::sync::mpsc::channel();
        AppState {
            document: Arc::new(Mutex::new(doc)),
            history: Arc::new(Mutex::new(photonic_core::history::CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(tx)),
            config: McpServerConfig::default(),
            path_policy: photonic_core::PathPolicy::test_default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
            video_engine: Arc::new(crate::handlers::video_jobs::VideoEngineHandle::new()),
            video_jobs: Arc::new(StdMutex::new(
                crate::handlers::video_jobs::JobRegistry::new(),
            )),
        }
    }

    /// A tiny opaque PNG, base64-encoded, for feeding `place_image`.
    fn png_b64(w: u32, h: u32) -> String {
        let bytes = RasterImage::filled(w, h, [255, 0, 0, 255]).to_png();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// World-space AABB top-left of a node, composing local bounds with the node
    /// transform exactly like `inspect_node`'s `world_bounds`.
    fn world_top_left(node: &photonic_core::SceneNode) -> (f64, f64) {
        let local = node.local_bounds().expect("raster has local bounds");
        let affine = node.transform.to_kurbo();
        let pts = [
            affine * kurbo::Point::new(local.x0, local.y0),
            affine * kurbo::Point::new(local.x1, local.y0),
            affine * kurbo::Point::new(local.x1, local.y1),
            affine * kurbo::Point::new(local.x0, local.y1),
        ];
        let x0 = pts.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let y0 = pts.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        (x0, y0)
    }

    /// Regression for the "placed images land on the wrong artboard / wrong
    /// coordinates" bug: `place_image` must position the raster's top-left at the
    /// requested DOCUMENT-space (x, y), independent of the active artboard.
    #[tokio::test]
    async fn place_image_lands_at_requested_document_coords() {
        // Two artboards: A1 at the origin, A2 far to the right. Make A2 active so
        // any "relative to active artboard" offset bug would shift the placement.
        let mut doc = Document::new("t", 200.0, 200.0);
        let a2 = Artboard::new("Artboard 2", 300.0, 0.0, 200.0, 200.0);
        let a2_id = a2.id;
        let a1_id = doc.artboards[0].id;
        doc.artboards.push(a2);
        doc.active_artboard = Some(a2_id);

        let state = test_state(doc);

        // Request placement squarely inside A2's rect, in document space.
        let (req_x, req_y) = (320.0, 40.0);
        let r = place_image(
            &state,
            PlaceImageArgs {
                path: None,
                data_base64: Some(png_b64(32, 32)),
                x: req_x,
                y: req_y,
                name: Some("img".into()),
                layer_id: None,
            },
        )
        .await;
        assert_ne!(r.is_error, Some(true), "place_image should succeed");

        let doc = state.document.lock().await;
        let node = doc
            .nodes
            .values()
            .find(|n| matches!(n.kind, SceneNodeKind::Raster(_)))
            .expect("raster node placed");

        // (1) World-space top-left equals the requested (x, y), NOT offset by the
        //     active artboard origin (which would land it at 620, 40).
        let (wx, wy) = world_top_left(node);
        assert!(
            (wx - req_x).abs() < 1e-6 && (wy - req_y).abs() < 1e-6,
            "world top-left ({wx}, {wy}) != requested ({req_x}, {req_y})"
        );

        // (2) The placed image is assigned to the artboard it actually overlaps
        //     (A2), and NOT to A1.
        let owner = doc
            .artboard_for_rect(wx, wy, wx + 32.0, wy + 32.0)
            .expect("placed image overlaps an artboard");
        assert_eq!(owner.id, a2_id, "image should belong to A2");
        assert_ne!(owner.id, a1_id, "image must not belong to A1");
    }

    #[tokio::test]
    async fn create_raster_layer_rejects_invalid_dimensions_without_mutation() {
        let invalid_dimensions = [
            (0, 1),
            (1, 0),
            (photonic_core::raster::MAX_RASTER_DIMENSION + 1, 1),
            (8193, 8192),
            (u32::MAX, u32::MAX),
        ];

        for (width, height) in invalid_dimensions {
            let state = test_state(Document::new("t", 200.0, 200.0));
            let before = serde_json::to_value(&*state.document.lock().await)
                .expect("document should serialize");

            let result = create_raster_layer(
                &state,
                CreateRasterLayerArgs {
                    width,
                    height,
                    x: 0.0,
                    y: 0.0,
                    fill: None,
                    name: None,
                    layer_id: None,
                },
            )
            .await;

            assert_eq!(result.is_error, Some(true), "{width}x{height} must fail");
            let after = serde_json::to_value(&*state.document.lock().await)
                .expect("document should serialize");
            assert_eq!(
                after, before,
                "rejected dimensions must not mutate document"
            );
            assert_eq!(state.history.lock().await.undo_depth(), 0);
        }
    }

    #[tokio::test]
    async fn create_raster_layer_accepts_small_dimensions() {
        let state = test_state(Document::new("t", 200.0, 200.0));
        let result = create_raster_layer(
            &state,
            CreateRasterLayerArgs {
                width: 2,
                height: 3,
                x: 0.0,
                y: 0.0,
                fill: Some(json!("#102030ff")),
                name: Some("small".to_string()),
                layer_id: None,
            },
        )
        .await;

        assert_ne!(result.is_error, Some(true));
        assert_eq!(state.document.lock().await.nodes.len(), 1);
        assert_eq!(state.history.lock().await.undo_depth(), 1);
    }
}
