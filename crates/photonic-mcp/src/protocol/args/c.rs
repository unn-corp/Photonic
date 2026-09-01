use super::*;
use photonic_core::color::Color;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Arguments for the native CSS-to-editable-vector compiler (#251).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateVectorsFromCssArgs {
    pub css: String,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub origin: Option<CssPointArg>,
    #[serde(default)]
    pub viewport: Option<CssViewportArg>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default = "default_css_strict")]
    pub strict: bool,
    #[serde(default)]
    pub dry_run: bool,
}
#[derive(Debug, Clone, Deserialize)]
pub struct CssPointArg {
    pub x: f64,
    pub y: f64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct CssViewportArg {
    pub width: f64,
    pub height: f64,
}
fn default_css_strict() -> bool {
    true
}

/// Arguments for the bounded JSX + Tailwind component importer (#252).
/// This is deliberately source-only: it never evaluates JavaScript, follows
/// imports, or loads a Tailwind configuration from the host machine.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateVectorsFromReactArgs {
    /// A static JSX fragment containing intrinsic HTML elements only.
    #[serde(default)]
    pub jsx: Option<String>,
    /// Untouched module source for one of the explicitly supported static
    /// snapshots.  Source is parsed as text only; it is never evaluated.
    #[serde(default)]
    pub source: Option<String>,
    /// Pinned literal data used to resolve a bounded dynamic collection (for
    /// example the catalogue passed to `tiles.map`).
    #[serde(default)]
    pub snapshot: Option<ReactSnapshotArg>,
    /// Local entry module. It and every resolved module must be underneath
    /// `module_roots`; this importer never fetches network source.
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub export_name: Option<String>,
    /// JSON-only static props snapshot. No functions, expressions or hooks.
    #[serde(default)]
    pub props: Option<Value>,
    #[serde(default)]
    pub module_roots: Vec<String>,
    #[serde(default)]
    pub theme_tokens: Option<ReactThemeTokensArg>,
    /// Event-handler policy for file-backed snapshots. By default any rendered
    /// handler is rejected. `strip` removes handlers without executing them and
    /// records each removed interaction in diagnostics and provenance.
    #[serde(default)]
    pub interaction_policy: Option<String>,
    /// Explicit values for bounded conditional content in resolved wrappers.
    /// For the kiosk slice, backgroundImage must be null and
    /// enableInactivity must be false.
    #[serde(default)]
    pub dynamic_content: Option<Value>,
    #[serde(default)]
    pub origin: Option<CssPointArg>,
    #[serde(default)]
    pub viewport: Option<CssViewportArg>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default = "default_css_strict")]
    pub strict: bool,
    #[serde(default)]
    pub dry_run: bool,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ReactThemeTokensArg {
    #[serde(default)]
    pub card: Option<String>,
    #[serde(default)]
    pub foreground: Option<String>,
    #[serde(default)]
    pub muted_foreground: Option<String>,
    #[serde(default)]
    pub border: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReactSnapshotArg {
    /// The only currently supported template: `bgch-hub-app-directory-v1`.
    pub template: String,
    #[serde(default)]
    pub tiles: Vec<ReactSnapshotTileArg>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReactSnapshotTileArg {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub color: Option<String>,
}

/// Arguments for the `set_paint` tool — apply one paint to many nodes at once,
/// each re-fit to its own bounding box (issue #202). The paint uses the same
/// object shape as `fill`; a gradient may set `"units": "bbox"` with 0–1 coords
/// to be resolved per node.
#[derive(Debug, Deserialize, Clone)]
pub struct SetPaintArgs {
    /// Nodes (ids or names) to paint. Order is irrelevant.
    pub node_ids: Vec<String>,
    /// Which paint slot to set: `"fill"` (default) or `"stroke"`.
    #[serde(default)]
    pub target: Option<String>,
    /// The paint to apply (same shape as `fill`).
    pub paint: FillArg,
}

/// Stroke specification from an MCP client.
#[derive(Debug, Deserialize, Clone)]
pub struct StrokeArg {
    pub color: Option<String>,
    pub width: Option<f64>,
    pub enabled: Option<bool>,
    pub opacity: Option<f32>,
    /// "butt" | "round" | "square"
    pub line_cap: Option<String>,
    /// "miter" | "round" | "bevel"
    pub line_join: Option<String>,
    /// "center" | "inside" | "outside"
    pub align: Option<String>,
    /// Dash pattern: alternating dash and gap lengths (e.g. [8,4] or [8,4,2,4]).
    /// Up to 6 values (3 dash/gap pairs). Empty or absent = solid stroke.
    #[serde(default)]
    pub dash_array: Option<Vec<f64>>,
    /// Phase offset into the dash pattern (pixels). Default 0.
    #[serde(default)]
    pub dash_offset: Option<f64>,
    /// Align dashes to path corners and endpoints so dashes are never clipped at corners.
    #[serde(default)]
    pub dash_corner_alignment: Option<bool>,
    /// Arrowhead at the path start: "none" | "filled_arrow" | "open_arrow". Default "none".
    #[serde(default)]
    pub arrowhead_start: Option<String>,
    /// Arrowhead at the path end: "none" | "filled_arrow" | "open_arrow". Default "none".
    #[serde(default)]
    pub arrowhead_end: Option<String>,
    /// Optional non-solid stroke paint (gradient/pattern), same object shape as
    /// `fill` (#201). When a gradient/pattern, the stroke geometry is painted
    /// with it instead of the flat `color`; a solid paint just sets the color.
    #[serde(default)]
    pub paint: Option<FillArg>,
}

impl StrokeArg {
    pub fn to_stroke(&self) -> Result<photonic_core::style::Stroke, String> {
        use photonic_core::style::{ArrowheadStyle, LineCap, LineJoin, Stroke, StrokeAlign};
        let enabled = self.enabled.unwrap_or(true);
        if !enabled {
            return Ok(Stroke::none());
        }
        let color = self
            .color
            .as_deref()
            .and_then(Color::from_hex)
            .unwrap_or(Color::BLACK);
        let width = self.width.unwrap_or(1.0);
        let mut stroke = Stroke::solid(color, width);
        if let Some(op) = self.opacity {
            stroke.opacity = op.clamp(0.0, 1.0);
        }
        if let Some(cap) = &self.line_cap {
            stroke.line_cap = match cap.to_lowercase().as_str() {
                "round" => LineCap::Round,
                "square" => LineCap::Square,
                _ => LineCap::Butt,
            };
        }
        if let Some(join) = &self.line_join {
            stroke.line_join = match join.to_lowercase().as_str() {
                "round" => LineJoin::Round,
                "bevel" => LineJoin::Bevel,
                _ => LineJoin::Miter,
            };
        }
        if let Some(align) = &self.align {
            stroke.align = match align.to_lowercase().as_str() {
                "inside" => StrokeAlign::Inside,
                "outside" => StrokeAlign::Outside,
                _ => StrokeAlign::Center,
            };
        }
        if let Some(dash) = &self.dash_array {
            // Clamp to at most 6 values (3 dash/gap pairs); reject negative values.
            let cleaned: Vec<f64> = dash.iter().take(6).map(|&v| v.max(0.0)).collect();
            stroke.dash_array = cleaned;
        }
        if let Some(offset) = self.dash_offset {
            stroke.dash_offset = offset;
        }
        if let Some(align) = self.dash_corner_alignment {
            stroke.dash_corner_alignment = align;
        }
        let parse_arrowhead = |s: &str| -> ArrowheadStyle {
            match s.to_lowercase().as_str() {
                "filled_arrow" | "filled" => ArrowheadStyle::FilledArrow,
                "open_arrow" | "open" => ArrowheadStyle::OpenArrow,
                _ => ArrowheadStyle::None,
            }
        };
        if let Some(ah) = &self.arrowhead_start {
            stroke.arrowhead_start = parse_arrowhead(ah);
        }
        if let Some(ah) = &self.arrowhead_end {
            stroke.arrowhead_end = parse_arrowhead(ah);
        }
        // Non-solid stroke paint (#201): a gradient/pattern paints the stroke
        // geometry; a solid paint just recolors it.
        if let Some(paint) = &self.paint {
            use photonic_core::style::FillKind;
            let f = paint.to_fill()?;
            match f.kind {
                FillKind::None => {}
                FillKind::Solid(c) => {
                    stroke.color = c;
                    stroke.paint = None;
                }
                other => stroke.paint = Some(other),
            }
        }
        Ok(stroke)
    }
}

/// Transform specification from an MCP client.
#[derive(Debug, Deserialize, Clone)]
pub struct TransformArg {
    /// [a, b, c, d, e, f] affine matrix
    pub matrix: Option<[f64; 6]>,
    pub translate: Option<TranslateArg>,
    pub rotate: Option<RotateArg>,
    pub scale: Option<ScaleArg>,
}

impl TransformArg {
    pub fn to_transform(&self) -> photonic_core::transform::Transform {
        use photonic_core::transform::Transform;
        if let Some(m) = self.matrix {
            return Transform { matrix: m };
        }
        let mut t = Transform::IDENTITY;
        if let Some(s) = &self.scale {
            t = t.then(&Transform::scale_around(s.sx, s.sy, s.origin_x, s.origin_y));
        }
        if let Some(r) = &self.rotate {
            t = t.then(&Transform::rotate_around(
                r.angle_degrees.to_radians(),
                r.origin_x,
                r.origin_y,
            ));
        }
        if let Some(tr) = &self.translate {
            t = t.then(&Transform::translate(tr.x, tr.y));
        }
        t
    }
}

// ─── measure_nodes ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MeasureNodesArgs {
    pub node_ids: Vec<Uuid>,
}

// ─── inspect_node ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InspectNodeArgs {
    /// Node ID (UUID string) or node name.
    pub id: String,
}

