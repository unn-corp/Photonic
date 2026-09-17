use crate::{
    annotation::{Annotation, AnnotationId},
    layer::{Layer, LayerId},
    node::{NodeId, SceneNode, SceneNodeKind},
    selection::Selection,
    style::PatternFill,
    Color,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Where a node lives in the scene hierarchy: directly in a layer, or nested
/// inside a group. Used by reparenting (drag-and-drop in the Layers panel, #210).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeContainer {
    Layer(LayerId),
    Group(NodeId),
}

/// A clash detected during [`Document::merge_3way`] that the auto-merge could
/// not resolve; `ours` is kept and the node is reported for manual resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeConflict {
    /// Both sides edited the same node differently.
    BothEdited(NodeId),
    /// One side edited the node while the other deleted it.
    EditVsDelete(NodeId),
}

impl MergeConflict {
    pub fn node_id(&self) -> NodeId {
        match self {
            MergeConflict::BothEdited(id) | MergeConflict::EditVsDelete(id) => *id,
        }
    }
}

/// Result of [`Document::merge_3way`]: the merged document plus any conflicts.
#[derive(Debug, Clone)]
pub struct MergeOutcome {
    pub merged: Document,
    pub conflicts: Vec<MergeConflict>,
}

/// Whether a guide is horizontal (fixed Y) or vertical (fixed X).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuideOrientation {
    Horizontal,
    Vertical,
}

/// A ruler guide — a horizontal or vertical reference line across the canvas.
/// Stored in the document; stripped from all export formats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub id: Uuid,
    /// Orientation of this guide.
    pub orientation: GuideOrientation,
    /// Position in document units: Y for horizontal guides, X for vertical guides.
    pub position: f64,
    /// Optional override color as [R, G, B, A] in [0,1] range.
    /// When `None`, the renderer uses the default guide color (cyan).
    #[serde(default)]
    pub color: Option<[f32; 4]>,
    /// Locked guides cannot be moved or deleted.
    #[serde(default)]
    pub locked: bool,
    /// When set, this is an angled construction line through (`position_x`, `position_y`)
    /// at the given angle in degrees (0° = horizontal, 90° = vertical, any angle allowed).
    /// The `position` and `orientation` fields are unused for angled guides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_degrees: Option<f64>,
    /// X coordinate for the origin point of an angled construction line.
    #[serde(default)]
    pub position_x: f64,
    /// Y coordinate for the origin point of an angled construction line.
    #[serde(default)]
    pub position_y: f64,
}

impl Guide {
    pub fn new(orientation: GuideOrientation, position: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            orientation,
            position,
            color: None,
            locked: false,
            angle_degrees: None,
            position_x: 0.0,
            position_y: 0.0,
        }
    }
}

/// File format version written into every saved `.photon` file.
/// Increment this when a breaking schema change is made.
///
/// - v1 → v2: introduced `SceneNodeKind::Raster` (pixel layers). Additive — v1
///   files contain no raster nodes and load unchanged; the migration is a
///   no-op version bump (see `migration::migrations`).
/// - v2 → v3: introduced the video-editor `timeline` field (01 §2). Additive —
///   `timeline` is `Option` + `#[serde(default)]`, so v2 files load untouched;
///   the migration is a no-op version bump.
/// - v3 → v4: clip anchors gained an explicit coordinate space. The migration
///   tags existing base and per-format reframe transforms as absolute without
///   changing their stored values.
/// - v4 → v5: the §35 scope / marker / group model (folded with the 01 §9.1
///   sibling changes). Tracks, sequence masters and media assets gain
///   effect+grade scopes; markers gain duration/category/anchor; `ClipEffect`
///   gains `id`/`version`; the unknown-preserving enum variants land. All
///   additive (serde defaults) except the deprecated `link_group` →
///   `GroupKind::AvLink` projection. The compiler now applies the effect scopes
///   in normative order (02 §2 / 35 §2.4).
pub const CURRENT_FORMAT_VERSION: u32 = 5;

fn default_format_version() -> u32 {
    CURRENT_FORMAT_VERSION
}

fn default_dpi() -> f64 {
    72.0
}

/// Document colour model. Rgb is the authoring/on-screen model; Cmyk marks the
/// document for print separation on export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    #[default]
    Rgb,
    Cmyk,
}

pub type DocumentId = Uuid;

/// A named export configuration stored in the document.
/// Profiles are applied via the `run_export_profile` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportProfile {
    /// Unique profile name used as a reference key.
    pub name: String,
    /// Target format: "svg", "png", "jpeg", or "webp".
    pub format: String,
    /// Raster-only: explicit output width in pixels.
    #[serde(default)]
    pub width: Option<u32>,
    /// Raster-only: explicit output height in pixels (overrides scale).
    #[serde(default)]
    pub height: Option<u32>,
    /// SVG-only: emit semantic `id` attributes on exported nodes (default true).
    #[serde(default)]
    pub semantic_ids: Option<bool>,
    /// SVG-only: decimal precision for coordinates (1–6, default 4).
    #[serde(default)]
    pub precision: Option<u32>,
}

impl ExportProfile {
    pub fn new_svg(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            format: "svg".to_string(),
            width: None,
            height: None,
            semantic_ids: Some(true),
            precision: Some(4),
        }
    }

    pub fn new_png(name: impl Into<String>, width: Option<u32>, height: Option<u32>) -> Self {
        Self {
            name: name.into(),
            format: "png".to_string(),
            width,
            height,
            semantic_ids: None,
            precision: None,
        }
    }

    pub fn new_pdf(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            format: "pdf".to_string(),
            width: None,
            height: None,
            semantic_ids: None,
            precision: None,
        }
    }
}

// ─── Symbols ─────────────────────────────────────────────────────────────────

/// A named symbol — a master node that can be placed as reusable instances.
/// Instances reference this symbol's master node via SceneNode::symbol_ref.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub id: Uuid,
    /// Unique symbol name.
    pub name: String,
    /// The master node ID. All instances share this node's geometry/style.
    pub master_node_id: NodeId,
}

impl Symbol {
    pub fn new(name: impl Into<String>, master_node_id: NodeId) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            master_node_id,
        }
    }
}

// ─── Pattern registry ─────────────────────────────────────────────────────────

/// A named, reusable pattern in the document-level pattern registry.
///
/// The registry is the *authoring* home for patterns (define once, apply many).
/// When applied to a node, a clone of `fill` (including its embedded tile) is
/// stored on the node so the rendered fill stays self-contained and independent
/// of the registry entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub id: Uuid,
    /// Unique display name.
    pub name: String,
    /// The pattern fill definition (embedded tile + transform).
    pub fill: PatternFill,
}

impl Pattern {
    pub fn new(name: impl Into<String>, fill: PatternFill) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            fill,
        }
    }
}

// ─── Color Swatches ───────────────────────────────────────────────────────────

/// A named document variable — a key-value pair that can be bound to text nodes
/// for data-driven design (e.g. names, prices, dates in a template).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentVariable {
    /// Unique variable name (used as the binding key).
    pub name: String,
    /// Current string value.
    pub value: String,
}

impl DocumentVariable {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// A named document-level color swatch. Provides a shared color reference that
/// can be applied to any node fill or stroke; updating a swatch propagates to
/// all nodes that match its current color.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorSwatch {
    pub id: Uuid,
    /// Unique display name.
    pub name: String,
    /// Color value as a CSS hex string (e.g. "#FF5733").
    pub color_hex: String,
}

impl ColorSwatch {
    pub fn new(name: impl Into<String>, color_hex: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            color_hex: color_hex.into(),
        }
    }
}

// ─── Gradient Swatches ───────────────────────────────────────────────────────

/// A named gradient swatch — a reusable gradient that can be saved from any node
/// and applied to other path nodes as a fill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradientSwatch {
    pub id: Uuid,
    /// Unique display name.
    pub name: String,
    /// The gradient fill stored as a JSON-serialized `Fill`.
    /// Stored as a string blob to avoid importing style types into document.rs.
    pub fill_json: String,
}

impl GradientSwatch {
    pub fn new(name: impl Into<String>, fill_json: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            fill_json: fill_json.into(),
        }
    }
}

// ─── Graphic Styles ───────────────────────────────────────────────────────────

/// A named graphic style — a reusable appearance preset storing fill, stroke,
/// and opacity. Stored as JSON string blobs to avoid importing style types here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphicStyle {
    pub id: Uuid,
    /// Unique display name.
    pub name: String,
    /// JSON-serialized `Fill`.
    pub fill_json: String,
    /// JSON-serialized `Stroke`.
    pub stroke_json: String,
    /// Node opacity (0.0–1.0).
    #[serde(default = "GraphicStyle::default_opacity")]
    pub opacity: f32,
}

impl GraphicStyle {
    pub fn default_opacity() -> f32 {
        1.0
    }

    pub fn new(
        name: impl Into<String>,
        fill_json: impl Into<String>,
        stroke_json: impl Into<String>,
        opacity: f32,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            fill_json: fill_json.into(),
            stroke_json: stroke_json.into(),
            opacity,
        }
    }
}

// ─── Variable Width Profiles ─────────────────────────────────────────────────

/// A named variable-width stroke profile. Width values are sampled along the
/// path (t=0 at start, t=1 at end). When applied, the average width is used for
/// uniform rendering; the profile is stored for future variable-width rendering
/// support.
///
/// Each width in [`widths`](Self::widths) has an explicit normalized
/// arc-length position in [`positions`](Self::positions). For documents written
/// before positions existed, `positions` deserializes empty and the samples are
/// treated as evenly spaced (see [`effective_positions`](Self::effective_positions)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidthProfile {
    pub id: Uuid,
    /// Unique display name.
    pub name: String,
    /// Width samples along the path (in document units). Must have at least 2
    /// values. Paired index-for-index with [`positions`](Self::positions).
    pub widths: Vec<f64>,
    /// Normalized arc-length position `[0, 1]` of each width sample. When empty
    /// (legacy files), samples are treated as evenly spaced.
    #[serde(default)]
    pub positions: Vec<f64>,
    /// Optional per-sample width for the right side of the stroke, enabling
    /// asymmetric profiles. `None` means the profile is symmetric and
    /// [`widths`](Self::widths) applies to both sides.
    #[serde(default)]
    pub widths_right: Option<Vec<f64>>,
}

