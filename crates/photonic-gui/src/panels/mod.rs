use egui::{Color32, RichText, Ui};
use egui_phosphor::regular as ph;
use photonic_core::{
    layer::LayerId,
    node::NodeId,
    ops::boolean::BooleanOp,
    style::{LineJoin, Stroke},
    Document, Fill, GaussianGlow, GlowEffect, PrimitiveKind, SceneNode, SceneNodeKind,
};
use uuid::Uuid;

use crate::app::AppMode;
use crate::color_popup::ColorPopup;
use crate::radial_wheel::WheelAction;
use crate::tools::Tool;

mod arrange;
mod assets;
mod document;
mod editors;
mod history;
mod inspector;
mod layers_panel;
/// Media pool panel + import ladder. `pub` for UI-path integration tests.
pub mod media_pool;
mod modify;
mod navigator;
mod toolbar;
mod tools_panel;
mod vertex_panel;
pub(crate) mod video;

pub(crate) use video::{ColorPageTab, ScopeKind, VideoPanelUi};

use arrange::*;
use assets::*;
use document::*;
use history::*;
use inspector::*;
use modify::*;

pub(crate) use editors::{
    default_checker_tile, draw_gaussian_glow_editor, draw_glow_editor, draw_stroke_editor,
};
pub use layers_panel::draw_layers_panel;
pub use toolbar::draw_toolbar;
pub use tools_panel::draw_tools_panel;
pub(crate) use vertex_panel::draw_vertex_panel;

// ─── Eyedropper types ─────────────────────────────────────────────────────────

/// Which color slot should receive the eyedropper result (node-agnostic).
#[derive(Debug, Clone)]
pub enum FillColorSlot {
    Solid,
    GradientStop(usize),
    FluidPoint(usize),
    MeshVertex(usize),
}

/// Full eyedropper target including node context.
#[derive(Debug, Clone)]
pub enum EyedropperTarget {
    NewShapeFill,
    NodeFillSolid {
        node_id: NodeId,
    },
    /// Sample one color and apply it as a solid fill to every listed node
    /// (multi-selection eyedropper). Recorded as a single undoable batch.
    NodesFillSolid {
        node_ids: Vec<NodeId>,
    },
    NodeFillGradStop {
        node_id: NodeId,
        idx: usize,
    },
    NodeFillFluid {
        node_id: NodeId,
        idx: usize,
    },
    NodeFillMesh {
        node_id: NodeId,
        idx: usize,
    },
    NodeStroke {
        node_id: NodeId,
    },
    /// Sample one color and apply it as the stroke color of every listed node
    /// (multi-selection eyedropper). Recorded as a single undoable batch;
    /// each node keeps its own stroke width/dash.
    NodesStroke {
        node_ids: Vec<NodeId>,
    },
    NodeOuterGlow {
        node_id: NodeId,
    },
    NodeInnerGlow {
        node_id: NodeId,
    },
    NodeGaussianGlow {
        node_id: NodeId,
    },
    /// Sample a color on a raster layer to begin a color-range mask-out session
    /// (hide every pixel within tolerance of the picked color).
    RasterColorRange {
        node_id: NodeId,
    },
    /// Recolor every object listed in `ids` (all currently sharing the `from`
    /// color) to the sampled color, as one undoable step. Driven by the
    /// "Fill colors in document" swatch picker's eyedropper button.
    RecolorSwatch {
        ids: Vec<NodeId>,
        from: [f32; 4],
    },
    /// Seed a clip grade's HSL qualifier from a pixel sampled off the program
    /// monitor (07 §5 / 13 §9.3). Extends the one eyedropper idiom into the video
    /// color page rather than a parallel picker; the app handler samples the
    /// engine frame and applies the sampled colour's HSL to the clip's
    /// `HslQualifier` op via `SetGrade`.
    GradeQualifier {
        seq: photonic_core::timeline::SequenceId,
        track: photonic_core::timeline::TrackId,
        clip: photonic_core::timeline::ClipId,
        op: photonic_core::timeline::GradeOpId,
    },
}

