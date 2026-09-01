use super::*;
use photonic_core::{
    color::Color, layer::BlendMode, ops::boolean::BooleanOp, style::LineJoin, DropShadow, Feather,
    GaussianGlow, GlowEffect, ObjectBlur,
};
use serde::Deserialize;
use uuid::Uuid;

fn default_spiral_segs() -> usize {
    16
}

/// Arguments for `create_spiral` tool
#[derive(Debug, Deserialize)]
pub struct CreateSpiralArgs {
    /// X coordinate of the spiral center.
    pub x: f64,
    /// Y coordinate of the spiral center.
    pub y: f64,
    /// Outer (maximum) radius in document units.
    pub outer_radius: f64,
    /// Inner (minimum) radius. Use 0 for a true spiral from the center.
    #[serde(default)]
    pub inner_radius: f64,
    /// Number of full rotations (e.g. 3.0 = three turns).
    pub turns: f64,
    /// Cubic Bézier segments per full turn (default 16; maximum [`MAX_GENERATED_WORK`]).
    #[serde(default = "default_spiral_segs")]
    pub segments_per_turn: usize,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Arguments for `create_polar_grid` tool
#[derive(Debug, Deserialize)]
pub struct CreatePolarGridArgs {
    /// X coordinate of the center.
    pub x: f64,
    /// Y coordinate of the center.
    pub y: f64,
    /// Outer (maximum) radius in document units.
    pub outer_radius: f64,
    /// Inner (minimum) radius. Use 0 for a full-disk polar grid (default: 0).
    #[serde(default)]
    pub inner_radius: Option<f64>,
    /// Number of concentric rings (default: 4).
    #[serde(default)]
    pub rings: Option<u32>,
    /// Number of radial sector dividers (default: 8).
    #[serde(default)]
    pub sectors: Option<u32>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Arguments for `create_grid` tool
#[derive(Debug, Deserialize)]
pub struct CreateGridArgs {
    /// X coordinate of the top-left corner.
    pub x: f64,
    /// Y coordinate of the top-left corner.
    pub y: f64,
    /// Total width of the grid.
    pub width: f64,
    /// Total height of the grid.
    pub height: f64,
    /// Number of columns (cell divisions horizontally). Default 4.
    #[serde(default)]
    pub cols: Option<u32>,
    /// Number of rows (cell divisions vertically). Default 4.
    #[serde(default)]
    pub rows: Option<u32>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Arguments for `create_path` tool
#[derive(Debug, Deserialize)]
pub struct CreatePathArgs {
    pub path_data: String,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub transform: Option<TransformArg>,
}

/// Arguments for `create_text` tool
#[derive(Debug, Deserialize)]
pub struct CreateTextArgs {
    pub content: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub font_size: Option<f64>,
    #[serde(default)]
    pub font_weight: Option<u16>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    /// "left" | "center" | "right"
    #[serde(default)]
    pub align: Option<String>,
    /// Line height multiplier (default: 1.2). 1.0 = tight, 2.0 = double-spaced.
    #[serde(default)]
    pub line_height: Option<f64>,
    /// Letter spacing in document units (default: 0.0). Positive = wider, negative = tighter.
    #[serde(default)]
    pub letter_spacing: Option<f64>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Glow effect parameters for `update_node`.
#[derive(Debug, Deserialize)]
pub struct GlowEffectArg {
    pub enabled: bool,
    /// Glow color as `[r, g, b, a]` in 0.0–1.0.
    pub color: [f32; 4],
    /// Overall opacity multiplier 0.0–1.0.
    pub opacity: f32,
    /// Glow spread radius in document units.
    pub size: f32,
    /// Corner join style: `"miter"` (default), `"round"`, or `"bevel"`.
    #[serde(default)]
    pub join: LineJoin,
}

impl From<GlowEffectArg> for GlowEffect {
    fn from(a: GlowEffectArg) -> Self {
        Self {
            enabled: a.enabled,
            color: Color {
                r: a.color[0],
                g: a.color[1],
                b: a.color[2],
                a: a.color[3],
            },
            opacity: a.opacity,
            size: a.size,
            join: a.join,
        }
    }
}

/// Gaussian glow effect parameters for `update_node`.
#[derive(Debug, Deserialize)]
pub struct GaussianGlowArg {
    pub enabled: bool,
    pub color: [f32; 4],
    pub opacity: f32,
    /// Blur radius (sigma) in document units.
    pub radius: f32,
}

impl From<GaussianGlowArg> for GaussianGlow {
    fn from(a: GaussianGlowArg) -> Self {
        Self {
            enabled: a.enabled,
            color: Color {
                r: a.color[0],
                g: a.color[1],
                b: a.color[2],
                a: a.color[3],
            },
            opacity: a.opacity,
            radius: a.radius,
        }
    }
}

/// Drop-shadow effect argument for `update_node`.
#[derive(Debug, Deserialize)]
pub struct DropShadowArg {
    pub enabled: bool,
    pub color: [f32; 4],
    pub opacity: f32,
    /// Offset in document units (positive dx = right, dy = down).
    pub dx: f32,
    pub dy: f32,
    /// Blur radius (sigma) in document units. 0 = hard-edged.
    pub blur: f32,
}

impl From<DropShadowArg> for DropShadow {
    fn from(a: DropShadowArg) -> Self {
        Self {
            enabled: a.enabled,
            color: Color {
                r: a.color[0],
                g: a.color[1],
                b: a.color[2],
                a: a.color[3],
            },
            opacity: a.opacity,
            dx: a.dx,
            dy: a.dy,
            blur: a.blur,
        }
    }
}

/// Object-blur effect argument for `update_node`.
#[derive(Debug, Deserialize)]
pub struct ObjectBlurArg {
    pub enabled: bool,
    /// Blur radius (sigma) in document units.
    pub radius: f32,
}

impl From<ObjectBlurArg> for ObjectBlur {
    fn from(a: ObjectBlurArg) -> Self {
        Self {
            enabled: a.enabled,
            radius: a.radius,
        }
    }
}

/// Feather effect argument for `update_node`.
#[derive(Debug, Deserialize)]
pub struct FeatherArg {
    pub enabled: bool,
    /// Feather radius in document units.
    pub radius: f32,
}

impl From<FeatherArg> for Feather {
    fn from(a: FeatherArg) -> Self {
        Self {
            enabled: a.enabled,
            radius: a.radius,
        }
    }
}

/// Arguments for `update_node` tool
#[derive(Debug, Deserialize)]
pub struct UpdateNodeArgs {
    pub node_id: Uuid,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    #[serde(default)]
    pub transform: Option<TransformArg>,
    #[serde(default)]
    pub opacity: Option<f32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub locked: Option<bool>,
    #[serde(default)]
    pub blend_mode: Option<BlendMode>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    // ── Text-node specific ────────────────────────────────────────────────────
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub font_size: Option<f64>,
    #[serde(default)]
    pub font_weight: Option<u16>,
    /// "left" | "center" | "right"
    #[serde(default)]
    pub text_align: Option<String>,
    #[serde(default)]
    pub outer_glow: Option<GlowEffectArg>,
    #[serde(default)]
    pub inner_glow: Option<GlowEffectArg>,
    #[serde(default)]
    pub gaussian_glow: Option<GaussianGlowArg>,
    #[serde(default)]
    pub drop_shadow: Option<DropShadowArg>,
    #[serde(default)]
    pub object_blur: Option<ObjectBlurArg>,
    #[serde(default)]
    pub feather: Option<FeatherArg>,
}

/// Arguments for `apply_transform` tool
#[derive(Debug, Deserialize)]
pub struct ApplyTransformArgs {
    #[serde(default)]
    pub node_ids: Vec<Uuid>,
    pub operation: TransformOperation,
    #[serde(default)]
    pub translate: Option<TranslateArg>,
    #[serde(default)]
    pub rotate: Option<RotateArg>,
    #[serde(default)]
    pub scale: Option<ScaleArg>,
    #[serde(default)]
    pub matrix: Option<[f64; 6]>,
    #[serde(default)]
    pub shear: Option<ShearArg>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformOperation {
    Translate,
    Rotate,
    Scale,
    Matrix,
    ReflectHorizontal,
    ReflectVertical,
    Shear,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TranslateArg {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RotateArg {
    pub angle_degrees: f64,
    #[serde(default)]
    pub origin_x: f64,
    #[serde(default)]
    pub origin_y: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScaleArg {
    pub sx: f64,
    pub sy: f64,
    #[serde(default)]
    pub origin_x: f64,
    #[serde(default)]
    pub origin_y: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShearArg {
    /// Horizontal shear factor: shifts x by `shear_x * y`.
    pub shear_x: f64,
    /// Vertical shear factor: shifts y by `shear_y * x`.
    #[serde(default)]
    pub shear_y: f64,
    /// X coordinate of the shear origin (default: 0).
    #[serde(default)]
    pub origin_x: f64,
    /// Y coordinate of the shear origin (default: 0).
    #[serde(default)]
    pub origin_y: f64,
}

/// Arguments for `create_layer` tool
#[derive(Debug, Deserialize)]
pub struct CreateLayerArgs {
    pub name: String,
    #[serde(default)]
    pub position: Option<usize>,
}

/// Arguments for `collect_in_new_layer` tool
#[derive(Debug, Deserialize)]
pub struct CollectInNewLayerArgs {
    /// IDs of nodes to collect. Group children are resolved to their top-level ancestor.
    pub node_ids: Vec<Uuid>,
    /// Name for the new layer (default: "Collected Layer").
    #[serde(default)]
    pub name: Option<String>,
    /// Position in the layer stack (0 = top/front; 1 = just below top). Defaults to top of stack.
    #[serde(default)]
    pub position: Option<usize>,
}

/// Arguments for `get_document_state` tool
#[derive(Debug, Deserialize, Default)]
pub struct GetDocumentStateArgs {
    #[serde(default)]
    pub include_path_data: bool,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    /// When true, return only {id, name, kind, z_index} per node — no styles or transforms.
    /// Use this when you only need to know what nodes exist, not their appearance.
    #[serde(default)]
    pub summary_only: bool,
}

/// Arguments for `get_node` tool
#[derive(Debug, Deserialize)]
pub struct GetNodeArgs {
    #[serde(default)]
    pub node_id: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Arguments for `screenshot` tool
#[derive(Debug, Deserialize, Default)]
pub struct ScreenshotArgs {
    #[serde(default)]
    pub scale: Option<f32>,
    #[serde(default)]
    pub region: Option<RegionArg>,
}

#[derive(Debug, Deserialize)]
pub struct RegionArg {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Arguments for `export` tool
#[derive(Debug, Deserialize)]
pub struct ExportArgs {
    pub format: ExportFormat,
    pub file_path: String,
    #[serde(default)]
    pub node_ids: Vec<Uuid>,
    #[serde(default)]
    pub dpi: Option<f64>,
    #[serde(default)]
    pub scale: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Svg,
    Png,
    Jpeg,
    Pdf,
}

/// Arguments for `delete_node` tool
#[derive(Debug, Deserialize)]
pub struct DeleteNodeArgs {
    pub node_ids: Vec<Uuid>,
}

/// Arguments for `undo`/`redo` tools
#[derive(Debug, Deserialize, Default)]
pub struct UndoRedoArgs {
    #[serde(default)]
    pub steps: Option<usize>,
}

/// Arguments for `create_checkpoint` tool
#[derive(Debug, Deserialize)]
pub struct CreateCheckpointArgs {
    pub name: String,
}

/// Arguments for `restore_checkpoint` tool
#[derive(Debug, Deserialize)]
pub struct RestoreCheckpointArgs {
    pub checkpoint_id: String,
}

/// Arguments for `diff_checkpoints` tool
#[derive(Debug, Deserialize)]
pub struct DiffCheckpointsArgs {
    /// UUID of the "from" (older/baseline) checkpoint.
    pub from_id: String,
    /// UUID of the "to" (newer/current) checkpoint.
    pub to_id: String,
}

/// Arguments for `reorder_node` tool
#[derive(Debug, Deserialize)]
pub struct ReorderNodeArgs {
    pub node_id: Uuid,
    pub operation: ReorderOperation,
    /// Required when operation is move_above or move_below.
    #[serde(default)]
    pub relative_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReorderOperation {
    SendToBack,
    BringToFront,
    SendBackward,
    BringForward,
    MoveAbove,
    MoveBelow,
}

/// Arguments for `group_nodes` tool
#[derive(Debug, Deserialize)]
pub struct GroupNodesArgs {
    pub node_ids: Vec<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
}

/// Arguments for `ungroup_nodes` tool
#[derive(Debug, Deserialize)]
pub struct UngroupNodesArgs {
    pub group_id: Uuid,
}

/// Arguments for `boolean_operation` tool
#[derive(Debug, Deserialize)]
pub struct BooleanOperationArgs {
    /// Base shape — result inherits its fill and stroke.
    pub target_id: Uuid,
    /// The cutting/combining shape (relevant for subtract).
    pub tool_id: Uuid,
    pub operation: BooleanOp,
    /// If true, original nodes are preserved alongside the result. Default: false.
    #[serde(default)]
    pub keep_originals: bool,
}

pub(super) fn default_true() -> bool {
    true
}

/// Arguments for `build_shape_from_points` tool
#[derive(Debug, Deserialize)]
pub struct BuildShapeFromPointsArgs {
    /// Array of [x, y] coordinate pairs defining the available vertices.
    pub points: Vec<[f64; 2]>,
    /// Indices into `points` defining the connection sequence.
    /// If omitted, connects points in order 0 → 1 → 2 → … → n-1.
    /// Use this to connect them in any custom order, e.g. [0, 2, 1, 3].
    #[serde(default)]
    pub connection_order: Option<Vec<usize>>,
    /// Whether to close the path back to the first connected point (default: true).
    #[serde(default = "default_true")]
    pub closed: bool,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Arguments for `align_nodes` tool
#[derive(Debug, Deserialize)]
pub struct AlignNodesArgs {
    /// IDs of the nodes to align (at least 2).
    pub node_ids: Vec<Uuid>,
    /// The alignment or distribution operation to perform.
    pub operation: AlignOperation,
    /// What to align relative to. Defaults to `selection` (the combined bounding box of all
    /// specified nodes). Use `canvas` to align relative to the document bounds.
    /// Use `key_object` combined with `key_object_id` to align to a specific node.
    #[serde(default)]
    pub anchor: AlignAnchor,
    /// When `anchor` is `key_object`, this node's bounding box is used as the fixed reference.
    /// The key object itself is not moved. Must be one of the `node_ids`.
    #[serde(default)]
    pub key_object_id: Option<Uuid>,
    /// When using `distribute_horizontal` or `distribute_vertical`, place exactly this many
    /// pixels between adjacent node edges. When omitted, nodes are evenly spaced so the two
    /// extremes stay fixed (existing behaviour).
    pub spacing: Option<f64>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum AlignOperation {
    /// Align left edges to the reference left edge.
    Left,
    /// Align horizontal centers to the reference horizontal center.
    CenterHorizontal,
    /// Align right edges to the reference right edge.
    Right,
    /// Align top edges to the reference top edge.
    Top,
    /// Align vertical centers to the reference vertical center.
    CenterVertical,
    /// Align bottom edges to the reference bottom edge.
    Bottom,
    /// Evenly space nodes horizontally (leftmost and rightmost nodes stay fixed).
    DistributeHorizontal,
    /// Evenly space nodes vertically (topmost and bottommost nodes stay fixed).
    DistributeVertical,
}

#[derive(Debug, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlignAnchor {
    /// Use the combined bounding box of all specified nodes as the reference. (default)
    #[default]
    Selection,
    /// Use the document canvas (0, 0, width, height) as the reference.
    Canvas,
    /// Use the bounding box of the node identified by `key_object_id` as the fixed reference.
    /// The key object itself is not moved.
    KeyObject,
}

/// Arguments for `find_nodes` tool.
/// All fields optional; combine with AND logic.
#[derive(Debug, Deserialize, Default)]
pub struct FindNodesArgs {
    /// Node must have ALL of these tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Node must have ANY of these tags.
    #[serde(default)]
    pub tags_any: Option<Vec<String>>,
    /// Case-insensitive substring match on node name.
    #[serde(default)]
    pub name_contains: Option<String>,
    /// "path" | "group" | "text"
    #[serde(default)]
    pub node_type: Option<String>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    /// If true, exclude invisible nodes (default: false).
    #[serde(default)]
    pub visible_only: Option<bool>,
    /// World-space AABB filter (reuses existing RegionArg).
    #[serde(default)]
    pub in_region: Option<RegionArg>,
    /// If true, return full node JSON; default false returns minimal summary.
    #[serde(default)]
    pub include_details: Option<bool>,
    /// Max results (default: 200).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Arguments for `duplicate_nodes` tool
#[derive(Debug, Deserialize)]
pub struct DuplicateNodesArgs {
    /// IDs of the nodes to duplicate.
    pub node_ids: Vec<Uuid>,
    /// How many copies to create per source node (default: 1, max: 100).
    #[serde(default)]
    pub count: Option<usize>,
    /// Position offset applied to each successive copy.
    /// Copy N is shifted by N * offset from the original.
    /// Default: {x: 10, y: 10}.
    #[serde(default)]
    pub offset: Option<TranslateArg>,
    /// Target layer for the copies. Defaults to the source node's layer.
    #[serde(default)]
    pub layer_id: Option<Uuid>,
}

/// Maximum total number of cells that `create_array` may materialize in grid mode.
pub const MAX_ARRAY_GRID_CELLS: usize = MAX_GENERATED_WORK;

/// Arguments for `create_array` tool — repeat a node in a grid or radial pattern.
#[derive(Debug, Deserialize)]
pub struct CreateArrayArgs {
    /// The source node to repeat. It stays in place; new copies are created around it.
    pub node_id: Uuid,
    /// Layout mode: `"grid"` or `"radial"`.
    pub mode: ArrayMode,

    // ── Grid params (ignored for radial) ─────────────────────────────────
    /// Number of rows in the grid (default 2). The source is row 0, col 0.
    /// The total grid size may not exceed [`MAX_ARRAY_GRID_CELLS`].
    #[serde(default)]
    pub rows: Option<usize>,
    /// Number of columns in the grid (default 2).
    /// The total grid size may not exceed [`MAX_ARRAY_GRID_CELLS`].
    #[serde(default)]
    pub cols: Option<usize>,
    /// Horizontal distance (px) between column centres (default 100).
    #[serde(default)]
    pub col_stride: Option<f64>,
    /// Vertical distance (px) between row centres (default 100).
    #[serde(default)]
    pub row_stride: Option<f64>,

    // ── Radial params (ignored for grid) ─────────────────────────────────
    /// Total number of instances including the source (default 6, min 2).
    /// The source counts as instance 0 — so `count = 6` creates 5 new copies.
    #[serde(default)]
    pub count: Option<usize>,
    /// X coordinate of the rotation centre (default 0).
    #[serde(default)]
    pub center_x: Option<f64>,
    /// Y coordinate of the rotation centre (default 0).
    #[serde(default)]
    pub center_y: Option<f64>,
    /// Angle in degrees at which the first copy is placed, measured clockwise
    /// from the source position. Remaining copies are evenly spread to fill
    /// 360°. Default: 0 (evenly distributed starting from the source angle).
    #[serde(default)]
    pub start_angle_degrees: Option<f64>,

    // ── Common ────────────────────────────────────────────────────────────
    /// If true, wrap all copies AND the source node into a new group. Default false.
    #[serde(default)]
    pub group_result: bool,
    /// Target layer for the copies. Defaults to the source node's layer.
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    /// Name prefix for generated copies, e.g. "Petal" → "Petal 1", "Petal 2", …
    /// Defaults to the source node's name.
    #[serde(default)]
    pub name_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrayMode {
    Grid,
    Radial,
}

/// Arguments for `style_transfer` tool
#[derive(Debug, Deserialize)]
pub struct StyleTransferArgs {
    /// The node whose visual style will be copied.
    pub source_id: Uuid,
    /// One or more nodes that will receive the style.
    pub target_ids: Vec<Uuid>,
    /// Which properties to copy. Valid values: "fill", "stroke", "opacity", "blend_mode".
    /// If omitted or empty, all four are copied.
    #[serde(default)]
    pub properties: Option<Vec<String>>,
}

/// Arguments for `find_replace_style` tool.
///
/// Searches every node in the document (or a scoped subset) for matching
/// style properties, then replaces them in a single undoable batch.
///
/// **Search criteria** (at least one required):
/// - `fill_color` / `stroke_color` — match by hex color with optional tolerance
/// - `stroke_width` — match by stroke width (fractional tolerance via `color_tolerance`)
/// - `font_family` — match text nodes by font family name (case-insensitive, exact)
///
/// **Replacements** (at least one required):
/// - `new_fill_color`, `new_stroke_color`, `new_opacity`
/// - `new_stroke_width`, `new_font_family`
///
/// Color matching works on solid fills **and** individual gradient stop /
/// fluid-point / mesh-vertex colors, so a gradient that uses the target color
/// in one stop will be partially updated.
#[derive(Debug, Deserialize, Default)]
pub struct FindReplaceStyleArgs {
    /// Hex color to search for in fills (solid or gradient stops). e.g. `"#FF0000"`.
    #[serde(default)]
    pub fill_color: Option<String>,
    /// Hex color to search for in strokes (enabled strokes only). e.g. `"#000000"`.
    #[serde(default)]
    pub stroke_color: Option<String>,
    /// Stroke width to search for (on enabled strokes). e.g. `2.0`.
    /// Fractional tolerance applies: `color_tolerance = 0.1` matches ±10% of this value.
    #[serde(default)]
    pub stroke_width: Option<f64>,
    /// Font family name to search for on text nodes (case-insensitive exact match).
    /// e.g. `"Inter"`.
    #[serde(default)]
    pub font_family: Option<String>,
    /// How similar a color must be to count as a match. `0.0` = exact (default),
    /// `1.0` = any color matches.  Distance is normalized Euclidean in linear
    /// RGB: `sqrt((r₁-r₂)² + (g₁-g₂)² + (b₁-b₂)²) / √3`.
    /// Also used as fractional tolerance for `stroke_width` matching.
    #[serde(default)]
    pub color_tolerance: Option<f32>,
    /// Replace every matched fill color (solid or stop) with this hex color.
    #[serde(default)]
    pub new_fill_color: Option<String>,
    /// Replace every matched stroke color with this hex color.
    #[serde(default)]
    pub new_stroke_color: Option<String>,
    /// Override the node-level opacity for every matched node. Range 0–1.
    #[serde(default)]
    pub new_opacity: Option<f32>,
    /// Replace every matched stroke width with this value (in document units).
    #[serde(default)]
    pub new_stroke_width: Option<f64>,
    /// Replace the font family on every matched text node.
    #[serde(default)]
    pub new_font_family: Option<String>,
    /// Restrict the search to nodes on this layer.
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    /// Restrict the search to these specific node IDs.
    #[serde(default)]
    pub node_ids: Option<Vec<Uuid>>,
    /// When `true`, return what would change but do not mutate the document.
    /// Useful for auditing before committing a large batch replacement.
    #[serde(default)]
    pub dry_run: bool,
}

// ─── FindReplaceTextArgs ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FindReplaceTextArgs {
    /// Text to search for. Plain string by default; treated as a regex when `regex: true`.
    pub find: String,
    /// Replacement string. When `regex: true`, capture group back-references ($1, $2, …) are supported.
    pub replace: String,
    /// Treat `find` as a regular expression. Default: false.
    #[serde(default)]
    pub regex: bool,
    /// Case-sensitive match. Default: true.
    #[serde(default = "default_true")]
    pub case_sensitive: bool,
    /// Preview matches without applying changes. Default: false.
    #[serde(default)]
    pub dry_run: bool,
    /// Scope to specific text node UUIDs. Omit to search all text nodes in the document.
    #[serde(default)]
    pub node_ids: Option<Vec<Uuid>>,
}

/// Arguments for `import_svg` tool
#[derive(Debug, Deserialize)]
pub struct ImportSvgArgs {
    #[serde(default)]
    pub svg_string: Option<String>,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
}

// ─── Shared argument types ───────────────────────────────────────────────────

/// A single control point for a fluid gradient (MCP input).
#[derive(Debug, Deserialize, Clone)]
pub struct FluidPointArg {
    pub x: f64,
    pub y: f64,
    pub color: String,
}

/// A single vertex for a mesh gradient (MCP input).
#[derive(Debug, Deserialize, Clone)]
pub struct MeshVertexArg {
    pub x: f64,
    pub y: f64,
    pub color: String,
}

/// Fill specification from an MCP client.
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FillArg {
    None,
    Solid {
        color: String,
    },
    Gradient {
        gradient_type: Option<String>,
        colors: Vec<String>,
        #[serde(default)]
        offsets: Option<Vec<f32>>,
        #[serde(default)]
        coords: Option<Vec<f64>>,
        /// Coordinate space for `coords`. `"user"` (default) = absolute
        /// document/world coordinates. `"bbox"` (a.k.a. `"objectBoundingBox"`) =
        /// coordinates are 0–1 relative to the target node's bounding box, and
        /// are resolved to absolute coords per node when applied via `set_paint`.
        /// Lets one "left→right blue→purple" gradient be reused across every icon
        /// with zero per-node coordinate bookkeeping.
        #[serde(default)]
        units: Option<String>,
    },
    /// Fluid (free-point) gradient: colors blended via inverse-distance weighting.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "type": "fluid_gradient",
    ///   "points": [
    ///     {"x": 100, "y": 50,  "color": "#ff0000"},
    ///     {"x": 300, "y": 50,  "color": "#0000ff"},
    ///     {"x": 200, "y": 200, "color": "#00ff00"}
    ///   ],
    ///   "power": 2.0
    /// }
    /// ```
    FluidGradient {
        points: Vec<FluidPointArg>,
        #[serde(default)]
        power: Option<f32>,
    },
    /// Mesh (vertex-grid) gradient: rows×cols grid of coloured vertices with
    /// bilinear interpolation within each cell.
    ///
    /// Example (2×2 grid):
    /// ```json
    /// {
    ///   "type": "mesh_gradient",
    ///   "rows": 2,
    ///   "cols": 2,
    ///   "vertices": [
    ///     {"x": 0,   "y": 0,   "color": "#ff0000"},
    ///     {"x": 200, "y": 0,   "color": "#00ff00"},
    ///     {"x": 0,   "y": 200, "color": "#0000ff"},
    ///     {"x": 200, "y": 200, "color": "#ffff00"}
    ///   ]
    /// }
    /// ```
    MeshGradient {
        rows: u32,
        cols: u32,
        vertices: Vec<MeshVertexArg>,
    },
    /// A tiled raster pattern fill. The tile is supplied inline as a base64 PNG,
    /// so the fill is fully self-contained (consistent with how gradients carry
    /// their own stops). For an ergonomic, reusable workflow prefer
    /// `define_pattern` + `apply_pattern_fill`, which resolve from the document
    /// pattern registry.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "type": "pattern",
    ///   "tile_base64": "<base64 PNG>",
    ///   "tile_type": "grid",
    ///   "scale": 1.0,
    ///   "rotation_degrees": 0.0,
    ///   "offset": [0, 0],
    ///   "spacing": 0.0
    /// }
    /// ```
    Pattern {
        /// Base64-encoded PNG (or any image format) for the tile.
        tile_base64: String,
        #[serde(default)]
        tile_type: Option<String>,
        #[serde(default)]
        scale: Option<f64>,
        #[serde(default)]
        rotation_degrees: Option<f64>,
        #[serde(default)]
        offset: Option<[f64; 2]>,
        #[serde(default)]
        spacing: Option<f64>,
    },
}

impl FillArg {
    /// If this is a bbox-units gradient, return a copy with `coords` resolved to
    /// absolute user-space against `bbox` = (x, y, w, h); otherwise clone
    /// unchanged. This is how one relative gradient definition is re-fit to each
    /// node's own bounding box (issue #202).
    pub fn resolved_for_bbox(&self, bbox: (f64, f64, f64, f64)) -> FillArg {
        let (bx, by, bw, bh) = bbox;
        match self {
            FillArg::Gradient {
                gradient_type,
                colors,
                offsets,
                coords,
                units,
            } if matches!(
                units.as_deref(),
                Some("bbox") | Some("objectBoundingBox") | Some("object_bounding_box")
            ) =>
            {
                let is_radial = gradient_type.as_deref() == Some("radial");
                let c = coords.clone().unwrap_or_else(|| {
                    if is_radial {
                        vec![0.5, 0.5, 0.5]
                    } else {
                        vec![0.0, 0.0, 1.0, 0.0]
                    }
                });
                let g = |i: usize, d: f64| c.get(i).copied().unwrap_or(d);
                let resolved = if is_radial {
                    let cx = bx + g(0, 0.5) * bw;
                    let cy = by + g(1, 0.5) * bh;
                    // Radius as a fraction of the larger box dimension.
                    let r = g(2, 0.5) * bw.max(bh);
                    vec![cx, cy, r]
                } else {
                    vec![
                        bx + g(0, 0.0) * bw,
                        by + g(1, 0.0) * bh,
                        bx + g(2, 1.0) * bw,
                        by + g(3, 0.0) * bh,
                    ]
                };
                FillArg::Gradient {
                    gradient_type: gradient_type.clone(),
                    colors: colors.clone(),
                    offsets: offsets.clone(),
                    coords: Some(resolved),
                    units: None,
                }
            }
            other => other.clone(),
        }
    }

    /// Convert to a `photonic_core::style::Fill`. Returns an error if colors can't be parsed.
    pub fn to_fill(&self) -> Result<photonic_core::style::Fill, String> {
        use photonic_core::style::{
            Fill, FluidGradient, FluidGradientPoint, Gradient, GradientStop, MeshGradient,
        };
        match self {
            FillArg::None => Ok(Fill::none()),
            FillArg::Solid { color } => {
                let c =
                    Color::from_hex(color).ok_or_else(|| format!("Invalid color: {}", color))?;
                Ok(Fill::solid(c))
            }
            FillArg::Gradient {
                gradient_type,
                colors,
                offsets,
                coords,
                units: _,
            } => {
                let parsed: Result<Vec<Color>, _> = colors
                    .iter()
                    .map(|c| Color::from_hex(c).ok_or_else(|| format!("Invalid color: {}", c)))
                    .collect();
                let parsed = parsed?;
                let stops: Vec<GradientStop> = parsed
                    .into_iter()
                    .enumerate()
                    .map(|(i, color)| {
                        let offset = offsets
                            .as_ref()
                            .and_then(|o| o.get(i).copied())
                            .unwrap_or(i as f32 / (colors.len() - 1).max(1) as f32);
                        GradientStop::new(offset, color)
                    })
                    .collect();

                let is_radial = gradient_type.as_deref() == Some("radial");
                let gradient = if is_radial {
                    let c = coords.as_deref().unwrap_or(&[0.5, 0.5, 0.5]);
                    Gradient::radial(
                        c.first().copied().unwrap_or(0.5),
                        c.get(1).copied().unwrap_or(0.5),
                        c.get(2).copied().unwrap_or(0.5),
                        stops,
                    )
                } else {
                    let c = coords.as_deref().unwrap_or(&[0.0, 0.0, 1.0, 0.0]);
                    Gradient::linear(
                        c.first().copied().unwrap_or(0.0),
                        c.get(1).copied().unwrap_or(0.0),
                        c.get(2).copied().unwrap_or(1.0),
                        c.get(3).copied().unwrap_or(0.0),
                        stops,
                    )
                };
                Ok(Fill::gradient(gradient))
            }
            FillArg::FluidGradient { points, power } => {
                let pts: Result<Vec<FluidGradientPoint>, String> = points
                    .iter()
                    .map(|p| {
                        let color = Color::from_hex(&p.color)
                            .ok_or_else(|| format!("Invalid color: {}", p.color))?;
                        Ok(FluidGradientPoint::new(p.x, p.y, color))
                    })
                    .collect();
                let mut fg = FluidGradient::new(pts?);
                if let Some(pw) = power {
                    fg.power = *pw;
                }
                Ok(Fill::fluid_gradient(fg))
            }
            FillArg::MeshGradient {
                rows,
                cols,
                vertices,
            } => {
                // The mesh is now a grid of colored cells; use the provided
                // colors (in order) as the `rows`×`cols` cell colors.
                let colors: Result<Vec<Color>, String> = vertices
                    .iter()
                    .map(|v| {
                        Color::from_hex(&v.color)
                            .ok_or_else(|| format!("Invalid color: {}", v.color))
                    })
                    .collect();
                Ok(Fill::mesh_gradient(MeshGradient::grid(
                    *rows, *cols, colors?,
                )))
            }
            FillArg::Pattern {
                tile_base64,
                tile_type,
                scale,
                rotation_degrees,
                offset,
                spacing,
            } => {
                use base64::Engine;
                use photonic_core::style::{PatternFill, PatternTileType};
                use photonic_core::RasterImage;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(tile_base64.as_bytes())
                    .map_err(|e| format!("Invalid base64 tile: {}", e))?;
                let tile = RasterImage::from_encoded(&bytes)
                    .map_err(|e| format!("Failed to decode pattern tile: {}", e))?;
                let mut pat = PatternFill::new(tile);
                if let Some(t) = tile_type {
                    pat.tile_type = PatternTileType::from_label(t)
                        .ok_or_else(|| format!("Unknown tile_type: {}", t))?;
                }
                if let Some(s) = scale {
                    pat.scale = *s;
                }
                if let Some(r) = rotation_degrees {
                    pat.rotation = r.to_radians();
                }
                if let Some(o) = offset {
                    pat.offset = *o;
                }
                if let Some(sp) = spacing {
                    pat.spacing = *sp;
                }
                Ok(Fill::pattern(pat))
            }
        }
    }
}