impl WidthProfile {
    /// Create a symmetric profile with evenly spaced sample positions.
    pub fn new(name: impl Into<String>, widths: Vec<f64>) -> Self {
        let positions = Self::uniform_positions(widths.len());
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            widths,
            positions,
            widths_right: None,
        }
    }

    /// Create a profile with explicit sample positions.
    pub fn with_positions(name: impl Into<String>, positions: Vec<f64>, widths: Vec<f64>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            widths,
            positions,
            widths_right: None,
        }
    }

    /// Evenly spaced positions for `count` samples: `[0, …, 1]`.
    pub fn uniform_positions(count: usize) -> Vec<f64> {
        match count {
            0 => Vec::new(),
            1 => vec![0.0],
            n => (0..n).map(|i| i as f64 / (n - 1) as f64).collect(),
        }
    }

    /// Positions to use for interpolation: the stored [`positions`](Self::positions)
    /// when they match the sample count, otherwise evenly spaced fallbacks.
    pub fn effective_positions(&self) -> Vec<f64> {
        if self.positions.len() == self.widths.len() {
            self.positions.clone()
        } else {
            Self::uniform_positions(self.widths.len())
        }
    }

    /// Compute the average width (used for uniform stroke rendering).
    pub fn average_width(&self) -> f64 {
        if self.widths.is_empty() {
            return 1.0;
        }
        self.widths.iter().sum::<f64>() / self.widths.len() as f64
    }
}

// ─── Property Constraints ────────────────────────────────────────────────────

/// A live parametric binding: `target_node_id.target_property = expression`.
///
/// The expression is evaluated over other nodes' properties (see
/// [`crate::ops::constraints`]) and re-applied after every document mutation, so
/// the target stays derived from its inputs. Supported properties: `x`, `y`,
/// `width`, `height`, `opacity`, `font_size` (referenceable); `x`, `y`,
/// `opacity`, `font_size` are also settable as targets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyConstraint {
    pub id: Uuid,
    pub target_node_id: NodeId,
    /// One of: `x`, `y`, `opacity`, `font_size`.
    pub target_property: String,
    /// Arithmetic expression; may reference `nodes['<id-or-name>'].<prop>`.
    pub expression: String,
}

impl PropertyConstraint {
    pub fn new(
        target_node_id: NodeId,
        target_property: impl Into<String>,
        expression: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            target_node_id,
            target_property: target_property.into(),
            expression: expression.into(),
        }
    }
}

// ─── Spot Colors ─────────────────────────────────────────────────────────────

// ─── Actions (Macro Sequences) ───────────────────────────────────────────────

/// A named action set — a recorded sequence of MCP tool calls that can be replayed.
/// Steps are stored as a JSON array of `{"tool": "...", "args": {...}}` objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSet {
    pub id: Uuid,
    /// Unique display name.
    pub name: String,
    /// JSON-encoded array of action steps: `[{"tool":"...","args":{...}},...]`.
    pub steps_json: String,
}

impl ActionSet {
    pub fn new(name: impl Into<String>, steps_json: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            steps_json: steps_json.into(),
        }
    }
}

// ─── Event Triggers ──────────────────────────────────────────────────────────

/// A mapping from a document lifecycle event to a named action to execute.
/// Valid event names: "on_open", "on_save", "on_node_create", "on_selection_change".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTrigger {
    /// The event name that fires this trigger.
    pub event: String,
    /// Name of the action set to execute when the event fires.
    pub action_name: String,
}

// ─── Document Grammar Rules ───────────────────────────────────────────────────

/// A named design rule that the document must satisfy.
/// Rule types and their `params_json` shapes:
///
/// - `palette_includes`  → `{"color_hex": "#rrggbb"}`  — a specific fill color must appear somewhere
/// - `max_colors`        → `{"count": N}`               — no more than N unique solid fill colors
/// - `min_text_size`     → `{"px": N}`                  — all text nodes must have font_size >= N
/// - `required_layer`    → `{"name": "..."}` (or `"prefix": "..."`) — a layer with this name must exist
/// - `max_node_count`    → `{"count": N}`               — total visible node count must not exceed N
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrammarRule {
    pub id: Uuid,
    /// Human-readable name for the rule.
    pub name: String,
    /// Discriminator: `palette_includes`, `max_colors`, `min_text_size`, `required_layer`, `max_node_count`.
    pub rule_type: String,
    /// JSON-encoded rule parameters (shape depends on `rule_type`).
    pub params_json: String,
}

impl GrammarRule {
    pub fn new(
        name: impl Into<String>,
        rule_type: impl Into<String>,
        params_json: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            rule_type: rule_type.into(),
            params_json: params_json.into(),
        }
    }
}

/// A named spot color — a print-production ink with a unique name, hex value,
/// and optional overprint flag. Stored in the document palette.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotColor {
    pub id: Uuid,
    /// Unique display name (e.g. "Pantone 485 C", "PROCESS CYAN").
    pub name: String,
    /// Color value as a CSS hex string (e.g. "#FF2400").
    pub hex: String,
    /// When true, this ink overprints underlying inks rather than knocking out.
    #[serde(default)]
    pub overprint: bool,
}

impl SpotColor {
    pub fn new(name: impl Into<String>, hex: impl Into<String>, overprint: bool) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            hex: hex.into(),
            overprint,
        }
    }
}

// ─── Paragraph Styles ────────────────────────────────────────────────────────

/// A named paragraph style — text alignment, spacing, and size at the block level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParagraphStyle {
    /// Unique name used as a reference key.
    pub name: String,
    /// Text alignment: "left", "center", "right", or "justify".
    #[serde(default)]
    pub align: Option<String>,
    /// Line height multiplier (e.g. 1.5).
    #[serde(default)]
    pub line_height: Option<f64>,
    /// Letter spacing in document units.
    #[serde(default)]
    pub letter_spacing: Option<f64>,
    /// Font size override.
    #[serde(default)]
    pub font_size: Option<f64>,
    /// Font family override.
    #[serde(default)]
    pub font_family: Option<String>,
}

// ─── Character Styles ─────────────────────────────────────────────────────────

/// A named character style preset — stores text formatting that can be
/// saved once and re-applied to any text node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterStyle {
    /// Unique name used as a reference key.
    pub name: String,
    /// Font family name (e.g. "Roboto", "sans-serif").
    #[serde(default)]
    pub font_family: Option<String>,
    /// Font size in document units.
    #[serde(default)]
    pub font_size: Option<f64>,
    /// CSS-style font weight (100–900). 400 = regular, 700 = bold.
    #[serde(default)]
    pub font_weight: Option<u16>,
    /// Fill color as CSS hex string (e.g. "#FF5733"). None = don't change.
    #[serde(default)]
    pub fill_hex: Option<String>,
    /// Letter spacing in document units.
    #[serde(default)]
    pub letter_spacing: Option<f64>,
    /// Line height multiplier (e.g. 1.5).
    #[serde(default)]
    pub line_height: Option<f64>,
}

/// Identifier for an [`Artboard`].
pub type ArtboardId = Uuid;

/// A named rectangular region in the document's shared coordinate space.
///
/// Photonic uses a **spatial** (Illustrator-style) multi-artboard model: every
/// node lives in one absolute coordinate space, and an artboard is simply a
/// named crop/export rectangle over that space. The document always has at
/// least one artboard; legacy single-canvas documents migrate to a single
/// artboard at the origin covering `(0, 0, width, height)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artboard {
    pub id: ArtboardId,
    pub name: String,
    /// Top-left corner in document coordinates.
    pub x: f64,
    pub y: f64,
    /// Size in document units (logical pixels at 96 dpi).
    pub width: f64,
    pub height: f64,
}

impl Artboard {
    pub fn new(name: impl Into<String>, x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            x,
            y,
            width,
            height,
        }
    }

    /// The artboard rectangle as `(min_x, min_y, max_x, max_y)`.
    pub fn rect(&self) -> (f64, f64, f64, f64) {
        (self.x, self.y, self.x + self.width, self.y + self.height)
    }
}

/// The root document — contains pages, layers, and the scene graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// File format version — used to detect incompatible saves.
    /// Defaults to `CURRENT_FORMAT_VERSION` for files that predate this field.
    #[serde(default = "default_format_version")]
    pub format_version: u32,
    pub id: DocumentId,
    pub name: String,
    /// Document dimensions in logical pixels (at 96 dpi).
    pub width: f64,
    pub height: f64,
    /// Ordered list of layer IDs (bottom to top in the stack).
    pub layer_order: Vec<LayerId>,
    /// All layers, keyed by ID.
    pub layers: HashMap<LayerId, Layer>,
    /// All nodes (across all layers), keyed by ID.
    pub nodes: HashMap<NodeId, SceneNode>,
    /// The currently active layer for new node creation.
    pub active_layer_id: Option<LayerId>,
    /// Current selection state (not serialized in the file format).
    #[serde(skip)]
    pub selection: Selection,
    /// Design annotations and review comments.
    /// Persisted in `.photon` files; stripped from all export formats.
    #[serde(default)]
    pub annotations: HashMap<AnnotationId, Annotation>,
    /// Ruler guides — horizontal and vertical reference lines.
    /// Persisted in `.photon` files; stripped from all export formats.
    #[serde(default)]
    pub guides: Vec<Guide>,
    /// Most recently used fill/stroke colors (capped at 20, deduped).
    /// Updated on every fill or stroke color change.
    #[serde(default)]
    pub recent_colors: Vec<Color>,
    /// Named export profiles stored with the document.
    /// Each profile captures format settings for one-command export.
    #[serde(default)]
    pub export_profiles: Vec<ExportProfile>,
    /// Named character styles for rapid text formatting.
    #[serde(default)]
    pub character_styles: Vec<CharacterStyle>,
    /// Named paragraph styles for rapid block-level text formatting.
    #[serde(default)]
    pub paragraph_styles: Vec<ParagraphStyle>,
    /// Named color swatches shared across the document.
    #[serde(default)]
    pub color_swatches: Vec<ColorSwatch>,
    /// Named document variables for data-driven design (text binding).
    #[serde(default)]
    pub variables: Vec<DocumentVariable>,
    /// Named symbols (master nodes) that can be instantiated across the document.
    #[serde(default)]
    pub symbols: Vec<Symbol>,
    /// Named gradient swatches — reusable gradients saved from node fills.
    #[serde(default)]
    pub gradient_swatches: Vec<GradientSwatch>,
    /// Named spot colors — print-production inks with overprint settings.
    #[serde(default)]
    pub spot_colors: Vec<SpotColor>,
    /// Named graphic styles — reusable fill+stroke+opacity presets.
    #[serde(default)]
    pub graphic_styles: Vec<GraphicStyle>,
    /// Named variable-width stroke profiles.
    #[serde(default)]
    pub width_profiles: Vec<WidthProfile>,
    /// Named reusable patterns — tiled raster fills defined once and applied to nodes.
    #[serde(default)]
    pub patterns: Vec<Pattern>,
    /// Live property constraints — parametric bindings of a node property to an
    /// expression over other nodes' properties. Re-evaluated after every mutation.
    #[serde(default)]
    pub constraints: Vec<PropertyConstraint>,
    /// Named design grammar rules — constraints the document must satisfy.
    #[serde(default)]
    pub grammar_rules: Vec<GrammarRule>,
    /// Named action sets — replayable MCP tool sequences.
    #[serde(default)]
    pub action_sets: Vec<ActionSet>,
    /// Print bleed in millimetres (added to all four sides of the artboard).
    /// 0.0 means no bleed. Typical values: 3.0 mm (EU) or 0.125 in ≈ 3.175 mm (US).
    #[serde(default)]
    pub bleed_mm: f64,
    /// Print slug area in millimetres (additional margin outside the bleed for printer marks).
    #[serde(default)]
    pub slug_mm: f64,
    /// Artboard safe-area margin from the top edge, in document units.
    #[serde(default)]
    pub margin_top: f64,
    /// Artboard safe-area margin from the right edge, in document units.
    #[serde(default)]
    pub margin_right: f64,
    /// Artboard safe-area margin from the bottom edge, in document units.
    #[serde(default)]
    pub margin_bottom: f64,
    /// Artboard safe-area margin from the left edge, in document units.
    #[serde(default)]
    pub margin_left: f64,
    /// Per-document edit-history size cap in MB (#195). `None` = use the global
    /// preference default. Set from the new-document flow and persisted in
    /// `.photon` so a project's history budget travels with the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_max_mb: Option<f64>,
    /// Script event triggers — named actions to fire on specific document events.
    #[serde(default)]
    pub event_triggers: Vec<EventTrigger>,
    /// Named panel workspace presets — each stores a properties-panel filter query.
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    /// Dimension annotations — measurement lines showing distances between node pairs.
    /// Persisted in `.photon` files; stripped from all export formats.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<DimensionAnnotation>,
    /// Artboards — named crop/export rectangles in the shared coordinate space.
    /// Always contains at least one entry after construction or load; legacy
    /// documents migrate to a single artboard covering `(0, 0, width, height)`.
    #[serde(default)]
    pub artboards: Vec<Artboard>,
    /// The currently active artboard (target for new work / export defaults).
    #[serde(default)]
    pub active_artboard: Option<ArtboardId>,
    /// Output resolution in pixels per inch; anchors physical-unit (mm/in/pt)
    /// conversion and PDF point scaling. Default 72.0 → 1 px == 1 pt (legacy).
    #[serde(default = "default_dpi")]
    pub dpi: f64,
    /// Preferred display unit for rulers/readouts; storage stays in px.
    #[serde(default)]
    pub display_unit: crate::units::DocumentUnit,
    /// Colour model used when exporting for print. Default Rgb.
    #[serde(default)]
    pub color_mode: ColorMode,
    /// The video-editor timeline project (01 §2). `None` until the first
    /// video-mode action creates it (undoably, via `TimelineCmd::CreateProject`).
    /// Additive at the 01 §2 seam: v2 files load with this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<crate::timeline::TimelineProject>,
}