/// An action requested by a panel widget, to be processed by the main draw loop.
#[derive(Debug)]
pub enum PanelAction {
    /// Reorder a node in z-order.
    ReorderNode { node_id: NodeId, op: ZOrderOp },
    /// Select a single node (e.g. clicked in the Layers tree).
    SelectNode { node_id: NodeId },
    /// Run a boolean operation on the two currently selected nodes.
    BooleanOp(BooleanOp),
    /// Restore the document to a named checkpoint.
    ///
    /// The right-side Change Log UI that produced this action was removed
    /// (#173) as redundant with the left-drawer Edit History; the restore
    /// handler is retained for a future menu relocation of checkpoint restore.
    #[allow(dead_code)]
    RestoreCheckpoint(Uuid),
    /// Update the fill of a selected node.
    UpdateNodeFill { node_id: NodeId, fill: Fill },
    /// Update the stroke of a selected node.
    UpdateNodeStroke { node_id: NodeId, stroke: Stroke },
    /// Apply the same fill to every listed node at once (multi-selection edit).
    /// Recorded as a single undoable batch.
    UpdateNodesFill { node_ids: Vec<NodeId>, fill: Fill },
    /// Apply the same stroke to every listed node at once (multi-selection edit).
    /// Recorded as a single undoable batch.
    UpdateNodesStroke {
        node_ids: Vec<NodeId>,
        stroke: Stroke,
    },
    /// Convert each listed path's stroke into a filled outline shape (Illustrator
    /// "Outline Stroke"). Paths without an enabled, positive-width stroke are
    /// skipped. Recorded as a single undoable batch.
    OutlineStroke { node_ids: Vec<NodeId> },
    /// Deep-clone a node and insert the copy at a small offset.
    DuplicateNode { node_id: NodeId },
    /// Remove a specific node by ID.
    DeleteNode { node_id: NodeId },
    /// Remove all currently selected nodes.
    DeleteSelected,
    /// Create a default-sized shape at a canvas position.
    CreateShapeAtPos {
        shape: ShapeKind,
        canvas_x: f64,
        canvas_y: f64,
        fill: [f32; 4],
    },
    /// Group the currently selected nodes (requires 2+ in selection).
    GroupSelected,
    /// Export the given nodes (or the current selection when empty) as SVG and
    /// write the result to the OS clipboard.
    CopyAsSvg { node_ids: Vec<NodeId> },
    /// Diff the current document state against a saved checkpoint, populating
    /// the canvas diff-highlight overlay.
    ///
    /// The right-side Change Log UI that produced this action was removed
    /// (#173) as redundant with the left-drawer Edit History; the diff handler
    /// is retained for a future menu relocation of checkpoint diff.
    #[allow(dead_code)]
    DiffWithCheckpoint { checkpoint_id: Uuid },
    /// Clear the active diff highlight overlay.
    ClearDiff,
    /// Insert a midpoint anchor on every segment of a path node.
    AddAnchorPoints { node_id: NodeId },
    /// Open the Simplify Path dialog for a path node.
    OpenSimplifyDialog { node_id: NodeId },
    /// Open the Merge Vertices by Distance (weld) dialog for a path node.
    OpenMergeVerticesDialog { node_id: NodeId },
    /// Invert the fill/stroke colors of the given nodes. Empty vec = use selection.
    InvertColors { node_ids: Vec<NodeId> },
    /// Convert fill/stroke colors to grayscale. Empty vec = use selection.
    ConvertToGrayscale { node_ids: Vec<NodeId> },
    /// Open the Find / Replace Text dialog (document-wide, no node target needed).
    OpenFindReplaceTextDialog,
    /// Dissolve a group node, re-inserting its children in place.
    UngroupNode { node_id: NodeId },
    /// Recursively dissolve a group and every nested group into leaf nodes.
    UngroupAllNode { node_id: NodeId },
    /// Close every open subpath in a path node (single-node) or merge two path
    /// nodes into one by connecting their nearest endpoints (two-node).
    JoinPaths { node_ids: Vec<NodeId> },
    /// Clip all selected nodes to the frontmost node's boundary (Pathfinder Crop).
    /// Empty vec = use current selection.
    PathfinderCrop { node_ids: Vec<NodeId> },
    /// Subtract all back nodes from the frontmost node (Pathfinder Minus Back).
    /// Empty vec = use current selection.
    PathfinderMinusBack { node_ids: Vec<NodeId> },
    /// Subtract the frontmost node from every back node (Pathfinder Minus Front).
    /// Empty vec = use current selection.
    PathfinderMinusFront { node_ids: Vec<NodeId> },
    /// Trim hidden areas from every selected node (Pathfinder Trim).
    /// Empty vec = use current selection.
    PathfinderTrim { node_ids: Vec<NodeId> },
    /// Convert fills to stroked outlines on selected nodes (Pathfinder Outline).
    /// Empty vec = use current selection.
    PathfinderOutline { node_ids: Vec<NodeId> },
    /// Select all document nodes sharing the given attribute with the reference node.
    SelectSame {
        node_id: NodeId,
        attribute: SelectSameAttr,
    },
    /// Reverse the winding direction of a path node.
    ReversePathDirection { node_id: NodeId },
    /// Average all anchor points of a path node to their centroid.
    AverageAnchorPoints { node_id: NodeId },
    /// Update the outer glow of a node.
    UpdateNodeOuterGlow { node_id: NodeId, glow: GlowEffect },
    /// Update the inner glow of a node.
    UpdateNodeInnerGlow { node_id: NodeId, glow: GlowEffect },
    /// Update the Gaussian glow of a node.
    UpdateNodeGaussianGlow { node_id: NodeId, glow: GaussianGlow },
    /// Lock or unlock a node (prevents canvas selection when locked).
    SetLocked { node_id: NodeId, locked: bool },
    /// Show or hide an individual node (does not affect layer visibility).
    SetVisible { node_id: NodeId, visible: bool },
    /// Move a node to an absolute canvas position by setting its translation directly.
    SetNodePosition { node_id: NodeId, x: f64, y: f64 },
    /// Set a single object's opacity (0.0–1.0).
    SetNodeOpacity { node_id: NodeId, opacity: f32 },
    /// Set a single object's blend mode.
    SetNodeBlendMode {
        node_id: NodeId,
        blend_mode: photonic_core::layer::BlendMode,
    },
    /// Replace a node's Layer Styles effect stack (one undoable step).
    SetNodeEffects {
        node_id: NodeId,
        effects: Vec<photonic_core::effects::LayerEffect>,
    },
    /// Resize a node to the given world-space width and height. A scale transform is
    /// composed onto the existing transform so the top-left anchor stays fixed.
    SetNodeSize {
        node_id: NodeId,
        width: f64,
        height: f64,
    },
    /// Rotate a node to an absolute angle (degrees). The rotation is applied around
    /// the node's world-space bounding-box center. Delta from the current angle is
    /// composed onto the existing transform.
    /// Rotate the selection to `angle_deg`. `node_ids[0]` is the primary whose
    /// current angle defines the delta; all are rotated about the shared center.
    RotateNode {
        node_ids: Vec<NodeId>,
        angle_deg: f64,
    },
    /// Split two overlapping path nodes at their intersection edges into distinct face nodes.
    /// Empty vec = use current selection (resolved in app.rs).
    PathfinderDivide { node_ids: Vec<NodeId> },
    /// Trim all selected nodes of hidden areas then merge same-color faces (Pathfinder Merge).
    /// Empty vec = use current selection (resolved in app.rs).
    PathfinderMerge { node_ids: Vec<NodeId> },
    /// Use the given path node as a cutting edge to divide all objects beneath it.
    /// The cutter is removed; each intersecting object below is split into inside/outside faces.
    DivideObjectsBelow { node_id: NodeId },
    /// Activate the eyedropper for the given color slot.
    StartEyedropper(EyedropperTarget),
    /// Remove the background of a raster layer with the local matting model,
    /// storing the result as a non-destructive foreground layer mask.
    RasterRemoveBackground { node_id: NodeId },
    /// Begin a color-range mask-out session on a raster layer: arms the
    /// eyedropper so the next canvas click samples the color to hide.
    StartRasterColorRange { node_id: NodeId },
    /// Live-update the active color-range session's fuzziness/contiguity and
    /// refresh its preview.
    SetRasterColorRangeParams { tolerance: f32, contiguous: bool },
    /// Commit the active color-range session as one undoable step.
    ApplyRasterColorRange,
    /// Discard the active color-range session, restoring the layer.
    CancelRasterColorRange,
    /// Remove a raster layer's non-destructive layer mask (undoable).
    ClearRasterMask { node_id: NodeId },
    /// Crop a raster layer's pixels (and mask) to the artboard bounds,
    /// discarding everything outside (undoable).
    CropRasterToArtboard { node_id: NodeId },
    /// Add a new empty layer at the top of the stack and make it active.
    AddLayer,
    /// Move the given nodes (or current selection if empty) into a new layer.
    CollectInNewLayer { node_ids: Vec<NodeId> },
    /// Blend fill colors linearly across 3+ selected path nodes.
    /// Empty vec = use current selection (resolved in app.rs).
    /// `direction`: "horizontal", "vertical", "depth", or "" for selection order.
    BlendColors {
        node_ids: Vec<NodeId>,
        direction: String,
    },
    /// Shift RGBA channel values on selected path nodes.
    AdjustColors {
        node_ids: Vec<NodeId>,
        delta_r: f32,
        delta_g: f32,
        delta_b: f32,
        delta_a: f32,
    },
    /// Move each node (or current selection if empty) into its own new layer.
    ReleaseToLayers { node_ids: Vec<NodeId> },
    /// Merge the given layers into one (bottommost in stack order is the target).
    MergeLayers { layer_ids: Vec<LayerId> },
    /// Flatten all layers in the document into a single layer.
    FlattenArtwork,
    /// Reorder the layer stack to `new_order` (bottom→top), e.g. from a
    /// drag-to-reorder in the Layers panel (#169). Applied as one undoable step.
    ReorderLayers { new_order: Vec<LayerId> },
    /// Reparent a node into a container (a group, or a layer) at an index —
    /// Layers-panel drag-and-drop (#210). The handler resolves the old container.
    ReparentNode {
        node_id: NodeId,
        new: photonic_core::document::NodeContainer,
        new_index: usize,
    },
    /// Open a file picker and place the chosen image into the document (#176).
    PlaceImageDialog,
    /// Open the Export dialog seeded from the Document-tab export settings (#176).
    OpenExportDialog,
    /// Open the Export dialog in batch mode: one file per artboard over a range.
    OpenArtboardExportDialog,
    /// Open the K-A6 Edit Duration dialog for a timeline clip.
    OpenEditDuration {
        seq: photonic_core::timeline::SequenceId,
        track: photonic_core::timeline::TrackId,
        clip: photonic_core::timeline::ClipId,
    },
    /// K-B14: freeze a clip at clip-relative `at` (zero-rate SpeedMap).
    FreezeFrame {
        seq: photonic_core::timeline::SequenceId,
        track: photonic_core::timeline::TrackId,
        clip: photonic_core::timeline::ClipId,
        at: photonic_core::timeline::Tick,
    },
    /// D-12: bind a gyro/IMU sidecar to a clip (22 §6.5).
    ImportMotionMetadata {
        clip: photonic_core::timeline::ClipId,
    },
    /// D-12: run (or re-run) stabilization analysis for a clip (22 §6.5).
    ///
    /// Analysis is *generation, not history* — it produces a cache entry, not
    /// an undo step, so re-running it never appears in the edit history.
    AnalyzeStabilization {
        clip: photonic_core::timeline::ClipId,
    },
    /// Set the color tag of a layer (None = clear).
    SetLayerColor {
        layer_id: LayerId,
        color: Option<[f32; 4]>,
    },
    /// Toggle template mode on a layer.
    SetLayerTemplate {
        layer_id: LayerId,
        is_template: bool,
    },
    /// Rename a layer.
    RenameLayer { layer_id: LayerId, name: String },
    /// Open a floating color picker for a path node's fill (`stroke = false`) or
    /// stroke (`stroke = true`) solid color. Raised from the radial menu.
    OpenColorPopup { node_id: NodeId, stroke: bool },
    /// Delete a layer and all the nodes it contains (undoable). Refuses to
    /// remove the last remaining layer.
    DeleteLayer { layer_id: LayerId },
    /// Show or hide an entire layer.
    SetLayerVisible { layer_id: LayerId, visible: bool },
    /// Lock or unlock an entire layer (prevents selecting its contents).
    SetLayerLocked { layer_id: LayerId, locked: bool },
    /// Set a layer's opacity (0.0–1.0); the layer composites as a unit.
    SetLayerOpacity { layer_id: LayerId, opacity: f32 },
    /// Set a layer's blend mode; the layer composites as a unit against those beneath.
    SetLayerBlendMode {
        layer_id: LayerId,
        blend_mode: photonic_core::layer::BlendMode,
    },
    /// Open the Options modal for a whole layer (name, blend, opacity, color, template…).
    OpenLayerOptions { layer_id: LayerId },
    /// Open the Options modal for a single object, scoped to its type.
    OpenObjectOptions { node_id: NodeId },
    /// Duplicate a layer and all its objects as a new layer above it.
    DuplicateLayer { layer_id: LayerId },
    /// Create a new empty group ("sublayer") nesting container. When a group is
    /// selected it is nested inside that group; otherwise it lands at the top of
    /// the active layer.
    AddSublayer,
    /// Add a layer mask using the smartest interpretation for the current
    /// selection: a raster alpha mask on a selected raster node, otherwise a
    /// clipping mask built from the current multi-selection (top object clips).
    AddLayerMaskSmart,
    /// Create a non-destructive adjustment layer of the given kind (MCP
    /// adjustment vocabulary, e.g. "brightness_contrast", "invert") at the top
    /// of the active layer, with sensible default parameters.
    AddAdjustmentLayer { kind: String },
    /// Align or distribute selected nodes.
    /// `operation`: left/center_horizontal/right/top/center_vertical/bottom/distribute_horizontal/distribute_vertical
    /// `key_object_id`: when set, align relative to this node's bounds (key object is not moved)
    AlignNodes {
        operation: String,
        key_object_id: Option<NodeId>,
    },
    /// Combine multiple path nodes into one compound path (even-odd fill rule creates holes).
    MakeCompoundPath { node_ids: Vec<NodeId> },
    /// Release a compound path back into individual path nodes, one per subpath.
    ReleaseCompoundPath { node_id: NodeId },
    /// Apply a shear (skew) transform to a node around its own centre.
    ShearNode {
        node_ids: Vec<NodeId>,
        shear_x: f64,
        shear_y: f64,
    },
    /// Round the position of one or more nodes to integer pixel coordinates.
    SnapToPixel { node_ids: Vec<NodeId> },
    /// Distribute node copies evenly along a guide path.
    /// `path_node_id` is the guide; `node_ids` are the sources to clone.
    DistributeOnPath {
        path_node_id: NodeId,
        node_ids: Vec<NodeId>,
        align: bool,
    },
    /// Remap every solid fill in the given nodes to the nearest color in the palette.
    RecolorArtwork {
        node_ids: Vec<NodeId>,
        palette: Vec<[f32; 4]>,
    },
    /// Live preview: set the solid fill of exactly these nodes to `to` WITHOUT
    /// recording history. Used while dragging the document color-swatch picker.
    RecolorPreview { ids: Vec<NodeId>, to: [f32; 4] },
    /// Commit a document color-swatch recolor as one undoable step: the given
    /// nodes change from `from` to `to` (undo restores `from`).
    RecolorCommit {
        ids: Vec<NodeId>,
        from: [f32; 4],
        to: [f32; 4],
    },
    /// Align selected nodes relative to the document canvas (artboard) bounds.
    AlignToArtboard { operation: String },
    /// Remove all unlocked guides from the document.
    ClearGuides,
    /// Convert anchor points to smooth joins for the given path nodes.
    ConvertToSmooth { node_ids: Vec<NodeId> },
    /// Convert anchor points to corner joins (cusps) for the given path nodes.
    ConvertToCorner { node_ids: Vec<NodeId> },
    /// Select all nodes of a given kind ("path", "text", "group", "same_layer").
    SelectByKind { kind: String, additive: bool },
    /// Apply zig-zag distortion to path nodes.
    ZigZagPath {
        node_ids: Vec<NodeId>,
        size: f64,
        ridges: usize,
        smooth: bool,
    },
    /// Apply pucker (contract inward) or bloat (expand outward) distortion.
    PuckerBloat {
        node_ids: Vec<NodeId>,
        strength: f64,
    },
    /// Roughen a path by randomly displacing points.
    RoughenPath {
        node_ids: Vec<NodeId>,
        size: f64,
        detail: usize,
        seed: u64,
    },
    /// Twirl a path — spiral rotation around centroid.
    TwirlPath {
        node_ids: Vec<NodeId>,
        angle_deg: f64,
    },
    /// Blend between two paths — create interpolated intermediate steps.
    BlendObjects {
        node_id_a: NodeId,
        node_id_b: NodeId,
        steps: usize,
    },
    /// Blend using Smooth Color mode — auto-compute steps from color distance.
    BlendObjectsSmoothColor {
        node_id_a: NodeId,
        node_id_b: NodeId,
    },
    /// Blend using Specified Distance mode — space steps by pixel distance.
    BlendObjectsSpacing {
        node_id_a: NodeId,
        node_id_b: NodeId,
        spacing: f64,
    },
    /// Apply scallop arcs to path segments.
    ScallopPath {
        node_ids: Vec<NodeId>,
        depth: f64,
        count: usize,
    },
    /// Apply crystallize spikes to path segments.
    CrystallizePath {
        node_ids: Vec<NodeId>,
        size: f64,
        count: usize,
    },
    /// Apply a named warp envelope distortion.
    WarpEnvelope {
        node_ids: Vec<NodeId>,
        warp_type: String,
        bend: f64,
    },
    /// Round sharp corners with arc fillets.
    RoundCorners { node_ids: Vec<NodeId>, radius: f64 },
    /// Move a single selected anchor of an edited path to a local position.
    SetAnchorPosition {
        node_id: NodeId,
        index: usize,
        x: f64,
        y: f64,
    },
    /// Round the selected straight corners of an edited path to `radius`.
    RoundSelectedCorners {
        node_id: NodeId,
        indices: Vec<usize>,
        radius: f64,
    },
    /// Convert the selected anchors of an edited path to smooth or corner.
    ConvertAnchorType {
        node_id: NodeId,
        indices: Vec<usize>,
        smooth: bool,
    },
    /// Delete the selected anchors of an edited path.
    DeleteAnchors {
        node_id: NodeId,
        indices: Vec<usize>,
    },
    /// Flip node(s) horizontally or vertically.
    FlipNodes {
        node_ids: Vec<NodeId>,
        horizontal: bool,
    },
    /// Set text typography properties (line height, letter spacing).
    SetTextTypography {
        node_id: NodeId,
        line_height: Option<f64>,
        letter_spacing: Option<f64>,
    },
    /// Add a drop shadow behind a node.
    AddDropShadow { node_id: NodeId },
    /// Create a sample radar chart at canvas center (5 axes, 2 series).
    CreateRadarChart,
    /// Create a sample stacked column chart at canvas center.
    CreateStackedBarChart,
    /// Create a parametric shape (Lissajous, Superellipse, Rose, etc.) at canvas center.
    CreateParametricShape { shape_type: String },
    /// Offset (expand or inset) a path node by a fixed distance, creating a copy.
    OffsetPath {
        node_ids: Vec<NodeId>,
        distance: f64,
    },
    /// Generate a Truchet tiling at canvas center.
    CreateTruchetTiling { style: String },
    /// Push selected nodes apart until their bounding boxes no longer overlap.
    DistributeNoOverlap { node_ids: Vec<NodeId> },
    /// Apply sinusoidal noise deformation to path anchor points.
    NoiseDeform {
        node_ids: Vec<NodeId>,
        amplitude: f64,
        style: String,
    },
    /// Duplicate and flip selected nodes to create mirrored copies.
    MirrorCopy { node_ids: Vec<NodeId>, axis: String },
    /// Create N evenly-spaced rotational copies around the node's center.
    RotateCopies { node_id: NodeId, count: usize },
    /// Copy appearance (fill/stroke/opacity) from source to target nodes.
    CopyAppearance {
        source_id: NodeId,
        target_ids: Vec<NodeId>,
        copy_fill: bool,
        copy_stroke: bool,
        copy_opacity: bool,
    },
    /// Remove a named export profile from the document.
    RemoveExportProfile { name: String },
    /// Pin guides at node edges/centers.
    PinObjectGuides { node_ids: Vec<NodeId> },
    /// Reverse the children order in selected group nodes.
    ReverseNodeOrder { node_ids: Vec<NodeId> },
    /// Copy document template JSON to the OS clipboard.
    CopyDocumentTemplate,
    /// Select all nodes whose fill color matches selected nodes.
    SelectSimilar {
        node_ids: Vec<NodeId>,
        match_by: String,
    },
    /// Tag a node for asset export with the given spec.
    TagNodeForExport {
        node_id: NodeId,
        name: String,
        format: String,
    },
    /// Remove the asset export tag from a node.
    RemoveExportTag { node_id: NodeId },
    /// Apply a named character style to a text node.
    ApplyCharacterStyle { node_id: NodeId, style_name: String },
    /// Delete a named character style from the document.
    DeleteCharacterStyle { name: String },
    /// Apply a named paragraph style to a text node.
    ApplyParagraphStyle { node_id: NodeId, style_name: String },
    /// Delete a named paragraph style from the document.
    DeleteParagraphStyle { name: String },
    /// Apply a named color swatch to a node's fill.
    ApplyColorSwatch {
        node_id: NodeId,
        swatch_name: String,
    },
    /// Delete a named color swatch from the document palette.
    DeleteColorSwatch { name: String },
    /// Load a predefined swatch library into the document palette.
    LoadSwatchLibrary {
        library: String,
        clear_existing: bool,
    },
    /// #207: import named color swatches from a design-tokens file (opens a picker).
    ImportDesignTokens,
    /// Apply a named width profile to the selected path node.
    ApplyWidthProfile {
        node_id: NodeId,
        profile_name: String,
    },
    /// Save a width profile from the selected node's current stroke width.
    SaveWidthProfile { stroke_width: f64, name: String },
    /// Delete a named width profile.
    DeleteWidthProfile { name: String },
    /// Rename an existing width profile (e.g. one shaped with the Width tool).
    RenameWidthProfile { old_name: String, new_name: String },
    /// Save a graphic style from the selected node.
    SaveGraphicStyle { node_id: NodeId, name: String },
    /// Apply a named graphic style to the selected node.
    ApplyGraphicStyle { node_id: NodeId, style_name: String },
    /// Delete a named graphic style.
    DeleteGraphicStyle { name: String },
    /// Save the gradient fill of a node as a named gradient swatch.
    SaveGradientSwatch { node_id: NodeId, name: String },
    /// Apply a named gradient swatch to a node's fill.
    ApplyGradientSwatch {
        node_id: NodeId,
        swatch_name: String,
    },
    /// Delete a named gradient swatch.
    DeleteGradientSwatch { name: String },
    /// Run composition analysis and store findings in app state.
    AnalyzeComposition,
    /// Detect rhythm patterns and store findings in app state.
    DetectRhythms,
    /// Define a named document grammar rule.
    DefineGrammarRule {
        name: String,
        rule_type: String,
        params_json: String,
    },
    /// Delete a named document grammar rule.
    DeleteGrammarRule { name: String },
    /// Check all grammar rules and store results in app state.
    CheckGrammar,
    /// Measure distances between selected nodes and store results.
    MeasureDistances { node_ids: Vec<NodeId> },
    /// Play a named action set.
    PlayAction { name: String },
    /// Delete a named action set.
    DeleteAction { name: String },
    /// Register an event trigger.
    RegisterEventTrigger { event: String, action_name: String },
    /// Remove an event trigger.
    RemoveEventTrigger {
        event: String,
        action_name: Option<String>,
    },
    /// Flatten transparency — bake opacity into color alpha values.
    FlattenTransparency,
    /// Apply flex layout to a group's children.
    ApplyFlexLayout {
        group_id: NodeId,
        direction: String,
        gap: f64,
        align: String,
        padding: f64,
    },
    /// Revert a node N edits back in history.
    UndoNode { node_id: NodeId, steps: usize },
    /// Apply grid layout to a group's children.
    ApplyGridLayout {
        group_id: NodeId,
        columns: usize,
        gap_x: f64,
        gap_y: f64,
    },
    /// Stack all children of a group at the same position.
    ApplyStackLayout {
        group_id: NodeId,
        align_h: String,
        align_v: String,
    },
    /// Refresh the displayed history entries (read-only trigger).
    RefreshHistory,
    /// Jump to a specific undo history index (linear; used by MCP/CLI).
    JumpToHistory { index: usize },
    /// Jump to a specific edit-tree node by id (branch-aware; used by the commit
    /// graph). Navigates across branches via the history tree.
    JumpToHistoryNode { id: u64 },
    /// Scale and position selected (or all) nodes to fill the artboard safe area.
    FitToMargins,
    /// Add a dimension annotation between two nodes.
    AddDimension {
        from_id: photonic_core::node::NodeId,
        to_id: photonic_core::node::NodeId,
        axis: String,
    },
    /// Remove a dimension annotation by ID.
    RemoveDimension { id: uuid::Uuid },
    /// Set document bleed and slug values.
    SetDocumentBleed { bleed_mm: f64, slug_mm: f64 },
    /// Add an angled construction line.
    AddConstructionLine { x: f64, y: f64, angle_degrees: f64 },
    /// Set artboard safe-area margins.
    SetArtboardMargins {
        top: f64,
        right: f64,
        bottom: f64,
        left: f64,
    },
    /// Define or update a spot color.
    DefineSpotColor {
        name: String,
        hex: String,
        overprint: bool,
    },
    /// Apply a spot color to a node.
    ApplySpotColor { node_id: NodeId, color_name: String },
    /// Delete a spot color.
    DeleteSpotColor { name: String },
    /// Name the current history state (label the HEAD commit).
    BranchCreate { name: String },
    /// Jump to a named state (non-destructive; navigates the edit tree).
    BranchSwitch { name: String },
    /// Remove a named state's name (deletes the label, keeps the commit).
    BranchDelete { name: String },
    /// Set or clear a specific commit's name — `None` clears it. Drives the
    /// commit-graph right-click "Name this state…" / "Remove name".
    LabelHistoryNode { id: u64, name: Option<String> },
    /// Make a clipping mask from a group node (topmost child becomes clip path).
    MakeClippingMask { group_id: NodeId },
    /// Release the clipping mask from a group node.
    ReleaseClippingMask { group_id: NodeId },
    /// Place a text node along a path spine.
    SetTextPath {
        text_node_id: NodeId,
        path_node_id: NodeId,
        offset: f64,
    },
    /// Remove the path spine from a text node.
    ClearTextPath { text_node_id: NodeId },
    /// Set the layout direction of a text node (horizontal/vertical).
    SetTextDirection { node_id: NodeId, vertical: bool },
    /// Set the font style (italic/oblique/normal) on a text node.
    SetFontStyle { node_id: NodeId, style: String },
    /// Set the font weight (100–900) on a text node.
    SetFontWeight { node_id: NodeId, weight: u16 },
    /// Flow a text node inside a closed path area.
    SetTextArea {
        text_node_id: NodeId,
        area_path_id: NodeId,
    },
    /// Remove the area boundary from a text node.
    ClearTextArea { text_node_id: NodeId },
    /// Set text decoration (underline/line-through/overline/none) on a text node.
    SetTextDecoration { node_id: NodeId, decoration: String },
    /// Set paragraph spacing and indent on a text node.
    SetParagraphOptions {
        node_id: NodeId,
        spacing_before: f64,
        spacing_after: f64,
        indent: f64,
    },
    /// Set custom tab stop positions on a text node.
    SetTabStops { node_id: NodeId, stops: Vec<f64> },
    /// Clear custom tab stops from a text node.
    ClearTabStops { node_id: NodeId },
    /// Set OpenType features on a text node.
    SetOpenTypeFeatures {
        node_id: NodeId,
        features: Vec<String>,
    },
    /// Set advanced character metrics (baseline shift + super/subscript) on a text node.
    SetCharacterMetrics {
        node_id: NodeId,
        baseline_shift: f64,
        script_position: photonic_core::node::ScriptPosition,
    },
    /// Link two text nodes as a threaded text chain.
    LinkTextFrames { from_id: NodeId, to_id: NodeId },
    /// Remove a text node from its thread chain.
    UnlinkTextFrames { node_id: NodeId },
    /// Bind a text node to a document variable.
    BindTextVariable {
        node_id: NodeId,
        variable_name: String,
    },
    /// Remove the variable binding from a text node.
    UnbindTextVariable { node_id: NodeId },
    /// Apply all document variable values to bound text nodes.
    ApplyVariables,
    /// Delete a document variable.
    DeleteVariable { name: String },
    /// Define a node as a named symbol master.
    DefineSymbol { node_id: NodeId, name: String },
    /// Place an instance of a named symbol at a position.
    PlaceSymbol { symbol_name: String },
    /// Break the symbol link on an instance node.
    BreakLinkToSymbol { node_id: NodeId },
    /// Delete a symbol from the registry.
    DeleteSymbol { name: String },
    /// Assign a path node as the blend spine for a group.
    SetBlendSpine { group_id: NodeId, path_id: NodeId },
    /// Clear the blend spine assignment from a group.
    ClearBlendSpine { group_id: NodeId },
    /// Reverse the direction of the blend spine path in a group.
    ReverseBlendSpine { group_id: NodeId },
    /// Expand a blend group into individual discrete objects.
    ExpandBlend { group_id: NodeId },
    /// Load a built-in symbol library into the document.
    LoadSymbolLibrary { library_name: String },
    /// Spray N symbol instances scattered around a center point.
    SpraySymbolInstances {
        symbol_name: String,
        count: usize,
        x: f64,
        y: f64,
        spread: f64,
    },
    /// Set per-instance color overrides on a symbol instance.
    SetSymbolOverride {
        node_id: NodeId,
        fill_hex: Option<String>,
        stroke_hex: Option<String>,
    },
    /// Clear all per-instance color overrides from a symbol instance.
    ClearSymbolOverrides { node_id: NodeId },
    /// Save the current prop_search as a named workspace.
    SaveWorkspace { name: String, search_query: String },
    /// Load a workspace by name (returns search_query to apply).
    LoadWorkspace { name: String },
    /// Delete a named workspace.
    DeleteWorkspace { name: String },
    /// Recenter the canvas viewport on a canvas-space point (Navigator click).
    CenterViewOn { canvas_x: f64, canvas_y: f64 },

