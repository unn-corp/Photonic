use super::*;
use serde::Deserialize;
use uuid::Uuid;

// ─── MCP Tool Call args/results ───────────────────────────────────────────────

/// Arguments for `save_document`.
#[derive(Debug, Deserialize, Default)]
pub struct SaveDocumentArgs {
    /// Native `.photon` destination. Omit to save to the current MCP document path.
    #[serde(default)]
    pub path: Option<String>,
}

/// Arguments for `copy_nodes_to_clipboard` tool
#[derive(Debug, Deserialize)]
pub struct CopyNodesToClipboardArgs {
    /// IDs of the nodes to copy into the clipboard.
    pub node_ids: Vec<Uuid>,
    /// Optional human-readable label for this clipboard entry.
    /// Defaults to "N node(s)".
    #[serde(default)]
    pub label: Option<String>,
}

/// Arguments for `get_clipboard_history` tool (no parameters).
#[derive(Debug, Deserialize, Default)]
pub struct GetClipboardHistoryArgs {}

/// Arguments for `paste_from_history` tool
#[derive(Debug, Deserialize)]
pub struct PasteFromHistoryArgs {
    /// Zero-based index into the clipboard ring (0 = most recent).
    pub index: usize,
    /// Horizontal offset applied to the pasted nodes (default: 0).
    #[serde(default)]
    pub offset_x: Option<f64>,
    /// Vertical offset applied to the pasted nodes (default: 0).
    #[serde(default)]
    pub offset_y: Option<f64>,
    /// Target layer for pasted nodes. Defaults to the document's active layer.
    #[serde(default)]
    pub layer_id: Option<Uuid>,
}

/// Arguments for `export_design_tokens` tool
#[derive(Debug, Deserialize, Default)]
pub struct ExportDesignTokensArgs {
    /// Output format: "json" (default) | "css" | "tailwind" | "style-dictionary"
    #[serde(default)]
    pub format: Option<String>,
}

/// Arguments for `import_design_tokens` tool — the counterpart to
/// `export_design_tokens`. Registers named color swatches from a tokens payload
/// (CSS custom properties, JSON, or style-dictionary), so brand colors can be
/// referenced by name (via `apply_color_swatch`) instead of hard-coded per icon.
#[derive(Debug, Deserialize, Default)]
pub struct ImportDesignTokensArgs {
    /// Inline tokens text (CSS `:root{--x:#hex}`, JSON, or style-dictionary).
    /// Provide this OR `path`.
    #[serde(default)]
    pub content: Option<String>,
    /// Path to a tokens file to read. Provide this OR `content`.
    #[serde(default)]
    pub path: Option<String>,
    /// Parse hint: "auto" (default) | "css" | "json".
    #[serde(default)]
    pub format: Option<String>,
    /// Optional prefix prepended to every imported swatch name (e.g. "brand/").
    #[serde(default)]
    pub prefix: Option<String>,
    /// Clear all existing swatches before importing (default: false).
    #[serde(default)]
    pub clear_existing: bool,
}