// ─── Dimension Annotation ─────────────────────────────────────────────────────

/// A measurement annotation showing the distance between two nodes.
/// Rendered as a dimension line with arrowheads and a distance label.
/// Stripped from all export formats; not a document-level selection item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionAnnotation {
    pub id: Uuid,
    /// ID of the "from" node.
    pub from_node: crate::node::NodeId,
    /// ID of the "to" node.
    pub to_node: crate::node::NodeId,
    /// Measurement axis: "x" (horizontal only), "y" (vertical only), or "diagonal" (Euclidean).
    #[serde(default = "default_axis")]
    pub axis: String,
    /// Perpendicular offset from the line between nodes in document units (for visual clearance).
    #[serde(default)]
    pub label_offset: f64,
    /// Cached X of the from-node bounding-box center at creation time.
    pub from_x: f64,
    /// Cached Y of the from-node bounding-box center at creation time.
    pub from_y: f64,
    /// Cached X of the to-node bounding-box center at creation time.
    pub to_x: f64,
    /// Cached Y of the to-node bounding-box center at creation time.
    pub to_y: f64,
}

fn default_axis() -> String {
    "diagonal".to_string()
}

impl DimensionAnnotation {
    pub fn new(
        from_node: crate::node::NodeId,
        to_node: crate::node::NodeId,
        axis: String,
        label_offset: f64,
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            from_node,
            to_node,
            axis,
            label_offset,
            from_x,
            from_y,
            to_x,
            to_y,
        }
    }

    /// The measured distance in document units according to axis.
    pub fn distance(&self) -> f64 {
        match self.axis.as_str() {
            "x" => (self.to_x - self.from_x).abs(),
            "y" => (self.to_y - self.from_y).abs(),
            _ => ((self.to_x - self.from_x).powi(2) + (self.to_y - self.from_y).powi(2)).sqrt(),
        }
    }
}

// ─── Workspace ────────────────────────────────────────────────────────────────

/// A named workspace preset that stores a properties-panel search filter.
///
/// `PartialEq` lets the save/delete call sites skip a no-op history entry when
/// the list is unchanged — an undo step that undoes nothing is worse than none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Unique workspace name.
    pub name: String,
    /// Properties-panel search query that filters which sections are visible.
    #[serde(default)]
    pub search_query: String,
}

// ─── Print presets ────────────────────────────────────────────────────────────

/// A standard print size preset — trim size, NOT including bleed.
#[derive(Debug, Clone, Copy)]
pub struct PrintPreset {
    pub name: &'static str,
    pub w: f64,
    pub h: f64,
    pub unit: crate::units::DocumentUnit,
}

impl PrintPreset {
    pub const BUSINESS_CARD_US: PrintPreset = PrintPreset {
        name: "Business Card (US)",
        w: 3.5,
        h: 2.0,
        unit: crate::units::DocumentUnit::In,
    };
    pub const BUSINESS_CARD_EU: PrintPreset = PrintPreset {
        name: "Business Card (EU)",
        w: 85.0,
        h: 55.0,
        unit: crate::units::DocumentUnit::Mm,
    };
}

impl Document {
    /// Create a new blank document with a default layer.
    pub fn new(name: impl Into<String>, width: f64, height: f64) -> Self {
        let default_layer = Layer::new("Layer 1");
        let layer_id = default_layer.id;
        let mut layers = HashMap::new();
        layers.insert(layer_id, default_layer);

        let artboard = Artboard::new("Artboard 1", 0.0, 0.0, width, height);
        let active_artboard = Some(artboard.id);

        Self {
            format_version: CURRENT_FORMAT_VERSION,
            id: Uuid::new_v4(),
            name: name.into(),
            width,
            height,
            layer_order: vec![layer_id],
            layers,
            nodes: HashMap::new(),
            active_layer_id: Some(layer_id),
            selection: Selection::new(),
            annotations: HashMap::new(),
            guides: Vec::new(),
            recent_colors: Vec::new(),
            export_profiles: Vec::new(),
            character_styles: Vec::new(),
            paragraph_styles: Vec::new(),
            color_swatches: Vec::new(),
            variables: Vec::new(),
            symbols: Vec::new(),
            gradient_swatches: Vec::new(),
            spot_colors: Vec::new(),
            graphic_styles: Vec::new(),
            width_profiles: Vec::new(),
            patterns: Vec::new(),
            constraints: Vec::new(),
            grammar_rules: Vec::new(),
            action_sets: Vec::new(),
            bleed_mm: 0.0,
            slug_mm: 0.0,
            margin_top: 0.0,
            margin_right: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            history_max_mb: None,
            event_triggers: Vec::new(),
            workspaces: Vec::new(),
            dimensions: Vec::new(),
            artboards: vec![artboard],
            active_artboard,
            dpi: 72.0,
            display_unit: crate::units::DocumentUnit::default(),
            color_mode: ColorMode::default(),
            timeline: None,
        }
    }

    /// Default A4-landscape artboard.
    pub fn default_artboard() -> Self {
        Self::new("Untitled", 1123.0, 794.0)
    }

    /// Create a document sized in physical units at `dpi`. width/height are stored
    /// in px = to_px(value, unit, dpi). Sets dpi and display_unit accordingly.
    pub fn new_print(
        name: impl Into<String>,
        w: f64,
        h: f64,
        unit: crate::units::DocumentUnit,
        dpi: f64,
    ) -> Self {
        let wpx = crate::units::to_px(w, unit, dpi);
        let hpx = crate::units::to_px(h, unit, dpi);
        let mut d = Document::new(name, wpx, hpx);
        d.dpi = dpi;
        d.display_unit = unit;
        d
    }

    /// Create a document from a print preset at `dpi`.
    pub fn from_preset(p: PrintPreset, dpi: f64) -> Self {
        Document::new_print(p.name, p.w, p.h, p.unit, dpi)
    }

    // ─── Artboards ──────────────────────────────────────────────────────────

    /// Ensure the document has at least one artboard. Legacy single-canvas
    /// documents (saved before multi-artboard) gain one artboard at the origin
    /// covering `(0, 0, width, height)`. Idempotent.
    pub fn ensure_default_artboard(&mut self) {
        if self.artboards.is_empty() {
            let ab = Artboard::new("Artboard 1", 0.0, 0.0, self.width, self.height);
            self.active_artboard = Some(ab.id);
            self.artboards.push(ab);
        }
        if self.active_artboard.is_none() {
            self.active_artboard = self.artboards.first().map(|a| a.id);
        }
    }

    /// The active artboard, or the first one if none is explicitly active.
    pub fn active_artboard(&self) -> Option<&Artboard> {
        self.active_artboard
            .and_then(|id| self.artboards.iter().find(|a| a.id == id))
            .or_else(|| self.artboards.first())
    }

    /// Append an artboard and make it active; returns its id.
    pub fn add_artboard(&mut self, artboard: Artboard) -> ArtboardId {
        let id = artboard.id;
        self.artboards.push(artboard);
        self.active_artboard = Some(id);
        id
    }

    /// Remove an artboard by id (no-op if it is the last remaining one). If the
    /// removed artboard was active, the first remaining artboard becomes active.
    pub fn remove_artboard(&mut self, id: ArtboardId) {
        if self.artboards.len() <= 1 {
            return;
        }
        self.artboards.retain(|a| a.id != id);
        if self.active_artboard == Some(id) {
            self.active_artboard = self.artboards.first().map(|a| a.id);
        }
    }

    /// The immediate parent group of `id`, if any (scan over all group nodes).
    pub fn parent_group_of(&self, id: &NodeId) -> Option<NodeId> {
        self.nodes.values().find_map(|n| match &n.kind {
            SceneNodeKind::Group(g) if g.children.contains(id) => Some(n.id),
            _ => None,
        })
    }

    /// The outermost group containing `id`, walking nested groups upward.
    /// `None` when the node is not in any group. Bounded by a depth cap so a
    /// malformed document with a group-parent cycle cannot hang the UI.
    pub fn outermost_group_of(&self, id: &NodeId) -> Option<NodeId> {
        let mut cur = self.parent_group_of(id)?;
        for _ in 0..64 {
            match self.parent_group_of(&cur) {
                Some(p) => cur = p,
                None => break,
            }
        }
        Some(cur)
    }