    // ── Media pool (video mode, 05 §2) ────────────────────────────────────────
    /// Open the OS multi-file picker and enqueue the chosen files for
    /// background import (probe + hash) into `bin`.
    MediaImportDialog {
        bin: Option<photonic_core::timeline::BinId>,
    },
    /// Create a media bin (folder).
    MediaCreateBin {
        name: String,
        parent: Option<photonic_core::timeline::BinId>,
    },
    /// Remove a media bin (assets fall back to the root).
    MediaRemoveBin { bin: photonic_core::timeline::BinId },
    /// Remove an asset from the pool.
    MediaRemoveAsset {
        asset: photonic_core::timeline::AssetId,
    },
    /// K-C5: remove every asset with zero timeline references (one undo batch).
    MediaRemoveUnused,
    /// K-C2: set or clear star rating (1–5 / None).
    MediaSetRating {
        asset: photonic_core::timeline::AssetId,
        rating: Option<u8>,
    },
    /// K-C2: replace free-form tags (resolved into the project TagId registry).
    MediaSetTags {
        asset: photonic_core::timeline::AssetId,
        tags: Vec<String>,
    },
    /// K-A8: create a subclip of `asset` over source range `[in_ticks, out_ticks)`.
    MediaCreateSubclip {
        asset: photonic_core::timeline::AssetId,
        in_ticks: i64,
        out_ticks: i64,
        name: Option<String>,
    },
    /// Move an asset to `bin` (`None` = pool root).
    MediaAssignBin {
        asset: photonic_core::timeline::AssetId,
        bin: Option<photonic_core::timeline::BinId>,
    },
    /// Open the OS file picker and relink an offline asset to the chosen file.
    MediaRelink {
        asset: photonic_core::timeline::AssetId,
    },
    /// Engine-wide proxy playback mode (05 §4; `EngineCmd::SetProxyMode`).
    MediaSetProxyMode { mode: photonic_video::ProxyMode },
    /// Build reusable editing proxies for every file-backed video in the pool.
    MediaGenerateProxies,
    /// Project policy: auto-queue L7 proxy generation after import (G-15C).
    MediaSetGenerateProxiesOnImport { enabled: bool },
    /// Attach a user-supplied proxy file to a video asset (G-15A).
    MediaAttachProxy {
        asset: photonic_core::timeline::AssetId,
    },
    /// Clear the asset's proxy ref without deleting user-owned attached files.
    MediaDetachProxy {
        asset: photonic_core::timeline::AssetId,
    },
    /// Insert the asset as a clip at the playhead on the first compatible
    /// track (double-click / context menu; drag-to-timeline is the primary
    /// path and is handled in the timeline panel itself).
    MediaInsertAtPlayhead {
        asset: photonic_core::timeline::AssetId,
    },