/// Arguments for `list_audit_log` tool
#[derive(Debug, Deserialize, Default)]
pub struct ListAuditLogArgs {
    /// Maximum number of entries to return (default 50, capped at 1000 and the
    /// audit response byte budget).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Arguments for `export_audit_log` tool (no parameters needed; the response
/// is bounded by the audit export byte budget).
#[derive(Debug, Deserialize, Default)]
pub struct ExportAuditLogArgs {}

/// Arguments for `export_svg` tool
#[derive(Debug, Deserialize, Default)]
pub struct ExportSvgArgs {
    /// When true, return only the inner SVG content without the outer <svg> wrapper.
    #[serde(default)]
    pub inner_only: bool,
    /// Emit slugified node/layer names as `id` attributes (default: true).
    pub semantic_ids: Option<bool>,
    /// Decimal precision for coordinate values, clamped 1–6 (default: 4).
    pub precision: Option<u8>,
}

/// Arguments for the `preview_selection` tool (issue #204) — render the selection
/// at target display sizes over light/dark backgrounds as a contact sheet, so
/// small-size legibility can be judged inside Photonic without a round-trip.
#[derive(Debug, Deserialize, Default)]
pub struct PreviewSelectionArgs {
    /// Node IDs to preview. If omitted/empty, uses the current selection.
    #[serde(default)]
    pub node_ids: Option<Vec<String>>,
    /// Target pixel sizes to render (default: [24, 32, 48]).
    #[serde(default)]
    pub sizes: Option<Vec<u32>>,
    /// Background swatches (hex) to composite each size over
    /// (default: ["#ffffff", "#0b0b12"]).
    #[serde(default)]
    pub backgrounds: Option<Vec<String>>,
    /// Padding as a fraction of the icon's square size (default: 0.15).
    #[serde(default)]
    pub pad: Option<f64>,
}

/// Arguments for the `export_pdf` tool.
#[derive(Debug, Deserialize, Default)]
pub struct ExportPdfArgs {
    /// Optional page background colour (`#rrggbb`). Omit for an unpainted
    /// (white-in-viewers) page.
    #[serde(default)]
    pub background: Option<String>,
    /// Also write the PDF to this filesystem path (base64 is still returned).
    #[serde(default)]
    pub path: Option<String>,
    /// Convert text to vector outlines (zero font deps). Default false.
    #[serde(default)]
    pub outline_text: Option<bool>,
    /// Render trim + registration marks. Default false.
    #[serde(default)]
    pub marks: Option<bool>,
    /// Colour model: "rgb" (default) or "cmyk".
    #[serde(default)]
    pub color_mode: Option<String>,
    /// CMYK ICC profile path (defaults to the bundled FOGRA39 when cmyk).
    #[serde(default)]
    pub profile: Option<String>,
    /// Per-artboard / multi-page export. Specific artboards, each a UUID, an
    /// exact name, or a 1-based index string ("1", "2", …). One clipped PDF page
    /// per artboard, in the given order. Omit (with `all`/`range` unset) to export
    /// the whole canvas as a single page.
    #[serde(default)]
    pub artboards: Option<Vec<String>>,
    /// Export every artboard, one page each (overrides `range`/`artboards`).
    #[serde(default)]
    pub all: Option<bool>,
    /// Export a 1-based inclusive artboard index range `[start, end]`.
    #[serde(default)]
    pub range: Option<[usize; 2]>,
    /// When exporting multiple artboards, write one file PER artboard instead of a
    /// single multi-page PDF. Requires `path` to contain a `{name}` or `{index}`
    /// placeholder (or an extension, before which `-{index}` is inserted).
    #[serde(default)]
    pub separate_files: Option<bool>,
}

/// Arguments for `export_selection_as_svg` tool
#[derive(Debug, Deserialize, Default)]
pub struct ExportSelectionArgs {
    /// Node IDs to export. If omitted or empty, uses the current document selection.
    #[serde(default)]
    pub node_ids: Option<Vec<String>>,
    /// When true, wrap the SVG in a React functional component (default: false).
    #[serde(default)]
    pub as_react_component: bool,
    /// Component name when `as_react_component` is true (default: "SvgIcon").
    pub component_name: Option<String>,
    /// Decimal precision for coordinates AND path data, clamped 1–6 (default: 4).
    /// Prevents bloated 15-decimal selection exports.
    #[serde(default)]
    pub precision: Option<u8>,
    /// viewBox framing: `"tight"` (default) = union bbox; `"square"` = a uniform
    /// centered square so a set of icons all export at the same aspect/scale.
    #[serde(default)]
    pub normalize: Option<String>,
    /// Padding as a fraction of the square side when `normalize: "square"`
    /// (e.g. 0.1 = 10% breathing room). Default 0.
    #[serde(default)]
    pub pad: Option<f64>,
}

/// One icon in an `export_icon_set` batch.
#[derive(Debug, Deserialize)]
pub struct IconSetEntryArg {
    /// Output file name (a `.svg` extension is added if absent).
    pub name: String,
    /// Node IDs comprising this icon (a single group id is fine).
    pub node_ids: Vec<String>,
}

/// Arguments for the `export_icon_set` tool — batch-export N tagged groups to
/// normalized `.svg` files in one call (the canonical icon-pipeline workflow).
#[derive(Debug, Deserialize, Default)]
pub struct ExportIconSetArgs {
    /// Explicit list of icons to export. If omitted, every top-level group in the
    /// document is exported, named by its (slugified) group name.
    #[serde(default)]
    pub icons: Option<Vec<IconSetEntryArg>>,
    /// Directory to write the `.svg` files into. If omitted, the SVGs are returned
    /// inline in the tool result instead of written to disk.
    #[serde(default)]
    pub out_dir: Option<String>,
    /// Decimal precision for coordinates and path data, clamped 1–6 (default: 4).
    #[serde(default)]
    pub precision: Option<u8>,
    /// viewBox framing: `"square"` (default here) or `"tight"`.
    #[serde(default)]
    pub normalize: Option<String>,
    /// Padding fraction for square normalization (default 0.1).
    #[serde(default)]
    pub pad: Option<f64>,
}

/// Arguments for `zig_zag_path` tool
#[derive(Debug, Deserialize)]
pub struct ZigZagPathArgs {
    /// Path node IDs to apply the zig-zag to.
    pub node_ids: Vec<String>,
    /// Peak-to-peak amplitude of the zig-zag (in document units). Default: 10.
    pub size: Option<f64>,
    /// Number of ridges (peaks) per original path segment. Default: 4.
    pub ridges_per_segment: Option<usize>,
    /// If true, produce smooth waves (cubic bezier); if false, sharp corners (line segments). Default: false.
    #[serde(default)]
    pub smooth: bool,
}

/// Arguments for `pucker_bloat` tool
#[derive(Debug, Deserialize)]
pub struct PuckerBloatArgs {
    /// Path node IDs to distort.
    pub node_ids: Vec<String>,
    /// Distortion strength: positive = bloat (expand outward), negative = pucker (contract inward).
    /// Range roughly -1.0 to 1.0 for subtle effects; larger values are more extreme. Default: 0.5.
    pub strength: Option<f64>,
    /// X coordinate of the distortion center. Defaults to the path's centroid.
    pub center_x: Option<f64>,
    /// Y coordinate of the distortion center. Defaults to the path's centroid.
    pub center_y: Option<f64>,
}

/// Arguments for `tag_nodes` tool
#[derive(Debug, Deserialize)]
pub struct TagNodesArgs {
    /// Node IDs. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Tags to add.
    #[serde(default)]
    pub add: Vec<String>,
    /// Tags to remove.
    #[serde(default)]
    pub remove: Vec<String>,
}

/// Arguments for `sample_color_at` tool
#[derive(Debug, Deserialize)]
pub struct SampleColorAtArgs {
    /// Canvas X coordinate.
    pub x: f64,
    /// Canvas Y coordinate.
    pub y: f64,
}

/// Arguments for `set_active_layer` tool
#[derive(Debug, Deserialize)]
pub struct SetActiveLayerArgs {
    /// Layer UUID or name.
    pub layer_id: String,
}

/// Arguments for `delete_layer` tool
#[derive(Debug, Deserialize)]
pub struct DeleteLayerArgs {
    /// Layer UUID or name to delete.
    pub layer_id: String,
    /// If true, also delete all nodes on the layer (default: false — nodes moved to first remaining layer).
    #[serde(default)]
    pub delete_nodes: bool,
}

/// Arguments for `move_to_layer` tool
#[derive(Debug, Deserialize)]
pub struct MoveToLayerArgs {
    /// Node IDs to move. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Target layer UUID or name.
    pub target_layer: String,
}

/// Arguments for `add_dimension_line` tool
#[derive(Debug, Deserialize)]
pub struct AddDimensionLineArgs {
    /// Start X.
    pub x1: f64,
    /// Start Y.
    pub y1: f64,
    /// End X.
    pub x2: f64,
    /// End Y.
    pub y2: f64,
    /// Offset distance for the dimension line from the measured points (default: 20).
    pub offset: Option<f64>,
    /// Font size for the label (default: 12).
    pub font_size: Option<f64>,
    /// Color hex for the dimension line (default: "#666666").
    pub color: Option<String>,
    pub layer_id: Option<String>,
}

/// Arguments for `reorder_layers` tool
#[derive(Debug, Deserialize)]
pub struct ReorderLayersArgs {
    /// New layer order as an array of layer UUIDs (bottom to top).
    pub layer_order: Vec<String>,
}

/// Arguments for `set_selection` tool
#[derive(Debug, Deserialize)]
pub struct SetSelectionArgs {
    /// Node IDs to select. Empty = clear selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// If true, add to existing selection instead of replacing (default: false).
    #[serde(default)]
    pub additive: bool,
}

/// Arguments for `flatten_group` tool
#[derive(Debug, Deserialize, Default)]
pub struct FlattenGroupArgs {
    /// Group node IDs to flatten. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
}

/// Arguments for `center_on_canvas` tool
#[derive(Debug, Deserialize, Default)]
pub struct CenterOnCanvasArgs {
    /// Node IDs. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Center horizontally (default: true).
    #[serde(default = "default_true_fn")]
    pub horizontal: bool,
    /// Center vertically (default: true).
    #[serde(default = "default_true_fn")]
    pub vertical: bool,
}

/// Arguments for `remove_fill` / `remove_stroke` tools
#[derive(Debug, Deserialize, Default)]
pub struct RemoveStyleArgs {
    /// Node IDs. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
}

/// Arguments for `fit_to_canvas` tool
#[derive(Debug, Deserialize, Default)]
pub struct FitToCanvasArgs {
    /// Padding in document units around the edges (default: 10).
    pub padding: Option<f64>,
    /// Node IDs to fit. Empty = all visible nodes.
    #[serde(default)]
    pub node_ids: Vec<String>,
}

/// Arguments for `create_scatter_plot` tool
#[derive(Debug, Deserialize)]
pub struct CreateScatterPlotArgs {
    /// Left X.
    pub x: f64,
    /// Bottom Y.
    pub y: f64,
    /// Plot width (default: 300).
    pub width: Option<f64>,
    /// Plot height (default: 300).
    pub height: Option<f64>,
    /// Data points as [x, y] pairs.
    pub points: Vec<[f64; 2]>,
    /// Dot radius (default: 4).
    pub dot_radius: Option<f64>,
    /// Dot color hex (default: "#4E79A7").
    pub color: Option<String>,
    pub layer_id: Option<String>,
}

/// Arguments for `scatter_copies` tool
#[derive(Debug, Deserialize)]
pub struct ScatterCopiesArgs {
    /// Source node ID to scatter.
    pub node_id: String,
    /// Number of copies (default: 20; maximum: [`MAX_GENERATED_WORK`]).
    pub count: Option<usize>,
    /// Scatter area left X.
    pub x: f64,
    /// Scatter area top Y.
    pub y: f64,
    /// Area width.
    pub width: f64,
    /// Area height.
    pub height: f64,
    /// Random rotation range in degrees (default: 0 = no rotation). Each copy gets random rotation in [-range, +range].
    pub rotation_range: Option<f64>,
    /// Scale variation range. Each copy scaled between [1-range, 1+range]. Default: 0 (no variation).
    pub scale_range: Option<f64>,
    /// Random seed (default: 42).
    pub seed: Option<u64>,
}

/// Arguments for `create_line_chart` tool
#[derive(Debug, Deserialize)]
pub struct CreateLineChartArgs {
    /// Left X.
    pub x: f64,
    /// Bottom Y (data grows upward).
    pub y: f64,
    /// Chart width (default: 300).
    pub width: Option<f64>,
    /// Chart height (default: 200).
    pub height: Option<f64>,
    /// Data series. Each series is an array of values.
    pub series: Vec<Vec<f64>>,
    /// Colors for each series as hex.
    #[serde(default)]
    pub colors: Vec<String>,
    /// Stroke width for lines (default: 2).
    pub stroke_width: Option<f64>,
    /// Smooth the lines using Catmull-Rom interpolation (default: true).
    #[serde(default = "default_true_fn")]
    pub smooth: bool,
    /// Fill area under each line (default: false).
    #[serde(default)]
    pub fill_area: bool,
    pub layer_id: Option<String>,
}

/// Arguments for `create_bar_chart` tool
#[derive(Debug, Deserialize)]
pub struct CreateBarChartArgs {
    /// Left X.
    pub x: f64,
    /// Bottom Y (bars grow upward).
    pub y: f64,
    /// Total chart width (default: 300).
    pub width: Option<f64>,
    /// Max bar height (default: 200).
    pub height: Option<f64>,
    /// Data values — each bar height is proportional to its value.
    pub values: Vec<f64>,
    /// Bar colors as hex. Cycles if fewer than values.
    #[serde(default)]
    pub colors: Vec<String>,
    /// Labels for each bar (optional).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Gap between bars as fraction of bar width (default: 0.2).
    pub gap: Option<f64>,
    /// If true, bars are horizontal instead of vertical (default: false).
    #[serde(default)]
    pub horizontal: bool,
    pub layer_id: Option<String>,
}

/// Arguments for `create_pie_chart` tool
#[derive(Debug, Deserialize)]
pub struct CreatePieChartArgs {
    /// Center X.
    pub cx: f64,
    /// Center Y.
    pub cy: f64,
    /// Radius (default: 80).
    pub radius: Option<f64>,
    /// Data values — each slice is proportional to its value.
    pub values: Vec<f64>,
    /// Slice colors as hex strings. Cycles if fewer than values.
    #[serde(default)]
    pub colors: Vec<String>,
    /// Labels for each slice (optional).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Inner radius for donut chart (default: 0 = solid pie).
    pub inner_radius: Option<f64>,
    pub layer_id: Option<String>,
}

/// Shape type for `create_parametric_shape`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParametricShapeType {
    /// Lissajous curve: x = A·sin(a·t + delta), y = B·sin(b·t).
    Lissajous,
    /// Superellipse (Lamé curve): |x/a|^n + |y/b|^n = 1.
    Superellipse,
    /// Rose curve (polar): r = cos(k·θ).
    Rose,
    /// Hypotrochoid: x = (R-r)·cos(t) + d·cos((R-r)/r·t), y = (R-r)·sin(t) - d·sin((R-r)/r·t).
    Hypotrochoid,
    /// Epicycloid: (R+r) circle rolling outside base circle R. d = r for standard epicycloid.
    Epicycloid,
}