// ─── layout_nodes ─────────────────────────────────────────────────────────────

/// Layout algorithm used by `layout_nodes`.
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    /// Arrange nodes in a left-to-right grid that wraps into rows.
    Grid,
    /// Arrange nodes evenly around a circle.
    Circle,
    /// Stack nodes left-to-right along the X axis.
    StackHorizontal,
    /// Stack nodes top-to-bottom along the Y axis.
    StackVertical,
}

/// Cross-axis alignment used by stack layouts.
#[derive(Debug, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum CrossAxisAlign {
    /// Align to the start edge (top for `stack_horizontal`, left for `stack_vertical`).
    #[default]
    Start,
    /// Align to the centre.
    Center,
    /// Align to the end edge.
    End,
}

/// Arguments for the `layout_nodes` tool.
#[derive(Debug, Deserialize)]
pub struct LayoutNodesArgs {
    /// IDs of the nodes to rearrange. Order determines placement.
    pub node_ids: Vec<Uuid>,

    /// Layout algorithm to apply.
    pub layout: LayoutMode,

    // ── Shared origin ─────────────────────────────────────────────────────────
    /// X origin of the layout. Defaults to the left edge of the current selection.
    #[serde(default)]
    pub x: Option<f64>,
    /// Y origin of the layout. Defaults to the top edge of the current selection.
    #[serde(default)]
    pub y: Option<f64>,