    // ── Captions (video mode, 04 §4.1 / 06) ──────────────────────────────────
    // The caption-editor drawer is `PropPanelCtx`-based (carries `doc: &
    // Document` for reads, no `&mut CommandHistory`) — same shape as `Media*`
    // above. It builds already-validated `TimelineCmd`s itself (via
    // `photonic_core::timeline::ops`/`CaptionCmd`, reading `ctx.doc`) and
    // hands them up here as one undo step.
    /// Several `TimelineCmd`s committed as ONE undo step (`Command::Batch`,
    /// via `CommandHistory::execute_discrete`) — every caption/TTS mutation
    /// (cue text/timing, split/merge, style cascade, auto-caption, voiceover
    /// placement), including transcript plans that cut synchronized media and
    /// captions together, routes through this single carrier.
    CaptionEditBatch(Vec<photonic_core::timeline::TimelineCmd>),

    // ── Clip inspector / effects browser (video mode, 04 §4.1) ───────────────
    // These video panels are `PropPanelCtx`-based (like every left-rail
    // drawer) so they carry `doc: &Document` for reads but no `&mut
    // CommandHistory` — mirrors why `Media*` above exists. Rather than one
    // named variant per field (transform/speed/reframe/effect-param/
    // transition — all just sub-fields of `Clip`), the panel builds the
    // already-validated `TimelineCmd` itself (via `photonic_core::timeline::
    // ops::*`, reading `ctx.doc`) and hands it up here as one of two generic
    // carriers, matching `ops_bridge.rs`'s "pure op → history" rule at the
    // `PropPanelCtx` boundary instead of inside a drawer fn.
    /// Move the session playhead to a tick — **not** a document mutation and
    /// therefore **not** an undo step (26 K-A2 marker navigation; the same rule
    /// K-G5's history browser follows). A panel only receives a *copy* of the
    /// playhead through [`video::VideoPanelUi`], and the engine seek is driven
    /// off `PhotonicApp::playhead` in `app/monitor.rs`, so a panel cannot seek
    /// directly — it queues this and the main loop assigns the field.
    SeekPlayhead { at: photonic_core::timeline::Tick },
    /// Committed as ONE non-folding undo step (button/toggle actions: add/
    /// remove/reorder effect, enable toggle, "Reset reframe", etc.).
    ClipEditDiscrete(photonic_core::timeline::TimelineCmd),
    /// Committed via the coalescing `history.execute` path (drag-scrub
    /// numeric fields — transform/speed/reframe/transition values), so a
    /// streamed drag folds into one undo step like every vector property
    /// drag (the coalesce anchor is driven globally by pointer-down/up,
    /// per `app/mod.rs`'s `begin_coalescing`/`end_coalescing`).
    ClipEditCoalesced(photonic_core::timeline::TimelineCmd),
    /// Several commands committed as ONE undo step (`Command::Batch`) — e.g.
    /// the Effects Browser's double-click-to-apply fallback (13 §6.3)
    /// applying one effect to every selected clip at once.
    ClipEditBatch(Vec<photonic_core::timeline::TimelineCmd>),
}