    /// All non-group descendant node ids of a group, recursively (the group's
    /// selectable/movable members). Group nodes themselves are not included —
    /// child transforms are absolute, so translating both a group node and its
    /// members would double-move. Cycle-safe via a visited set.
    pub fn group_member_ids(&self, group_id: &NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![*group_id];
        let mut seen = std::collections::HashSet::new();
        while let Some(gid) = stack.pop() {
            if !seen.insert(gid) {
                continue;
            }
            let Some(SceneNodeKind::Group(g)) = self.nodes.get(&gid).map(|n| &n.kind) else {
                continue;
            };
            for cid in &g.children {
                match self.nodes.get(cid).map(|n| &n.kind) {
                    Some(SceneNodeKind::Group(_)) => stack.push(*cid),
                    Some(_) => out.push(*cid),
                    None => {}
                }
            }
        }
        out
    }

    /// The artboard "owning" the canvas-space rectangle `(x0, y0)..(x1, y1)`:
    /// the one with the largest overlap area, preferring the active artboard on
    /// ties. `None` when no artboard overlaps the rect at all.
    pub fn artboard_for_rect(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Option<&Artboard> {
        let (rx0, rx1) = (x0.min(x1), x0.max(x1));
        let (ry0, ry1) = (y0.min(y1), y0.max(y1));
        let mut best: Option<(&Artboard, f64)> = None;
        for a in &self.artboards {
            let (ax0, ay0, ax1, ay1) = a.rect();
            let w = (rx1.min(ax1) - rx0.max(ax0)).max(0.0);
            let h = (ry1.min(ay1) - ry0.max(ay0)).max(0.0);
            let overlap = w * h;
            if overlap <= 0.0 {
                continue;
            }
            let wins = match best {
                None => true,
                Some((cur, cur_overlap)) => {
                    overlap > cur_overlap
                        || (overlap == cur_overlap
                            && Some(a.id) == self.active_artboard
                            && Some(cur.id) != self.active_artboard)
                }
            };
            if wins {
                best = Some((a, overlap));
            }
        }
        best.map(|(a, _)| a)
    }

    /// Bounding box of all artboards as `(min_x, min_y, max_x, max_y)`.
    /// Falls back to `(0, 0, width, height)` when there are no artboards.
    pub fn artboards_bounds(&self) -> (f64, f64, f64, f64) {
        if self.artboards.is_empty() {
            return (0.0, 0.0, self.width, self.height);
        }
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for a in &self.artboards {
            let (ax0, ay0, ax1, ay1) = a.rect();
            min_x = min_x.min(ax0);
            min_y = min_y.min(ay0);
            max_x = max_x.max(ax1);
            max_y = max_y.max(ay1);
        }
        (min_x, min_y, max_x, max_y)
    }

    /// Record a recently used color. Deduplicates and caps at 20 entries.
    /// New color is inserted at the front (most-recent first).
    pub fn record_recent_color(&mut self, color: Color) {
        self.recent_colors.retain(|c| {
            (c.r - color.r).abs() > f32::EPSILON
                || (c.g - color.g).abs() > f32::EPSILON
                || (c.b - color.b).abs() > f32::EPSILON
                || (c.a - color.a).abs() > f32::EPSILON
        });
        self.recent_colors.insert(0, color);
        self.recent_colors.truncate(20);
    }

    // --- Layer operations ---

    pub fn add_layer(&mut self, layer: Layer) -> LayerId {
        let id = layer.id;
        self.layers.insert(id, layer);
        self.layer_order.push(id);
        id
    }

    pub fn remove_layer(&mut self, id: &LayerId) -> Option<Layer> {
        if let Some(pos) = self.layer_order.iter().position(|l| l == id) {
            self.layer_order.remove(pos);
        }
        // Remove all nodes belonging to this layer
        self.nodes.retain(|_, node| &node.layer_id != id);
        self.layers.remove(id)
    }

    pub fn get_layer(&self, id: &LayerId) -> Option<&Layer> {
        self.layers.get(id)
    }

    pub fn get_layer_mut(&mut self, id: &LayerId) -> Option<&mut Layer> {
        self.layers.get_mut(id)
    }

    pub fn active_layer(&self) -> Option<&Layer> {
        self.active_layer_id
            .as_ref()
            .and_then(|id| self.layers.get(id))
    }

    // --- Node operations ---

    /// Add a node to the specified layer (or the active layer if None).
    pub fn add_node(&mut self, mut node: SceneNode, layer_id: Option<LayerId>) -> NodeId {
        let layer_id = layer_id
            .or(self.active_layer_id)
            .unwrap_or_else(|| *self.layer_order.last().expect("document has no layers"));

        node.layer_id = layer_id;
        let node_id = node.id;

        if let Some(layer) = self.layers.get_mut(&layer_id) {
            layer.node_ids.push(node_id);
        }
        self.nodes.insert(node_id, node);
        node_id
    }

    /// Remove a node by ID. Returns the removed node.
    pub fn remove_node(&mut self, id: &NodeId) -> Option<SceneNode> {
        if let Some(node) = self.nodes.remove(id) {
            if let Some(layer) = self.layers.get_mut(&node.layer_id) {
                layer.node_ids.retain(|nid| nid != id);
            }
            Some(node)
        } else {
            None
        }
    }

    pub fn get_node(&self, id: &NodeId) -> Option<&SceneNode> {
        self.nodes.get(id)
    }

    pub fn get_node_mut(&mut self, id: &NodeId) -> Option<&mut SceneNode> {
        self.nodes.get_mut(id)
    }

    /// Find a node by name (returns first match).
    pub fn find_node_by_name(&self, name: &str) -> Option<&SceneNode> {
        self.nodes.values().find(|n| n.name == name)
    }

    /// Find nodes by tag.
    pub fn find_nodes_by_tag(&self, tag: &str) -> Vec<&SceneNode> {
        self.nodes
            .values()
            .filter(|n| n.tags.iter().any(|t| t == tag))
            .collect()
    }

    // --- Annotation operations ---

    /// Add an annotation and return its ID.
    pub fn add_annotation(&mut self, ann: Annotation) -> AnnotationId {
        let id = ann.id;
        self.annotations.insert(id, ann);
        id
    }

    /// Mark an annotation as resolved. Returns `true` if found, `false` if not.
    pub fn resolve_annotation(&mut self, id: &AnnotationId) -> bool {
        if let Some(ann) = self.annotations.get_mut(id) {
            ann.resolved = true;
            true
        } else {
            false
        }
    }

    /// Remove an annotation entirely. Returns the removed annotation if it existed.
    pub fn remove_annotation(&mut self, id: &AnnotationId) -> Option<Annotation> {
        self.annotations.remove(id)
    }

    /// Returns nodes in draw order (bottom layer, bottom node first).
    /// Group nodes are expanded recursively — their path children are yielded in place.
    pub fn nodes_in_draw_order(&self) -> Vec<&SceneNode> {
        let mut result = vec![];
        for layer_id in &self.layer_order {
            if let Some(layer) = self.layers.get(layer_id) {
                if !layer.visible {
                    continue;
                }
                for node_id in &layer.node_ids {
                    self.collect_draw_nodes(node_id, &mut result);
                }
            }
        }
        result
    }

    /// Nodes of a single layer in draw order (bottom node first), with groups
    /// expanded recursively — the per-layer analogue of [`nodes_in_draw_order`].
    /// Returns empty if the layer is hidden or unknown. Used by renderers that
    /// composite each layer as an isolated unit (layer opacity / blend / mask).
    ///
    /// [`nodes_in_draw_order`]: Document::nodes_in_draw_order
    pub fn draw_nodes_in_layer(&self, layer_id: &LayerId) -> Vec<&SceneNode> {
        let mut result = vec![];
        if let Some(layer) = self.layers.get(layer_id) {
            if !layer.visible {
                return result;
            }
            for node_id in &layer.node_ids {
                self.collect_draw_nodes(node_id, &mut result);
            }
        }
        result
    }

    fn collect_draw_nodes<'a>(&'a self, node_id: &NodeId, out: &mut Vec<&'a SceneNode>) {
        if let Some(node) = self.nodes.get(node_id) {
            if !node.visible {
                return;
            }
            match &node.kind {
                // A live-boolean group renders as a single resolved path, so it
                // is treated as a leaf here (resolved in `resolve_render_node`)
                // rather than descending into its operands (#25).
                SceneNodeKind::Group(g) if g.live_boolean.is_none() => {
                    for child_id in &g.children {
                        self.collect_draw_nodes(child_id, out);
                    }
                }
                _ => out.push(node),
            }
        }
    }

    /// Maximum symbol-nesting depth before resolution bails out (cycle guard).
    const SYMBOL_MAX_DEPTH: u8 = 8;