    // ── Grid ──────────────────────────────────────────────────────────────────
    /// Number of columns (default: ceil(sqrt(N))).
    #[serde(default)]
    pub columns: Option<usize>,
    /// Horizontal gap between cells in pixels (default: 20).
    #[serde(default)]
    pub gap_x: Option<f64>,
    /// Vertical gap between cells in pixels (default: 20).
    #[serde(default)]
    pub gap_y: Option<f64>,
    /// Fixed cell width. Defaults to the widest node in the set.
    #[serde(default)]
    pub cell_width: Option<f64>,
    /// Fixed cell height. Defaults to the tallest node in the set.
    #[serde(default)]
    pub cell_height: Option<f64>,

    // ── Circle ────────────────────────────────────────────────────────────────
    /// Circle centre X. Defaults to the combined bounding-box centre.
    #[serde(default)]
    pub cx: Option<f64>,
    /// Circle centre Y. Defaults to the combined bounding-box centre.
    #[serde(default)]
    pub cy: Option<f64>,
    /// Radius of the circle in pixels (default: 200).
    #[serde(default)]
    pub radius: Option<f64>,
    /// Angle of the first node in degrees, measured from the positive X axis (default: 0).
    #[serde(default)]
    pub start_angle: Option<f64>,

    // ── Stack ─────────────────────────────────────────────────────────────────
    /// Gap between successive nodes in pixels (default: 20).
    #[serde(default)]
    pub gap: Option<f64>,
    /// Cross-axis alignment: `start` / `center` / `end` (default: `start`).
    #[serde(default)]
    pub align: CrossAxisAlign,
}

// ─── set_node_size ────────────────────────────────────────────────────────────

/// Arguments for the `set_node_size` tool.
#[derive(Debug, Deserialize)]
pub struct SetNodeSizeArgs {
    /// ID of the node to resize.
    pub node_id: Uuid,
    /// Target width in pixels. Omit to derive from height (requires `maintain_aspect_ratio`).
    #[serde(default)]
    pub width: Option<f64>,
    /// Target height in pixels. Omit to derive from width (requires `maintain_aspect_ratio`).
    #[serde(default)]
    pub height: Option<f64>,
    /// When true and both dimensions are given, use the smaller scale factor for both axes
    /// so the shape fits inside the requested box without distortion.
    /// When true and only one dimension is given, scale the other axis proportionally.
    /// Default: false (each axis scaled independently to hit the exact requested size).
    #[serde(default)]
    pub maintain_aspect_ratio: bool,
    /// The point on the node's bounding box that stays fixed during the resize.
    /// Default: `top_left`.
    #[serde(default)]
    pub anchor: SizeAnchor,
}

/// Which corner/edge of the bounding box to keep fixed when resizing.
#[derive(Debug, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum SizeAnchor {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    LeftCenter,
    Center,
    RightCenter,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

// ─── auto_name_nodes ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct AutoNameNodesArgs {
    /// "selection" = active selection only; "document" = all nodes (default).
    #[serde(default)]
    pub scope: Option<String>,
    /// If true, also rename nodes that already have non-generic names. Default: false.
    #[serde(default)]
    pub overwrite: bool,
    /// If true, return proposed renames without applying them. Default: false.
    #[serde(default)]
    pub dry_run: bool,
}

// ─── get_css_preview ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct GetCssPreviewArgs {
    /// Node UUID or name. If omitted, the first node in document order is used.
    #[serde(default)]
    pub id: Option<String>,
}

// ─── check_style_continuity ───────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct CheckStyleContinuityArgs {
    /// Node UUIDs to analyse. If absent or empty, the entire document is analysed.
    #[serde(default)]
    pub node_ids: Vec<Uuid>,
    /// Which property groups to check. Valid values: "fill", "stroke", "opacity", "font".
    /// Defaults to all four when omitted.
    #[serde(default)]
    pub checks: Vec<String>,
    /// Minimum occurrences for a value to be considered "dominant". Default: 2.
    /// Nodes whose value appears fewer than this many times are flagged as outliers.
    #[serde(default)]
    pub outlier_threshold: Option<usize>,
}

// ─── SimplifyPathArgs ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SimplifyPathArgs {
    /// UUID of the path node to simplify.
    pub node_id: Uuid,
    /// Ramer-Douglas-Peucker tolerance in document coordinates.
    /// Larger values remove more points. Typical range: 0.1–10.0.
    pub tolerance: f64,
    /// If true, return point counts without modifying the document. Default false.
    #[serde(default)]
    pub dry_run: bool,
}

// ─── OutlineStrokeArgs ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct OutlineStrokeArgs {
    /// UUIDs of path nodes whose stroke should be converted to an outline path.
    pub node_ids: Vec<Uuid>,
    /// If true, the original node's stroke is removed but the node remains.
    /// If false (default), same behaviour — the original stroke is disabled and
    /// a new outline node is placed above it.
    #[serde(default)]
    pub keep_original: bool,
}

// ─── OffsetPathArgs ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct OffsetPathArgs {
    /// UUIDs of path nodes to offset.
    pub node_ids: Vec<Uuid>,
    /// Offset distance in document units. Positive = outset (expand), negative = inset (shrink).
    pub distance: f64,
    /// Corner join style: "miter" (default), "round", or "bevel".
    #[serde(default)]
    pub join_style: Option<String>,
    /// If true (default), add the offset path as a new node above the original.
    /// If false, replace the original node with the offset result.
    #[serde(default)]
    pub create_copy: Option<bool>,
}