/// Discriminant for which shape the radial wheel should create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    Shape(PrimitiveKind),
    Text,
}

impl ShapeKind {
    pub fn label(self) -> &'static str {
        match self {
            ShapeKind::Shape(PrimitiveKind::Rectangle) => "Rectangle",
            ShapeKind::Shape(PrimitiveKind::RoundedRect) => "Rounded Rect",
            ShapeKind::Shape(PrimitiveKind::Ellipse) => "Ellipse",
            ShapeKind::Shape(PrimitiveKind::Polygon) => "Polygon",
            ShapeKind::Shape(PrimitiveKind::Star) => "Star",
            ShapeKind::Shape(PrimitiveKind::Line) => "Line",
            ShapeKind::Shape(PrimitiveKind::Arc) => "Arc",
            ShapeKind::Text => "Text",
        }
    }
}

impl PanelAction {
    /// Translate a `WheelAction` into the appropriate `PanelAction`.
    /// `canvas_pos` is where the wheel was opened; `fill` is the current fill color.
    pub fn from_wheel_action(wa: WheelAction, canvas_pos: (f64, f64), fill: [f32; 4]) -> Self {
        let (cx, cy) = canvas_pos;
        match wa {
            WheelAction::CreateRect => Self::CreateShapeAtPos {
                shape: ShapeKind::Shape(PrimitiveKind::Rectangle),
                canvas_x: cx,
                canvas_y: cy,
                fill,
            },
            WheelAction::CreateRoundedRect => Self::CreateShapeAtPos {
                shape: ShapeKind::Shape(PrimitiveKind::RoundedRect),
                canvas_x: cx,
                canvas_y: cy,
                fill,
            },
            WheelAction::CreateEllipse => Self::CreateShapeAtPos {
                shape: ShapeKind::Shape(PrimitiveKind::Ellipse),
                canvas_x: cx,
                canvas_y: cy,
                fill,
            },
            WheelAction::CreatePolygon => Self::CreateShapeAtPos {
                shape: ShapeKind::Shape(PrimitiveKind::Polygon),
                canvas_x: cx,
                canvas_y: cy,
                fill,
            },
            WheelAction::CreateStar => Self::CreateShapeAtPos {
                shape: ShapeKind::Shape(PrimitiveKind::Star),
                canvas_x: cx,
                canvas_y: cy,
                fill,
            },
            WheelAction::CreateText => Self::CreateShapeAtPos {
                shape: ShapeKind::Text,
                canvas_x: cx,
                canvas_y: cy,
                fill,
            },
            WheelAction::DuplicateNode(id) => Self::DuplicateNode { node_id: id },
            WheelAction::DeleteNode(id) => Self::DeleteNode { node_id: id },
            WheelAction::BringForward(id) => Self::ReorderNode {
                node_id: id,
                op: ZOrderOp::BringForward,
            },
            WheelAction::SendBackward(id) => Self::ReorderNode {
                node_id: id,
                op: ZOrderOp::SendBackward,
            },
            WheelAction::BringToFront(id) => Self::ReorderNode {
                node_id: id,
                op: ZOrderOp::BringToFront,
            },
            WheelAction::SendToBack(id) => Self::ReorderNode {
                node_id: id,
                op: ZOrderOp::SendToBack,
            },
            WheelAction::GroupSelected => Self::GroupSelected,
            WheelAction::DeleteSelected => Self::DeleteSelected,
            WheelAction::BoolUnion => Self::BooleanOp(BooleanOp::Union),
            WheelAction::BoolSubtract => Self::BooleanOp(BooleanOp::Subtract),
            WheelAction::BoolIntersect => Self::BooleanOp(BooleanOp::Intersect),
            WheelAction::BoolExclude => Self::BooleanOp(BooleanOp::Exclude),
            WheelAction::CopyAsSvg(id) => Self::CopyAsSvg { node_ids: vec![id] },
            // Empty vec signals "use the current selection" — resolved in app.rs.
            WheelAction::CopyAsSvgSelection => Self::CopyAsSvg { node_ids: vec![] },
            WheelAction::AddAnchorPoints(id) => Self::AddAnchorPoints { node_id: id },
            WheelAction::SimplifyPath(id) => Self::OpenSimplifyDialog { node_id: id },
            WheelAction::MergeVertices(id) => Self::OpenMergeVerticesDialog { node_id: id },
            WheelAction::OutlineStroke(id) => Self::OutlineStroke { node_ids: vec![id] },
            WheelAction::ReversePathDirection(id) => Self::ReversePathDirection { node_id: id },
            WheelAction::AverageAnchorPoints(id) => Self::AverageAnchorPoints { node_id: id },
            WheelAction::ClosePath(id) => Self::JoinPaths { node_ids: vec![id] },
            WheelAction::InvertColors(id) => Self::InvertColors { node_ids: vec![id] },
            WheelAction::InvertColorsSelected => Self::InvertColors { node_ids: vec![] },
            WheelAction::ConvertToGrayscale(id) => Self::ConvertToGrayscale { node_ids: vec![id] },
            WheelAction::ConvertToGrayscaleSelected => {
                Self::ConvertToGrayscale { node_ids: vec![] }
            }
            WheelAction::UngroupNode(id) => Self::UngroupNode { node_id: id },
            WheelAction::UngroupAllNode(id) => Self::UngroupAllNode { node_id: id },
            WheelAction::EditFillColor(id) => Self::OpenColorPopup {
                node_id: id,
                stroke: false,
            },
            WheelAction::EditStrokeColor(id) => Self::OpenColorPopup {
                node_id: id,
                stroke: true,
            },
        }
    }
}

/// Which attribute to match in Select Same operations (mirrors MCP SelectSameAttribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectSameAttr {
    FillColor,
    StrokeColor,
    StrokeWeight,
    Opacity,
    BlendMode,
    ObjectType,
}

#[derive(Debug)]
pub enum ZOrderOp {
    SendToBack,
    BringToFront,
    SendBackward,
    BringForward,
}

/// Draw the right properties panel.
/// Returns an optional action if the user clicked a boolean operation button.
/// In-progress edit state for the document color-swatch recolor picker.
/// Stored in egui temp memory so the inline picker survives across frames
/// without threading another `&mut` parameter through `draw_properties_panel`.
#[derive(Clone)]
pub(crate) struct RecolorSwatchEdit {
    /// Nodes captured at click time whose fill matched the clicked swatch.
    /// Preview and commit operate on exactly these, so picking a color that
    /// collides with another group never recolors the wrong objects.
    ids: Vec<NodeId>,
    /// The original color clicked — undo target / revert color.
    original: [f32; 4],
    /// The color currently shown in the document (last preview applied).
    applied: [f32; 4],
    /// The live color being edited in the picker.
    current: [f32; 4],
}