    /// Resolve a node's render geometry/style from its symbol master.
    ///
    /// Symbol instances are created as a frozen clone of the master's geometry
    /// (see `place_symbol`), so edits to the master would otherwise never reach
    /// existing instances. This returns a node whose `kind` is taken from the
    /// *current* master — with the instance's own transform, opacity,
    /// visibility and per-instance fill/stroke colour overrides preserved — so
    /// master edits propagate live. Non-instances are returned borrowed and
    /// unchanged (zero-cost on the render hot path).
    ///
    /// Nested symbols (a master that is itself an instance) are followed up to
    /// [`Self::SYMBOL_MAX_DEPTH`]; cycles and dangling references fall back to
    /// rendering the instance's frozen copy.
    ///
    /// Note: only single-node masters propagate today. A group master is
    /// flattened to leaf nodes at placement time and those leaves carry no
    /// `symbol_ref`, so group/nested-group propagation is tracked as follow-up.
    pub fn resolve_render_node<'a>(
        &'a self,
        node: &'a SceneNode,
    ) -> std::borrow::Cow<'a, SceneNode> {
        use std::borrow::Cow;
        // Live-boolean group (#25): render as its single resolved path, styled by
        // the bottom-most path child, keeping the group's transform/opacity/blend.
        if let SceneNodeKind::Group(g) = &node.kind {
            if g.live_boolean.is_some() {
                if let Some(path) = self.resolve_live_boolean(node.id) {
                    let mut pn = crate::node::PathNode::new(path);
                    if let Some((fill, stroke)) = g
                        .children
                        .iter()
                        .filter_map(|c| self.nodes.get(c))
                        .find_map(|c| match &c.kind {
                            SceneNodeKind::Path(p) => Some((p.fill.clone(), p.stroke.clone())),
                            _ => None,
                        })
                    {
                        pn.fill = fill;
                        pn.stroke = stroke;
                    }
                    let mut resolved = node.clone();
                    resolved.kind = SceneNodeKind::Path(pn);
                    return Cow::Owned(resolved);
                }
            }
            return Cow::Borrowed(node);
        }
        let Some(sym_id) = node.symbol_ref else {
            return Cow::Borrowed(node);
        };

        // Follow the symbol → master chain, guarding against cycles/depth.
        let mut current_sym = Some(sym_id);
        let mut master: Option<&SceneNode> = None;
        let mut depth = 0u8;
        while let Some(sid) = current_sym {
            if depth > Self::SYMBOL_MAX_DEPTH {
                // Nesting too deep (likely a cycle): render the frozen copy.
                return Cow::Borrowed(node);
            }
            let Some(sym) = self.symbols.iter().find(|s| s.id == sid) else {
                // Dangling symbol reference: render the frozen copy.
                break;
            };
            let Some(m) = self.nodes.get(&sym.master_node_id) else {
                break;
            };
            if m.id == node.id {
                // Instance is its own master — would loop forever.
                return Cow::Borrowed(node);
            }
            master = Some(m);
            current_sym = m.symbol_ref;
            depth += 1;
        }

        let Some(master) = master else {
            return Cow::Borrowed(node);
        };

        // Master geometry/style with instance placement + overrides on top.
        let mut resolved = node.clone();
        resolved.kind = master.kind.clone();
        Self::apply_symbol_overrides(&mut resolved);
        Cow::Owned(resolved)
    }

    /// Apply a symbol instance's hex colour overrides onto its (master-derived)
    /// `kind`. A fill override replaces a solid fill; a stroke override replaces
    /// the stroke colour. No-op when overrides are absent or unparseable.
    fn apply_symbol_overrides(node: &mut SceneNode) {
        use crate::style::FillKind;
        let fill = node
            .symbol_fill_override
            .as_deref()
            .and_then(Color::from_hex);
        let stroke = node
            .symbol_stroke_override
            .as_deref()
            .and_then(Color::from_hex);
        if fill.is_none() && stroke.is_none() {
            return;
        }
        if let SceneNodeKind::Path(pn) = &mut node.kind {
            if let Some(c) = fill {
                pn.fill.kind = FillKind::Solid(c);
                pn.fill.enabled = true;
            }
            if let Some(c) = stroke {
                pn.stroke.color = c;
                pn.stroke.enabled = true;
            }
        }
    }

    /// Returns node count across all layers.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Find which layer contains a node and its index within that layer's node_ids.
    pub fn node_layer_and_index(&self, node_id: &NodeId) -> Option<(LayerId, usize)> {
        for layer_id in &self.layer_order {
            if let Some(layer) = self.layers.get(layer_id) {
                if let Some(pos) = layer.node_ids.iter().position(|id| id == node_id) {
                    return Some((*layer_id, pos));
                }
            }
        }
        None
    }

    /// Return the ID of the top-level ancestor of `node_id`.
    ///
    /// If the node already appears in a layer's `node_ids` it is returned as-is.
    /// If it is a group child, the search walks up the group hierarchy until a
    /// top-level node is found.  Returns `None` if the node does not exist.
    pub fn top_level_ancestor(&self, node_id: NodeId) -> Option<NodeId> {
        self.nodes.get(&node_id)?;
        // Already top-level?
        let is_top = self.layer_order.iter().any(|lid| {
            self.layers
                .get(lid)
                .map(|l| l.node_ids.contains(&node_id))
                .unwrap_or(false)
        });
        if is_top {
            return Some(node_id);
        }
        // Walk the group hierarchy
        for (&group_id, node) in &self.nodes {
            if let SceneNodeKind::Group(g) = &node.kind {
                if g.children.contains(&node_id) {
                    return self.top_level_ancestor(group_id);
                }
            }
        }
        None
    }

    /// Find the container (a layer, or a group) and index that directly holds
    /// `node_id`. Searches top-level layers first, then every group's children.
    pub fn node_container_and_index(&self, node_id: &NodeId) -> Option<(NodeContainer, usize)> {
        for lid in &self.layer_order {
            if let Some(layer) = self.layers.get(lid) {
                if let Some(pos) = layer.node_ids.iter().position(|id| id == node_id) {
                    return Some((NodeContainer::Layer(*lid), pos));
                }
            }
        }
        for (&gid, node) in &self.nodes {
            if let crate::node::SceneNodeKind::Group(g) = &node.kind {
                if let Some(pos) = g.children.iter().position(|id| id == node_id) {
                    return Some((NodeContainer::Group(gid), pos));
                }
            }
        }
        None
    }

    /// True if `node` is `ancestor` itself or lives anywhere in its subtree —
    /// the cycle guard for reparenting (you can't move a group into itself/a
    /// descendant).
    pub fn is_ancestor_or_self(&self, ancestor: NodeId, node: NodeId) -> bool {
        if ancestor == node {
            return true;
        }
        if let Some(crate::node::SceneNodeKind::Group(g)) =
            self.nodes.get(&ancestor).map(|n| &n.kind)
        {
            g.children
                .iter()
                .any(|&c| self.is_ancestor_or_self(c, node))
        } else {
            false
        }
    }

    /// Resolve a **live boolean** group (#25) to its single computed path, or
    /// `None` if the group has no `live_boolean` operator or no path children.
    /// Direct path children are transformed into the group's local space and
    /// folded left-to-right (bottom child first) with the operator. Non-path
    /// children are ignored. A failed boolean step keeps the accumulator so a
    /// degenerate operand can't wipe the whole result.
    pub fn resolve_live_boolean(&self, group_id: NodeId) -> Option<crate::path::PathData> {
        use crate::node::SceneNodeKind;
        let node = self.nodes.get(&group_id)?;
        let SceneNodeKind::Group(g) = &node.kind else {
            return None;
        };
        let op = g.live_boolean?;
        let mut acc: Option<crate::path::PathData> = None;
        for cid in &g.children {
            let Some(child) = self.nodes.get(cid) else {
                continue;
            };
            let SceneNodeKind::Path(pn) = &child.kind else {
                continue;
            };
            let mut bez = pn.path_data.to_bez_path();
            bez.apply_affine(child.transform.to_kurbo());
            let pd = crate::path::PathData::from_bez_path(&bez);
            acc = Some(match acc {
                None => pd,
                Some(a) => crate::ops::boolean::boolean_op(&a, &pd, op).unwrap_or(a),
            });
        }
        acc
    }

    /// 3-way merge of node objects between two diverged document states sharing
    /// the common ancestor `base` (e.g. the LCA of two edit-tree branch tips).
    /// `self`/`ours` is the side we keep on conflict; `theirs` is merged in.
    ///
    /// Non-conflicting changes from both sides are combined: nodes added on
    /// `theirs` are brought in (with their container membership), nodes deleted
    /// on `theirs` (and untouched on `ours`) are removed, and one-sided edits are
    /// taken. Where both sides changed the same node differently — or one edited
    /// while the other deleted — `ours` is kept and the clash is reported in
    /// [`MergeOutcome::conflicts`] for later resolution.
    pub fn merge_3way(&self, base: &Document, theirs: &Document) -> MergeOutcome {
        use crate::node::SceneNode;
        use std::collections::BTreeSet;
        let ours = self;
        let node_eq =
            |a: &SceneNode, b: &SceneNode| serde_json::to_vec(a).ok() == serde_json::to_vec(b).ok();
        let mut merged = ours.clone();
        let mut conflicts = Vec::new();

        let ids: BTreeSet<NodeId> = base
            .nodes
            .keys()
            .chain(ours.nodes.keys())
            .chain(theirs.nodes.keys())
            .copied()
            .collect();

        // Membership edits are applied after the node-set pass so container lists
        // are only touched once we know the final add/remove decisions.
        let mut to_add: Vec<NodeId> = Vec::new();
        let mut to_remove: Vec<NodeId> = Vec::new();

        for id in ids {
            match (
                base.nodes.get(&id),
                ours.nodes.get(&id),
                theirs.nodes.get(&id),
            ) {
                (Some(b), Some(o), Some(t)) => {
                    let o_changed = !node_eq(b, o);
                    let t_changed = !node_eq(b, t);
                    if t_changed && !o_changed {
                        merged.nodes.insert(id, t.clone());
                    } else if o_changed && t_changed && !node_eq(o, t) {
                        conflicts.push(MergeConflict::BothEdited(id));
                    }
                }
                // Added on theirs only → bring it in.
                (None, None, Some(t)) => {
                    merged.nodes.insert(id, t.clone());
                    to_add.push(id);
                }
                // Added on ours only → already present.
                (None, Some(_), None) => {}
                // Added on both with the same id (rare): conflict if they differ.
                (None, Some(o), Some(t)) if !node_eq(o, t) => {
                    conflicts.push(MergeConflict::BothEdited(id));
                }
                (None, Some(_), Some(_)) => {}
                // Deleted on theirs.
                (Some(b), Some(o), None) => {
                    if node_eq(b, o) {
                        merged.nodes.remove(&id);
                        to_remove.push(id);
                    } else {
                        conflicts.push(MergeConflict::EditVsDelete(id));
                    }
                }
                // Deleted on ours; theirs still has it.
                (Some(b), None, Some(t)) => {
                    if !node_eq(b, t) {
                        conflicts.push(MergeConflict::EditVsDelete(id));
                    }
                }
                // Deleted on both, or nonexistent.
                (Some(_), None, None) | (None, None, None) => {}
            }
        }

        // Bring theirs-added nodes into the same container they had on `theirs`
        // (matched by id), falling back to the active layer if that container no
        // longer exists on the merged side.
        for id in to_add {
            let placed = theirs
                .node_container_and_index(&id)
                .and_then(|(c, _)| merged.container_list_mut(c))
                .map(|list| {
                    if !list.contains(&id) {
                        list.push(id);
                    }
                })
                .is_some();
            if !placed {
                if let Some(lid) = merged.active_layer_id {
                    if let Some(l) = merged.layers.get_mut(&lid) {
                        l.node_ids.push(id);
                    }
                }
            }
        }
        // Drop deleted nodes from their container on the merged side.
        for id in to_remove {
            if let Some((c, _)) = ours.node_container_and_index(&id) {
                if let Some(list) = merged.container_list_mut(c) {
                    list.retain(|&n| n != id);
                }
            }
        }

        MergeOutcome { merged, conflicts }
    }

    /// Mutable access to a container's ordered child-id list.
    pub(crate) fn container_list_mut(&mut self, c: NodeContainer) -> Option<&mut Vec<NodeId>> {
        match c {
            NodeContainer::Layer(lid) => self.layers.get_mut(&lid).map(|l| &mut l.node_ids),
            NodeContainer::Group(gid) => match self.nodes.get_mut(&gid).map(|n| &mut n.kind) {
                Some(crate::node::SceneNodeKind::Group(g)) => Some(&mut g.children),
                _ => None,
            },
        }
    }

    /// Find the shared layer and indices for a set of nodes.
    /// Returns None if any node is missing or they span different layers.
    pub fn nodes_layer_and_indices(
        &self,
        node_ids: &[NodeId],
    ) -> Option<(LayerId, Vec<(NodeId, usize)>)> {
        if node_ids.is_empty() {
            return None;
        }
        let mut result = Vec::new();
        let mut common_layer: Option<LayerId> = None;
        for &nid in node_ids {
            let (layer_id, idx) = self.node_layer_and_index(&nid)?;
            if let Some(lid) = common_layer {
                if lid != layer_id {
                    return None; // nodes span different layers
                }
            } else {
                common_layer = Some(layer_id);
            }
            result.push((nid, idx));
        }
        Some((common_layer?, result))
    }

    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from a JSON string, migrating the raw document forward to
    /// [`CURRENT_FORMAT_VERSION`] first.
    ///
    /// Older documents are upgraded through the [`crate::migration`] chain before
    /// deserialization. A document saved by a *newer* build loads leniently
    /// (unknown fields dropped) while within
    /// [`crate::migration::COMPAT_WINDOW`]; beyond that window it is rejected.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(json)?;
        Self::from_value(value)
    }

    /// Deserialize from an already-parsed JSON tree, migrating it forward to
    /// [`CURRENT_FORMAT_VERSION`] first. Shared by [`from_json`] and the
    /// `.photon` file wrapper (which carries the document as a sub-value).
    ///
    /// The [`LoadReport`](crate::timeline::load::LoadReport) is discarded; use
    /// [`from_value_with_report`](Self::from_value_with_report) to observe the
    /// unknown-variant findings (39 §2.2).
    pub fn from_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        Self::from_value_with_report(value).map(|(doc, _report)| doc)
    }

    /// Like [`from_value`](Self::from_value) but also returns the load-time
    /// [`LoadReport`](crate::timeline::load::LoadReport), which names any
    /// unknown enum variants preserved from a newer-build file (39 §2.2 rule 3:
    /// once per load). The report is empty for a document with no unknowns.
    pub fn from_value_with_report(
        mut value: serde_json::Value,
    ) -> Result<(Self, crate::timeline::load::LoadReport), serde_json::Error> {
        use crate::migration;

        // Migrate at the JSON-tree level before struct deserialization so new
        // fields can be filled and moved fields renamed without the in-memory
        // types having to understand older layouts.
        let file_version = migration::detect_version(&value);

        if file_version > CURRENT_FORMAT_VERSION {
            if file_version - CURRENT_FORMAT_VERSION > migration::COMPAT_WINDOW {
                return Err(serde::de::Error::custom(format!(
                    "unsupported format version {} (this build supports up to {}, \
                     compatibility window +{})",
                    file_version,
                    CURRENT_FORMAT_VERSION,
                    migration::COMPAT_WINDOW
                )));
            }
            // Within the window: fall through and load leniently.
        } else {
            migration::run_migrations(&mut value, CURRENT_FORMAT_VERSION)
                .map_err(serde::de::Error::custom)?;
        }

        let mut doc: Document = serde_json::from_value(value)?;
        // Legacy documents predate multi-artboard — synthesize the first one.
        doc.ensure_default_artboard();
        // Finalize a loaded timeline: flag orphaned property paths (repair) and
        // enforce the per-sequence invariants (reject a corrupt/overlapping
        // timeline with a load error). Only runs when a timeline is present
        // (01 §4, §6.2).
        let report = if let Some(timeline) = doc.timeline.as_mut() {
            crate::timeline::load::finalize_load(timeline).map_err(serde::de::Error::custom)?
        } else {
            crate::timeline::load::LoadReport::default()
        };
        Ok((doc, report))
    }
}