// ─── SplitIntoGridArgs ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SplitIntoGridArgs {
    /// UUID of the source path node whose bounding box defines the grid area.
    pub node_id: Uuid,
    /// Number of rows (≥ 1). The total grid size may not exceed [`MAX_GENERATED_WORK`].
    pub rows: usize,
    /// Number of columns (≥ 1). The total grid size may not exceed [`MAX_GENERATED_WORK`].
    pub cols: usize,
    /// Horizontal gutter width in document units between columns (default 0).
    #[serde(default)]
    pub gutter_x: Option<f64>,
    /// Vertical gutter height in document units between rows (default 0).
    #[serde(default)]
    pub gutter_y: Option<f64>,
    /// When true, keep the original node. Default: false (original is deleted).
    #[serde(default)]
    pub keep_original: Option<bool>,
    /// Layer to place new nodes in. Defaults to the source node's layer.
    #[serde(default)]
    pub layer_id: Option<Uuid>,
}

// ─── ReleaseToLayersArgs ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ReleaseToLayersArgs {
    /// IDs of nodes to release. Group children are resolved to their top-level ancestor.
    pub node_ids: Vec<Uuid>,
    /// Optional prefix for the new layer names. Each layer is named
    /// "<prefix> 1", "<prefix> 2", … (default: "Layer").
    #[serde(default)]
    pub name_prefix: Option<String>,
}

// ─── MergeLayersArgs ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MergeLayersArgs {
    /// IDs of the layers to merge. Must contain at least 2 entries.
    pub layer_ids: Vec<Uuid>,
    /// Optional name for the surviving (target) layer.
    /// Defaults to the name of the first layer in document order among those selected.
    #[serde(default)]
    pub target_name: Option<String>,
}

// ─── FlattenArtworkArgs ───────────────────────────────────────────────────────

/// Arguments for `update_layer`.
#[derive(Debug, Deserialize)]
pub struct UpdateLayerArgs {
    /// UUID of the layer to update.
    pub layer_id: Uuid,
    /// New name for the layer. Omit to keep existing name.
    #[serde(default)]
    pub name: Option<String>,
    /// Set layer visibility. Omit to keep existing value.
    #[serde(default)]
    pub visible: Option<bool>,
    /// Set layer lock state. Omit to keep existing value.
    #[serde(default)]
    pub locked: Option<bool>,
    /// Color tag for the layer as [r, g, b, a] with values 0.0–1.0.
    /// Pass `null` to clear the color. Omit to keep existing color.
    #[serde(default)]
    pub color: Option<Option<[f32; 4]>>,
    /// Mark this layer as a template layer (locked, dimmed reference for tracing).
    /// Omit to keep existing value.
    #[serde(default)]
    pub is_template: Option<bool>,
    /// Layer opacity, 0.0–1.0. The layer composites as a unit at this opacity.
    /// Omit to keep existing value.
    #[serde(default)]
    pub opacity: Option<f32>,
    /// Layer blend mode (e.g. "normal", "multiply", "screen", "overlay",
    /// "color_dodge", "hue", "luminosity"). The layer composites as a unit with
    /// this mode against the layers beneath. Omit to keep existing value.
    #[serde(default)]
    pub blend_mode: Option<String>,
    /// Whether the layer is included in export/print output (Illustrator's Print
    /// option). Non-print layers stay on the canvas but are excluded from exports.
    /// Omit to keep existing value.
    #[serde(default)]
    pub print: Option<bool>,
}

/// Arguments for `flatten_artwork`.
#[derive(Debug, Deserialize, Default)]
pub struct FlattenArtworkArgs {
    /// Optional name for the surviving layer. Defaults to the name of the
    /// bottom-most layer in document order.
    #[serde(default)]
    pub target_name: Option<String>,
}

// ─── BlendColorsArgs ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BlendColorsArgs {
    /// Ordered list of path node UUIDs to blend. Minimum 2.
    /// The first and last nodes keep their existing solid fill colors;
    /// intermediate nodes receive linearly interpolated colors.
    pub node_ids: Vec<Uuid>,
    /// Optional axis for auto-sorting nodes before blending.
    /// "horizontal" → sort by bounding-box center X (left → right),
    /// "vertical"   → sort by bounding-box center Y (top → bottom),
    /// "depth"      → sort by z-order (bottom layer/node first).
    /// Omit to use the supplied node_ids order as-is.
    #[serde(default)]
    pub direction: Option<String>,
}

// ─── InvertColorsArgs ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InvertColorsArgs {
    /// UUIDs of path nodes to invert. If omitted, all path nodes in the document are inverted.
    #[serde(default)]
    pub node_ids: Option<Vec<Uuid>>,
}

// ─── AdjustColorsArgs ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct AdjustColorsArgs {
    /// UUIDs of path nodes to adjust. If omitted, all path nodes in the document are adjusted.
    #[serde(default)]
    pub node_ids: Option<Vec<Uuid>>,
    /// Amount to add to the red channel (−1.0 to 1.0). Default 0.
    #[serde(default)]
    pub delta_r: f32,
    /// Amount to add to the green channel (−1.0 to 1.0). Default 0.
    #[serde(default)]
    pub delta_g: f32,
    /// Amount to add to the blue channel (−1.0 to 1.0). Default 0.
    #[serde(default)]
    pub delta_b: f32,
    /// Amount to add to the alpha channel (−1.0 to 1.0). Default 0.
    #[serde(default)]
    pub delta_a: f32,
}

// ─── ConvertToGrayscaleArgs ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ConvertToGrayscaleArgs {
    /// UUIDs of path nodes to convert. If omitted, all path nodes in the document are converted.
    #[serde(default)]
    pub node_ids: Option<Vec<Uuid>>,
}

// ─── Tool result type ─────────────────────────────────────────────────────────

/// Standard MCP tool result wrapper.
#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub content: Vec<ContentItem>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "isError")]
    pub is_error: Option<bool>,
}