pub(crate) struct PropPanelCtx<'a> {
    pub(crate) doc: &'a Document,
    pub(crate) active_tool: Tool,
    pub(crate) fill_color: &'a mut [f32; 4],
    pub(crate) polygon_sides: &'a mut u32,
    pub(crate) star_points: &'a mut u32,
    pub(crate) star_inner_ratio: &'a mut f32,
    pub(crate) rounded_rect_radius: &'a mut f64,
    pub(crate) spiral_turns: &'a mut f32,
    pub(crate) spiral_inner_radius: &'a mut f32,
    pub(crate) spiral_segs_per_turn: &'a mut u32,
    pub(crate) selected_node: Option<&'a SceneNode>,
    pub(crate) selected_id: Option<NodeId>,
    pub(crate) selection_count: usize,
    pub(crate) selected_ids: &'a [NodeId],
    pub(crate) point_edit_node: Option<NodeId>,
    pub(crate) point_selected: &'a [usize],
    pub(crate) prop_search: &'a mut String,
    pub(crate) shear_x: &'a mut f64,
    pub(crate) shear_y: &'a mut f64,
    pub(crate) line_snap_45: &'a mut bool,
    pub(crate) color_guide_rule: &'a mut String,
    pub(crate) arc_start_angle: &'a mut f64,
    pub(crate) arc_end_angle: &'a mut f64,
    pub(crate) arc_open: &'a mut bool,
    pub(crate) grid_cols: &'a mut u32,
    pub(crate) grid_rows: &'a mut u32,
    pub(crate) polar_grid_rings: &'a mut u32,
    pub(crate) polar_grid_sectors: &'a mut u32,
    pub(crate) polar_grid_inner_ratio: &'a mut f32,
    pub(crate) recolor_palette_input: &'a mut String,
    pub(crate) magic_wand_attribute: &'a mut SelectSameAttr,
    pub(crate) magic_wand_tolerance: &'a mut f64,
    pub(crate) eraser_radius: &'a mut f64,
    /// Proportional Move (Direct Select sub-variant): falloff radius ("spread")
    /// and curve exponent, editable in Tool Options and adjusted live by scroll.
    pub(crate) prop_spread: &'a mut f64,
    pub(crate) prop_falloff_k: &'a mut f64,
    /// Raster color-range mask-out: fuzziness (0..1) and contiguous (wand) flag.
    pub(crate) raster_mask_tolerance: &'a mut f32,
    pub(crate) raster_mask_contiguous: &'a mut bool,
    /// `Some(rgba)` while a color-range session is live on the selected raster
    /// layer (drives the swatch + Apply/Cancel controls). `None` otherwise.
    pub(crate) raster_color_range_target: Option<[u8; 4]>,
    /// Whether the background-removal model is already downloaded (drives the
    /// first-run "~4.7 MB download" hint on the button).
    pub(crate) rmbg_model_cached: bool,
    pub(crate) composition_findings: &'a [String],
    pub(crate) rhythm_findings: &'a [String],
    pub(crate) branch_name_input: &'a mut String,
    pub(crate) swatch_library_selected: &'a mut String,
    pub(crate) graphic_style_name_input: &'a mut String,
    pub(crate) width_profile_name_input: &'a mut String,
    pub(crate) grammar_rules: &'a [(String, String)],
    pub(crate) grammar_rule_name_input: &'a mut String,
    pub(crate) grammar_rule_type_selected: &'a mut String,
    pub(crate) grammar_rule_params_input: &'a mut String,
    pub(crate) grammar_check_results: &'a [(String, bool, String)],
    pub(crate) distance_results: &'a [(String, String, f64, f64, f64)],
    pub(crate) action_names: &'a [(String, usize)],
    /// Full edit tree flattened for the commit graph (newest node first).
    pub(crate) history_graph: &'a [photonic_core::history::HistoryGraphNode],
    /// The HEAD node id in the edit tree.
    pub(crate) history_current: u64,
    /// The history node id matching the on-disk file, if saved — drives the
    /// "Last Save" marker in the commit graph. `None` for never-saved documents.
    pub(crate) history_last_saved: Option<u64>,
    /// Document-tab import/export settings (#176).
    pub(crate) doc_export: &'a mut crate::app::DocExportSettings,
    pub(crate) bleed_mm_input: &'a mut f64,
    pub(crate) slug_mm_input: &'a mut f64,
    pub(crate) construction_angle: &'a mut f64,
    pub(crate) construction_x: &'a mut f64,
    pub(crate) construction_y: &'a mut f64,
    pub(crate) margin_top: &'a mut f64,
    pub(crate) margin_right: &'a mut f64,
    pub(crate) margin_bottom: &'a mut f64,
    pub(crate) margin_left: &'a mut f64,
    pub(crate) event_trigger_event: &'a mut String,
    pub(crate) event_trigger_action: &'a mut String,
    pub(crate) workspace_name_input: &'a mut String,
    /// Media pool drawer state (video mode, 05 §2).
    pub(crate) media_ui: &'a mut media_pool::MediaPoolUi,
    /// Whether a `VideoEngine` session is attached (proxy toggle hint).
    pub(crate) engine_online: bool,
    /// Current engine proxy-mode intent (05 §4).
    pub(crate) proxy_mode: photonic_video::ProxyMode,
    /// Video-editor session state the video-mode drawers read/mutate (04 §4.1).
    /// The left-rail video drawers (`ClipInspector`/`Effects`/`Captions`/
    /// `NodeEditor`) reach their per-panel state through here so panel builders
    /// never touch `PhotonicApp` or `draw_drawer`.
    #[allow(dead_code)] // read as each left-rail video panel story is filled in.
    pub(crate) video: VideoPanelUi<'a>,
    pub(crate) action: Option<PanelAction>,
    pub(crate) q: String,
    pub(crate) forced_open: Option<bool>,
}

impl<'a> PropPanelCtx<'a> {
    pub(crate) fn matches(&self, label: &str) -> bool {
        self.q.is_empty() || label.to_lowercase().contains(&self.q)
    }
}

/// One of the six Canva-style drawer groups surfaced by the left icon rail.
///
/// Each group owns a disjoint slice of the ~43 property section functions; every
/// section is reachable through exactly one group. `draw_drawer` renders the
/// active group, so only one group's sections are visible at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DrawerGroup {
    /// Tool palette: selection/navigation/drawing/path tools + pinned hotbar.
    Tools,
    /// Selection inspector: navigator, selected node, tool/shape options, symbol
    /// overrides, text-variable binding (+ the Direct-Select vertex panel).
    Inspector,
    /// Shape/appearance operations: combine, boolean, blend, pathfinder, etc.
    Modify,
    /// Alignment, distribution, distances, dimensions, layer ops.
    Arrange,
    /// Swatches, gradients, styles, width profiles, symbols, variables, libraries.
    Assets,
    /// Document-level tooling: export, data-viz, analysis, grammar, actions, etc.
    Document,
    /// Edit history and branches.
    History,
    /// Video mode (04 §4.1): import, bins, asset list, probe metadata, proxy
    /// status/toggle. Interior owned by 05-import-export.md.
    MediaPool,
    /// Video mode (04 §4.1): selected clip's transform/speed/effects-stack/
    /// transition params — the `Clip`/`ClipEffect` analogue of `Inspector`.
    /// This doc owns the panel shell; widgets source from `prop_registry`.
    ClipInspector,
    /// Video mode (04 §4.1): effect browser/catalog, drag-to-apply onto the
    /// selected clip. Interior owned by 08-fusion-node-flows.md.
    Effects,
    /// Video mode (04 §4.1): caption track list, cue text/timing editor, style
    /// panel. Interior owned by 06-captions-ai.md.
    Captions,
    /// Video mode (04 §4.1): node palette + node inspector — NOT the graph
    /// canvas itself (that lives in the central panel's node-canvas content
    /// state, 08 §6.1). Interior owned by 08-fusion-node-flows.md.
    NodeEditor,
    /// Video mode (04 §4.1): starter title/lower-third/caption-card presets
    /// that insert a `ClipSource::Text` clip at the playhead, plus a basic
    /// text/size/color/position editor for the selected Text clip (05 §4b /
    /// 17 G-12 minimal — not the full VectorDoc title-template system).
    /// Interior owned by `panels/video/titles.rs`.
    Titles,
    /// Video mode (26 K-A2): the markers workflow — a searchable, filterable,
    /// sortable list of every marker on the active sequence in both scopes,
    /// click-to-navigate (zero undo units), name/note/category/position/
    /// **duration** editing (a duration > 0 is what makes a marker *ranged*,
    /// the unit K-F2's per-marker export fans out over), and the project's
    /// `MarkerCategory` registry with reassign-on-delete.
    /// Interior owned by `panels/video/markers.rs`.
    Markers,
    /// Video mode (04 §4.1): second preview surface for the raw armed
    /// source asset, its own scrub bar, and true source in/out marks
    /// (17-nle-parity-round2.md §G-10 — Larger, needs its own mini-spec).
    /// Interior owned by `panels/video/source_monitor.rs`; the real
    /// dual-monitor surface is mostly `app/monitor.rs` (out of this crate
    /// module's territory).
    SourceMonitor,
    /// Video mode (04 §4.1): multicam angle picker + sync controls (17 G-20
    /// — Larger). Interior owned by `panels/video/multicam.rs`; the real
    /// multi-camera source sequence + live angle cutting is
    /// `photonic-video-engine` + `app/monitor.rs` territory.
    Multicam,
    /// Video mode (04 §4.1): text-based (transcript) editing — select a
    /// word range, ripple the matching timeline clip range (17 G-18 —
    /// Larger, nice-to-have). Interior owned by `panels/video/transcript.rs`.
    Transcript,
    /// A group written by a newer build. Never offered by [`all_for_mode`], and
    /// normalized to the default in [`crate::preferences::AppPreferences::load`]
    /// so one unknown token cannot discard the whole preferences file.
    ///
    /// Without this arm, serde's `#[serde(default = …)]` does not help: it
    /// covers a *missing* field, not one that fails to deserialize, so a single
    /// unrecognised drawer token fails the whole struct and `load`'s
    /// `unwrap_or_default()` throws away every other preference the user set —
    /// keymap, hotbar usage, drawer widths, snap toggles. Same forward-compat
    /// rule as `MarkerAnchor`/`GroupKind` (39 §2.2).
    ///
    /// [`all_for_mode`]: DrawerGroup::all_for_mode
    #[serde(other)]
    Unknown,
}