/// Arguments for `create_parametric_shape` tool
#[derive(Debug, Deserialize)]
pub struct CreateParametricShapeArgs {
    /// Center X.
    pub cx: f64,
    /// Center Y.
    pub cy: f64,
    /// Shape type.
    pub shape_type: ParametricShapeType,
    /// Overall scale / outer radius (default: 80).
    pub radius: Option<f64>,
    /// Lissajous/Superellipse: X semi-axis ratio relative to radius (default: 1.0).
    pub ratio_x: Option<f64>,
    /// Lissajous/Superellipse: Y semi-axis ratio relative to radius (default: 1.0).
    pub ratio_y: Option<f64>,
    /// Lissajous: frequency ratio `a` (default: 3).
    pub freq_a: Option<f64>,
    /// Lissajous: frequency ratio `b` (default: 2).
    pub freq_b: Option<f64>,
    /// Lissajous: phase delta in degrees (default: 90).
    pub delta_deg: Option<f64>,
    /// Superellipse: exponent n (default: 2.5; 2 = ellipse, >2 = squircle, <2 = astroid-like).
    pub exponent: Option<f64>,
    /// Rose: number of petals k. Even k → 2k petals, odd k → k petals (default: 5).
    pub petals: Option<f64>,
    /// Hypotrochoid/Epicycloid: rolling circle radius r as fraction of R (default: 0.4).
    pub inner_ratio: Option<f64>,
    /// Hypotrochoid: pen distance d as fraction of r (default: 1.0 = standard hypocycloid).
    pub pen_ratio: Option<f64>,
    /// Number of sample points to generate the path (default: 360).
    pub points: Option<usize>,
    /// Fill style.
    pub fill: Option<FillArg>,
    /// Stroke style.
    pub stroke: Option<StrokeArg>,
    pub layer_id: Option<String>,
}