// ─── In-canvas color sampling ─────────────────────────────────────────────────

/// Sample the topmost visible node at canvas-space coordinates `(x, y)`.
///
/// Walks layers and nodes from top to bottom (reverse of the rendering order
/// stored in `layer_order` / `node_ids`) and returns the colour of the first
/// node that actually covers the point:
///
/// - **Path** nodes: a non-zero winding-rule hit test (the canvas point is
///   mapped into the node's *local* space first, so moved/scaled/rotated shapes
///   test correctly), then `FillKind::sample_at` at the canvas coordinate — the
///   same space the renderer samples gradients in, so the eyedropper matches
///   what's on screen. Fill-disabled paths are skipped (the point falls through).
/// - **Raster** nodes (images): the pixel at the mapped local coordinate,
///   honouring the layer mask and node opacity. Fully-transparent/masked pixels
///   are skipped so the eyedropper samples whatever shows through beneath them.
///
/// Text and group nodes are skipped. Returns `None` if nothing opaque covers the
/// point.
pub fn sample_fill_at(doc: &Document, x: f64, y: f64) -> Option<[f32; 4]> {
    use kurbo::Shape;
    let pt = kurbo::Point::new(x, y);

    for lid in doc.layer_order.iter().rev() {
        let layer = match doc.layers.get(lid) {
            Some(l) if l.visible => l,
            _ => continue,
        };
        for nid in layer.node_ids.iter().rev() {
            let node = match doc.nodes.get(nid) {
                Some(n) if n.visible => n,
                _ => continue,
            };
            // Map the canvas point into the node's local space for hit-testing.
            // (A non-invertible transform yields non-finite coords, which fail
            // every containment test below — the node is simply skipped.)
            let local = node.transform.to_kurbo().inverse() * pt;
            match &node.kind {
                SceneNodeKind::Path(pn) => {
                    if !pn.fill.enabled {
                        continue;
                    }
                    let bez = pn.path_data.to_bez_path();
                    if bez.winding(local) != 0 {
                        let opacity = pn.fill.opacity * node.opacity;
                        // Gradients are sampled in canvas space by the renderer,
                        // so sample there too for an on-screen-accurate colour.
                        return Some(pn.fill.kind.sample_at(x, y, opacity));
                    }
                }
                SceneNodeKind::Raster(rn) => {
                    if rn.is_adjustment_layer() {
                        continue;
                    }
                    if local.x < 0.0
                        || local.y < 0.0
                        || local.x >= rn.image.width as f64
                        || local.y >= rn.image.height as f64
                    {
                        continue;
                    }
                    let (px, py) = (local.x as u32, local.y as u32);
                    let rgba = rn.image.pixel(px, py);
                    // A layer mask (and node opacity) can hide the pixel — if it
                    // is not visible here, keep looking beneath it.
                    let cov = rn.mask.as_ref().map(|m| m.coverage(px, py)).unwrap_or(1.0);
                    let visible = (rgba[3] as f32 / 255.0) * cov * node.opacity;
                    if visible <= 0.0 {
                        continue;
                    }
                    return Some([
                        rgba[0] as f32 / 255.0,
                        rgba[1] as f32 / 255.0,
                        rgba[2] as f32 / 255.0,
                        rgba[3] as f32 / 255.0,
                    ]);
                }
                SceneNodeKind::Text(_) | SceneNodeKind::Group(_) => continue,
            }
        }
    }
    None
}