impl DrawerGroup {
    /// All groups shown on the left rail in Vector mode, in order (top to
    /// bottom). History is intentionally absent — it now lives on the right
    /// rail (see [`RightDrawerGroup`]) — but the `History` variant is retained
    /// so `draw_drawer` can still render it there.
    pub const ALL: [DrawerGroup; 6] = [
        DrawerGroup::Tools,
        DrawerGroup::Inspector,
        DrawerGroup::Modify,
        DrawerGroup::Arrange,
        DrawerGroup::Assets,
        DrawerGroup::Document,
    ];

    /// Left-rail groups in Video mode (04 §4.1) — Media Pool first, matching
    /// every reference NLE's left-most-panel convention. The three round-2
    /// (17-nle-parity-round2.md) choke-point additions — SourceMonitor,
    /// Multicam, Transcript — trail the P1 set; each is a compile-clean stub
    /// until its named story fills it in.
    pub const VIDEO_ALL: [DrawerGroup; 10] = [
        DrawerGroup::MediaPool,
        DrawerGroup::ClipInspector,
        DrawerGroup::Effects,
        DrawerGroup::Markers,
        DrawerGroup::Captions,
        DrawerGroup::NodeEditor,
        DrawerGroup::Titles,
        DrawerGroup::SourceMonitor,
        DrawerGroup::Multicam,
        DrawerGroup::Transcript,
    ];

    /// Which group set the left rail offers for `mode` (04 §4).
    pub fn all_for_mode(mode: AppMode) -> &'static [DrawerGroup] {
        match mode {
            AppMode::Vector => &Self::ALL,
            AppMode::Video => &Self::VIDEO_ALL,
        }
    }

    /// Phosphor glyph shown on the rail button.
    pub fn icon(self) -> &'static str {
        match self {
            DrawerGroup::Tools => ph::TOOLBOX,
            DrawerGroup::Inspector => ph::SLIDERS_HORIZONTAL,
            DrawerGroup::Modify => ph::MAGIC_WAND,
            DrawerGroup::Arrange => ph::ARROWS_OUT_CARDINAL,
            DrawerGroup::Assets => ph::SWATCHES,
            DrawerGroup::Document => ph::FILE_TEXT,
            DrawerGroup::History => ph::CLOCK_COUNTER_CLOCKWISE,
            DrawerGroup::MediaPool => ph::FILM_STRIP,
            DrawerGroup::ClipInspector => ph::FRAME_CORNERS,
            DrawerGroup::Effects => ph::SPARKLE,
            DrawerGroup::Captions => ph::CLOSED_CAPTIONING,
            DrawerGroup::NodeEditor => ph::FLOW_ARROW,
            DrawerGroup::Titles => ph::TEXT_T,
            DrawerGroup::Markers => ph::MAP_PIN,
            DrawerGroup::SourceMonitor => ph::MONITOR_PLAY,
            DrawerGroup::Multicam => ph::SQUARES_FOUR,
            DrawerGroup::Transcript => ph::ARTICLE,
            DrawerGroup::Unknown => ph::QUESTION,
        }
    }

    /// Human-readable title shown in the drawer header and rail tooltip.
    pub fn title(self) -> &'static str {
        match self {
            DrawerGroup::Tools => "Tools",
            DrawerGroup::Inspector => "Inspector",
            DrawerGroup::Modify => "Modify",
            DrawerGroup::Arrange => "Arrange",
            DrawerGroup::Assets => "Assets",
            DrawerGroup::Document => "Document",
            DrawerGroup::History => "History",
            DrawerGroup::MediaPool => "Media Pool",
            DrawerGroup::ClipInspector => "Clip Inspector",
            DrawerGroup::Effects => "Effects",
            DrawerGroup::Captions => "Captions",
            DrawerGroup::NodeEditor => "Node Editor",
            DrawerGroup::Titles => "Titles",
            DrawerGroup::Markers => "Markers",
            DrawerGroup::SourceMonitor => "Source Monitor",
            DrawerGroup::Multicam => "Multicam",
            DrawerGroup::Transcript => "Transcript",
            DrawerGroup::Unknown => "Unknown",
        }
    }

    /// Whether this drawer has any content worth showing in the current context.
    /// Rail icons for groups returning `false` are disabled, and an open drawer
    /// that loses its content auto-collapses. Tools, Inspector (navigator + tool
    /// options), and the always-on library/document/history groups are always
    /// available; the operation drawers (Modify/Arrange) need a selection.
    ///
    /// Video-mode groups (04 §4.1): Media Pool/Effects/Captions/Node Editor are
    /// always available; Clip Inspector needs a **timeline clip** selection
    /// (`clip_selection_count`). Modify/Arrange need a **vector node** selection
    /// (`node_selection_count`). Passing the node count for Clip Inspector was
    /// a stub that left the rail icon permanently disabled in video mode —
    /// speed ramp / transform / freeze all live in that drawer and were
    /// unreachable without this split.
    pub fn has_content(self, node_selection_count: usize, clip_selection_count: usize) -> bool {
        match self {
            DrawerGroup::Tools
            | DrawerGroup::Inspector
            | DrawerGroup::Assets
            | DrawerGroup::Document
            | DrawerGroup::History
            | DrawerGroup::MediaPool
            | DrawerGroup::Effects
            | DrawerGroup::Captions
            | DrawerGroup::NodeEditor
            | DrawerGroup::Titles
            // Markers is always reachable: its category editor and
            // "add at playhead" are useful before a single marker exists.
            | DrawerGroup::Markers
            // Round-2 (17) additions are always reachable, like the P1 video
            // groups above — none of them gate on the vector node-selection
            // count (SourceMonitor/Multicam gate on an armed source/multicam
            // clip once their stories land; Transcript gates on the sequence
            // having captions; both are stub-empty until then).
            | DrawerGroup::SourceMonitor
            | DrawerGroup::Multicam
            | DrawerGroup::Transcript => true,
            DrawerGroup::Modify | DrawerGroup::Arrange => node_selection_count >= 1,
            // Timeline clip selection, not vector nodes.
            DrawerGroup::ClipInspector => clip_selection_count >= 1,
            // A group this build does not know has no sections to render, so it
            // never has content: the rail icon stays disabled and an `Unknown`
            // that survives to the UI auto-collapses instead of drawing an empty
            // drawer. `AppPreferences::load` normalizes it away first; this is
            // the second line of defence.
            DrawerGroup::Unknown => false,
        }
    }
}

/// One of the panels surfaced by the *right* icon rail — the mirror image of
/// [`DrawerGroup`] on the left. Each toggles a right-side drawer that used to be
/// stacked together in the always-on right panel (Layers over AI Chat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum RightDrawerGroup {
    /// The layer stack.
    Layers,
    /// The AI (Claude) chat assistant.
    Chat,
    /// Edit history and branches (moved here from the left rail).
    History,
    /// Video mode (04 §4.1): wheels/curves/HSL qualifier/LUT browser for the
    /// selected clip's grade. Interior owned by 07-color-grading.md.
    ColorControls,
    /// Video mode (04 §4.1): track fader strips, master bus meters, per-track
    /// EQ/comp/automation entry points. Interior owned by 09-audio-mixer.md.
    AudioMixer,
    /// A group written by a newer build. Never offered by [`all_for_mode`], and
    /// normalized to the default in [`crate::preferences::AppPreferences::load`]
    /// so one unknown token cannot discard the whole preferences file. Same
    /// forward-compat rule as [`DrawerGroup::Unknown`] — this enum has the
    /// identical bug, and fixing one of the two would only look done.
    ///
    /// [`all_for_mode`]: RightDrawerGroup::all_for_mode
    #[serde(other)]
    Unknown,
}

impl RightDrawerGroup {
    /// All groups in rail order (top to bottom), Vector mode.
    pub const ALL: [RightDrawerGroup; 3] = [
        RightDrawerGroup::Layers,
        RightDrawerGroup::Chat,
        RightDrawerGroup::History,
    ];

    /// Right-rail groups in Video mode (04 §4.1). `Layers` stays (a flattened
    /// clip list still helps keyboard-driven selection); `Chat`/`History` are
    /// mode-agnostic already.
    pub const VIDEO_ALL: [RightDrawerGroup; 5] = [
        RightDrawerGroup::Layers,
        RightDrawerGroup::ColorControls,
        RightDrawerGroup::AudioMixer,
        RightDrawerGroup::Chat,
        RightDrawerGroup::History,
    ];

    /// Which group set the right rail offers for `mode` (04 §4).
    pub fn all_for_mode(mode: AppMode) -> &'static [RightDrawerGroup] {
        match mode {
            AppMode::Vector => &Self::ALL,
            AppMode::Video => &Self::VIDEO_ALL,
        }
    }

    /// Phosphor glyph shown on the rail button.
    pub fn icon(self) -> &'static str {
        match self {
            RightDrawerGroup::Layers => ph::STACK,
            RightDrawerGroup::Chat => ph::CHAT_CIRCLE_DOTS,
            RightDrawerGroup::History => ph::CLOCK_COUNTER_CLOCKWISE,
            RightDrawerGroup::ColorControls => ph::PALETTE,
            RightDrawerGroup::AudioMixer => ph::SLIDERS,
            RightDrawerGroup::Unknown => ph::QUESTION,
        }
    }

    /// Human-readable title shown in the rail tooltip / drawer header.
    pub fn title(self) -> &'static str {
        match self {
            RightDrawerGroup::Layers => "Layers",
            RightDrawerGroup::Chat => "AI Chat",
            RightDrawerGroup::History => "History",
            RightDrawerGroup::ColorControls => "Color Controls",
            RightDrawerGroup::AudioMixer => "Audio Mixer",
            RightDrawerGroup::Unknown => "Unknown",
        }
    }
}