/// Arguments for `create_stacked_bar_chart` tool
#[derive(Debug, Deserialize)]
pub struct CreateStackedBarChartArgs {
    /// Left X (vertical) or top-left X (horizontal).
    pub x: f64,
    /// Bottom Y (vertical bars grow upward) or top Y (horizontal bars grow rightward).
    pub y: f64,
    /// Total chart width (default: 300).
    pub width: Option<f64>,
    /// Total chart height (default: 200).
    pub height: Option<f64>,
    /// Data series. Each series is one dataset (e.g. one category). All series must have the same length.
    pub series: Vec<Vec<f64>>,
    /// Colors for each series as hex. Cycles if fewer than series count.
    #[serde(default)]
    pub colors: Vec<String>,
    /// Labels for each stack position (column/row). Optional.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Series names for node labeling. Optional.
    #[serde(default)]
    pub series_names: Vec<String>,
    /// Gap between stacks as fraction of bar width (default: 0.2).
    pub gap: Option<f64>,
    /// If true, bars are horizontal instead of vertical (default: false).
    #[serde(default)]
    pub horizontal: bool,
    pub layer_id: Option<String>,
}

/// Arguments for `create_radar_chart` tool
#[derive(Debug, Deserialize)]
pub struct CreateRadarChartArgs {
    /// Center X.
    pub cx: f64,
    /// Center Y.
    pub cy: f64,
    /// Outer radius of the chart (default: 100).
    pub radius: Option<f64>,
    /// Data series. Each series is an array of values, one per axis. All series must have equal length.
    pub series: Vec<Vec<f64>>,
    /// Axis labels (one per axis). Optional.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Series names for legend / node labeling. Optional.
    #[serde(default)]
    pub series_names: Vec<String>,
    /// Fill colors for each series as hex. Cycles if fewer than series.
    #[serde(default)]
    pub colors: Vec<String>,
    /// Stroke width for series polygons (default: 1.5).
    pub stroke_width: Option<f64>,
    /// Number of concentric grid rings (default: 4).
    pub grid_rings: Option<usize>,
    /// Fill series polygons with semi-transparent color (default: true).
    #[serde(default = "default_true_fn")]
    pub fill_area: bool,
    pub layer_id: Option<String>,
}

// ─── CreateTruchetTilingArgs ──────────────────────────────────────────────────

/// Arguments for `create_truchet_tiling` tool
#[derive(Debug, Deserialize)]
pub struct CreateTruchetTilingArgs {
    /// Top-left X of the tiling region.
    pub x: f64,
    /// Top-left Y of the tiling region.
    pub y: f64,
    /// Width of the tiling region (default: 200).
    pub width: Option<f64>,
    /// Height of the tiling region (default: 200).
    pub height: Option<f64>,
    /// Size of each individual tile (default: 40). Minimum 4.
    pub tile_size: Option<f64>,
    /// Tile style: "arcs" (default, quarter-circle arcs), "diagonals" (straight diagonal), or "triangles".
    #[serde(default)]
    pub style: Option<String>,
    /// Random seed for reproducible tilings (default: 42).
    pub seed: Option<u64>,
    /// Stroke/fill color for tile patterns as hex (default: "#1a1a2e").
    #[serde(default)]
    pub color: Option<String>,
    /// Background fill color as hex. If absent, no background rectangle is added.
    #[serde(default)]
    pub background: Option<String>,
    /// Stroke width for arc/diagonal styles (default: 2.0).
    pub stroke_width: Option<f64>,
    pub layer_id: Option<String>,
}