/// A page (for multi-page documents — future use).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub id: Uuid,
    pub name: String,
    pub document: Document,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        node::{PathNode, SceneNode, SceneNodeKind},
        path::PathData,
        style::Fill,
        Color,
    };

    #[test]
    fn merge_3way_combines_nonconflicts_and_flags_conflicts() {
        use crate::style::Fill;
        let mk = |hex: &str| {
            PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))
                .with_fill(Fill::solid(Color::from_hex(hex).unwrap()))
        };
        let set_fill = |doc: &mut Document, id: NodeId, hex: &str| {
            if let SceneNodeKind::Path(p) = &mut doc.nodes.get_mut(&id).unwrap().kind {
                p.fill = Fill::solid(Color::from_hex(hex).unwrap());
            }
        };
        let json = |doc: &Document, id: &NodeId| serde_json::to_vec(&doc.nodes[id]).unwrap();

        let mut base = Document::new("t", 100.0, 100.0);
        let lid = base.layer_order[0];
        let a = SceneNode::new("A", lid, SceneNodeKind::Path(mk("#ff0000")));
        let aid = a.id;
        base.add_node(a, Some(lid));

        // 1. theirs adds a node; ours untouched → brought in + placed in the layer.
        {
            let ours = base.clone();
            let mut theirs = base.clone();
            let b = SceneNode::new("B", lid, SceneNodeKind::Path(mk("#00ff00")));
            let bid = b.id;
            theirs.add_node(b, Some(lid));
            let out = ours.merge_3way(&base, &theirs);
            assert!(out.conflicts.is_empty());
            assert!(out.merged.nodes.contains_key(&bid));
            assert!(out.merged.layers[&lid].node_ids.contains(&bid));
        }

        // 2. theirs edits A; ours untouched → take theirs.
        {
            let ours = base.clone();
            let mut theirs = base.clone();
            set_fill(&mut theirs, aid, "#0000ff");
            let out = ours.merge_3way(&base, &theirs);
            assert!(out.conflicts.is_empty());
            assert_eq!(
                json(&out.merged, &aid),
                json(&theirs, &aid),
                "theirs edit taken"
            );
        }

        // 3. both edit A differently → conflict, keep ours.
        {
            let mut ours = base.clone();
            let mut theirs = base.clone();
            set_fill(&mut ours, aid, "#00ff00");
            set_fill(&mut theirs, aid, "#0000ff");
            let out = ours.merge_3way(&base, &theirs);
            assert_eq!(out.conflicts, vec![MergeConflict::BothEdited(aid)]);
            assert_eq!(
                json(&out.merged, &aid),
                json(&ours, &aid),
                "ours kept on conflict"
            );
        }

        // 4. theirs deletes A; ours untouched → removed.
        {
            let ours = base.clone();
            let mut theirs = base.clone();
            theirs.remove_node(&aid);
            let out = ours.merge_3way(&base, &theirs);
            assert!(out.conflicts.is_empty());
            assert!(!out.merged.nodes.contains_key(&aid));
            assert!(!out.merged.layers[&lid].node_ids.contains(&aid));
        }

        // 5. ours edits A, theirs deletes A → conflict, keep ours.
        {
            let mut ours = base.clone();
            set_fill(&mut ours, aid, "#00ff00");
            let mut theirs = base.clone();
            theirs.remove_node(&aid);
            let out = ours.merge_3way(&base, &theirs);
            assert_eq!(out.conflicts, vec![MergeConflict::EditVsDelete(aid)]);
            assert!(out.merged.nodes.contains_key(&aid));
        }
    }

    #[test]
    fn live_boolean_group_resolves_union_intersection_and_round_trips() {
        use crate::node::GroupNode;
        use crate::ops::boolean::BooleanOp;
        let mut doc = Document::new("t", 100.0, 100.0);
        let lid = doc.layer_order[0];
        // Two overlapping 10×10 rects: A=(0,0,10,10), B=(5,5,15,15).
        let a = SceneNode::new(
            "a",
            lid,
            SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))),
        );
        let b = SceneNode::new(
            "b",
            lid,
            SceneNodeKind::Path(PathNode::new(PathData::rect(5.0, 5.0, 10.0, 10.0))),
        );
        let (aid, bid) = (a.id, b.id);
        doc.add_node(a, Some(lid));
        doc.add_node(b, Some(lid));
        let mut group = GroupNode::new();
        group.children = vec![aid, bid];
        group.live_boolean = Some(BooleanOp::Union);
        let gnode = SceneNode::new("bool", lid, SceneNodeKind::Group(group));
        let gid = gnode.id;
        doc.add_node(gnode, Some(lid));

        let bbox = |d: &Document| d.resolve_live_boolean(gid).unwrap().bounding_box().unwrap();
        let ub = bbox(&doc);
        assert!(
            (ub.x0).abs() < 1e-6 && (ub.y0).abs() < 1e-6 && (ub.x1 - 15.0).abs() < 1e-6,
            "union bbox should span (0,0)-(15,15), got {ub:?}"
        );

        set_bool(&mut doc, gid, Some(BooleanOp::Intersect));
        let ib = bbox(&doc);
        assert!(
            (ib.x0 - 5.0).abs() < 1e-6 && (ib.x1 - 10.0).abs() < 1e-6,
            "intersection bbox should span (5,5)-(10,10), got {ib:?}"
        );

        // A live-boolean group serializes/deserializes (serde default None otherwise).
        set_bool(&mut doc, gid, Some(BooleanOp::Subtract));
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            &back.nodes.get(&gid).unwrap().kind,
            SceneNodeKind::Group(g) if g.live_boolean == Some(BooleanOp::Subtract)
        ));

        // Plain group → no resolved path.
        set_bool(&mut doc, gid, None);
        assert!(doc.resolve_live_boolean(gid).is_none());
    }

    fn set_bool(doc: &mut Document, gid: NodeId, op: Option<crate::ops::boolean::BooleanOp>) {
        if let SceneNodeKind::Group(g) = &mut doc.nodes.get_mut(&gid).unwrap().kind {
            g.live_boolean = op;
        }
    }

    #[test]
    fn live_boolean_group_svg_exports_single_resolved_path() {
        use crate::export::{export_svg, SvgExportOptions};
        use crate::node::GroupNode;
        use crate::ops::boolean::BooleanOp;
        use crate::style::Fill;
        let mut doc = Document::new("t", 100.0, 100.0);
        let lid = doc.layer_order[0];
        let a = SceneNode::new(
            "a",
            lid,
            SceneNodeKind::Path(
                PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))
                    .with_fill(Fill::solid(Color::from_hex("#ff0000").unwrap())),
            ),
        );
        let b = SceneNode::new(
            "b",
            lid,
            SceneNodeKind::Path(PathNode::new(PathData::rect(5.0, 5.0, 10.0, 10.0))),
        );
        let (aid, bid) = (a.id, b.id);
        doc.add_node(a, Some(lid));
        doc.add_node(b, Some(lid));
        let mut group = GroupNode::new();
        group.children = vec![aid, bid];
        group.live_boolean = Some(BooleanOp::Union);
        doc.add_node(
            SceneNode::new("bool", lid, SceneNodeKind::Group(group)),
            Some(lid),
        );

        let svg = export_svg(&doc, &SvgExportOptions::default());
        // The group emits a single <path> (the union), not nested child rects,
        // and it carries the bottom child's red fill.
        assert!(svg.contains("<path"), "expected a resolved path in {svg}");
        assert!(
            svg.to_lowercase().contains("ff0000"),
            "resolved path should take the bottom child's red fill, got {svg}"
        );
    }

    #[test]
    fn per_document_history_cap_round_trips_and_defaults_none() {
        // Fresh documents don't pin a history cap (falls back to global prefs).
        let mut doc = Document::new("t", 100.0, 100.0);
        assert!(doc.history_max_mb.is_none());
        // A set value survives serialize → deserialize (#195).
        doc.history_max_mb = Some(120.0);
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.history_max_mb, Some(120.0));
        // Old files without the field deserialize to None.
        let no_field = serde_json::to_string(&Document::new("t", 100.0, 100.0)).unwrap();
        assert!(
            !no_field.contains("history_max_mb"),
            "None should skip the key"
        );
    }

    /// Build a minimal document with one layer and the supplied path nodes
    /// (given in bottom-to-top order).
    fn doc_with_nodes(nodes: Vec<(PathData, Color)>) -> Document {
        let mut doc = Document::new("test", 400.0, 400.0);
        let layer_id = doc.layer_order[0];
        for (path_data, color) in nodes {
            let pn = PathNode::new(path_data).with_fill(Fill::solid(color));
            let node = SceneNode::new("n", layer_id, SceneNodeKind::Path(pn));
            let nid = node.id;
            doc.nodes.insert(nid, node);
            doc.layers.get_mut(&layer_id).unwrap().node_ids.push(nid);
        }
        doc
    }

    #[test]
    fn group_helpers_resolve_nested_membership() {
        use crate::node::GroupNode;
        let path = PathData::rect(0.0, 0.0, 10.0, 10.0);
        let mut doc = doc_with_nodes(vec![
            (path.clone(), Color::RED),
            (path.clone(), Color::BLUE),
            (path, Color::GREEN),
        ]);
        let layer_id = doc.layer_order[0];
        let ids: Vec<NodeId> = doc.layers[&layer_id].node_ids.clone();
        let (a, b, c) = (ids[0], ids[1], ids[2]);

        // inner = group(a, b); outer = group(inner, c)
        let mut inner = GroupNode::new();
        inner.children = vec![a, b];
        let inner_node = SceneNode::new("inner", layer_id, SceneNodeKind::Group(inner));
        let inner_id = inner_node.id;
        doc.add_node(inner_node, Some(layer_id));
        let mut outer = GroupNode::new();
        outer.children = vec![inner_id, c];
        let outer_node = SceneNode::new("outer", layer_id, SceneNodeKind::Group(outer));
        let outer_id = outer_node.id;
        doc.add_node(outer_node, Some(layer_id));

        assert_eq!(doc.parent_group_of(&a), Some(inner_id));
        assert_eq!(doc.parent_group_of(&inner_id), Some(outer_id));
        assert_eq!(doc.outermost_group_of(&a), Some(outer_id));
        assert_eq!(doc.outermost_group_of(&c), Some(outer_id));
        assert_eq!(doc.outermost_group_of(&outer_id), None);

        // Members = the three leaves, never the group nodes themselves.
        let mut members = doc.group_member_ids(&outer_id);
        members.sort();
        let mut expected = vec![a, b, c];
        expected.sort();
        assert_eq!(members, expected);
    }

    #[test]
    fn artboard_for_rect_picks_max_overlap() {
        let mut doc = Document::new("test", 100.0, 100.0);
        // Default artboard covers (0,0,100,100); add a second to its right.
        let second = Artboard::new("Artboard 2", 150.0, 0.0, 100.0, 100.0);
        let second_id = doc.add_artboard(second);
        // Rect mostly over the second artboard.
        let hit = doc
            .artboard_for_rect(140.0, 10.0, 240.0, 90.0)
            .expect("hit");
        assert_eq!(hit.id, second_id);
        // Rect fully over the first.
        let hit = doc.artboard_for_rect(10.0, 10.0, 90.0, 90.0).expect("hit");
        assert_eq!(hit.rect().0, 0.0);
        // Rect overlapping neither.
        assert!(doc.artboard_for_rect(400.0, 400.0, 500.0, 500.0).is_none());
    }

    #[test]
    fn no_hit_returns_none() {
        let path = PathData::rect(10.0, 10.0, 50.0, 50.0);
        let doc = doc_with_nodes(vec![(path, Color::RED)]);
        // Point outside the rectangle
        assert!(sample_fill_at(&doc, 200.0, 200.0).is_none());
    }

    #[test]
    fn samples_raster_pixels() {
        use crate::node::RasterNode;
        use crate::raster::image::RasterImage;
        let mut img = RasterImage::filled(10, 10, [0, 0, 0, 255]);
        img.set_pixel(3, 4, [10, 200, 30, 255]); // a distinct green pixel
        let mut doc = Document::new("t", 100.0, 100.0);
        let layer_id = doc.layer_order[0];
        let mut node = SceneNode::new("img", layer_id, SceneNodeKind::Raster(RasterNode::new(img)));
        node.transform = crate::transform::Transform::translate(20.0, 20.0);
        let nid = node.id;
        doc.nodes.insert(nid, node);
        doc.layers.get_mut(&layer_id).unwrap().node_ids.push(nid);

        // Canvas (23,24) maps to the green pixel (3,4) of the image at origin (20,20).
        let s = sample_fill_at(&doc, 23.5, 24.5).expect("raster hit");
        assert!((s[0] - 10.0 / 255.0).abs() < 1e-3, "r");
        assert!((s[1] - 200.0 / 255.0).abs() < 1e-3, "g");
        assert!((s[2] - 30.0 / 255.0).abs() < 1e-3, "b");
        // Outside the image → nothing.
        assert!(sample_fill_at(&doc, 5.0, 5.0).is_none());
    }

    #[test]
    fn transparent_raster_pixel_falls_through() {
        use crate::node::RasterNode;
        use crate::raster::image::RasterImage;
        // A red path beneath, a raster with a transparent hole on top.
        let mut doc = doc_with_nodes(vec![(PathData::rect(0.0, 0.0, 100.0, 100.0), Color::RED)]);
        let layer_id = doc.layer_order[0];
        let img = RasterImage::filled(10, 10, [0, 0, 0, 0]); // fully transparent
        let node = SceneNode::new("img", layer_id, SceneNodeKind::Raster(RasterNode::new(img)));
        let nid = node.id;
        doc.nodes.insert(nid, node);
        doc.layers.get_mut(&layer_id).unwrap().node_ids.push(nid); // on top
                                                                   // Over the transparent raster → samples the red path underneath.
        let s = sample_fill_at(&doc, 5.0, 5.0).expect("hit");
        assert!(
            (s[0] - 1.0).abs() < 1e-6 && s[1].abs() < 1e-6,
            "should see red beneath"
        );
    }

    #[test]
    fn transformed_path_hit_test_uses_local_space() {
        // A 10×10 rect translated to (50,50). The canvas point (55,55) is inside
        // the moved shape; (5,5) is not — the pre-fix code (winding local geometry
        // against the raw canvas point) got this backwards.
        let mut doc = Document::new("t", 200.0, 200.0);
        let layer_id = doc.layer_order[0];
        let pn =
            PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0)).with_fill(Fill::solid(Color::BLUE));
        let mut node = SceneNode::new("r", layer_id, SceneNodeKind::Path(pn));
        node.transform = crate::transform::Transform::translate(50.0, 50.0);
        let nid = node.id;
        doc.nodes.insert(nid, node);
        doc.layers.get_mut(&layer_id).unwrap().node_ids.push(nid);

        assert!(
            sample_fill_at(&doc, 55.0, 55.0).is_some(),
            "inside moved rect"
        );
        assert!(
            sample_fill_at(&doc, 5.0, 5.0).is_none(),
            "at the old (untransformed) spot"
        );
    }

    #[test]
    fn single_node_hit() {
        let path = PathData::rect(0.0, 0.0, 100.0, 100.0);
        let doc = doc_with_nodes(vec![(path, Color::RED)]);
        let sampled = sample_fill_at(&doc, 50.0, 50.0).expect("expected a hit");
        assert!((sampled[0] - 1.0).abs() < 1e-6, "red channel");
        assert!((sampled[1]).abs() < 1e-6, "green channel");
        assert!((sampled[2]).abs() < 1e-6, "blue channel");
        assert!((sampled[3] - 1.0).abs() < 1e-6, "alpha channel");
    }

    #[test]
    fn overlapping_topmost_wins() {
        // bottom = red square, top = blue square (both cover (50,50))
        let bottom = PathData::rect(0.0, 0.0, 100.0, 100.0);
        let top = PathData::rect(25.0, 25.0, 75.0, 75.0);
        // doc_with_nodes adds nodes bottom-to-top; last entry = topmost
        let doc = doc_with_nodes(vec![(bottom, Color::RED), (top, Color::BLUE)]);
        let sampled = sample_fill_at(&doc, 50.0, 50.0).expect("expected a hit");
        // Blue (top) should win
        assert!(
            (sampled[2] - 1.0).abs() < 1e-6,
            "blue channel should be 1.0"
        );
        assert!((sampled[0]).abs() < 1e-6, "red channel should be 0.0");
    }

    #[test]
    fn width_profile_new_seeds_uniform_positions() {
        let wp = WidthProfile::new("taper", vec![2.0, 6.0, 10.0]);
        assert_eq!(wp.positions, vec![0.0, 0.5, 1.0]);
        assert!(wp.widths_right.is_none());
        assert!((wp.average_width() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn width_profile_legacy_json_defaults_positions_empty_then_falls_back() {
        // A document written before `positions`/`widths_right` existed.
        let json =
            r#"{"id":"00000000-0000-0000-0000-000000000001","name":"old","widths":[1.0,3.0]}"#;
        let wp: WidthProfile = serde_json::from_str(json).unwrap();
        assert!(wp.positions.is_empty(), "legacy files have no positions");
        assert!(wp.widths_right.is_none());
        // Fallback fills evenly spaced positions matching the sample count.
        assert_eq!(wp.effective_positions(), vec![0.0, 1.0]);
    }

    #[test]
    fn width_profile_with_explicit_positions_preserved() {
        let wp = WidthProfile::with_positions("t", vec![0.0, 0.3, 1.0], vec![1.0, 5.0, 2.0]);
        assert_eq!(wp.effective_positions(), vec![0.0, 0.3, 1.0]);
    }
}

#[cfg(test)]
mod format_version_tests {
    use super::*;

    #[test]
    fn current_version_round_trips() {
        let doc = Document::new("v1", 100.0, 100.0);
        let json = doc.to_json().unwrap();
        let loaded = Document::from_json(&json).unwrap();
        assert_eq!(loaded.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(loaded.name, "v1");
    }

    #[test]
    fn missing_version_defaults_and_loads() {
        // A pre-versioning document: a real document with format_version removed.
        let mut value: serde_json::Value =
            serde_json::from_str(&Document::new("old", 10.0, 10.0).to_json().unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("format_version");
        assert!(value.get("format_version").is_none());
        let loaded = Document::from_json(&value.to_string()).unwrap();
        assert_eq!(loaded.name, "old");
        assert_eq!(loaded.format_version, CURRENT_FORMAT_VERSION);
    }

    #[test]
    fn slightly_newer_version_loads_leniently() {
        // One version ahead with an unknown field — within COMPAT_WINDOW.
        let mut value: serde_json::Value =
            serde_json::from_str(&Document::new("future", 10.0, 10.0).to_json().unwrap()).unwrap();
        value["format_version"] =
            serde_json::Value::from(CURRENT_FORMAT_VERSION + crate::migration::COMPAT_WINDOW);
        value["a_field_from_the_future"] = serde_json::Value::from("ignored");
        let loaded = Document::from_json(&value.to_string()).unwrap();
        assert_eq!(loaded.name, "future");
    }

    #[test]
    fn far_future_version_is_rejected() {
        let mut value: serde_json::Value =
            serde_json::from_str(&Document::new("toonew", 10.0, 10.0).to_json().unwrap()).unwrap();
        value["format_version"] =
            serde_json::Value::from(CURRENT_FORMAT_VERSION + crate::migration::COMPAT_WINDOW + 1);
        assert!(Document::from_json(&value.to_string()).is_err());
    }
}

#[cfg(test)]
mod symbol_resolution_tests {
    use super::*;
    use crate::node::PathNode;
    use crate::path::PathData;
    use crate::style::{Fill, FillKind};

    fn solid_fill(node: &SceneNode) -> Color {
        match &node.kind {
            SceneNodeKind::Path(pn) => match pn.fill.kind {
                FillKind::Solid(c) => c,
                _ => panic!("expected solid fill"),
            },
            _ => panic!("expected path node"),
        }
    }

    /// Build a master path node (red fill) + register it as symbol "sym".
    fn doc_with_symbol() -> (Document, NodeId, Uuid) {
        let mut doc = Document::new("test", 100.0, 100.0);
        let layer = doc.active_layer_id.unwrap();
        let mut master = SceneNode::new(
            "master",
            layer,
            SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))),
        );
        if let SceneNodeKind::Path(pn) = &mut master.kind {
            pn.fill = Fill::solid(Color::RED);
        }
        let master_id = doc.add_node(master, None);
        let sym = Symbol::new("sym", master_id);
        let sym_id = sym.id;
        doc.symbols.push(sym);
        (doc, master_id, sym_id)
    }

    fn make_instance(doc: &Document, master_id: NodeId, sym_id: Uuid) -> SceneNode {
        let mut inst = doc.nodes.get(&master_id).unwrap().clone();
        inst.id = Uuid::new_v4();
        inst.name = "instance".into();
        inst.symbol_ref = Some(sym_id);
        inst.transform = crate::transform::Transform::translate(50.0, 50.0);
        // Freeze a stale green fill on the instance copy.
        if let SceneNodeKind::Path(pn) = &mut inst.kind {
            pn.fill = Fill::solid(Color::GREEN);
        }
        inst
    }

    #[test]
    fn master_edits_propagate_to_instances() {
        let (mut doc, master_id, sym_id) = doc_with_symbol();
        let inst = make_instance(&doc, master_id, sym_id);
        let inst_id = doc.add_node(inst, None);

        // Edit the master fill to blue *after* the instance was placed.
        if let Some(SceneNodeKind::Path(pn)) = doc.nodes.get_mut(&master_id).map(|n| &mut n.kind) {
            pn.fill = Fill::solid(Color::BLUE);
        }

        let inst_ref = doc.nodes.get(&inst_id).unwrap();
        let resolved = doc.resolve_render_node(inst_ref);
        // Geometry/style now reflects the current master (blue), not the frozen green.
        assert_eq!(solid_fill(&resolved), Color::BLUE);
        // Instance placement is preserved.
        assert_eq!(resolved.transform.apply(0.0, 0.0), (50.0, 50.0));
    }

    #[test]
    fn fill_override_takes_precedence_over_master() {
        let (doc, master_id, sym_id) = doc_with_symbol();
        let mut inst = make_instance(&doc, master_id, sym_id);
        inst.symbol_fill_override = Some("#00ffff".into()); // cyan
        let resolved = doc.resolve_render_node(&inst);
        let c = solid_fill(&resolved);
        assert!((c.r - 0.0).abs() < 1e-3 && (c.g - 1.0).abs() < 1e-3 && (c.b - 1.0).abs() < 1e-3);
    }

    #[test]
    fn non_instance_is_borrowed_unchanged() {
        let (doc, master_id, _sym_id) = doc_with_symbol();
        let master = doc.nodes.get(&master_id).unwrap();
        let resolved = doc.resolve_render_node(master);
        assert!(matches!(resolved, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn self_referential_symbol_does_not_loop() {
        // A symbol whose master is the instance itself must not infinite-loop.
        let mut doc = Document::new("test", 100.0, 100.0);
        let layer = doc.active_layer_id.unwrap();
        let node = SceneNode::new(
            "n",
            layer,
            SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 5.0, 5.0))),
        );
        let nid = doc.add_node(node, None);
        let sym = Symbol::new("self", nid);
        let sym_id = sym.id;
        doc.symbols.push(sym);
        // Point the node at the symbol whose master is the node itself.
        doc.nodes.get_mut(&nid).unwrap().symbol_ref = Some(sym_id);
        let n = doc.nodes.get(&nid).unwrap();
        let resolved = doc.resolve_render_node(n);
        assert!(matches!(resolved, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn dangling_symbol_ref_renders_frozen_copy() {
        let (doc, master_id, _sym_id) = doc_with_symbol();
        let mut inst = make_instance(&doc, master_id, Uuid::new_v4()); // unknown symbol id
        inst.symbol_ref = Some(Uuid::new_v4());
        let resolved = doc.resolve_render_node(&inst);
        // Falls back to the instance's own (green) copy.
        assert_eq!(solid_fill(&resolved), Color::GREEN);
    }
}

#[cfg(test)]
mod print_preset_tests {
    use super::*;
    use crate::units::DocumentUnit;

    #[test]
    fn business_card_us_300dpi() {
        let doc = Document::from_preset(PrintPreset::BUSINESS_CARD_US, 300.0);
        // 3.5 in × 300 dpi = 1050 px wide; 2.0 in × 300 dpi = 600 px tall
        assert!(
            (doc.width - 1050.0).abs() < 1e-9,
            "expected width 1050 px, got {}",
            doc.width
        );
        assert!(
            (doc.height - 600.0).abs() < 1e-9,
            "expected height 600 px, got {}",
            doc.height
        );
        assert!((doc.dpi - 300.0).abs() < 1e-9);
        assert_eq!(doc.display_unit, DocumentUnit::In);
    }

    #[test]
    fn business_card_us_72dpi() {
        let doc = Document::from_preset(PrintPreset::BUSINESS_CARD_US, 72.0);
        // 3.5 in × 72 dpi = 252 px; 2.0 in × 72 dpi = 144 px
        assert!(
            (doc.width - 252.0).abs() < 1e-9,
            "expected width 252 px, got {}",
            doc.width
        );
        assert!(
            (doc.height - 144.0).abs() < 1e-9,
            "expected height 144 px, got {}",
            doc.height
        );
    }
}