impl ToolResult {
    pub fn text(msg: impl Into<String>) -> Self {
        Self {
            content: vec![ContentItem::text(msg)],
            is_error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            content: vec![ContentItem::text(msg)],
            is_error: Some(true),
        }
    }

    pub fn with_data(mut self, data: impl Serialize) -> Self {
        if let Ok(v) = serde_json::to_value(data) {
            self.content.push(ContentItem::json(v));
        }
        self
    }

    pub fn with_image(mut self, base64_png: String) -> Self {
        self.content.push(ContentItem::image(base64_png));
        self
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Resource {
        resource: Value,
    },
}

impl ContentItem {
    pub fn text(msg: impl Into<String>) -> Self {
        Self::Text { text: msg.into() }
    }

    pub fn json(v: Value) -> Self {
        Self::Text {
            text: serde_json::to_string_pretty(&v).unwrap_or_default(),
        }
    }

    pub fn image(base64_png: String) -> Self {
        Self::Image {
            data: base64_png,
            mime_type: "image/png".to_string(),
        }
    }
}

// ─── Annotation Args ─────────────────────────────────────────────────────────

/// Arguments for `add_annotation`.
#[derive(Debug, Deserialize)]
pub struct AddAnnotationArgs {
    /// The comment or design note text (required, non-empty).
    pub text: String,
    /// Node to attach this annotation to. Omit for a document-level note.
    #[serde(default)]
    pub node_id: Option<Uuid>,
    /// Optional author identity (e.g. `"claude"`, `"design-reviewer"`).
    #[serde(default)]
    pub author: Option<String>,
}

/// Arguments for `add_anchor_points`.
#[derive(Debug, Deserialize, Default)]
pub struct AddAnchorPointsArgs {
    /// IDs of path nodes to subdivide.
    pub node_ids: Vec<Uuid>,
    /// Number of subdivision passes (default 1, max 8).
    #[serde(default)]
    pub passes: Option<u32>,
}

/// Arguments for `clean_up`.
#[derive(Debug, Deserialize, Default)]
pub struct CleanUpArgs {
    /// Remove paths with no drawing segments (only MoveTo or empty). Default true.
    #[serde(default)]
    pub remove_stray_points: Option<bool>,
    /// Remove paths with no visible fill AND no visible stroke. Default true.
    #[serde(default)]
    pub remove_unpainted: Option<bool>,
    /// Remove text nodes whose content is empty or whitespace-only. Default true.
    #[serde(default)]
    pub remove_empty_text: Option<bool>,
    /// If true, report what would be removed without deleting anything. Default false.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// Arguments for `join_paths`.
#[derive(Debug, Deserialize, Default)]
pub struct JoinPathsArgs {
    /// One or two path node IDs.
    ///
    /// * **1 node** — every open subpath in the node is closed (a `ClosePath`
    ///   element is appended to each open subpath).
    /// * **2 nodes** — the two paths are merged into one by connecting their
    ///   nearest open endpoints with a straight line segment.  The result node
    ///   inherits the style of the first listed node; the second node is
    ///   removed.
    pub node_ids: Vec<Uuid>,
}

/// Arguments for `reverse_path_direction`.
#[derive(Debug, Deserialize, Default)]
pub struct ReversePathDirectionArgs {
    /// IDs of path nodes whose winding direction to reverse.
    pub node_ids: Vec<Uuid>,
}

/// Arguments for `average_anchor_points`.
#[derive(Debug, Deserialize, Default)]
pub struct AverageAnchorPointsArgs {
    /// IDs of path nodes to average.
    pub node_ids: Vec<Uuid>,
    /// Which axis to average: `"horizontal"` (X only), `"vertical"` (Y only),
    /// or `"both"` (default).
    #[serde(default)]
    pub axis: Option<String>,
}

/// Arguments for `list_annotations`.
#[derive(Debug, Deserialize, Default)]
pub struct ListAnnotationsArgs {
    /// Filter to annotations attached to a specific node.
    #[serde(default)]
    pub node_id: Option<Uuid>,
    /// When `true`, include resolved annotations. Defaults to `false`.
    #[serde(default)]
    pub include_resolved: Option<bool>,
}

/// Arguments for `resolve_annotation`.
#[derive(Debug, Deserialize)]
pub struct ResolveAnnotationArgs {
    /// UUID of the annotation to mark as resolved.
    pub annotation_id: Uuid,
}

/// Arguments for `pathfinder_crop`.
#[derive(Debug, Deserialize, Default)]
pub struct PathfinderCropArgs {
    /// Two or more path node IDs. The frontmost node (highest z-order) acts as
    /// the clipping boundary. All other nodes are clipped to that boundary in
    /// place (their paths are replaced by `path ∩ frontmost_path`). The
    /// frontmost node is removed at the end. Single undoable step.
    pub node_ids: Vec<Uuid>,
}

/// Arguments for `pathfinder_minus_back`.
#[derive(Debug, Deserialize, Default)]
pub struct PathfinderMinusBackArgs {
    /// Two or more path node IDs. The back nodes (all except the frontmost) are
    /// subtracted from the frontmost node's path; the back nodes are removed.
    /// The frontmost node's fill/stroke style is preserved. Single undoable step.
    pub node_ids: Vec<Uuid>,
}

/// Arguments for `pathfinder_minus_front`.
#[derive(Debug, Deserialize, Default)]
pub struct PathfinderMinusFrontArgs {
    /// Two or more path node IDs. The frontmost node (highest z-order) is
    /// subtracted from each back node's path; the frontmost node is removed.
    /// Each back node's fill/stroke style is preserved. Single undoable step.
    pub node_ids: Vec<Uuid>,
}

/// Arguments for `pathfinder_trim`.
#[derive(Debug, Deserialize, Default)]
pub struct PathfinderTrimArgs {
    /// Two or more path node IDs. Each node has the paths of all nodes above it
    /// (higher z-order) subtracted from it, removing hidden areas. Strokes are
    /// disabled on all result nodes. All nodes are retained (none removed).
    /// Single undoable step.
    pub node_ids: Vec<Uuid>,
}

/// Arguments for `pathfinder_outline`.
#[derive(Debug, Deserialize, Default)]
pub struct PathfinderOutlineArgs {
    /// One or more path node IDs. Each node's solid fill color is transferred to
    /// its stroke; the fill is removed; the stroke is enabled. Gradient fills
    /// fall back to black. Existing stroke width is preserved (default 1 pt if
    /// no stroke was set). Single undoable step.
    pub node_ids: Vec<Uuid>,
}

/// Arguments for `divide_objects_below`.
#[derive(Debug, Deserialize)]
pub struct DivideObjectsBelowArgs {
    /// The path node ID to use as the cutting edge. All nodes beneath it in
    /// z-order that overlap it will be split. The cutter is removed afterward.
    pub node_id: Uuid,
}

/// Arguments for `pathfinder_divide`.
#[derive(Debug, Deserialize, Default)]
pub struct PathfinderDivideArgs {
    /// Exactly two path node IDs: [back_node, front_node] (z-order). The two
    /// shapes are split at every overlap edge into up to three distinct faces.
    /// New path nodes are created for each face; the originals are removed.
    /// Face colors are inherited from whichever source shape contained them.
    pub node_ids: Vec<Uuid>,
    /// Layer to place the result nodes in. Defaults to the back node's layer.
    pub layer_id: Option<Uuid>,
}

/// Arguments for `pathfinder_merge`.
#[derive(Debug, Deserialize, Default)]
pub struct PathfinderMergeArgs {
    /// Two or more path node IDs (any order; back-to-front z-order is resolved automatically).
    /// Each node is trimmed of areas covered by nodes above it, then nodes sharing the same
    /// solid fill color are merged (unioned) into a single shape. Non-solid fills each become
    /// a separate result node. Strokes are disabled on all result nodes.
    pub node_ids: Vec<Uuid>,
    /// Layer to place the result nodes in. Defaults to the backmost source node's layer.
    pub layer_id: Option<Uuid>,
}

/// Which attribute to match against in `select_same`.
#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SelectSameAttribute {
    /// Match nodes whose solid fill color is within tolerance of the reference.
    #[default]
    FillColor,
    /// Match nodes whose solid stroke color is within tolerance of the reference.
    StrokeColor,
    /// Match nodes whose stroke width is within tolerance of the reference.
    StrokeWeight,
    /// Match nodes whose opacity is within tolerance of the reference.
    Opacity,
    /// Match nodes that share the same blend mode as the reference.
    BlendMode,
    /// Match nodes of the same node type (path / group / text).
    ObjectType,
}

/// Arguments for `select_same`.
#[derive(Debug, Deserialize, Default)]
pub struct SelectSameArgs {
    /// ID of the reference node whose attribute value is matched against.
    pub node_id: Uuid,
    /// Which attribute to match.
    pub attribute: SelectSameAttribute,
    /// How close two values must be to count as "same". Applies to color
    /// (Euclidean RGBA distance in [0,1] space), stroke weight, and opacity.
    /// Defaults to 0.01 (exact match in practice). Ignored for blend_mode and object_type.
    #[serde(default)]
    pub tolerance: Option<f64>,
    /// If true, include the reference node itself in the results. Default: true.
    #[serde(default)]
    pub include_self: Option<bool>,
}

// ─── Compound Path Args ──────────────────────────────────────────────────────

/// Arguments for `make_compound_path`.
#[derive(Debug, Deserialize, Default)]
pub struct MakeCompoundPathArgs {
    /// IDs of the path nodes to combine into a single compound path.
    /// Must contain at least 2 path nodes. The bottommost node's fill/stroke
    /// is used for the resulting compound path.
    pub node_ids: Vec<Uuid>,
    /// Optional name for the resulting compound path node.
    #[serde(default)]
    pub name: Option<String>,
}

/// Arguments for `make_live_boolean`.
#[derive(Debug, Deserialize, Default)]
pub struct MakeLiveBooleanArgs {
    /// IDs of the path nodes to combine. Must contain at least 2 path nodes.
    /// They are grouped into a non-destructive live-boolean group; the operands
    /// stay individually editable and the boolean recomputes on every edit.
    pub node_ids: Vec<Uuid>,
    /// Boolean operator: `union`, `intersect`, `subtract`, `exclude`, or `divide`.
    pub operation: String,
    /// Optional name for the resulting live-boolean group.
    #[serde(default)]
    pub name: Option<String>,
}

/// Arguments for `release_compound_path`.
#[derive(Debug, Deserialize, Default)]
pub struct ReleaseCompoundPathArgs {
    /// ID of the compound path node to release back into individual paths.
    pub node_id: Uuid,
}

// ─── ColorGuideArgs ──────────────────────────────────────────────────────────

/// Arguments for the `color_guide` tool.
#[derive(Debug, Deserialize)]
pub struct ColorGuideArgs {
    /// Base color as a hex string (#RRGGBB or #RRGGBBAA). Defaults to the
    /// solid fill of the first selected node when omitted.
    #[serde(default)]
    pub base_color: Option<String>,
    /// Harmony rule: "complementary" | "analogous" | "triadic" |
    /// "split_complementary" | "tetradic" | "monochromatic".
    /// Defaults to "complementary".
    #[serde(default)]
    pub rule: Option<String>,
}

/// Arguments for `recolor_artwork` tool
#[derive(Debug, Deserialize)]
pub struct RecolorArtworkArgs {
    /// IDs of nodes whose solid fills should be remapped. If empty, applies to all path nodes.
    #[serde(default)]
    pub node_ids: Vec<Uuid>,
    /// Target palette as hex strings (#RRGGBB or #RRGGBBAA). Each node's fill is replaced
    /// with the nearest palette color by Euclidean RGB distance.
    pub palette: Vec<String>,
}

/// Arguments for `distribute_on_path` tool
#[derive(Debug, Deserialize)]
pub struct DistributeOnPathArgs {
    /// ID of the path node to use as the distribution guide.
    pub path_node_id: Uuid,
    /// IDs of the nodes to distribute along the path. Each node is cloned `count` times.
    pub node_ids: Vec<Uuid>,
    /// Number of copies to place along the path. Defaults to the number of source nodes.
    #[serde(default)]
    pub count: Option<usize>,
    /// If true, rotate each copy to align with the path's tangent direction. Default: false.
    #[serde(default)]
    pub align_to_path: Option<bool>,
    /// Target layer for the new copies. Defaults to the guide path's layer.
    #[serde(default)]
    pub layer_id: Option<Uuid>,
}

// ─── Export Profile Args ──────────────────────────────────────────────────────

/// Arguments for `add_export_profile` tool
#[derive(Debug, Deserialize)]
pub struct AddExportProfileArgs {
    /// Unique profile name. If a profile with this name exists, it is replaced.
    pub name: String,
    /// Target format: "svg", "png", "jpeg", or "webp".
    pub format: String,
    /// Raster-only: explicit pixel width.
    pub width: Option<u32>,
    /// Raster-only: explicit pixel height (overrides scale).
    pub height: Option<u32>,
    /// SVG-only: emit semantic id attributes (default true).
    pub semantic_ids: Option<bool>,
    /// SVG-only: coordinate decimal precision 1–6 (default 4).
    pub precision: Option<u32>,
}

/// Arguments for `remove_export_profile` tool
#[derive(Debug, Deserialize)]
pub struct RemoveExportProfileArgs {
    /// Name of the profile to remove.
    pub name: String,
}

/// Arguments for `run_export_profile` tool
#[derive(Debug, Deserialize)]
pub struct RunExportProfileArgs {
    /// Name of the profile to run.
    pub name: String,
}

// ─── PinObjectGuidesArgs ──────────────────────────────────────────────────────

/// Arguments for `pin_object_guides` tool
#[derive(Debug, Deserialize)]
pub struct PinObjectGuidesArgs {
    /// UUIDs or names of nodes to pin guides from. Uses current selection if empty.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Which edges to pin: "all" (default), "center", "edges", or a comma-separated
    /// subset of "top","bottom","left","right","center_h","center_v".
    #[serde(default)]
    pub edges: Option<String>,
}

// ─── Document Template Args ───────────────────────────────────────────────────

/// Arguments for `apply_document_template` tool
#[derive(Debug, Deserialize)]
pub struct ApplyDocumentTemplateArgs {
    /// Template JSON (from get_document_template). Canvas size, guides, export
    /// profiles, and new layers are applied to the current document.
    pub template_json: String,
}

// ─── PromptHistoryArgs ────────────────────────────────────────────────────────

/// Arguments for `set_node_prompt` tool
#[derive(Debug, Deserialize)]
pub struct SetNodePromptArgs {
    /// UUID or name of the node to annotate.
    pub node_id: String,
    /// The prompt text to record.
    pub prompt: String,
    /// How to add the prompt: "append" (default), "prepend", or "replace" (clears history first).
    #[serde(default)]
    pub mode: Option<String>,
}

/// Arguments for `get_node_prompts` tool
#[derive(Debug, Deserialize)]
pub struct GetNodePromptsArgs {
    /// UUID or name of the node.
    pub node_id: String,
}

// ─── ReverseNodeOrderArgs ─────────────────────────────────────────────────────

/// Arguments for `reverse_node_order` tool
#[derive(Debug, Deserialize)]
pub struct ReverseNodeOrderArgs {
    /// UUIDs or names of group nodes whose children order should be reversed.
    /// Uses current selection if empty.
    #[serde(default)]
    pub node_ids: Vec<String>,
}

// ─── RotateCopiesArgs ─────────────────────────────────────────────────────────

/// Arguments for `rotate_copies` tool
#[derive(Debug, Deserialize)]
pub struct RotateCopiesArgs {
    /// UUID or name of the node to copy and rotate.
    pub node_id: String,
    /// Total number of copies in the radial arrangement (including the original). Minimum: 2.
    pub count: usize,
    /// X coordinate of the rotation center in document units. Defaults to the node's bounding-box center.
    #[serde(default)]
    pub cx: Option<f64>,
    /// Y coordinate of the rotation center in document units. Defaults to the node's bounding-box center.
    #[serde(default)]
    pub cy: Option<f64>,
    /// When true, wrap all copies (including original) in a new Group node. Default: false.
    #[serde(default)]
    pub group: bool,
}

// ─── CopyAppearanceArgs ───────────────────────────────────────────────────────

/// Arguments for `copy_appearance` tool
#[derive(Debug, Deserialize)]
pub struct CopyAppearanceArgs {
    /// UUID or name of the source node to copy appearance from.
    pub source_id: String,
    /// UUIDs or names of target nodes to apply the appearance to.
    pub target_ids: Vec<String>,
    /// Copy fill. Default: true.
    #[serde(default = "default_true")]
    pub copy_fill: bool,
    /// Copy stroke. Default: true.
    #[serde(default = "default_true")]
    pub copy_stroke: bool,
    /// Copy opacity. Default: true.
    #[serde(default = "default_true")]
    pub copy_opacity: bool,
}

// ─── MirrorCopyArgs ───────────────────────────────────────────────────────────

/// Arguments for `mirror_copy` tool
#[derive(Debug, Deserialize)]
pub struct MirrorCopyArgs {
    /// UUIDs or names of nodes to mirror. Uses current selection if empty.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// "horizontal" — flip left-right (default), or "vertical" — flip top-bottom.
    #[serde(default)]
    pub axis: Option<String>,
}

// ─── NoiseDeformArgs ──────────────────────────────────────────────────────────

/// Arguments for `noise_deform` tool
#[derive(Debug, Deserialize)]
pub struct NoiseDeformArgs {
    /// UUIDs or names of path nodes to deform.
    pub node_ids: Vec<String>,
    /// Maximum displacement amplitude in document units (default: 8.0).
    pub amplitude: Option<f64>,
    /// Spatial frequency: higher = tighter waves (default: 0.05 cycles/px).
    pub frequency: Option<f64>,
    /// Phase seed — shifts the wave pattern (default: 0.0).
    pub seed: Option<f64>,
    /// Axis to deform: "both" (default), "x", or "y".
    #[serde(default)]
    pub axis: Option<String>,
}

// ─── DistributeNoOverlapArgs ──────────────────────────────────────────────────

/// Arguments for `distribute_no_overlap` tool
#[derive(Debug, Deserialize)]
pub struct DistributeNoOverlapArgs {
    /// UUIDs or names of nodes to un-overlap. Uses current selection if empty.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Minimum gap between bounding boxes in px (default: 4.0).
    pub padding: Option<f64>,
    /// Maximum number of resolution iterations (default: 100, max: 500).
    pub max_iterations: Option<usize>,
}

/// Arguments for `snap_to_pixel` tool
#[derive(Debug, Deserialize)]
pub struct SnapToPixelArgs {
    /// IDs of nodes whose position should be rounded to the nearest integer.
    pub node_ids: Vec<Uuid>,
}

// ─── Scissors Cut Args ───────────────────────────────────────────────────────

/// Arguments for the `scissors_cut` tool.
#[derive(Debug, Deserialize)]
pub struct ScissorsCutArgs {
    /// ID of the path node to cut.
    pub node_id: Uuid,
    /// X coordinate in document (canvas) space of the cut point.
    pub canvas_x: f64,
    /// Y coordinate in document (canvas) space of the cut point.
    pub canvas_y: f64,
}

// ─── Guide Args ──────────────────────────────────────────────────────────────

/// Arguments for `add_guide` tool.
#[derive(Debug, Deserialize)]
pub struct AddGuideArgs {
    /// "horizontal" for a fixed-Y guide, "vertical" for a fixed-X guide.
    pub orientation: String,
    /// Position in document units (Y for horizontal, X for vertical).
    pub position: f64,
    /// Optional override color as [R, G, B, A] in [0,1] range.
    #[serde(default)]
    pub color: Option<[f32; 4]>,
}

/// Arguments for `remove_guide` tool.
#[derive(Debug, Deserialize)]
pub struct RemoveGuideArgs {
    /// UUID of the guide to remove.
    pub guide_id: Uuid,
}

/// Arguments for `list_guides` and `clear_guides` (no parameters).
#[derive(Debug, Deserialize, Default)]
pub struct ListGuidesArgs {}

#[derive(Debug, Deserialize, Default)]
pub struct ClearGuidesArgs {}

// ─── Magic Wand Select Args ───────────────────────────────────────────────────

/// Arguments for the `magic_wand_select` tool.
#[derive(Debug, Deserialize)]
pub struct MagicWandSelectArgs {
    /// X coordinate in document (canvas) space to click.
    pub canvas_x: f64,
    /// Y coordinate in document (canvas) space to click.
    pub canvas_y: f64,
    /// Which attribute to match across all nodes.
    #[serde(default)]
    pub attribute: SelectSameAttribute,
    /// Tolerance for numeric/color comparisons. Defaults to 0.01.
    #[serde(default)]
    pub tolerance: Option<f64>,
}

// ─── Convert Anchor Points Args ──────────────────────────────────────────────

/// Arguments for the `convert_anchor_points` tool.
#[derive(Debug, Deserialize, Default)]
pub struct ConvertAnchorPointsArgs {
    /// IDs of path nodes to convert. Non-path nodes are skipped.
    pub node_ids: Vec<Uuid>,
    /// Conversion mode: "smooth" makes junction handles collinear; "corner" retracts handles to anchor points (cusps).
    #[serde(default)]
    pub mode: ConvertAnchorMode,
}

/// Anchor point conversion mode.
#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConvertAnchorMode {
    /// Make junction handles collinear through each interior anchor.
    #[default]
    Smooth,
    /// Retract cubic handles to their anchor points (sharp cusps).
    Corner,
}

// ─── Lasso Select Args ───────────────────────────────────────────────────────

/// Arguments for the `lasso_select` tool.
#[derive(Debug, Deserialize, Default)]
pub struct LassoSelectArgs {
    /// Polygon boundary in canvas (document) coordinates. Each element is `[x, y]`.
    /// Minimum 3 points. The polygon is automatically closed.
    pub points: Vec<[f64; 2]>,
    /// When true (default), select nodes whose bounding-box centroid is inside the polygon.
    /// When false, select nodes whose AABB fully intersects — i.e. at least one corner is inside.
    #[serde(default = "default_true")]
    pub centroid_mode: bool,
    /// When true, add to the existing selection instead of replacing it.
    #[serde(default)]
    pub additive: bool,
}