/// Render one drawer group: the group header + the shared search bar, then that
/// group's section functions in their original dispatch order. Returns any
/// queued [`PanelAction`].
///
/// Replaces the former always-on `draw_properties_panel` monolith: the section
/// functions are unchanged, just partitioned across the six [`DrawerGroup`]s.
pub(crate) fn draw_drawer(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    group: DrawerGroup,
) -> Option<PanelAction> {
    ui.label(
        RichText::new(group.title().to_uppercase())
            .small()
            .color(crate::theme::section_header_color(ui)),
    );
    ui.add_space(2.0);

    // ── Search bar ────────────────────────────────────────────────
    // Filters the section headers within the *open* drawer (each section fn
    // honours `ctx.q` / `ctx.forced_open`).
    // On the History drawer the query filters history entries + branches, not
    // property sections — so label it accordingly.
    let search_hint = if group == DrawerGroup::History {
        "Search history…"
    } else {
        "Search properties…"
    };
    if group != DrawerGroup::Transcript {
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut *ctx.prop_search)
                    .hint_text(search_hint)
                    .desired_width(ui.available_width() - 24.0),
            );
            if !ctx.prop_search.is_empty()
                && ui
                    .small_button(ph::X)
                    .on_hover_text("Clear search")
                    .clicked()
            {
                ctx.prop_search.clear();
                response.surrender_focus();
            }
        });
    }
    ui.add_space(4.0);

    // An empty query matches everything; a non-empty query forces matching
    // headers open so their contents are visible.
    ctx.q = ctx.prop_search.trim().to_lowercase();
    ctx.forced_open = if ctx.q.is_empty() { None } else { Some(true) };

    match group {
        DrawerGroup::Inspector => {
            // ── Context-aware: vertex editing (Direct Selection) ──────────
            // When a path is in point-edit mode, the inspector shows ONLY
            // anchor/vertex properties — node Transform/Fill/Stroke/Path
            // sections are suppressed, like Illustrator. This early-return
            // belongs to the Inspector drawer.
            if ctx.active_tool == Tool::DirectSelect {
                if let Some(nid) = ctx.point_edit_node {
                    if let Some(node) = ctx.doc.nodes.get(&nid) {
                        draw_vertex_panel(ui, node, nid, ctx.point_selected, &mut ctx.action);
                        return ctx.action.take();
                    }
                }
            }
            draw_navigator_section(ui, ctx);
            draw_selected_node(ui, ctx);
            draw_tool_shape_options(ui, ctx);
            draw_symbol_overrides(ui, ctx);
            draw_text_variable_binding(ui, ctx);
        }
        DrawerGroup::Modify => {
            draw_combine(ui, ctx);
            draw_boolean_ops(ui, ctx);
            draw_blend(ui, ctx);
            draw_pathfinder(ui, ctx);
            draw_distribute_on_path(ui, ctx);
            draw_compound_path(ui, ctx);
            draw_clipping_mask(ui, ctx);
            draw_blend_colors(ui, ctx);
            draw_adjust_colors(ui, ctx);
            draw_flatten_transparency(ui, ctx);
            draw_copy_appearance(ui, ctx);
        }
        DrawerGroup::Arrange => {
            draw_arrange_align(ui, ctx);
            draw_alignment(ui, ctx);
            draw_distribute_no_overlap(ui, ctx);
            draw_align_to_artboard(ui, ctx);
            draw_distances(ui, ctx);
            draw_dimension_annotations(ui, ctx);
            draw_layer_operations(ui, ctx);
        }
        DrawerGroup::Assets => {
            draw_color_swatches(ui, ctx);
            draw_spot_colors(ui, ctx);
            draw_gradient_swatches(ui, ctx);
            draw_graphic_styles(ui, ctx);
            draw_width_profiles(ui, ctx);
            draw_symbols_panel(ui, ctx);
            draw_variables(ui, ctx);
            draw_libraries_export(ui, ctx);
        }
        DrawerGroup::Document => {
            draw_import_export(ui, ctx);
            draw_export_profiles(ui, ctx);
            draw_data_visualization(ui, ctx);
            draw_analysis(ui, ctx);
            draw_composition_analysis(ui, ctx);
            draw_document_grammar(ui, ctx);
            draw_document_workflow(ui, ctx);
            draw_actions(ui, ctx);
            draw_event_triggers(ui, ctx);
            draw_workspaces(ui, ctx);
        }
        DrawerGroup::History => {
            // Named branches are now labels on commits in the graph itself — the
            // separate "Branches" accordion has been retired.
            draw_edit_history(ui, ctx);
        }
        DrawerGroup::MediaPool => media_pool::draw_media_pool(ui, ctx),
        DrawerGroup::ClipInspector => video::clip_inspector::draw_clip_inspector(ui, ctx),
        DrawerGroup::Effects => video::effects_browser::draw_effects_browser(ui, ctx),
        DrawerGroup::Captions => video::caption_editor::draw_caption_editor(ui, ctx),
        DrawerGroup::NodeEditor => video::node_editor::draw_node_editor_palette(ui, ctx),
        DrawerGroup::Titles => video::titles::draw_titles(ui, ctx),
        DrawerGroup::Markers => video::markers::draw_markers(ui, ctx),
        DrawerGroup::SourceMonitor => video::source_monitor::draw_source_monitor(ui, ctx),
        DrawerGroup::Multicam => video::multicam::draw_multicam(ui, ctx),
        DrawerGroup::Transcript => video::transcript::draw_transcript(ui, ctx),
        // Tools is rendered by the app layer (it needs tool state, not the
        // property ctx), so it is never routed through draw_drawer.
        DrawerGroup::Tools => {}
        // A group written by a newer build has no sections here to render.
        // `AppPreferences::load` normalizes it to the default and `has_content`
        // reports false, so this arm is unreachable in practice — it exists so
        // the catch-all can never draw a half-populated drawer.
        DrawerGroup::Unknown => {}
    }

    ctx.action.take()
}

// ─── Fill editor ─────────────────────────────────────────────────────────────

/// Render a small eyedropper icon button. Returns `true` when clicked.
pub(crate) fn eyedropper_btn(ui: &mut Ui) -> bool {
    ui.small_button(ph::EYEDROPPER)
        .on_hover_text("Pick color from screen (Esc to cancel)")
        .clicked()
}

// ─── Stroke editor ────────────────────────────────────────────────────────────

// ─── Glow editor ──────────────────────────────────────────────────────────────

// ─── Gaussian Glow editor ─────────────────────────────────────────────────────

// ─── Audit panel ──────────────────────────────────────────────────────────────

/// Floating window showing recent MCP tool calls from the in-memory audit log.
pub fn draw_audit_panel(
    ctx: &egui::Context,
    audit_log: &Option<std::sync::Arc<std::sync::Mutex<photonic_core::AuditLog>>>,
    open: &mut bool,
    filter: &mut String,
) {
    egui::Window::new("MCP Audit Log")
        .id(egui::Id::new("audit_panel"))
        .default_size([560.0, 380.0])
        .min_width(400.0)
        .min_height(200.0)
        .open(open)
        .show(ctx, |ui| {
            let Some(log_arc) = audit_log else {
                ui.label("Audit log not available (headless mode).");
                return;
            };
            let (all, total) = match log_arc.lock() {
                Ok(log) => {
                    let entries: Vec<_> = log.entries().iter().rev().take(200).cloned().collect();
                    let total = log.total_recorded();
                    (entries, total)
                }
                Err(_) => {
                    ui.label("Audit log lock unavailable.");
                    return;
                }
            };

            // ── Filter bar ────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.label("Filter:");
                ui.text_edit_singleline(filter);
                if ui.small_button(ph::X).clicked() {
                    filter.clear();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("{} total recorded", total));
                });
            });
            ui.separator();

            let filter_lower = filter.to_lowercase();
            let entries: Vec<_> = if filter_lower.is_empty() {
                all
            } else {
                all.into_iter()
                    .filter(|e: &photonic_core::AuditEntry| {
                        e.tool_name.to_lowercase().contains(&filter_lower)
                    })
                    .collect()
            };

            if entries.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak("No audit entries yet — make an MCP tool call to see it here.");
                });
                return;
            }

            // ── Header row ────────────────────────────────────────────────
            egui::Grid::new("audit_header")
                .num_columns(4)
                .min_col_width(40.0)
                .show(ui, |ui| {
                    ui.strong("#");
                    ui.strong("Time");
                    ui.strong("Tool");
                    ui.strong("ms");
                    ui.end_row();
                });
            ui.separator();

            // ── Scrollable rows ───────────────────────────────────────────
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("audit_entries")
                        .num_columns(4)
                        .striped(true)
                        .min_col_width(40.0)
                        .show(ui, |ui| {
                            for entry in &entries {
                                // ID
                                ui.weak(format!("{}", entry.id));

                                // Timestamp — show HH:MM:SS only
                                let ts_short =
                                    entry.timestamp.get(11..19).unwrap_or(&entry.timestamp);
                                ui.weak(ts_short);

                                // Tool name — color by error status
                                if entry.is_error {
                                    ui.colored_label(
                                        Color32::from_rgb(220, 80, 80),
                                        &entry.tool_name,
                                    );
                                } else {
                                    ui.colored_label(
                                        Color32::from_rgb(100, 200, 120),
                                        &entry.tool_name,
                                    );
                                }

                                // Duration
                                ui.weak(format!("{}ms", entry.duration_ms));

                                ui.end_row();

                                // Result summary (spans all columns)
                                if !entry.result_summary.is_empty() {
                                    ui.label(""); // id col
                                    let summary = if entry.result_summary.len() > 120 {
                                        format!("{}…", &entry.result_summary[..120])
                                    } else {
                                        entry.result_summary.clone()
                                    };
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(summary).weak().italics().size(10.5),
                                        )
                                        .wrap(),
                                    );
                                    ui.label("");
                                    ui.label("");
                                    ui.end_row();
                                }
                            }
                        });
                });
        });
}