/// Arguments for `point_on_path` tool
#[derive(Debug, Deserialize)]
pub struct PointOnPathArgs {
    /// Path node ID.
    pub node_id: String,
    /// Position(s) along the path as fractions 0.0–1.0. Can be a single value or array.
    pub t: Vec<f64>,
}

/// Arguments for `create_speech_bubble` tool
#[derive(Debug, Deserialize)]
pub struct CreateSpeechBubbleArgs {
    /// Center X of the bubble body.
    pub cx: f64,
    /// Center Y of the bubble body.
    pub cy: f64,
    /// Bubble body width (default: 120).
    pub width: Option<f64>,
    /// Bubble body height (default: 60).
    pub height: Option<f64>,
    /// Corner radius (default: 15).
    pub corner_radius: Option<f64>,
    /// Tail tip X coordinate (where the tail points to).
    pub tail_x: Option<f64>,
    /// Tail tip Y coordinate.
    pub tail_y: Option<f64>,
    /// Tail width at the base (default: 20).
    pub tail_width: Option<f64>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    pub layer_id: Option<String>,
}

/// Arguments for `set_visibility` tool
#[derive(Debug, Deserialize)]
pub struct SetVisibilityArgs {
    /// Node IDs. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Visible state. Omit to toggle.
    pub visible: Option<bool>,
}

/// Arguments for `set_locked` tool
#[derive(Debug, Deserialize)]
pub struct SetLockedArgs {
    /// Node IDs. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Locked state. Omit to toggle.
    pub locked: Option<bool>,
}

/// Arguments for `select_all` tool
#[derive(Debug, Deserialize, Default)]
pub struct SelectAllArgs {
    /// If provided, only select nodes on this layer (UUID or name).
    pub layer_id: Option<String>,
}

/// Arguments for `deselect_all` tool (no arguments needed).
#[derive(Debug, Deserialize, Default)]
pub struct DeselectAllArgs {}

/// Arguments for `set_blend_mode` tool
#[derive(Debug, Deserialize)]
pub struct SetBlendModeArgs {
    /// Node IDs. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Blend mode name: normal, multiply, screen, overlay, darken, lighten, color_dodge, color_burn, hard_light, soft_light, difference, exclusion, hue, saturation, color, luminosity.
    pub blend_mode: String,
}

/// Arguments for `set_opacity` tool
#[derive(Debug, Deserialize)]
pub struct SetOpacityArgs {
    /// Node IDs. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Opacity value 0.0–1.0.
    pub opacity: f32,
}

/// Arguments for `randomize_colors` tool
#[derive(Debug, Deserialize)]
pub struct RandomizeColorsArgs {
    /// Node IDs. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Color palette as hex strings. If omitted, generates random colors.
    #[serde(default)]
    pub palette: Vec<String>,
    /// Random seed (default: 42).
    pub seed: Option<u64>,
    /// Randomize fill colors (default: true).
    #[serde(default = "default_true_fn")]
    pub fill: bool,
    /// Randomize stroke colors (default: false).
    #[serde(default)]
    pub stroke: bool,
}

fn default_true_fn() -> bool {
    true
}

/// Arguments for `duplicate_layer` tool
#[derive(Debug, Deserialize)]
pub struct DuplicateLayerArgs {
    /// Layer ID (UUID or name) to duplicate.
    pub layer_id: String,
    /// Name for the duplicated layer. Defaults to "<original name> Copy".
    pub name: Option<String>,
}

/// Arguments for `swap_fill_stroke` tool
#[derive(Debug, Deserialize, Default)]
pub struct SwapFillStrokeArgs {
    /// Node IDs. Empty = use selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
}

/// Arguments for `resize_canvas` tool
#[derive(Debug, Deserialize)]
pub struct ResizeCanvasArgs {
    /// New canvas width.
    pub width: f64,
    /// New canvas height.
    pub height: f64,
}

/// Arguments for `create_heart` tool
#[derive(Debug, Deserialize)]
pub struct CreateHeartArgs {
    /// Center X.
    pub cx: f64,
    /// Center Y (bottom tip of heart).
    pub cy: f64,
    /// Heart size (width, default: 60).
    pub size: Option<f64>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    pub layer_id: Option<String>,
}

/// Arguments for `create_gear` tool
#[derive(Debug, Deserialize)]
pub struct CreateGearArgs {
    /// Center X.
    pub cx: f64,
    /// Center Y.
    pub cy: f64,
    /// Outer radius (tip of teeth, default: 50).
    pub outer_radius: Option<f64>,
    /// Inner radius (base of teeth, default: 35).
    pub inner_radius: Option<f64>,
    /// Hole radius (center hole, default: 10). Set to 0 for no hole.
    pub hole_radius: Option<f64>,
    /// Number of teeth (default: 12).
    pub teeth: Option<usize>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    pub layer_id: Option<String>,
}

/// Arguments for `create_qr_code` tool
#[derive(Debug, Deserialize)]
pub struct CreateQrCodeArgs {
    /// Content to encode (URL or text). Required.
    pub data: String,
    /// Module shape: "square" (default), "rounded", "dot", or "connected" (blob).
    #[serde(default)]
    pub module_shape: Option<String>,
    /// Error-correction level: "l", "m" (default), "q", or "h". Higher = more
    /// robust (needed for a centre logo) but denser.
    #[serde(default)]
    pub ecc: Option<String>,
    /// Corner radius as a fraction of one module, 0..=0.5 (default 0.4). Used by
    /// "rounded" and "connected".
    #[serde(default)]
    pub radius: Option<f64>,
    /// Total artwork size in document units incl. quiet zone (default 200).
    #[serde(default)]
    pub size: Option<f64>,
    /// Quiet-zone margin in modules (default 4; keep >= 2 to stay scannable).
    #[serde(default)]
    pub quiet_zone: Option<u32>,
    /// Fill for the dark modules — solid or gradient (default solid black).
    #[serde(default)]
    pub fill: Option<FillArg>,
    /// Background fill behind the code: a `#rrggbb` hex (default white) or
    /// `"none"` for transparent. A light background keeps the code scannable on
    /// any canvas.
    #[serde(default)]
    pub background: Option<String>,
    /// Top-left X (default 0).
    #[serde(default)]
    pub x: Option<f64>,
    /// Top-left Y (default 0).
    #[serde(default)]
    pub y: Option<f64>,
    pub layer_id: Option<String>,
}

/// Arguments for `flip_nodes` tool
#[derive(Debug, Deserialize)]
pub struct FlipNodesArgs {
    /// Node IDs to flip. Empty = use current selection.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Flip axis: "horizontal" (mirror across vertical axis) or "vertical" (mirror across horizontal axis).
    pub axis: String,
}

/// Arguments for `create_cross` tool
#[derive(Debug, Deserialize)]
pub struct CreateCrossArgs {
    /// Center X.
    pub cx: f64,
    /// Center Y.
    pub cy: f64,
    /// Total size (width and height of the cross, default: 60).
    pub size: Option<f64>,
    /// Arm thickness (default: 20).
    pub thickness: Option<f64>,
    /// Rotation in degrees (default: 0). Set to 45 for an X shape.
    pub rotation: Option<f64>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    pub layer_id: Option<String>,
}

/// Arguments for `measure_path` tool
#[derive(Debug, Deserialize)]
pub struct MeasurePathArgs {
    /// Path node ID to measure.
    pub node_id: String,
}

/// Arguments for `measure_distance` tool
#[derive(Debug, Deserialize)]
pub struct MeasureDistanceArgs {
    /// First point [x, y] or node ID.
    pub from: MeasureTarget,
    /// Second point [x, y] or node ID.
    pub to: MeasureTarget,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MeasureTarget {
    Point([f64; 2]),
    NodeId(String),
}

/// Arguments for `create_arrow_shape` tool
#[derive(Debug, Deserialize)]
pub struct CreateArrowShapeArgs {
    /// Arrow tip X.
    pub x: f64,
    /// Arrow tip Y.
    pub y: f64,
    /// Total arrow length (default: 100).
    pub length: Option<f64>,
    /// Arrow head width (default: 40).
    pub head_width: Option<f64>,
    /// Arrow head depth as fraction of length (default: 0.4).
    pub head_depth: Option<f64>,
    /// Shaft width (default: 16).
    pub shaft_width: Option<f64>,
    /// Direction in degrees. 0 = pointing right. (default: 0).
    pub direction: Option<f64>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    pub layer_id: Option<String>,
}

/// Arguments for `create_donut` tool
#[derive(Debug, Deserialize)]
pub struct CreateDonutArgs {
    /// Center X.
    pub cx: f64,
    /// Center Y.
    pub cy: f64,
    /// Outer radius (default: 50).
    pub outer_radius: Option<f64>,
    /// Inner radius (default: 25). Creates the hole.
    pub inner_radius: Option<f64>,
    /// Start angle in degrees for partial arcs (default: 0). 0 = full ring.
    pub start_angle: Option<f64>,
    /// End angle in degrees for partial arcs (default: 360). Set < 360 for a partial donut.
    pub end_angle: Option<f64>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    pub layer_id: Option<String>,
}

/// Arguments for `create_sunburst` tool
#[derive(Debug, Deserialize)]
pub struct CreateSunburstArgs {
    /// Center X coordinate.
    pub cx: f64,
    /// Center Y coordinate.
    pub cy: f64,
    /// Inner radius (default: 20).
    pub inner_radius: Option<f64>,
    /// Outer radius (default: 100).
    pub outer_radius: Option<f64>,
    /// Number of rays (default: 24). Must be even for alternating wedges.
    pub rays: Option<usize>,
    /// Fill color hex for the rays (default: "#FFD700" gold).
    pub color: Option<String>,
    /// Layer ID.
    pub layer_id: Option<String>,
}

/// Arguments for `create_wave_pattern` tool
#[derive(Debug, Deserialize)]
pub struct CreateWavePatternArgs {
    /// Left X coordinate.
    pub x: f64,
    /// Top Y coordinate.
    pub y: f64,
    /// Pattern width.
    pub width: f64,
    /// Pattern height.
    pub height: f64,
    /// Number of wave lines (default: 8).
    pub lines: Option<usize>,
    /// Wavelength in document units (default: 40).
    pub wavelength: Option<f64>,
    /// Amplitude in document units (default: 10).
    pub amplitude: Option<f64>,
    /// Stroke style.
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    /// Layer ID.
    pub layer_id: Option<String>,
}

/// Arguments for `hatch_fill` tool
#[derive(Debug, Deserialize)]
pub struct HatchFillArgs {
    /// Path node IDs to fill with hatching.
    pub node_ids: Vec<String>,
    /// Spacing between hatch lines (default: 5.0).
    pub spacing: Option<f64>,
    /// Angle of hatch lines in degrees (default: 45).
    pub angle: Option<f64>,
    /// Second angle for cross-hatching. Omit for single-direction hatching.
    pub cross_angle: Option<f64>,
    /// Line stroke width (default: 1.0).
    pub stroke_width: Option<f64>,
    /// Line color hex (default: uses path fill color).
    pub color: Option<String>,
}

/// Arguments for `stipple_fill` tool
#[derive(Debug, Deserialize)]
pub struct StippleFillArgs {
    /// Path node IDs — the shapes to fill with stipple dots.
    pub node_ids: Vec<String>,
    /// Number of dots to place (default: 200).
    pub count: Option<usize>,
    /// Dot radius in document units (default: 1.5).
    pub dot_radius: Option<f64>,
    /// Dot color hex (default: uses the path's fill color).
    pub color: Option<String>,
    /// Random seed (default: 42).
    pub seed: Option<u64>,
}

/// Arguments for `add_drop_shadow` tool
#[derive(Debug, Deserialize)]
pub struct AddDropShadowArgs {
    /// Node IDs to add shadows to.
    pub node_ids: Vec<String>,
    /// Shadow X offset (default: 5).
    pub offset_x: Option<f64>,
    /// Shadow Y offset (default: 5).
    pub offset_y: Option<f64>,
    /// Shadow color as hex (default: "#000000").
    pub color: Option<String>,
    /// Shadow opacity 0–1 (default: 0.4).
    pub opacity: Option<f32>,
}

/// Arguments for `transform_copies` tool
#[derive(Debug, Deserialize)]
pub struct TransformCopiesArgs {
    /// Source node ID to copy.
    pub node_id: String,
    /// Number of copies to create (default: 5).
    pub copies: Option<usize>,
    /// X translation offset per copy (default: 0).
    #[serde(default)]
    pub translate_x: Option<f64>,
    /// Y translation offset per copy (default: 0).
    #[serde(default)]
    pub translate_y: Option<f64>,
    /// Rotation per copy in degrees (default: 0).
    #[serde(default)]
    pub rotate: Option<f64>,
    /// Scale factor per copy (default: 1.0 = no scaling). 0.9 = shrink 10% each copy.
    #[serde(default)]
    pub scale: Option<f64>,
    /// Opacity change per copy (multiplied cumulatively, default: 1.0).
    #[serde(default)]
    pub opacity_step: Option<f32>,
}

/// Arguments for `round_corners` tool
#[derive(Debug, Deserialize)]
pub struct RoundCornersArgs {
    /// Path node IDs.
    pub node_ids: Vec<String>,
    /// Corner radius in document units (default: 10).
    pub radius: Option<f64>,
}

/// Arguments for `create_flare` tool
#[derive(Debug, Deserialize)]
pub struct CreateFlareArgs {
    /// Center X coordinate.
    pub cx: f64,
    /// Center Y coordinate.
    pub cy: f64,
    /// Halo radius in document units (default: 50).
    pub halo_radius: Option<f64>,
    /// Number of radiating rays (default: 12; maximum: [`MAX_GENERATED_WORK`]).
    pub ray_count: Option<usize>,
    /// Length of rays beyond the halo (default: 80).
    pub ray_length: Option<f64>,
    /// Number of concentric rings (default: 3; maximum: [`MAX_GENERATED_WORK`]).
    pub ring_count: Option<usize>,
    /// Halo color as hex (default: "#fffbe6" warm yellow).
    pub halo_color: Option<String>,
    /// Ray opacity 0.0–1.0 (default: 0.3).
    pub ray_opacity: Option<f32>,
    /// Layer ID (default: active layer).
    pub layer_id: Option<String>,
}

/// Arguments for `warp_envelope` tool
#[derive(Debug, Deserialize)]
pub struct WarpEnvelopeArgs {
    /// Path node IDs to warp.
    pub node_ids: Vec<String>,
    /// Warp preset: "arc", "bulge", "wave", "flag", "squeeze", "inflate", "fisheye".
    pub warp_type: String,
    /// Primary bend amount (-1.0 to 1.0, default: 0.5). Positive = bend down/right.
    pub bend: Option<f64>,
    /// Horizontal distortion (-1.0 to 1.0, default: 0). Only used by some presets.
    pub distort_h: Option<f64>,
    /// Vertical distortion (-1.0 to 1.0, default: 0). Only used by some presets.
    pub distort_v: Option<f64>,
}

/// Arguments for `crystallize_path` tool
#[derive(Debug, Deserialize)]
pub struct CrystallizePathArgs {
    /// Path node IDs to crystallize.
    pub node_ids: Vec<String>,
    /// Height of each spike in document units (default: 10).
    pub size: Option<f64>,
    /// Number of spikes per original segment (default: 3).
    pub count: Option<usize>,
}

/// Arguments for `scallop_path` tool
#[derive(Debug, Deserialize)]
pub struct ScallopPathArgs {
    /// Path node IDs to apply scallop to.
    pub node_ids: Vec<String>,
    /// Depth of each scallop arc in document units (default: 10). Positive = inward.
    pub depth: Option<f64>,
    /// Number of scallop arcs per original segment (default: 1).
    pub count: Option<usize>,
}

/// Arguments for `blend_objects` tool
#[derive(Debug, Deserialize)]
pub struct BlendObjectsArgs {
    /// First (start) path node ID.
    pub node_id_a: String,
    /// Second (end) path node ID.
    pub node_id_b: String,
    /// Number of intermediate steps to generate (default: 5). Ignored when `smooth_color` is true or `spacing` is set.
    pub steps: Option<usize>,
    /// Smooth Color mode: auto-compute steps so each step changes color by ≤ 1 RGB unit (0–255 scale).
    /// When true, `steps` is ignored.
    #[serde(default)]
    pub smooth_color: bool,
    /// Specified Distance mode: space blend steps by this many pixels (world-space).
    /// Steps = ceil(center_distance / spacing). When set, `steps` and `smooth_color` are ignored.
    pub spacing: Option<f64>,
}

/// Arguments for `twirl_path` tool
#[derive(Debug, Deserialize)]
pub struct TwirlPathArgs {
    /// Path node IDs to twirl.
    pub node_ids: Vec<String>,
    /// Rotation angle in degrees. Positive = counter-clockwise. Default: 90.
    pub angle: Option<f64>,
    /// X coordinate of twirl center. Defaults to path centroid.
    pub center_x: Option<f64>,
    /// Y coordinate of twirl center. Defaults to path centroid.
    pub center_y: Option<f64>,
}

/// Arguments for `proportional_move_anchor` tool
#[derive(Debug, Deserialize)]
pub struct ProportionalMoveAnchorArgs {
    /// Path node ID (UUID or name) to edit.
    pub node_id: String,
    /// Element indices of the anchor point(s) to move directly (the "primary"
    /// anchors, weight 1.0). Positional indices into the path's element list, as
    /// reported by `inspect_node` / `get_node`.
    pub anchor_indices: Vec<usize>,
    /// Displacement of the primary anchors in the path's local X (path units).
    pub dx: f64,
    /// Displacement of the primary anchors in the path's local Y (path units).
    pub dy: f64,
    /// Falloff radius ("spread") in path units — neighbours within this distance
    /// of a primary anchor follow proportionally. Default: 120.
    pub spread: Option<f64>,
    /// Falloff curve exponent: 1 = linear, >1 = sharp (influence concentrated
    /// near the primary), <1 = soft/broad plateau. Default: 2.
    pub curve: Option<f64>,
}

/// Arguments for `roughen_path` tool
#[derive(Debug, Deserialize)]
pub struct RoughenPathArgs {
    /// Path node IDs to roughen.
    pub node_ids: Vec<String>,
    /// Maximum displacement in document units (default: 5.0).
    pub size: Option<f64>,
    /// Number of points to add per segment before roughening (0 = roughen existing points only). Default: 0.
    pub detail: Option<usize>,
    /// Random seed for reproducible results. Default: 42.
    pub seed: Option<u64>,
}

/// Arguments for `create_curvature_path` tool
#[derive(Debug, Deserialize)]
pub struct CreateCurvaturePathArgs {
    /// Ordered array of `[x, y]` canvas-space points the curve passes through.
    pub points: Vec<[f64; 2]>,
    /// If true, close the curve back to the first point (default: false).
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub fill: Option<FillArg>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    /// Layer to place the node on. Defaults to the active layer.
    pub layer_id: Option<String>,
}

/// Arguments for `delete_anchor_point` tool
#[derive(Debug, Deserialize)]
pub struct DeleteAnchorPointArgs {
    /// Path node ID (UUID or name).
    pub node_id: String,
    /// Zero-based indices of BezPath elements to remove.
    pub anchor_indices: Vec<usize>,
}

/// Arguments for `export_raster` tool
#[derive(Debug, Deserialize, Default)]
pub struct ExportRasterArgs {
    /// Output format: "png" (default), "jpeg", "webp", "gif", or "tiff".
    #[serde(default)]
    pub format: Option<String>,
    /// Output width in pixels. Defaults to document width. When resizing, the
    /// dimensions must be at most 16384×16384 and 67108864 pixels total.
    pub width: Option<u32>,
    /// Output height in pixels. Defaults to document height. When resizing, the
    /// dimensions must be at most 16384×16384 and 67108864 pixels total.
    pub height: Option<u32>,
    /// JPEG quality 1–100 (default: 90). Ignored for PNG.
    pub quality: Option<u8>,
    /// Write the encoded image to this filesystem path (parent dirs are created)
    /// and return a small result (path + dimensions) instead of the base64 payload.
    /// Omit to return base64 inline. Keeps full-resolution PNGs off the MCP socket.
    #[serde(default)]
    pub path: Option<String>,
}

/// Arguments for the `export_artboards` tool.
#[derive(Debug, Deserialize, Default)]
pub struct ExportArtboardsArgs {
    /// Specific artboards to export — each a UUID, an exact name, or a 1-based
    /// index string ("1", "2", …). Exported in the order given.
    #[serde(default)]
    pub artboards: Option<Vec<String>>,
    /// Inclusive 1-based index range `[start, end]` (e.g. `[2, 5]` = artboards 2–5).
    #[serde(default)]
    pub range: Option<[usize; 2]>,
    /// Export every artboard in document order.
    #[serde(default)]
    pub all: Option<bool>,
    /// Output format: "png" (default), "jpeg", "webp", "gif", or "tiff".
    #[serde(default)]
    pub format: Option<String>,
    /// Output pixels per document unit (default 1.0). Clamped to 0.05–8.0.
    #[serde(default)]
    pub scale: Option<f64>,
    /// Transparent background instead of the artboard fill (default false).
    #[serde(default)]
    pub transparent: Option<bool>,
    /// JPEG/WebP quality 1–100.
    #[serde(default)]
    pub quality: Option<u8>,
    /// Write each artboard's encoded image to disk (parent dirs created) and return
    /// a small result (path + dimensions) instead of base64. With multiple artboards,
    /// use a `{name}`/`{index}` placeholder in the path (or an extension, before which
    /// `-{index}` is inserted). Omit to return base64 inline.
    #[serde(default)]
    pub path: Option<String>,
    /// Expand each artboard's rectangle by the document bleed (`bleed_mm` at the
    /// export scale/DPI) on all four sides, so the PNG includes the print bleed area.
    /// Default false (trim only).
    #[serde(default)]
    pub bleed: Option<bool>,
}

/// Arguments for `create_shape` tool
#[derive(Debug, Deserialize)]
pub struct CreateShapeArgs {
    pub shape_type: ShapeType,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub rx: Option<f64>,
    #[serde(default)]
    pub sides: Option<usize>,
    #[serde(default)]
    pub inner_radius: Option<f64>,
    /// Corner radius for rounded_rect shapes (default: 10.0).
    #[serde(default)]
    pub corner_radius: Option<f64>,
    /// Arc start angle in degrees (0° = 3 o'clock). Default 0.
    #[serde(default)]
    pub arc_start_angle: Option<f64>,
    /// Arc end angle in degrees. Default 270 (¾ circle).
    #[serde(default)]
    pub arc_end_angle: Option<f64>,
    /// If true, draw an open arc (no chord back to center). Default false = closed pie.
    #[serde(default)]
    pub arc_open: Option<bool>,
    #[serde(default)]
    pub fill: Option<FillArg>,
    /// Convenience shorthand for a solid fill colour (`#rrggbb`). Ignored when
    /// `fill` is also provided.
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub stroke: Option<StrokeArg>,
    #[serde(default)]
    pub layer_id: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

pub use photonic_core::PrimitiveKind as ShapeType;
