mod dialogs;
mod geometry;
mod hotbar_ui;
mod panel_actions;
mod raster_ops;
mod search_ui;
use geometry::*;
mod demos;
use demos::*;
mod hit_test;
use hit_test::*;
mod editing_workflows;
/// G-10 source marks (session-only). `pub` so UI-path integration tests can
/// drive the same types the transport and command palette use.
pub mod source_marks;
// `pub` (not `pub(crate)`): CAP-022's crash-recovery integration test
// (crates/photonic-gui/tests/timeline_recovery.rs) drives the real write/load
// path via test-only hooks in `autosave.rs`, and external integration test
// crates can only reach items through a fully `pub` module chain. See the
// hooks' doc comments in `autosave.rs` for the full rationale.
pub mod autosave;
mod clipboard;
mod close_guard;
mod command_center;
mod direct_select;
pub mod engine;
mod erase_tools;
pub(crate) mod gradient_handles;
pub(crate) mod layer_ops;
mod menu_drawer;
pub(crate) mod mode;
pub(crate) mod monitor;
mod proportional_move;
mod recovery;
// `pub` (29 §3 / CAP-019): the acceptance-story harness's GUI arm calls
// `reframe::fit_clips_to_active_format` directly, the same entry the reframe
// widget uses.
pub mod reframe;
mod rulers;
mod tabs;
/// Timeline panel + edit interact helpers. `pub` so UI-path integration tests
/// can construct `PendingSource` the same way Insert/Overwrite does.
pub mod timeline;
mod tool_handlers;
mod width_tool;
use egui::{Color32, RichText};
use egui_phosphor::regular as ph;
use kurbo::{BezPath, PathEl, Point};
pub use mode::AppMode;
use photonic_core::{
    history::{Command, CommandHistory, HistoryGraphNode},
    layer::LayerId,
    node::{GroupNode, NodeId, PathNode},
    ops::artboard_ops,
    timeline::{ClipId, CueId, GradeOpId, GraphId, GraphNodeId, SequenceId, Tick, TrackId},
    Color, Document, Fill, Layer, PathData, SceneNode, SceneNodeKind, Selection, Stroke,
};
use photonic_render::{CanvasView, ExportBackground, ExportOptions, PhotonicRenderer};
pub(crate) use rulers::GuideEditPopup;
use std::path::Path;
use std::sync::Arc;
use timeline::TimelineView;

use crate::{
    hotbar::{self, HotbarAction, HotbarBucket, HotbarEffect, HotbarItem, HotbarMode},
    panels::{
        self, ColorPageTab, DrawerGroup, EyedropperTarget, PanelAction, RightDrawerGroup,
        ScopeKind, SelectSameAttr, ShapeKind, VideoPanelUi, ZOrderOp,
    },
    preferences::AppPreferences,
    radial_wheel::{WheelContext, WheelNodeKind, WheelState},
    tools::Tool,
};

pub use clipboard::NativeClipboardPaste;

/// Marker kept in the native clipboard after Photonic copies scene objects.
/// It makes egui emit a paste event even though the actual object snapshot is
/// held in-process by [`GuiClipboard`].
pub(crate) const INTERNAL_OBJECT_CLIPBOARD_MARKER: &str = "photonic:objects";

/// In-process copy/paste buffer for scene objects (Ctrl+C / Ctrl+V).
///
/// Stores a *detached* snapshot of each copied object as a full subtree — a
/// group carries all of its descendants — keyed by the object's original ids.
/// Because it holds cloned node data (not references into a document), it
/// survives switching between open documents, so paste works both within one
/// document and across files. Pasting remaps every id to fresh ones via
/// [`photonic_core::ops::cloning::clone_subtrees`], so the same buffer can be
/// pasted repeatedly without collisions.
#[derive(Default, Clone)]
pub struct GuiClipboard {
    /// Top-level object ids that were copied, in selection order.
    roots: Vec<NodeId>,
    /// Every node in the copied subtrees (roots + descendants), by original id.
    nodes: std::collections::HashMap<NodeId, SceneNode>,
}

impl GuiClipboard {
    /// True when nothing has been copied yet.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Capture the given top-level objects (and every descendant) from `doc`.
    /// Replaces any previous contents. Ignores ids not present in `doc`.
    pub fn capture<'a>(&mut self, doc: &Document, ids: impl IntoIterator<Item = &'a NodeId>) {
        self.roots.clear();
        self.nodes.clear();
        for id in ids {
            if !doc.nodes.contains_key(id) || self.roots.contains(id) {
                continue;
            }
            self.roots.push(*id);
            let mut stack = vec![*id];
            while let Some(nid) = stack.pop() {
                if let Some(node) = doc.nodes.get(&nid) {
                    if let SceneNodeKind::Group(g) = &node.kind {
                        stack.extend(g.children.iter().copied());
                    }
                    self.nodes.insert(nid, node.clone());
                }
            }
        }
    }

    /// Build a paste command for the current contents targeting `target_layer`,
    /// offset by `(dx, dy)`. Returns the command plus the fresh root ids to
    /// select, or `None` if the clipboard is empty.
    pub fn paste_command(
        &self,
        target_layer: LayerId,
        dx: f64,
        dy: f64,
    ) -> Option<(Command, Vec<NodeId>)> {
        if self.roots.is_empty() {
            return None;
        }
        let (roots, nodes) = photonic_core::ops::cloning::clone_subtrees(
            &self.nodes,
            &self.roots,
            target_layer,
            dx,
            dy,
        );
        if roots.is_empty() {
            return None;
        }
        let cmd = Command::AddSubtree {
            layer_id: target_layer,
            roots: roots.clone(),
            nodes,
        };
        Some((cmd, roots))
    }
}

/// A floating fill/stroke color picker raised from the radial menu.
#[derive(Clone, Copy)]
pub(crate) struct ColorPopupState {
    /// Path node whose color is being edited.
    pub node_id: NodeId,
    /// Edit the stroke color when true, otherwise the solid fill color.
    pub stroke: bool,
    /// Screen-space anchor where the picker first appears.
    pub pos: egui::Pos2,
}

/// Cached hotbar ordering. Rebuilt only when the signature (context bucket,
/// single-group flag, or mode) changes — never per click or per frame — so the
/// Adaptive order stays calm.
struct HotbarCacheState {
    bucket: HotbarBucket,
    single_is_group: bool,
    single_is_fillable_path: bool,
    mode: HotbarMode,
    items: Vec<HotbarItem>,
}

// ─── Eyedropper ───────────────────────────────────────────────────────────────

/// State for the in-canvas eyedropper tool.
///
/// Color sampling is performed entirely within the document's own canvas by
/// converting the egui cursor position to canvas coordinates and calling
/// `photonic_core::sample_fill_at`.  No screen capture or external portal is
/// used, so this works correctly on Wayland.
#[derive(Default)]
pub struct EyedropperState {
    pub target: Option<EyedropperTarget>,
    /// Skip the very first `primary_clicked` after activation so the button's
    /// own release doesn't immediately trigger a sample.
    skip_click: bool,
}

impl EyedropperState {
    pub fn active(&self) -> bool {
        self.target.is_some()
    }

    fn cancel(&mut self) {
        self.target = None;
        self.skip_click = false;
    }
}

// ─── Raster color-range masking ───────────────────────────────────────────────

/// Live "hide pixels by color" session on a raster layer.
///
/// While active, the document holds a *preview*: `original` with the matching
/// pixels subtracted from its layer mask. Fuzziness/contiguous changes rebuild
/// the preview from `original` (so they never accumulate); Apply commits
/// `original → current` as one undoable `UpdateNode`; Cancel restores
/// `original` verbatim.
pub struct RasterColorRangeSession {
    pub node_id: NodeId,
    /// Color sampled from the layer's own pixels (straight RGBA).
    pub target: [u8; 4],
    /// Seed pixel (node-local) for contiguous / magic-wand mode.
    pub seed: (u32, u32),
    /// The node as it was before the preview.
    pub original: SceneNode,
}

// ─── Drawer kind ──────────────────────────────────────────────────────────────

/// Which top-bar drawer is currently open (replaces floating popover menus).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrawerKind {
    File,
    Edit,
    Tools,
}

/// Which corner handle is being dragged during a resize operation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResizeHandle {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Which side of an anchor a bezier control handle belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    /// Incoming handle — `c2` of the `CurveTo` whose endpoint is this anchor.
    In,
    /// Outgoing handle — `c1` of the `CurveTo` element following this anchor.
    Out,
}

/// What the Direct Selection tool is currently dragging.
#[derive(Debug, Clone)]
pub enum DirectDrag {
    /// Moving the set of selected anchor points.
    Anchors,
    /// Dragging a single bezier control handle on `anchor` (`In`/`Out` side).
    Handle { anchor: usize, kind: HandleKind },
    /// Dragging the Live-Corners rounding widget. `pivot` is the anchor whose
    /// widget was grabbed; the same radius is applied to all selected straight
    /// corners. `origin_bez` is the path captured at drag start (local space).
    /// `grab_dist` is the pivot-corner→press distance in local units, subtracted
    /// so the radius starts at 0 on grab instead of snapping to the widget offset.
    Corner {
        pivot: usize,
        origin_bez: kurbo::BezPath,
        grab_dist: f64,
    },
    /// Moving the whole shape by dragging its fill/interior. `start_e`/`start_f`
    /// are the node transform's original translation (`matrix[4]`/`matrix[5]`),
    /// captured at press so the per-frame delta stays stable (#164).
    /// `ref_x`/`ref_y` are a canvas-space reference point (the node's bbox
    /// top-left) captured at press, so grid snap aligns the shape's edge to the
    /// grid instead of quantizing the raw displacement — mirroring the Move
    /// tool's `move_snap_ref` (#181 requirement 3). `None` when bounds are
    /// unavailable, in which case snap falls back to the raw target.
    Shape {
        start_e: f64,
        start_f: f64,
        ref_pt: Option<(f64, f64)>,
    },
}

// ─── Diff highlight ────────────────────────────────────────────────────────────

/// Category of a node in a checkpoint diff, used to colour canvas highlights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffCategory {
    /// Present in current doc but not in the baseline checkpoint — shown green.
    Added,
    /// Present in the baseline checkpoint but not in the current doc — shown red.
    Removed,
    /// Present in both but with changed properties — shown yellow.
    Modified,
}

const FILE_OPTIONS: &[&str] = &["Document", "Save", "Export"];
const EDIT_OPTIONS: &[&str] = &[
    "Appearance",
    "Canvas",
    "Tool Defaults",
    "Behavior",
    "Keyboard Shortcuts",
    "Privacy & Diagnostics",
];

// ─── Export dialog ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExportFormat {
    Png,
    Jpeg,
    WebP,
    Gif,
    Tiff,
    Ico,
    Svg,
}

/// Which part of the document a Document-tab export covers (#176).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportArea {
    /// The whole document canvas.
    Document,
    /// The tight bounding box of all content.
    ContentBounds,
    /// The bounding box of the current selection.
    Selection,
    /// The active artboard's rectangle.
    Artboard,
}

/// Persistent Document-tab import/export settings (#176). Seeds the export
/// dialog when the user clicks Export… from the Document drawer.
#[derive(Debug, Clone, Copy)]
pub struct DocExportSettings {
    pub format: ExportFormat,
    /// Output scale multiplier applied to the document/region pixel size.
    pub scale: f32,
    pub area: ExportArea,
}

impl Default for DocExportSettings {
    fn default() -> Self {
        Self {
            format: ExportFormat::Png,
            scale: 1.0,
            area: ExportArea::Document,
        }
    }
}

/// Multi-artboard batch export mode for the Export dialog.
#[derive(Clone, Copy, PartialEq)]
pub enum ArtboardExport {
    /// Single image (whole document or one region). No batch.
    Off,
    /// One file per artboard for the 1-based inclusive index range [start, end].
    Range { start: usize, end: usize },
}

pub struct ExportDialog {
    pub format: ExportFormat,
    pub background: ExportBackground,
    pub crop_to_content: bool,
    pub png_width: u32,
    pub png_height: u32,
    /// When `Range`, export one file per artboard instead of a single image.
    pub artboard_export: ArtboardExport,
    /// Output pixels-per-document-unit for batch artboard export.
    pub artboard_scale: f32,
    /// Single-image export target: `Some(id)` crops to that artboard, `None`
    /// exports the whole document (or the selection/content region).
    pub artboard_target: Option<photonic_core::ArtboardId>,
    pub ico_size_16: bool,
    pub ico_size_32: bool,
    pub ico_size_48: bool,
    pub ico_size_256: bool,
    /// JPEG quality (1–100).
    pub jpeg_quality: u8,
    /// Aspect ratio of the document at the time the dialog was opened.
    aspect: f64,
    /// Explicit export region `(x, y, w, h)` — set when exporting a selection or
    /// bounds from the Document tab (#176). Overrides `crop_to_content`.
    pub region_override: Option<(f64, f64, f64, f64)>,
}

impl ExportDialog {
    pub fn new(doc: &Document) -> Self {
        Self {
            format: ExportFormat::Png,
            background: ExportBackground::Transparent,
            crop_to_content: true,
            png_width: doc.width as u32,
            png_height: doc.height as u32,
            artboard_export: ArtboardExport::Off,
            artboard_scale: 1.0,
            artboard_target: None,
            ico_size_16: true,
            ico_size_32: true,
            ico_size_48: true,
            ico_size_256: true,
            jpeg_quality: 90,
            aspect: doc.width / doc.height,
            region_override: None,
        }
    }

    pub fn export_opts(&self) -> ExportOptions {
        let ico_sizes = [
            self.ico_size_16.then_some(16u32),
            self.ico_size_32.then_some(32),
            self.ico_size_48.then_some(48),
            self.ico_size_256.then_some(256),
        ]
        .into_iter()
        .flatten()
        .collect();
        ExportOptions {
            background: self.background,
            // An explicit region (Document-tab selection/bounds export) wins over
            // the generic crop-to-content toggle.
            crop_to_content: self.crop_to_content && self.region_override.is_none(),
            ico_sizes,
            jpeg_quality: self.jpeg_quality,
            region: self.region_override,
            overprint_preview: false,
        }
    }
}

/// Which tab is active in the console panel.
#[derive(PartialEq, Clone, Copy, Default)]
pub enum ConsoleTab {
    #[default]
    Lua,
    Claude,
}

// ─── Simplify dialog ─────────────────────────────────────────────────────────

struct SimplifyDialog {
    node_id: NodeId,
    node_name: String,
    tolerance: f64,
    /// When true, fit smooth cubic Béziers (min anchors) instead of the
    /// straight-line Ramer-Douglas-Peucker reduction.
    fit_curves: bool,
    /// Corner-angle threshold (degrees) for curve-fit mode: joins gentler than
    /// this fuse into a curve, sharper ones stay as cusps.
    corner_angle_deg: f64,
    /// Curve-fit mode: also flatten & re-fit existing curve segments (vs.
    /// preserving them and only fitting straight-line runs).
    refit_existing: bool,
    /// Anchor-point count of the original path, captured when the dialog opens.
    orig_points: usize,
    /// Result cached for the parameters below, so the (potentially expensive)
    /// op runs only when a parameter changes, not every frame.
    preview: Option<PathData>,
    /// Parameters the cached `preview` was built for. `NaN` tol means "not built
    /// yet" so the first comparison always misses and forces a build.
    cached_tol: f64,
    cached_fit: bool,
    cached_angle: f64,
    cached_refit: bool,
}

impl SimplifyDialog {
    /// Recompute the preview from the current parameters if any changed.
    fn refresh(&mut self, path: &PathData) {
        let stale = self.preview.is_none()
            || self.cached_tol != self.tolerance
            || self.cached_fit != self.fit_curves
            || self.cached_angle != self.corner_angle_deg
            || self.cached_refit != self.refit_existing;
        if stale {
            self.preview = Some(self.compute(path));
            self.cached_tol = self.tolerance;
            self.cached_fit = self.fit_curves;
            self.cached_angle = self.corner_angle_deg;
            self.cached_refit = self.refit_existing;
        }
    }

    /// Run the selected operation on `path` with the current parameters.
    fn compute(&self, path: &PathData) -> PathData {
        if self.fit_curves {
            photonic_core::ops::fit_curves::fit_curves(
                path,
                &photonic_core::ops::fit_curves::FitOptions {
                    accuracy: self.tolerance,
                    corner_angle_deg: self.corner_angle_deg,
                    refit_existing: self.refit_existing,
                },
            )
        } else {
            photonic_core::ops::simplify::simplify_path(path, self.tolerance)
        }
    }
}

// ─── Merge Vertices by Distance dialog ────────────────────────────────────────

struct MergeVerticesDialog {
    node_id: NodeId,
    node_name: String,
    threshold: f64,
    /// Anchor-point count of the original path, captured when the dialog opens.
    orig_points: usize,
    /// Welded result cached for `cached_thr`, so the weld runs only on a
    /// threshold change (or first build), not every frame.
    preview: Option<PathData>,
    /// Threshold the cached `preview` was built for. `NaN` means "not built
    /// yet" so the first comparison always misses and forces a build.
    cached_thr: f64,
}

struct FindReplaceTextDialog {
    find: String,
    replace: String,
    regex: bool,
    case_sensitive: bool,
    selection_only: bool,
}

/// What an [`ObjectOptionsDialog`] is editing — a whole layer or a single object.
enum OptionsTarget {
    Layer(photonic_core::layer::LayerId),
    Node(NodeId),
}

/// Working copy of a layer's or object's settings, edited in the Options modal
/// (right-click any Layers-tab row → Options…) and applied as one `UpdateLayer`
/// or `UpdateNode` on OK. Fields are shown scoped to the target's type.
struct ObjectOptionsDialog {
    target: OptionsTarget,
    /// Human label of the target's kind: "Layer", "Path", "Group", "Text", "Image".
    kind_label: &'static str,
    name: String,
    visible: bool,
    locked: bool,
    opacity: f32,
    blend_mode: photonic_core::layer::BlendMode,
    // Layer-only:
    is_template: bool,
    color_enabled: bool,
    color: [f32; 4],
    print: bool,
    lock_transparency: bool,
    lock_pixels: bool,
    lock_position: bool,
    // Group-node only:
    is_group: bool,
    clip_children: bool,
    // Pristine originals, captured at open, for live-preview revert (Cancel) and
    // the single undo step (OK: orig → edited).
    orig_layer: Option<photonic_core::layer::Layer>,
    orig_node: Option<photonic_core::node::SceneNode>,
}

impl ObjectOptionsDialog {
    fn from_layer(
        layer_id: photonic_core::layer::LayerId,
        l: &photonic_core::layer::Layer,
    ) -> Self {
        Self {
            target: OptionsTarget::Layer(layer_id),
            kind_label: "Layer",
            name: l.name.clone(),
            visible: l.visible,
            locked: l.locked,
            opacity: l.opacity,
            blend_mode: l.blend_mode,
            is_template: l.is_template,
            color_enabled: l.color.is_some(),
            color: l.color.unwrap_or([0.42, 0.51, 0.9, 1.0]),
            print: l.print,
            lock_transparency: l.lock_transparency,
            lock_pixels: l.lock_pixels,
            lock_position: l.lock_position,
            is_group: false,
            clip_children: false,
            orig_layer: Some(l.clone()),
            orig_node: None,
        }
    }

    fn from_node(node_id: NodeId, n: &photonic_core::node::SceneNode) -> Self {
        use photonic_core::node::SceneNodeKind as K;
        let (kind_label, is_group, clip_children) = match &n.kind {
            K::Path(_) => ("Path", false, false),
            K::Group(g) => ("Group", true, g.clip_children),
            K::Text(_) => ("Text", false, false),
            K::Raster(_) => ("Image", false, false),
        };
        Self {
            target: OptionsTarget::Node(node_id),
            kind_label,
            name: n.name.clone(),
            visible: n.visible,
            locked: n.locked,
            opacity: n.opacity,
            blend_mode: n.blend_mode,
            is_template: false,
            color_enabled: false,
            color: [0.42, 0.51, 0.9, 1.0],
            print: true,
            lock_transparency: false,
            lock_pixels: false,
            lock_position: false,
            is_group,
            clip_children,
            orig_layer: None,
            orig_node: Some(n.clone()),
        }
    }

    /// The layer's fields set to this dialog's currently-edited values (a template
    /// layer is implicitly locked). Used for live preview and the final command.
    fn edited_layer(&self, base: &photonic_core::layer::Layer) -> photonic_core::layer::Layer {
        let mut l = base.clone();
        l.name = self.name.clone();
        l.visible = self.visible;
        l.locked = if self.is_template { true } else { self.locked };
        l.opacity = self.opacity.clamp(0.0, 1.0);
        l.blend_mode = self.blend_mode;
        l.is_template = self.is_template;
        l.color = if self.color_enabled {
            Some(self.color)
        } else {
            None
        };
        l.print = self.print;
        l.lock_transparency = self.lock_transparency;
        l.lock_pixels = self.lock_pixels;
        l.lock_position = self.lock_position;
        l
    }

    /// The node with this dialog's currently-edited values applied.
    fn edited_node(&self, base: &photonic_core::node::SceneNode) -> photonic_core::node::SceneNode {
        let mut n = base.clone();
        n.name = self.name.clone();
        n.visible = self.visible;
        n.locked = self.locked;
        n.opacity = self.opacity.clamp(0.0, 1.0);
        n.blend_mode = self.blend_mode;
        if let photonic_core::node::SceneNodeKind::Group(g) = &mut n.kind {
            g.clip_children = self.clip_children;
        }
        n
    }
}

// ── Extracted sub-structs ─────────────────────────────────────────────────────

/// State for the Lua REPL console panel.
#[derive(Default)]
pub struct LuaConsoleState {
    pub visible: bool,
    pub expanded: bool,
    pub tab: ConsoleTab,
    pub input: String,
    pub log: Vec<(bool, String)>,
    /// Lua code queued for execution by main.rs after the draw lock is released.
    pub pending: Option<String>,
}

/// State for the in-app Claude chat panel.
#[derive(Default)]
pub struct ClaudeChatState {
    /// Chat history: (is_user, message_text).
    pub messages: Vec<(bool, String)>,
    pub input: String,
    pub busy: bool,
    /// Message queued for main.rs to dispatch to the Claude subprocess.
    pub pending: Option<String>,
}

/// State for the floating MCP audit log panel.
#[derive(Default)]
pub struct AuditPanelState {
    /// Shared MCP audit log (set by main.rs after construction).
    pub log: Option<Arc<std::sync::Mutex<photonic_core::AuditLog>>>,
    pub panel_open: bool,
    pub filter: String,
}

/// State for the diff highlight overlay shown after AI edits.
#[derive(Default)]
pub struct DiffOverlayState {
    /// Added/modified nodes to highlight on the canvas (node_id, category).
    pub highlights: Vec<(NodeId, DiffCategory)>,
    /// Pre-computed canvas-space bounding boxes for removed nodes (not in doc).
    pub removed_boxes: Vec<egui::Rect>,
    pub overlay_active: bool,
}

/// A cached egui texture for a raster node, with the content hash it was built
/// from so it can be invalidated when the pixels or mask change.
struct RasterTexCache {
    handle: egui::TextureHandle,
    hash: u64,
}

/// A cached egui texture for the Pixel/Overprint Preview overlay (#22). The
/// `hash` folds the document content, active mode, and target pixel size so the
/// expensive headless re-render only runs when something the preview depends on
/// changes.
struct PreviewTexCache {
    handle: egui::TextureHandle,
    hash: u64,
}

/// In-progress artboard move: the board id and cursor→origin grab offset.
struct ArtboardDrag {
    id: photonic_core::ArtboardId,
    grab_dx: f64,
    grab_dy: f64,
}

/// One open document and its per-document state, for the multi-document tab bar.
///
/// The engine fields (`document`/`history`/`view`) are authoritative only while
/// this tab is **parked** (inactive). For the active tab they hold stale
/// placeholders — its live state is the `&mut Document/CommandHistory/CanvasView`
/// threaded through [`PhotonicApp::draw`] plus `PhotonicApp::{current_file,
/// selected_id}`. The tab-bar meta (`title`, `dirty`, `last_saved_node`) is kept
/// current for the active tab every frame.
pub struct DocTab {
    /// Parked document contents (swapped into the shared Arc when activated).
    pub document: Document,
    /// Parked undo/redo history.
    pub history: CommandHistory,
    /// Parked viewport pan/zoom.
    pub view: CanvasView,
    /// Path of this doc's .photon file (None = never saved to a user file).
    pub current_file: Option<std::path::PathBuf>,
    /// Selected node in this doc.
    pub selected_id: Option<NodeId>,
    /// Tab-bar label (document name / file stem).
    pub title: String,
    /// Unsaved changes since the last save/autosave.
    pub dirty: bool,
    /// Recovery-folder autosave file for an untitled doc (cleaned up on real save).
    pub recovery_path: Option<std::path::PathBuf>,
    /// History node id matching the on-disk file — drives the "Last Save" marker.
    pub last_saved_node: Option<u64>,
    /// Vector/Video mode this tab was in (04 §1). Restored in `switch_tab`.
    pub mode: AppMode,
    /// Timeline zoom/scroll for this tab (04 §2.1/§6).
    pub timeline_view: TimelineView,
    /// Playhead position for this tab's sequence (04 §6).
    pub playhead: Tick,
    /// Selected clip ids in this tab's timeline panel (04 §2.6/§6).
    pub timeline_selection: Vec<ClipId>,
}

/// Modal timeline tool armed from the timeline mini-toolbar's segmented
/// control (17-nle-parity-round2.md §G-13 — Premiere's V/A/N/Y/U row).
/// Additive over today's hover-zone + modifier drag-kind inference
/// (`timeline::interact::resolve_drag_kind`): once the G-13 story wires it
/// in, an armed non-`Select` tool biases which edit a lane drag performs.
/// Session-only, like `timeline_razor_active` below (which this will
/// eventually fold `Razor` into — untouched here, existing razor behavior
/// is unchanged by adding this enum).
#[allow(dead_code)] // variants beyond the default are constructed by the G-13 story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TimelineTool {
    /// Standard pointer: select/move/trim by hover zone (today's default).
    #[default]
    Select,
    /// Blade: click a lane to split the clip under the cursor (spec 16 §4).
    Razor,
    /// Pan the timeline view by dragging anywhere in the lane area.
    Hand,
    /// Slip: drag shifts a clip's source in/out without moving its timeline
    /// position or duration.
    Slip,
    /// Slide: drag moves a clip while trimming its neighbours to fill the
    /// gap; its own in/out is unchanged.
    Slide,
}

pub struct PhotonicApp {
    pub active_tool: Tool,
    /// The tool that was active on the previous frame. Used to edge-detect
    /// switching *into* a tool (e.g. seeding Direct Select's point-edit state
    /// from the current object selection — #164).
    last_tool: Tool,
    pub fill_color: [f32; 4],
    pub polygon_sides: u32,
    pub star_points: u32,
    pub star_inner_ratio: f32,
    pub rounded_rect_radius: f64,
    pub spiral_turns: f32,
    pub spiral_inner_radius: f32,
    pub spiral_segs_per_turn: u32,
    /// Pending shear values typed into the Properties panel (applied on "Apply Shear" click).
    pub shear_x: f64,
    pub shear_y: f64,
    /// Line tool: snap endpoint to the nearest 45° angle from the start point.
    pub line_snap_45: bool,
    /// Currently selected harmony rule in the Color Guide panel.
    pub color_guide_rule: String,
    /// Arc tool: start angle in degrees (0 = 3 o'clock).
    pub arc_start_angle: f64,
    /// Arc tool: end angle in degrees.
    pub arc_end_angle: f64,
    /// Arc tool: if true, draw open arc; if false, close the arc (pie sector).
    pub arc_open: bool,
    /// Grid tool: number of columns.
    pub grid_cols: u32,
    /// Grid tool: number of rows.
    pub grid_rows: u32,
    /// Polar Grid tool: number of concentric rings.
    pub polar_grid_rings: u32,
    /// Polar Grid tool: number of radial sectors.
    pub polar_grid_sectors: u32,
    /// Polar Grid tool: inner radius fraction (0 = full disk).
    pub polar_grid_inner_ratio: f32,
    /// Layer IDs checked in the layers panel for multi-layer operations (e.g. Merge).
    pub selected_layer_ids: Vec<photonic_core::layer::LayerId>,

    /// Currently selected node (Select tool).
    pub selected_id: Option<NodeId>,

    /// Vector/Video editor mode for the active tab (04 §1). Per-tab — swapped
    /// with the parked tab's `DocTab::mode` in `switch_tab`, mirroring
    /// `selected_id`.
    pub mode: AppMode,
    /// Timeline zoom/scroll for the active tab (04 §2.1/§6). Per-tab, same
    /// swap discipline as `mode`.
    pub timeline_view: TimelineView,
    /// Playhead position for the active tab's sequence (04 §6). Per-tab.
    pub playhead: Tick,
    /// Selected clip ids in the timeline panel for the active tab (04 §2.6/§6).
    /// Per-tab.
    pub timeline_selection: Vec<ClipId>,
    /// Timeline magnet/snap toggle (04 §2.5). NOT per-tab — persisted to
    /// `AppPreferences` like other UI toggles (`prefs.save()` pattern), not to
    /// the document (04 §6).
    pub timeline_snap_enabled: bool,

    // ── 3/4-point editing session state (spec 16 §1) ────────────────────────
    // Insert/Overwrite/Lift/Extract support model. Session-only (this struct is
    // not serde) and deliberately NOT in the document — the armed source and the
    // patch targets are editor state, not sequence content.
    /// The armed 3-point source (source op + trim in/out) for Insert/Overwrite.
    /// `None` = nothing armed; set from source marks (G-10) or selected clip
    /// (spec 16 §4 minimal arming) at edit time.
    pub(crate) pending_source: Option<timeline::interact::PendingSource>,
    /// G-10 source marks + armed asset (session-only, non-undoable; 24 §3.3).
    pub(crate) source_marks: source_marks::SourceMarksSession,
    /// Source-patch target (spec 16 §1 M-3): which track receives the edit.
    /// `None` = default to the first enabled track of the source's kind
    /// (`interact::resolve_target_track`).
    pub(crate) target_video_track: Option<TrackId>,
    pub(crate) target_audio_track: Option<TrackId>,
    /// Razor/blade tool armed (spec 16 §4 M-4): a lane click splits the clip at
    /// the click point. The lane-click handler lives in the timeline panel; this
    /// bit (toggled by `C`) arms it.
    pub(crate) timeline_razor_active: bool,

    // ── Program monitor / transport (04 §3.2) ───────────────────────────────
    // Session-only placeholder playback state — there is no engine until P3
    // lands, so "playing" is simulated by advancing `playhead` from wall-clock
    // dt each frame (`advance_monitor_playback`, `app/monitor.rs`) purely so
    // the transport/scrub UX is testable pre-engine. Real playback replaces
    // this with `EngineCmd`/`EngineStatus` (02 §1) without changing the public
    // transport methods' call sites (command palette, keyboard, buttons).
    /// True while the placeholder transport is "playing".
    pub(crate) monitor_playing: bool,
    /// Playback direction while `monitor_playing` (J = reverse, L = forward).
    pub(crate) monitor_play_reverse: bool,
    /// J/L repeat-press speed multiplier (reference-NLE convention, 04 §3.2).
    pub(crate) monitor_play_speed: f64,
    /// Timeline loop-playback toggle (04 §3.2 transport bar).
    pub(crate) monitor_loop_enabled: bool,
    /// Safe-area guide overlay toggle (04 §3.3).
    pub(crate) monitor_safe_area: bool,
    /// K-E3 composition grid: 0=off, 1=thirds, 2=golden ratio (cycled from toolbar).
    pub(crate) monitor_comp_grid: u8,
    /// Transform tool toggle: show the in-monitor reframe/transform handles for
    /// the selected clip only when on (off by default — the handles are opt-in,
    /// not always drawn over the picture).
    pub(crate) monitor_transform_tool: bool,
    /// One-time keyboard-shortcut overlay (Space/JKL/I/O/S) visible — opened
    /// automatically on first video-mode entry and re-openable via `?` (04
    /// §1.2 First-run hint). Session state; the "has this been shown before"
    /// bit is `prefs.video_shortcuts_intro_shown` (persisted).
    pub(crate) show_video_shortcut_sheet: bool,
    /// This session may still show the first-run video coach card. Latched from
    /// `!prefs.video_coach_shown_once` at startup so the guide is a *first-run*
    /// guide: once it has appeared on one launch it never returns, whether or
    /// not the user pressed Next/Skip before quitting. The "Reset video coach
    /// marks" preference re-arms both this and the persisted bit.
    pub(crate) coach_allowed_this_session: bool,
    /// First frame's CLI/double-click auto-enter check (04 §1.2 "Auto-enter on
    /// open") has run. Guards `ensure_initial_tab`'s one-shot sibling check so
    /// it only inspects `doc.timeline` once, on the very first `draw` call.
    pub(crate) initial_mode_checked: bool,
    /// Live video-engine session (02 §1), attached by the host after the
    /// shared wgpu device exists. `None` = engine-less host (tests, no GPU):
    /// the monitor/transport fall back to the placeholder wall-clock paths.
    pub engine: Option<engine::EngineBridge>,
    /// K-F1 multi-job export queue (multi-format + marker multi-export).
    pub(crate) render_queue: photonic_video::export::RenderQueue,
    /// Whether the render-queue inspector panel is open (K-F1).
    pub(crate) render_queue_panel_open: bool,
    /// Media pool drawer state + background import channel (05 §2).
    pub(crate) media_pool_ui: panels::media_pool::MediaPoolUi,
    /// Session clip-thumbnail + waveform caches feeding the timeline lane
    /// painter (spec 15 — NLE parity gap 10). `None` until the first video-mode
    /// paint; `draw_timeline_panel` constructs it lazily and refreshes it each
    /// frame from the active document's media pool. Session-only (it owns the
    /// caches' background worker threads and is never serialized).
    pub(crate) timeline_media: Option<timeline::TimelineMediaCaches>,

    // ── Video-mode panel session state (04 §4.1) ────────────────────────────
    // Storage the six video panel stories read/mutate through
    // `panels::VideoPanelUi` (built by `video_panel_ui`), so panel builders
    // never touch this struct. Session-only (this struct is not serde; anything
    // that must persist belongs in `AppPreferences`). Each field names its
    // owning panel/spec.
    /// [color_page, 07 §1] Selected grade op in the clip's grade stack.
    pub(crate) selected_grade_op: Option<GradeOpId>,
    /// [color_page, 07 §6] Active Color Controls sub-tab.
    pub(crate) color_page_tab: ColorPageTab,
    /// [color_page, 07 §6] Floating scopes panel visibility.
    pub(crate) scopes_panel_open: bool,
    /// [color_page, 07 §6] Which scope the floating panel shows.
    pub(crate) scope_kind: ScopeKind,
    /// [node_editor, 08 §6.1] Graph open in the central node canvas, if any.
    pub(crate) open_graph: Option<GraphId>,
    /// [node_editor, 08 §6.1] Selected node in the open graph (drives the
    /// left-rail inspector).
    pub(crate) selected_graph_node: Option<GraphNodeId>,
    /// [node_editor, 08 §6.1] Whether the central panel shows the node canvas
    /// instead of the program monitor.
    pub(crate) node_canvas_active: bool,
    /// [audio_mixer, 09] Track strips whose EQ/comp/automation are expanded.
    pub(crate) mixer_expanded_tracks: std::collections::HashSet<TrackId>,
    /// [audio_mixer, 09] The mixer is a floating window, not a right-rail
    /// drawer. A channel strip is 88 px wide and its fader column alone is
    /// ~140 px tall, so a rack of them plus the master bus never fitted the
    /// 220–480 px drawer — every strip past the first was clipped. The rail
    /// button toggles this instead.
    pub(crate) audio_mixer_window_open: bool,
    /// [clip_inspector, 04 §4.1 / 01 §6] Clip whose animated props the keyframe
    /// editor targets.
    pub(crate) keyframe_editor_target: Option<ClipId>,
    /// [caption_editor, 06] Caption cue currently being edited.
    pub(crate) caption_edit_cue: Option<CueId>,
    /// [export_dialog, 05 §3] Whether the video export dialog is open.
    pub(crate) export_dialog_open: bool,
    /// [export_dialog, 05 §3] Name of the last-used export preset, seed for the
    /// dialog. Session-only for now; the export story persists it to prefs.
    pub(crate) last_export_preset: String,
    /// [duration_dialog, 26 K-A6] Open Edit Duration session (position/in/out/
    /// duration + ripple). `None` = closed.
    pub(crate) edit_duration_dialog:
        Option<crate::panels::video::duration_dialog::EditDurationDialog>,
    /// [timeline K-A7] Keyboard grab session — arrow keys preview a move; Enter
    /// commits one undo unit, Esc cancels. `None` = not grabbing.
    pub(crate) timeline_grab: Option<crate::app::timeline::interact::GrabSession>,

    // ── 17-nle-parity-round2.md choke-point session state ───────────────────
    // Session fields for the round-2 deferred features (source monitor,
    // multicam, transcript, the modal timeline-tool palette, sequence tabs,
    // nested-sequence breadcrumbs). Same discipline as the block above:
    // session-only (not serde), each field tagged with its owning story. The
    // four panel-facing fields are threaded through `VideoPanelUi` alongside
    // the P1 fields above (see `video_panel_ui()`); `timeline_tool` is read
    // directly off `self` by the (out-of-territory) timeline-panel story, so
    // it stays `#[allow(dead_code)]` here until that story lands.
    /// [source_monitor, 17 G-10] Scrub-bar playhead within the armed
    /// source's own media. The armed source and its in/out trim marks
    /// already live on `pending_source` above (spec 16 §1).
    pub(crate) source_monitor_scrub: Option<Tick>,
    /// [multicam, 17 G-20] Angle currently cut to in the open multicam clip.
    pub(crate) multicam_active_angle: Option<u8>,
    /// [multicam, 17 G-20] Whether the central panel shows the multicam
    /// angle grid instead of the program monitor.
    pub(crate) multicam_view_open: bool,
    /// [transcript, 17 G-18] Whether the transcript editing panel is open.
    pub(crate) transcript_panel_open: bool,
    /// [transcript, 17 G-18] Scroll offset (px) of the transcript word list.
    pub(crate) transcript_scroll: f32,
    /// Document-scoped transcript action waiting for the drawer's egui context.
    pub(crate) pending_transcript_command: Option<(
        uuid::Uuid,
        SequenceId,
        panels::video::transcript::TranscriptCommand,
    )>,
    /// [timeline-panel, 17 G-13] Armed modal timeline tool (see
    /// `TimelineTool`'s doc comment). Read directly off `self` by
    /// `app/timeline/mod.rs`'s mini-toolbar once that story lands.
    #[allow(dead_code)]
    pub(crate) timeline_tool: TimelineTool,
    /// [seq_tabs, 17 G-17] Sequence ids pinned open as tabs, in display
    /// order. `TimelineProject::active_sequence` (document state) decides
    /// which tab is highlighted. Flat like `pending_source`/
    /// `target_video_track` above rather than per-`DocTab` — per-document-tab
    /// swap-on-switch is future work in `tabs.rs`, out of this skeleton.
    pub(crate) open_sequence_tabs: Vec<SequenceId>,
    /// [seq_tabs, 17 G-16/G-17] Breadcrumb stack of sequence ids drilled
    /// into via nested-sequence navigation — empty at the top level.
    pub(crate) nested_sequence_breadcrumbs: Vec<SequenceId>,
    pub(crate) precision_trim: Option<editing_workflows::PrecisionTrimSession>,
    pub(crate) sequence_views:
        std::collections::HashMap<(uuid::Uuid, SequenceId), editing_workflows::SequenceViewState>,
    pub(crate) view_sequence: Option<(uuid::Uuid, SequenceId)>,

    /// Canvas-space position where the current drag began (shape creation).
    drag_start_canvas: Option<(f64, f64)>,

    /// Accumulated anchor points for the in-progress pen path (canvas space).
    pen_points: Vec<(f64, f64)>,

    /// Whether we are currently dragging a selected node to move it.
    moving: bool,
    /// Snapshots of the selected nodes captured at the start of a move drag.
    /// Used to record a single undoable UpdateNode batch on release. Empty
    /// until the first move frame actually shifts the selection.
    move_drag_origins: Vec<SceneNode>,
    /// Original translations (id, tx, ty) of the selection captured at move
    /// start, so the move can be applied absolutely and snapped to the grid.
    move_snap_origins: Vec<(NodeId, f64, f64)>,
    /// Selection bounding-box top-left at move start — the point snapped to the
    /// grid as the selection is dragged.
    move_snap_ref: Option<(f64, f64)>,
    /// Full selection bounding box `(x0, y0, x1, y1)` at move start, in canvas
    /// space — used as the basis for object-aware snapping (#66).
    move_snap_bbox: Option<(f64, f64, f64, f64)>,
    /// Active smart-guide snap from the current move drag, or `None`. Set each
    /// frame the dragged selection aligns to a nearby node; cleared on release.
    /// The paint pass reads this to draw guide lines + distance labels.
    last_snap_result: Option<crate::snap::SnapResult>,
    /// Canvas-space cursor position where the move drag began.
    move_snap_press: Option<(f64, f64)>,
    /// True when the current move drag is duplicating the selection (Alt-drag):
    /// copies were spawned at drag start and the originals stay put.
    dup_drag: bool,
    /// Dragging an artboard (moves the board + its artwork).
    artboard_drag: Option<ArtboardDrag>,
    /// Resizing an artboard: (id, corner 0=TL/1=TR/2=BL/3=BR, orig x, y, w, h).
    artboard_resize: Option<(photonic_core::ArtboardId, u8, f64, f64, f64, f64)>,
    /// Inline-renaming an artboard: (id, edit buffer).
    artboard_rename: Option<(photonic_core::ArtboardId, String)>,
    /// Request focus for the rename field on the next frame it draws.
    artboard_rename_focus: bool,
    /// Set on new/open to fit all artboards to the viewport on the next frame
    /// (once the actual viewport rect is known).
    fit_pending: bool,
    /// Artboard list snapshot taken at the start of a move/resize/rename/add/
    /// remove, so the change is recorded as one undoable SetArtboards step.
    artboard_pre: Option<Vec<photonic_core::Artboard>>,
    /// Global search (command palette) query string.
    global_search: String,
    /// On-device semantic index for the global search (background embedder).
    semantic: crate::global_search::SemanticIndex,
    /// Ctrl/Cmd+K command palette (#140): open state, query, and selection.
    command_palette_open: bool,
    command_palette_query: String,
    command_palette_sel: usize,
    /// Request focus for the palette input on the frame it opens.
    command_palette_focus: bool,
    /// MCP tool selected from the palette. The host drains this only after the
    /// egui closure releases its document lock.
    mcp_operation_request: Option<String>,
    /// Command id currently capturing a new key in the Keyboard Shortcuts page.
    shortcut_capture: Option<String>,
    /// In-flight self-update check (result polled each frame).
    update_rx: Option<std::sync::mpsc::Receiver<crate::update::UpdateStatus>>,
    /// In-flight launch-time "is a newer release available?" check (no download).
    update_check_rx: Option<std::sync::mpsc::Receiver<crate::update::UpdateCheck>>,
    /// Newer version found by the launch check; drives the update prompt banner.
    update_available: Option<String>,
    /// Whether the once-per-launch update check has been kicked off yet.
    update_checked_startup: bool,
    /// Release notes to show in the "What's New" popup (versions newly skipped).
    whats_new_notes: Vec<crate::release_notes::ReleaseNote>,
    /// Whether the "What's New" popup is currently open.
    show_whats_new: bool,
    /// Whether the once-per-launch "did the version change?" check has run.
    whats_new_checked: bool,

    // ── Crash reporting (#59) ─────────────────────────────────────────────────
    /// Pending local crash report files found on launch (oldest first). Drives
    /// the one-time consent dialog / report banner. Empty = nothing to offer.
    pending_crash_reports: Vec<std::path::PathBuf>,
    /// Whether the once-per-launch scan for pending crash reports has run.
    crash_reports_scanned: bool,

    /// Which corner handle is being dragged (None = not resizing).
    resizing: Option<ResizeHandle>,
    /// Canvas-space bounding box captured at the start of a resize drag.
    resize_origin_bounds: Option<(f64, f64, f64, f64)>,
    /// Node transform matrix captured at the start of a resize drag.
    resize_origin_transform: Option<[f64; 6]>,
    /// Font size captured at resize-drag start (TextNode only).
    resize_origin_font_size: Option<f64>,

    /// Transforms of all selected nodes captured at the start of a multi-node resize.
    resize_multi_origins: Vec<(NodeId, [f64; 6])>,
    /// Full snapshots of the nodes captured at the start of a resize drag, used
    /// to record a single undoable UpdateNode batch on release. Rotation reuses
    /// this (release records any transform change as one undo step).
    resize_drag_origins: Vec<SceneNode>,

    /// True while dragging in a corner rotation zone (rotate-in-place).
    rotating: bool,
    /// Canvas-space pivot (selection-bbox centre) for the active rotation.
    rotate_pivot: (f64, f64),
    /// Pointer angle (radians, atan2 about the pivot) at rotation-drag start.
    rotate_start_angle: f64,
    /// Each selected node's transform matrix captured at rotation-drag start,
    /// so the drag applies an absolute rotation (pivot ∘ orig) every frame.
    rotate_origins: Vec<(NodeId, [f64; 6])>,

    /// Screen-space position where a marquee (drag-select) began; None when inactive.
    marquee_start: Option<egui::Pos2>,

    // ── Direct Selection (point edit) tool state ─────────────────────────────
    /// The node whose anchor points are currently being edited.
    point_edit_node: Option<NodeId>,
    /// Indices into the BezPath element array that are currently selected.
    point_selected: Vec<usize>,
    /// The anchor element index most recently right-clicked in Direct Select,
    /// i.e. the target of the anchor context menu. `None` when no anchor menu is
    /// active (right-click missed all anchors). Persisted across frames because
    /// egui keeps the context menu open and re-runs its closure each frame.
    point_context_anchor: Option<usize>,
    /// Snapshot of the node captured at drag-start (None when not dragging).
    /// Used to build the UpdateNode undo command on drag release.
    point_drag_origin: Option<SceneNode>,
    /// What the current Direct Selection drag is manipulating (None = anchors
    /// or no active drag). Set on drag-start, cleared on release.
    point_drag_mode: Option<DirectDrag>,
    /// Screen-space position where a Direct Select anchor marquee (rubber-band
    /// vertex select) began; `None` when no marquee is in progress. Tracked
    /// separately from `point_drag_mode` because a marquee changes only the
    /// anchor selection, not geometry (no undo). #181.
    point_marquee_start: Option<egui::Pos2>,

    // ── Proportional Move (Direct Select sub-variant) state ──────────────────
    /// Falloff radius in local path units — the "spread" scale. Persisted default,
    /// adjusted live by scroll while dragging an anchor.
    pub prop_spread: f64,
    /// Falloff curve exponent — the "curve" scale (1 = linear, higher = sharper).
    /// Adjusted live by Shift+scroll while dragging an anchor.
    pub prop_falloff_k: f64,

    // ── Shape Builder tool state ──────────────────────────────────────────────
    /// Node under cursor in Shape Builder mode (for highlight preview).
    shape_builder_hovered: Option<NodeId>,
    /// Nodes touched during the current Shape Builder drag (in touch order).
    shape_builder_drag_ids: Vec<NodeId>,
    /// True when Alt was held at the start of the current drag (subtract mode).
    shape_builder_subtract_mode: bool,

    // ── Console / REPL ────────────────────────────────────────────────────────
    pub lua_console: LuaConsoleState,

    /// Actions queued by panel widgets (z-order, boolean ops) to be processed
    /// after all panels have finished drawing, with access to doc + history.
    pub pending_panel_actions: Vec<PanelAction>,
    /// Color chosen via the Fill/Stroke picker this interaction, recorded into
    /// `recent_colors` only once the pointer is released (#171) — avoids
    /// streaming the whole drag path into the Recent swatch list.
    pending_recent_color: Option<Color>,
    /// Set when the persistent rail fill swatch (#172) mutates the active
    /// color, so the full `prefs.save()` disk write is deferred to the frame
    /// the picker interaction settles (pointer released) instead of firing on
    /// every dragged slider frame — same commit-on-release discipline as
    /// `pending_recent_color` (#171).
    fill_swatch_dirty: bool,
    /// Cached adaptive-hotbar ordering for the current context (see
    /// [`HotbarCacheState`]). `None` until first built.
    hotbar_cache: Option<HotbarCacheState>,
    /// Canvas viewport rect captured this frame — used to recenter the view
    /// when the Navigator emits a `CenterViewOn` action.
    last_canvas_rect: Option<egui::Rect>,
    /// [`last_canvas_rect`](Self::last_canvas_rect) in physical pixels, captured
    /// with the `pixels_per_point` in force when it was laid out. Read by the
    /// host through [`canvas_viewport_px`](Self::canvas_viewport_px).
    last_canvas_rect_px: Option<(u32, u32, u32, u32)>,
    /// egui time (seconds) of the last throttled history size-cap check, so
    /// size-mode enforcement runs ~every 1.5 s instead of every frame.
    last_history_size_check: f64,
    /// Latch so the proactive "history approaching size limit" warning (#197)
    /// fires once per breach of the soft threshold, re-arming when pressure drops.
    history_pressure_warned: bool,
    /// Throttled cache for the History settings readout: (egui time, bytes).
    /// `history_byte_size()` serializes the whole history, so the readout reuses
    /// this for ~0.5 s rather than recomputing on every repaint.
    cached_history_bytes: (f64, u64),

    // ── Claude chat ───────────────────────────────────────────────────────────
    pub claude_chat: ClaudeChatState,

    // ── File I/O ──────────────────────────────────────────────────────────────
    /// Path of the currently open .photon file (None = unsaved new document).
    pub current_file: Option<std::path::PathBuf>,
    /// One-shot status message shown in the toolbar after save/load.
    file_status: Option<String>,
    /// Export settings modal — Some while open.
    export_dialog: Option<ExportDialog>,
    /// Simplify Path dialog — Some while open.
    simplify_dialog: Option<SimplifyDialog>,
    /// Merge Vertices by Distance dialog — Some while open.
    merge_vertices_dialog: Option<MergeVerticesDialog>,
    /// Find / Replace Text dialog — Some while open.
    find_replace_text_dialog: Option<FindReplaceTextDialog>,
    /// Options modal for a Layers-tab row (layer or object), type-scoped — Some while open.
    object_options_dialog: Option<ObjectOptionsDialog>,

    // ── Multi-document tabs ───────────────────────────────────────────────────
    /// All open documents, in tab-bar order. The ACTIVE tab's live engine state
    /// (Document / CommandHistory / CanvasView) lives in the shared `Arc<Mutex>`s
    /// and `self.current_file` / `self.selected_id`; its `DocTab` engine fields
    /// are stale placeholders while active (only `title`/`dirty`/`last_saved_node`
    /// are kept current for it each frame). Inactive tabs are fully parked here.
    /// Switching is a `mem::swap` between the `&mut` params and a parked tab —
    /// the renderer/MCP/REPL keep reading the same shared Arc. Empty only briefly
    /// before the first document is installed.
    pub tabs: Vec<DocTab>,
    /// Index into `tabs` of the active document.
    pub active_tab: usize,
    /// Set by tab UI / handlers to request a switch to this index next; applied
    /// inside `draw` where the `&mut doc/history/view` params are available.
    pending_tab_switch: Option<usize>,
    /// Set by tab UI to request closing this index; applied inside `draw`.
    pending_tab_close: Option<usize>,
    /// Monotonic counter for naming fresh "Untitled N" documents.
    untitled_counter: usize,

    // ── Autosave & close guards ───────────────────────────────────────────────
    /// Frame-clock time (`ctx.input().time`) of the last autosave pass.
    last_autosave: Option<f64>,
    /// The app is trying to quit: show the unsaved-changes-on-quit modal.
    pub close_requested: bool,
    /// Resolved: the host may now exit the event loop (polled in `main.rs`).
    pub close_confirmed: bool,
    /// Pending close of a single dirty tab awaiting the Save/Discard/Cancel modal.
    close_tab_prompt: Option<usize>,
    /// Untitled-document autosaves found in the recovery folder at launch, awaiting
    /// the restore/discard prompt. Empty once handled.
    recovery_candidates: Vec<std::path::PathBuf>,
    /// Whether the recovery folder has been scanned this session (one-shot).
    recovery_scanned: bool,

    // ── Welcome screen ────────────────────────────────────────────────────────
    /// Show the welcome/new-document screen instead of the editor.
    pub show_welcome: bool,
    /// State for the welcome screen (form fields + recent docs list).
    pub welcome: crate::welcome::WelcomeState,
    /// In-editor File ▸ New modal — Some while open, holding the shared
    /// new-document form (same flow as the welcome screen's New Canvas panel).
    new_document_modal: Option<crate::welcome::NewDocumentForm>,

    // ── Smooth viewport animation ─────────────────────────────────────────────
    smooth: SmoothViewState,

    // ── Preferences ───────────────────────────────────────────────────────────
    pub prefs: AppPreferences,
    /// Which top-bar drawer is open, if any.
    pub active_drawer: Option<DrawerKind>,
    /// Height of the open File/Edit/Tools menu drawer this frame, 0 when none is
    /// open. That drawer is a full-width foreground strip anchored under the top
    /// toolbar, so it lands squarely on top of the left rail and left drawer —
    /// its own option list ends up stacked over the rail's icons. The left
    /// panels step down by this much while it is open.
    pub(crate) menu_drawer_height: f32,
    /// Which option is selected in the currently open drawer (index into the options list).
    /// Resets to None whenever active_drawer changes.
    selected_drawer_option: Option<usize>,

    // ── Radial wheel ──────────────────────────────────────────────────────────
    /// Open right-click selection wheel, or None when closed.
    radial_wheel: Option<WheelState>,

    /// Floating fill/stroke color picker raised from the radial menu, or None.
    color_popup: Option<ColorPopupState>,

    /// The gradient control handle currently being dragged on the canvas.
    gradient_drag: Option<gradient_handles::GradHandle>,

    // ── Audit panel ───────────────────────────────────────────────────────────
    pub audit: AuditPanelState,

    // ── Diff highlight overlay ────────────────────────────────────────────────
    pub diff: DiffOverlayState,

    // ── View preview modes ───────────────────────────────────────────────────
    /// When true, the canvas shows path wireframes only (no fills or strokes).
    /// Mutually exclusive with `pixel_preview` and `overprint_preview`.
    pub outline_mode: bool,
    /// When true, the active artboard is overlaid with a nearest-sampled render
    /// at its export pixel size so true aliasing/pixel snapping is visible (#22).
    /// Mutually exclusive with the other view modes.
    pub pixel_preview: bool,
    /// When true, overprint-flagged spot inks composite with Multiply in a
    /// nearest-sampled export render overlaid on the active artboard (#22).
    /// Mutually exclusive with the other view modes.
    pub overprint_preview: bool,
    /// Cached preview-overlay texture + the content/mode/size hash it was built
    /// from. Re-rendered only when the hash changes.
    preview_tex_cache: Option<PreviewTexCache>,
    /// Lazily-created headless renderer powering the preview overlay so the GUI
    /// reuses one GPU device instead of spinning one up every frame.
    preview_renderer: Option<photonic_render::HeadlessRenderer>,

    /// Cache of uploaded egui textures for raster nodes, keyed by node id.
    /// Re-uploaded only when the pixel/mask content hash changes.
    raster_tex_cache: std::collections::HashMap<photonic_core::node::NodeId, RasterTexCache>,

    /// Lazily-loaded Photonic logo texture for the top toolbar (embedded PNG).
    logo_texture: Option<egui::TextureHandle>,

    // ── Interactive raster brush state ─────────────────────────────────────────
    /// Brush radius (pixels) for the RasterBrush/RasterEraser tools.
    pub raster_brush_radius: f32,
    /// Brush edge hardness (0 soft .. 1 hard).
    pub raster_brush_hardness: f32,
    /// Pre-stroke snapshot of the node being painted, for a single undo step.
    raster_stroke_orig: Option<(photonic_core::node::NodeId, photonic_core::node::SceneNode)>,
    /// Local-space points accumulated during the current drag.
    raster_stroke_pts: Vec<(f32, f32)>,

    // ── Raster masking (color range / remove background) ──────────────────────
    /// Fuzziness (0..1) for the color-range / magic-wand mask-out.
    pub raster_mask_tolerance: f32,
    /// Contiguous (magic-wand flood from the click) vs global color range.
    pub raster_mask_contiguous: bool,
    /// Live color-range session; the doc holds its preview while `Some`.
    pub raster_color_range: Option<RasterColorRangeSession>,
    /// In-flight background-removal job (worker thread → matte result).
    rmbg_rx: Option<std::sync::mpsc::Receiver<Result<photonic_core::Mask, String>>>,
    /// Node targeted by the in-flight background-removal job.
    rmbg_node: Option<NodeId>,
    /// Whether the matting model is on disk (checked once at startup, set true
    /// after a successful run) — drives the first-use download hint.
    rmbg_model_cached: bool,

    // ── Guides ────────────────────────────────────────────────────────────────
    /// When true, ruler guides are rendered on the canvas (toggle with Ctrl+;).
    pub guides_visible: bool,
    /// Active drag originating from a ruler strip to create a new guide.
    /// `Horizontal` = dragged out of the top ruler; `Vertical` = left ruler.
    /// `None` when no ruler-create drag is in progress.
    pub ruler_drag: Option<photonic_core::GuideOrientation>,
    /// Live canvas-space position of the guide being created from a ruler drag
    /// (Y for horizontal, X for vertical). Used for the floating drag label.
    pub ruler_drag_pos: f64,
    /// Index into `doc.guides` of the guide currently being moved by a drag.
    /// `None` when no existing guide is being dragged.
    pub guide_dragging: Option<usize>,
    /// Snapshot of `doc.guides` captured at the start of a guide move/create
    /// drag, used as the `old` state for the undoable `Command::SetGuides`.
    pub guide_drag_old: Option<Vec<photonic_core::Guide>>,
    /// Open exact-position editor popup for a guide (double-click to open).
    pub(crate) guide_edit_popup: Option<GuideEditPopup>,

    // ── Isolation Mode ───────────────────────────────────────────────────────
    /// When set, only children of this group are selectable/editable.
    /// None = normal editing mode.
    pub isolated_group: Option<NodeId>,

    // ── Pencil tool state ────────────────────────────────────────────────────
    /// Canvas-space points collected during an active pencil drag.
    pencil_points: Vec<(f64, f64)>,

    // ── Lasso tool state ─────────────────────────────────────────────────────
    /// Screen-space points collected during an active lasso drag.
    lasso_points: Vec<egui::Pos2>,

    // ── Knife / Eraser (destructive path edit) tool state ─────────────────────
    /// Canvas-space points collected during an active Eraser drag.
    eraser_points: Vec<(f64, f64)>,
    /// Eraser head radius in canvas units (scales with zoom). Default ~10px.
    pub eraser_radius: f64,
    /// Canvas-space points collected during an active Knife drag.
    knife_points: Vec<(f64, f64)>,

    // ── Magic Wand tool options ───────────────────────────────────────────────
    /// Which attribute the Magic Wand matches when clicked.
    pub magic_wand_attribute: SelectSameAttr,
    /// Tolerance for the Magic Wand tool (color/numeric difference threshold).
    pub magic_wand_tolerance: f64,

    // ── GUI Clipboard ─────────────────────────────────────────────────────────
    /// Objects copied with Ctrl+C, stored in-process for Ctrl+V / Ctrl+Shift+V.
    /// Captures each selected object as a full subtree (a group's descendants
    /// too), so a group of paths or images pastes intact — within the document
    /// and across open documents (the snapshot is detached from the source doc).
    pub gui_clipboard: GuiClipboard,
    /// Clipboard content captured by the native window host for the next GUI
    /// frame. This is separate from `gui_clipboard` because egui only exposes
    /// text clipboard events and cannot carry image pixels.
    pending_native_clipboard_paste: Option<(NativeClipboardPaste, bool)>,

    /// Clips copied (Ctrl+C) / cut (Ctrl+X) in video mode, held in-process for
    /// Ctrl+V paste-at-playhead. Session-wide (not per-tab), mirroring
    /// `gui_clipboard`; each entry is a full clone of the clip (grade/effects/
    /// trim preserved) plus its source track kind. (NLE parity QW-3.)
    pub(crate) timeline_clipboard: Vec<command_center::ClipboardClip>,

    // ── Composition Analysis ──────────────────────────────────────────────────
    /// Latest findings from the composition analyzer (shown in the GUI panel).
    pub composition_findings: Vec<String>,
    /// Latest rhythm patterns from the rhythm detector (shown in the GUI panel).
    pub rhythm_findings: Vec<String>,

    // ── Named states ─────────────────────────────────────────────────────────
    /// Text input for naming the current history state (a labeled commit).
    pub branch_name_input: String,

    /// Selected swatch library name for the Color Swatches panel dropdown.
    pub swatch_library_selected: String,
    /// Text input for naming a new graphic style in the Graphic Styles panel.
    pub graphic_style_name_input: String,
    /// Text input for naming a new width profile in the Width Profiles panel.
    pub width_profile_name_input: String,
    // ── Width tool (interactive variable-width stroke editing) ──────────────
    /// Path node the Width-tool cursor is currently hovering, if any.
    pub width_tool_hovered_node: Option<NodeId>,
    /// Normalized arc-length position `[0, 1]` on the hovered path under the cursor.
    pub width_tool_hovered_t: f64,
    /// Index (into the active profile's samples) of the width handle being edited.
    pub width_tool_selected_point: Option<usize>,
    /// Which side handle is being dragged: `true` = right/bottom, `false` = left/top.
    pub width_tool_drag_right: bool,
    /// Canvas-space `y` recorded when a width-handle drag began (for delta math).
    pub width_tool_drag_origin_y: Option<f64>,
    /// Snapshot of `doc.width_profiles` taken at drag start, for a single undo step.
    pub width_tool_profiles_before: Option<Vec<photonic_core::WidthProfile>>,
    /// Text input for naming the profile saved from the Width tool.
    pub width_tool_save_name: String,
    /// Cached grammar rule list: (name, rule_type).
    pub grammar_rules: Vec<(String, String)>,
    /// Text input for the new grammar rule name.
    pub grammar_rule_name_input: String,
    /// Selected rule type for the grammar rule form.
    pub grammar_rule_type_selected: String,
    /// JSON params text for the grammar rule form.
    pub grammar_rule_params_input: String,
    /// Latest grammar check results: (rule_name, passed, message).
    pub grammar_check_results: Vec<(String, bool, String)>,
    /// Latest distance measurements: (from_name, to_name, h_gap, v_gap, center_dist).
    pub distance_results: Vec<(String, String, f64, f64, f64)>,
    /// Cached action set names: (name, step_count).
    pub action_names: Vec<(String, usize)>,
    /// Cached edit-tree topology for the branching commit graph (newest first).
    pub history_graph: Vec<HistoryGraphNode>,
    /// HEAD node id in the edit tree, cached alongside `history_graph`.
    pub history_current: u64,
    /// Shared flag the MCP modal sets to ask the host (winit app) to re-spawn
    /// the MCP server thread after it has failed (#170). `None` until wired up by
    /// the host after construction.
    pub mcp_restart_requested: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Whether the MCP server status/restart modal is open (#170).
    pub show_mcp_modal: bool,
    /// Document-tab import/export settings (#176).
    pub doc_export: DocExportSettings,
    /// Bleed input (mm) for print settings panel.
    pub bleed_mm_input: f64,
    /// Slug input (mm) for print settings panel.
    pub slug_mm_input: f64,
    /// Construction line angle input (degrees).
    pub construction_angle: f64,
    /// Construction line origin X.
    pub construction_x: f64,
    /// Construction line origin Y.
    pub construction_y: f64,
    /// Artboard margin top input (document units).
    pub margin_top_input: f64,
    /// Artboard margin right input (document units).
    pub margin_right_input: f64,
    /// Artboard margin bottom input (document units).
    pub margin_bottom_input: f64,
    /// Artboard margin left input (document units).
    pub margin_left_input: f64,
    /// Selected event name for event trigger panel.
    pub event_trigger_event: String,
    /// Selected action name for event trigger panel.
    pub event_trigger_action: String,
    /// Input field for workspace name in the workspaces panel.
    pub workspace_name_input: String,

    // ── Properties panel ─────────────────────────────────────────────────────
    /// Which drawer group is currently open in the left rail, or `None` when the
    /// drawer is collapsed and only the rail shows. Mirrors `prefs.open_drawer`
    /// and is persisted there. Defaults to `Some(Inspector)`.
    pub open_drawer: Option<DrawerGroup>,
    /// Last group that was open, used to keep rendering the correct drawer
    /// content during the close (collapse) animation after `open_drawer` flips
    /// to `None`. Not persisted.
    pub last_drawer_group: DrawerGroup,
    /// Which group is open in the *right* rail, or `None` when the right drawer
    /// is collapsed and only the right rail shows. Mirrors `prefs.open_right_drawer`.
    pub open_right_drawer: Option<RightDrawerGroup>,
    /// Last open right group, kept for the right drawer's close animation (mirror
    /// of [`Self::last_drawer_group`]). Not persisted.
    pub last_right_drawer_group: RightDrawerGroup,
    /// Live search query that filters which property accordions are visible.
    pub prop_search: String,
    /// Recolor panel: comma-separated hex palette input.
    pub recolor_palette_input: String,

    // ── Eyedropper ────────────────────────────────────────────────────────────
    pub eyedropper: EyedropperState,
    /// Window top-left in logical screen coordinates (updated by main.rs each frame).
    pub window_logical_pos: (i32, i32),
    /// DPI scale factor of the main window (updated by main.rs each frame).
    pub window_scale_factor: f32,
}

/// Interpolation state for smooth zoom and WASD pan.
struct SmoothViewState {
    /// Target zoom in log-space; actual zoom lerps toward `exp(log_zoom_target)`.
    log_zoom_target: f64,
    /// Screen-space pivot used when lerping zoom (last scroll position).
    zoom_pivot: (f64, f64),
    /// Current pan velocity (px/s) applied by WASD keys.
    pan_vel_x: f64,
    pan_vel_y: f64,
}

impl Default for SmoothViewState {
    fn default() -> Self {
        Self {
            log_zoom_target: 0.0, // ln(1.0)
            zoom_pivot: (640.0, 400.0),
            pan_vel_x: 0.0,
            pan_vel_y: 0.0,
        }
    }
}

// ─── Two-column drawer helper ─────────────────────────────────────────────────

/// Renders a two-column menu: fixed-width nav on the left, content on the right.
/// Returns the (possibly updated) selected option index.
fn draw_two_column_menu(
    ui: &mut egui::Ui,
    left_col_width: f32,
    content_height: f32,
    options: &[&str],
    selected: Option<usize>,
    content: impl FnOnce(&mut egui::Ui, Option<usize>),
) -> Option<usize> {
    let mut new_selection = selected;
    let right_col_width = (ui.available_width() - left_col_width - 16.0).max(120.0);

    ui.horizontal(|ui| {
        // ── Left nav ─────────────────────────────────────────────────────
        ui.allocate_ui_with_layout(
            egui::vec2(left_col_width, content_height),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("drawer_nav_scroll")
                    .max_height(content_height)
                    .show(ui, |ui| {
                        ui.set_width(left_col_width); // prevents NaN from infinite available_width
                        for (i, label) in options.iter().enumerate() {
                            if ui
                                .selectable_label(new_selection == Some(i), *label)
                                .clicked()
                            {
                                new_selection = Some(i);
                            }
                        }
                    });
            },
        );

        ui.separator();

        // ── Right content ─────────────────────────────────────────────────
        ui.allocate_ui_with_layout(
            egui::vec2(right_col_width, content_height),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("drawer_content_scroll")
                    .max_height(content_height)
                    .show(ui, |ui| {
                        ui.set_min_width(right_col_width); // prevents collapse
                        content(ui, new_selection);
                    });
            },
        );
    });

    new_selection
}

impl Default for PhotonicApp {
    fn default() -> Self {
        Self {
            active_tool: Tool::Select,
            last_tool: Tool::Select,
            fill_color: [0.22, 0.47, 0.87, 1.0],
            polygon_sides: 6,
            star_points: 5,
            star_inner_ratio: 0.45,
            rounded_rect_radius: 10.0,
            spiral_turns: 3.0,
            spiral_inner_radius: 0.0,
            spiral_segs_per_turn: 16,
            shear_x: 0.0,
            shear_y: 0.0,
            line_snap_45: false,
            color_guide_rule: "complementary".to_string(),
            arc_start_angle: 0.0,
            arc_end_angle: 270.0,
            arc_open: false,
            grid_cols: 4,
            grid_rows: 4,
            polar_grid_rings: 4,
            polar_grid_sectors: 8,
            polar_grid_inner_ratio: 0.0,
            selected_layer_ids: Vec::new(),
            selected_id: None,
            mode: AppMode::default(),
            timeline_view: TimelineView::default(),
            playhead: Tick::default(),
            timeline_selection: Vec::new(),
            timeline_snap_enabled: true,
            pending_source: None,
            source_marks: source_marks::SourceMarksSession::default(),
            target_video_track: None,
            target_audio_track: None,
            timeline_razor_active: false,
            monitor_playing: false,
            monitor_play_reverse: false,
            monitor_play_speed: 1.0,
            monitor_loop_enabled: false,
            monitor_safe_area: false,
            monitor_comp_grid: 0,
            monitor_transform_tool: false,
            show_video_shortcut_sheet: false,
            coach_allowed_this_session: false,
            initial_mode_checked: false,
            engine: None,
            /// K-F1 shared multi-job export queue (marker multi-export / multi-format).
            render_queue: photonic_video::export::RenderQueue::new(),
            render_queue_panel_open: false,
            media_pool_ui: panels::media_pool::MediaPoolUi::default(),
            timeline_media: None,
            selected_grade_op: None,
            color_page_tab: ColorPageTab::default(),
            scopes_panel_open: false,
            scope_kind: ScopeKind::default(),
            open_graph: None,
            selected_graph_node: None,
            node_canvas_active: false,
            mixer_expanded_tracks: std::collections::HashSet::new(),
            audio_mixer_window_open: false,
            keyframe_editor_target: None,
            caption_edit_cue: None,
            export_dialog_open: false,
            last_export_preset: String::new(),
            edit_duration_dialog: None,
            timeline_grab: None,
            source_monitor_scrub: None,
            multicam_active_angle: None,
            multicam_view_open: false,
            transcript_panel_open: false,
            transcript_scroll: 0.0,
            pending_transcript_command: None,
            timeline_tool: TimelineTool::default(),
            open_sequence_tabs: Vec::new(),
            nested_sequence_breadcrumbs: Vec::new(),
            precision_trim: None,
            sequence_views: std::collections::HashMap::new(),
            view_sequence: None,
            drag_start_canvas: None,
            pen_points: Vec::new(),
            moving: false,
            move_drag_origins: Vec::new(),
            move_snap_origins: Vec::new(),
            move_snap_ref: None,
            move_snap_bbox: None,
            last_snap_result: None,
            move_snap_press: None,
            dup_drag: false,
            artboard_drag: None,
            artboard_resize: None,
            artboard_rename: None,
            artboard_rename_focus: false,
            fit_pending: false,
            artboard_pre: None,
            global_search: String::new(),
            semantic: crate::global_search::SemanticIndex::new(
                crate::global_search::items()
                    .iter()
                    .map(crate::global_search::corpus_text)
                    .collect(),
            ),
            command_palette_open: false,
            command_palette_query: String::new(),
            command_palette_sel: 0,
            command_palette_focus: false,
            mcp_operation_request: None,
            shortcut_capture: None,
            update_rx: None,
            update_check_rx: None,
            update_available: None,
            update_checked_startup: false,
            whats_new_notes: Vec::new(),
            show_whats_new: false,
            whats_new_checked: false,
            pending_crash_reports: Vec::new(),
            crash_reports_scanned: false,
            resizing: None,
            resize_origin_bounds: None,
            resize_origin_transform: None,
            resize_origin_font_size: None,
            resize_multi_origins: Vec::new(),
            resize_drag_origins: Vec::new(),
            rotating: false,
            rotate_pivot: (0.0, 0.0),
            rotate_start_angle: 0.0,
            rotate_origins: Vec::new(),
            marquee_start: None,
            point_edit_node: None,
            point_selected: Vec::new(),
            point_context_anchor: None,
            point_drag_origin: None,
            point_drag_mode: None,
            point_marquee_start: None,
            prop_spread: proportional_move::DEFAULT_SPREAD,
            prop_falloff_k: proportional_move::DEFAULT_CURVE,
            shape_builder_hovered: None,
            shape_builder_drag_ids: Vec::new(),
            shape_builder_subtract_mode: false,
            lua_console: LuaConsoleState {
                log: vec![(
                    false,
                    "Photonic Lua REPL — type `photonic` to see the API".into(),
                )],
                ..LuaConsoleState::default()
            },

            pending_panel_actions: Vec::new(),
            pending_recent_color: None,
            fill_swatch_dirty: false,
            hotbar_cache: None,
            last_canvas_rect: None,
            last_canvas_rect_px: None,
            last_history_size_check: 0.0,
            history_pressure_warned: false,
            cached_history_bytes: (f64::NEG_INFINITY, 0),

            claude_chat: ClaudeChatState::default(),

            current_file: None,
            file_status: None,
            export_dialog: None,
            simplify_dialog: None,
            merge_vertices_dialog: None,
            find_replace_text_dialog: None,
            object_options_dialog: None,
            tabs: Vec::new(),
            active_tab: 0,
            pending_tab_switch: None,
            pending_tab_close: None,
            untitled_counter: 0,
            last_autosave: None,
            close_requested: false,
            close_confirmed: false,
            close_tab_prompt: None,
            recovery_candidates: Vec::new(),
            recovery_scanned: false,
            smooth: SmoothViewState::default(),
            prefs: AppPreferences::default(),
            active_drawer: None,
            menu_drawer_height: 0.0,
            selected_drawer_option: None,

            show_welcome: false,
            welcome: crate::welcome::WelcomeState::new(),
            new_document_modal: None,

            radial_wheel: None,
            color_popup: None,
            gradient_drag: None,

            audit: AuditPanelState::default(),

            diff: DiffOverlayState::default(),

            composition_findings: Vec::new(),
            rhythm_findings: Vec::new(),
            branch_name_input: String::new(),
            swatch_library_selected: String::new(),
            graphic_style_name_input: String::new(),
            width_profile_name_input: String::new(),
            width_tool_hovered_node: None,
            width_tool_hovered_t: 0.0,
            width_tool_selected_point: None,
            width_tool_drag_right: false,
            width_tool_drag_origin_y: None,
            width_tool_profiles_before: None,
            width_tool_save_name: String::new(),
            grammar_rules: Vec::new(),
            grammar_rule_name_input: String::new(),
            grammar_rule_type_selected: String::new(),
            grammar_rule_params_input: String::new(),
            grammar_check_results: Vec::new(),
            distance_results: Vec::new(),
            action_names: Vec::new(),
            history_graph: Vec::new(),
            history_current: 0,
            mcp_restart_requested: None,
            show_mcp_modal: false,
            doc_export: DocExportSettings::default(),
            bleed_mm_input: 0.0,
            slug_mm_input: 0.0,
            construction_angle: 45.0,
            construction_x: 0.0,
            construction_y: 0.0,
            margin_top_input: 0.0,
            margin_right_input: 0.0,
            margin_bottom_input: 0.0,
            margin_left_input: 0.0,
            event_trigger_event: String::new(),
            event_trigger_action: String::new(),
            workspace_name_input: String::new(),

            open_drawer: Some(DrawerGroup::Tools),
            last_drawer_group: DrawerGroup::Tools,
            open_right_drawer: Some(RightDrawerGroup::Layers),
            last_right_drawer_group: RightDrawerGroup::Layers,
            prop_search: String::new(),
            recolor_palette_input: String::new(),

            eyedropper: EyedropperState::default(),
            logo_texture: None,
            raster_tex_cache: std::collections::HashMap::new(),
            raster_brush_radius: 16.0,
            raster_brush_hardness: 0.8,
            raster_stroke_orig: None,
            raster_stroke_pts: Vec::new(),
            raster_mask_tolerance: 0.25,
            raster_mask_contiguous: false,
            raster_color_range: None,
            rmbg_rx: None,
            rmbg_node: None,
            rmbg_model_cached: photonic_matte::model_is_cached(),
            window_logical_pos: (0, 0),
            window_scale_factor: 1.0,
            outline_mode: false,
            pixel_preview: false,
            overprint_preview: false,
            preview_tex_cache: None,
            preview_renderer: None,
            guides_visible: true,
            ruler_drag: None,
            ruler_drag_pos: 0.0,
            guide_dragging: None,
            guide_drag_old: None,
            guide_edit_popup: None,
            isolated_group: None,
            pencil_points: Vec::new(),
            lasso_points: Vec::new(),
            eraser_points: Vec::new(),
            eraser_radius: 10.0,
            knife_points: Vec::new(),
            magic_wand_attribute: SelectSameAttr::FillColor,
            magic_wand_tolerance: 0.05,
            gui_clipboard: GuiClipboard::default(),
            timeline_clipboard: Vec::new(),
            pending_native_clipboard_paste: None,
        }
    }
}

/// Load a document from disk, supporting `.photon` and `.svg` files.
/// Run a blocking `rfd` file dialog OFF the winit/Wayland event-loop thread.
///
/// `rfd`'s portal-backed dialogs (`pick_file`/`save_file`) internally
/// `pollster::block_on` an async XDG-desktop-portal call on the *calling*
/// thread. When that caller is the egui draw closure — which runs inside
/// winit's Wayland calloop event-loop callback — the portal's D-Bus events get
/// delivered back into winit's calloop re-entrantly (`calloop: Received an
/// event for non-existence source`) and the process aborts with SIGABRT
/// (`org.freedesktop.DBus.Error.UnknownMethod: Object does not exist at
/// .../request/...ashpd_...`). Spawning the dialog on a dedicated thread gives
/// the portal its own context and avoids the re-entrancy. The UI thread blocks
/// on `join()` while the dialog is open, which is the expected modal behaviour.
pub(crate) fn run_file_dialog<F>(f: F) -> Option<std::path::PathBuf>
where
    F: FnOnce() -> Option<std::path::PathBuf> + Send + 'static,
{
    std::thread::spawn(f).join().unwrap_or(None)
}

/// [`run_file_dialog`] for a multi-select picker.
///
/// Every native dialog **must** go through one of these two. `rfd` on Linux
/// talks to `xdg-desktop-portal` over D-Bus and blocks on the reply; called
/// straight from the render thread that reply never comes, because the portal
/// handshake needs the app to keep servicing its own event loop. The window
/// then stops responding and the desktop offers to force-quit it, which reads
/// as "importing media crashes the app" (and leaves a `SI_USER` SIGABRT core,
/// not a panic). Handing the call to a scratch thread keeps the portal
/// conversation off the thread that has to answer it.
pub(crate) fn run_file_dialog_multi<F>(f: F) -> Option<Vec<std::path::PathBuf>>
where
    F: FnOnce() -> Option<Vec<std::path::PathBuf>> + Send + 'static,
{
    std::thread::spawn(f).join().unwrap_or(None)
}

/// Image extensions accepted by Place Image / Open / drag-and-drop.
const IMAGE_EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "webp", "bmp", "gif", "tif", "tiff"];

/// Whether a path looks like an importable raster image (by extension).
fn is_image_path(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Fit and center the artboard bounding box `(bx0,by0,bx1,by1)` (canvas units)
/// inside the on-screen viewport `rect`, so the artwork lands properly scaled in
/// the actual visible canvas area (not the full window). Sets zoom + pan
/// directly in the same point-space the overlays and GPU camera share.
/// One result row in the global-search popup: icon + title + dim description,
/// full-width and clickable. `dim` styles semantic/related results more subtly.
fn search_result_row(
    ui: &mut egui::Ui,
    icon: &str,
    title: &str,
    description: &str,
    dim: bool,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let title_color = if dim {
        egui::Color32::from_gray(185)
    } else {
        egui::Color32::from_gray(235)
    };
    let desc = if description.chars().count() > 52 {
        format!("{}…", description.chars().take(51).collect::<String>())
    } else {
        description.to_string()
    };
    let p = ui.painter();
    p.text(
        egui::pos2(rect.left() + 11.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(15.0),
        title_color,
    );
    p.text(
        egui::pos2(rect.left() + 34.0, rect.top() + 7.0),
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::proportional(13.0),
        title_color,
    );
    p.text(
        egui::pos2(rect.left() + 34.0, rect.top() + 21.0),
        egui::Align2::LEFT_TOP,
        desc,
        egui::FontId::proportional(10.0),
        egui::Color32::from_gray(130),
    );
    resp.clicked()
}

/// Draw a horizontal distance measurement between two boards' facing edges
/// (left board's right edge → right board's left edge), with end ticks and a px
/// label, at their vertical-overlap midline. Rects are `(x, y, w, h)` in canvas.
fn draw_h_gap(
    p: &egui::Painter,
    view: &CanvasView,
    l: (f64, f64, f64, f64),
    r: (f64, f64, f64, f64),
    color: egui::Color32,
) {
    let lx = l.0 + l.2;
    let rx = r.0;
    if rx <= lx {
        return;
    }
    let ym = (l.1.max(r.1) + (l.1 + l.3).min(r.1 + r.3)) * 0.5;
    let (sx1, sy) = view.canvas_to_screen(lx, ym);
    let (sx2, _) = view.canvas_to_screen(rx, ym);
    let (sx1, sy, sx2) = (sx1 as f32, sy as f32, sx2 as f32);
    let stroke = egui::Stroke::new(1.0, color);
    p.line_segment([egui::pos2(sx1, sy), egui::pos2(sx2, sy)], stroke);
    p.line_segment(
        [egui::pos2(sx1, sy - 4.0), egui::pos2(sx1, sy + 4.0)],
        stroke,
    );
    p.line_segment(
        [egui::pos2(sx2, sy - 4.0), egui::pos2(sx2, sy + 4.0)],
        stroke,
    );
    gap_label(p, egui::pos2((sx1 + sx2) * 0.5, sy - 9.0), rx - lx, color);
}

/// Vertical distance measurement (top board's bottom → bottom board's top).
fn draw_v_gap(
    p: &egui::Painter,
    view: &CanvasView,
    t: (f64, f64, f64, f64),
    b: (f64, f64, f64, f64),
    color: egui::Color32,
) {
    let ty = t.1 + t.3;
    let by = b.1;
    if by <= ty {
        return;
    }
    let xm = (t.0.max(b.0) + (t.0 + t.2).min(b.0 + b.2)) * 0.5;
    let (sx, sy1) = view.canvas_to_screen(xm, ty);
    let (_, sy2) = view.canvas_to_screen(xm, by);
    let (sx, sy1, sy2) = (sx as f32, sy1 as f32, sy2 as f32);
    let stroke = egui::Stroke::new(1.0, color);
    p.line_segment([egui::pos2(sx, sy1), egui::pos2(sx, sy2)], stroke);
    p.line_segment(
        [egui::pos2(sx - 4.0, sy1), egui::pos2(sx + 4.0, sy1)],
        stroke,
    );
    p.line_segment(
        [egui::pos2(sx - 4.0, sy2), egui::pos2(sx + 4.0, sy2)],
        stroke,
    );
    gap_label(p, egui::pos2(sx + 14.0, (sy1 + sy2) * 0.5), by - ty, color);
}

/// A small filled px label for a distance measurement.
fn gap_label(p: &egui::Painter, center: egui::Pos2, value: f64, color: egui::Color32) {
    let galley = p.layout_no_wrap(
        format!("{:.0}", value),
        egui::FontId::proportional(10.0),
        egui::Color32::WHITE,
    );
    let rect = egui::Rect::from_center_size(center, galley.size() + egui::vec2(6.0, 3.0));
    p.rect_filled(rect, 2.0, color);
    p.galley(
        rect.center() - galley.size() * 0.5,
        galley,
        egui::Color32::WHITE,
    );
}

fn fit_artboard_to_rect(view: &mut CanvasView, rect: egui::Rect, bounds: (f64, f64, f64, f64)) {
    let (bx0, by0, bx1, by1) = bounds;
    let bw = (bx1 - bx0).max(1.0);
    let bh = (by1 - by0).max(1.0);
    let zoom_x = rect.width() as f64 / bw;
    let zoom_y = rect.height() as f64 / bh;
    let zoom = (zoom_x.min(zoom_y) * 0.92).clamp(0.01, 64.0);
    view.zoom = zoom;
    let cx = (bx0 + bx1) * 0.5;
    let cy = (by0 + by1) * 0.5;
    view.pan_x = rect.center().x as f64 - cx * zoom;
    view.pan_y = rect.center().y as f64 - cy * zoom;
}

/// Load a `.photon` (or `.svg`) file, returning the document and — for
/// `.photon` files that embed it — the persistent history snapshot to restore.
/// SVG imports and legacy history-less `.photon` files yield `None` history.
fn load_document(
    path: &Path,
) -> Result<(Document, Option<photonic_core::HistorySnapshot>), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if ext == "svg" && !content.trim_start().starts_with('{') {
        photonic_core::import_svg(&content)
            .map(|doc| (doc, None))
            .map_err(|e| e.to_string())
    } else {
        photonic_core::load_photon(&content).map_err(|e| e.to_string())
    }
}

/// Only native `.photon` projects are writable in place. Imported source files
/// become unsaved documents so Save/Autosave can never overwrite the source.
fn native_project_path(path: &Path) -> Option<std::path::PathBuf> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("photon"))
        .then(|| path.to_path_buf())
}

#[cfg(test)]
mod file_lifecycle_tests {
    use super::{load_document, native_project_path};
    use std::path::Path;

    #[test]
    fn only_native_projects_are_writable_in_place() {
        assert_eq!(
            native_project_path(Path::new("project.photon")),
            Some(Path::new("project.photon").to_path_buf())
        );
        assert_eq!(
            native_project_path(Path::new("PROJECT.PHOTON")),
            Some(Path::new("PROJECT.PHOTON").to_path_buf())
        );
        assert_eq!(native_project_path(Path::new("artwork.svg")), None);
        assert_eq!(native_project_path(Path::new("photo.png")), None);
    }

    #[test]
    fn photon_json_mislabeled_as_svg_remains_recoverable() {
        let path = std::env::temp_dir().join(format!(
            "photonic-mislabeled-svg-{}.svg",
            std::process::id()
        ));
        let document = photonic_core::Document::new("Recovered", 64.0, 32.0);
        let json = photonic_core::save_photon(&document, None).unwrap();
        std::fs::write(&path, json).unwrap();

        let (loaded, history) = load_document(&path).expect("mislabeled project should open");
        assert_eq!(loaded.name, "Recovered");
        assert!(history.is_none());
        assert_eq!(native_project_path(&path), None);

        std::fs::remove_file(path).unwrap();
    }
}

/// Human-readable byte size (B / KB / MB) for the history-size readout.
fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / MB)
    }
}

// ─── Crash reporting helpers (#59) ─────────────────────────────────────────────

/// Reveal a folder in the OS file manager (best-effort, never blocks/panics).
fn open_path_in_file_manager(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Percent-encode a string for a URL query value (RFC 3986 unreserved kept).
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The serverless GitHub "new issue" base URL for the Photonic repo (matches the
/// update channel — `update::REPO_OWNER` / `REPO_NAME`).
fn issue_new_base() -> String {
    format!(
        "https://github.com/{}/{}/issues/new",
        crate::update::REPO_OWNER,
        crate::update::REPO_NAME,
    )
}

/// A blank "Report a bug" issue URL (no crash attached).
fn blank_issue_url() -> String {
    format!(
        "{}?labels=bug&title={}",
        issue_new_base(),
        percent_encode("Bug report: "),
    )
}

/// A pre-filled GitHub issue URL for a captured crash report. The body is the
/// user-reviewable Markdown built by the report; GitHub opens it in the browser
/// where the user edits/submits — nothing is sent automatically. The body is
/// bounded so the resulting URL stays within practical browser limits.
fn issue_url_for_report(report: &photonic_core::CrashReport) -> String {
    // GitHub's issue-prefill GET endpoint rejects very long URLs (HTTP 414 URI
    // Too Long), so the bound must be on the *final, percent-encoded* URL — not
    // the raw body. Percent-encoding expands most backtrace bytes (spaces,
    // slashes, colons, parens, newlines) to `%XX`, roughly 3x, so a raw body
    // that "fits" can balloon past the limit once encoded. We trim the raw body
    // until the whole encoded URL is under a safe ceiling.
    const MAX_URL: usize = 7000;
    const TRIM_NOTE: &str = "\n…(truncated to fit GitHub's URL limit)…\n```\n";

    let base = issue_new_base();
    let title = percent_encode(&report.issue_title());
    let prefix = format!("{base}?labels=crash&title={title}&body=");
    let full = report.issue_body();

    // Fast path: the untrimmed body already fits within the encoded budget.
    let encoded_full = percent_encode(&full);
    if prefix.len() + encoded_full.len() <= MAX_URL {
        return format!("{prefix}{encoded_full}");
    }

    // Otherwise trim the raw body (always on a char boundary) until the encoded
    // body + the closing trim note fit the remaining budget. The note re-closes
    // the backtrace's ``` fence so the Markdown isn't left mid-block.
    let budget = MAX_URL.saturating_sub(prefix.len() + percent_encode(TRIM_NOTE).len());
    let mut cut = full.len();
    loop {
        while cut > 0 && !full.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut == 0 {
            break;
        }
        let encoded_len = percent_encode(&full[..cut]).len();
        if encoded_len <= budget {
            break;
        }
        // Shrink by ~the encoded overflow (≥1 raw byte per encoded char),
        // floored so we always make progress and converge quickly.
        let over = encoded_len - budget;
        cut = cut.saturating_sub((over / 3).max(64));
    }

    let mut body = full[..cut].to_string();
    body.push_str(TRIM_NOTE);
    format!("{prefix}{}", percent_encode(&body))
}

/// Install the history restored from a just-opened file. With an embedded
/// snapshot we restore it; without one (SVG / legacy `.photon`) we reset, so a
/// previously-open project's history can't carry over into the new document.
fn apply_opened_history(
    history: &mut CommandHistory,
    snap: Option<photonic_core::HistorySnapshot>,
) {
    match snap {
        Some(s) => history.restore_state(s),
        None => history.reset(),
    }
}

/// Serialize a document together with its persistent history into a `.photon`
/// file. Enforces the configured size cap first so the written history respects
/// the user's budget.
fn write_photon_file(
    path: &Path,
    doc: &Document,
    history: &mut CommandHistory,
) -> Result<(), String> {
    history.enforce_size();
    let snap = history.snapshot_state();
    let json = photonic_core::save_photon(doc, Some(&snap)).map_err(|e| e.to_string())?;
    photonic_core::write_atomic_file(path, json.as_bytes()).map_err(|e| e.to_string())
}

impl PhotonicApp {
    /// Queue clipboard content read by the native window host. `paste_in_place`
    /// mirrors Ctrl+Shift+V; the queue is consumed by the global shortcut
    /// handler on the next egui frame.
    pub fn queue_native_clipboard_paste(
        &mut self,
        payload: NativeClipboardPaste,
        paste_in_place: bool,
    ) {
        self.pending_native_clipboard_paste = Some((payload, paste_in_place));
    }

    /// Take a palette MCP request for execution by the application host.
    /// The canvas viewport in **physical pixels** `(x, y, w, h)` as laid out on
    /// the previous frame, or `None` before the first frame.
    ///
    /// The host clips the document present to this so the artboard cannot bleed
    /// into the gutters between the floating panel cards. Converted here rather
    /// than in the host because the conversion must use the same
    /// `pixels_per_point` egui laid the rect out with — reading it a frame later
    /// can pick up a different value across a DPI or zoom change and clip the
    /// canvas to the wrong box.
    pub fn canvas_viewport_px(&self) -> Option<(u32, u32, u32, u32)> {
        self.last_canvas_rect_px
    }

    pub fn take_mcp_operation_request(&mut self) -> Option<String> {
        self.mcp_operation_request.take()
    }

    pub fn set_mcp_operation_status(&mut self, status: String) {
        self.file_status = Some(status);
    }
    pub fn new() -> Self {
        let prefs = AppPreferences::load();
        let fill_color = prefs.default_fill_color;
        let console_visible = prefs.console_open_on_start;
        let open_drawer = prefs.open_drawer;
        let mut s = Self::default();
        s.prefs = prefs;
        s.fill_color = fill_color;
        s.lua_console.visible = console_visible;
        s.timeline_snap_enabled = s.prefs.timeline_snap_enabled;
        // First-run coach: allowed only on a launch that has never shown it.
        s.coach_allowed_this_session = !s.prefs.video_coach_shown_once;
        s.open_drawer = open_drawer;
        if let Some(g) = open_drawer {
            s.last_drawer_group = g;
        }
        // History moved to the right rail. Migrate a persisted left-History open
        // state so it doesn't leave an unreachable open drawer on the left.
        if s.open_drawer == Some(DrawerGroup::History) {
            s.open_drawer = None;
            s.prefs.open_right_drawer = Some(RightDrawerGroup::History);
        }
        s.open_right_drawer = s.prefs.open_right_drawer;
        if let Some(g) = s.prefs.open_right_drawer {
            s.last_right_drawer_group = g;
        }
        // The mixer moved out of the right drawer into a floating window; a
        // preferences file from before that would otherwise open an empty
        // drawer with no way to notice it was the mixer.
        if s.open_right_drawer == Some(RightDrawerGroup::AudioMixer) {
            s.open_right_drawer = None;
            s.prefs.open_right_drawer = None;
            s.last_right_drawer_group = RightDrawerGroup::Layers;
            s.audio_mixer_window_open = true;
        }
        s
    }

    /// Start with the welcome screen shown (used when no file is given on the CLI).
    pub fn new_with_welcome() -> Self {
        let prefs = AppPreferences::load();
        let fill_color = prefs.default_fill_color;
        let console_visible = prefs.console_open_on_start;
        let open_drawer = prefs.open_drawer;
        let mut s = Self {
            show_welcome: true,
            ..Self::default()
        };
        s.prefs = prefs;
        s.fill_color = fill_color;
        s.lua_console.visible = console_visible;
        s.timeline_snap_enabled = s.prefs.timeline_snap_enabled;
        // First-run coach: allowed only on a launch that has never shown it.
        s.coach_allowed_this_session = !s.prefs.video_coach_shown_once;
        s.open_drawer = open_drawer;
        if let Some(g) = open_drawer {
            s.last_drawer_group = g;
        }
        // History moved to the right rail. Migrate a persisted left-History open
        // state so it doesn't leave an unreachable open drawer on the left.
        if s.open_drawer == Some(DrawerGroup::History) {
            s.open_drawer = None;
            s.prefs.open_right_drawer = Some(RightDrawerGroup::History);
        }
        s.open_right_drawer = s.prefs.open_right_drawer;
        if let Some(g) = s.prefs.open_right_drawer {
            s.last_right_drawer_group = g;
        }
        // The mixer moved out of the right drawer into a floating window; a
        // preferences file from before that would otherwise open an empty
        // drawer with no way to notice it was the mixer.
        if s.open_right_drawer == Some(RightDrawerGroup::AudioMixer) {
            s.open_right_drawer = None;
            s.prefs.open_right_drawer = None;
            s.last_right_drawer_group = RightDrawerGroup::Layers;
            s.audio_mixer_window_open = true;
        }
        s
    }

    /// Borrow the video-editor session state as a [`VideoPanelUi`] (04 §4.1) for
    /// the video panels that do **not** go through [`PropPanelCtx`] — the
    /// right-rail Color/Audio drawers, the floating scopes/export panels, and the
    /// central node canvas. The left-rail video drawers get the same view via
    /// `ctx.video` inside [`Self::draw_property_drawer_content`] instead (that one
    /// must inline the borrows, since the surrounding `PropPanelCtx` literal
    /// already holds disjoint `&mut self` field borrows).
    fn video_panel_ui(&mut self) -> VideoPanelUi<'_> {
        VideoPanelUi {
            selection: &self.timeline_selection,
            selected_grade_op: &mut self.selected_grade_op,
            color_page_tab: &mut self.color_page_tab,
            scopes_panel_open: &mut self.scopes_panel_open,
            scope_kind: &mut self.scope_kind,
            open_graph: &mut self.open_graph,
            selected_graph_node: &mut self.selected_graph_node,
            node_canvas_active: &mut self.node_canvas_active,
            mixer_expanded_tracks: &mut self.mixer_expanded_tracks,
            keyframe_editor_target: &mut self.keyframe_editor_target,
            caption_edit_cue: &mut self.caption_edit_cue,
            export_dialog_open: &mut self.export_dialog_open,
            last_export_preset: &mut self.last_export_preset,
            playhead: self.playhead,
            source_marks: &mut self.source_marks,
            source_audition: self
                .engine
                .as_ref()
                .and_then(|engine| engine.status().source_audition.clone()),
            source_audition_error: self
                .engine
                .as_ref()
                .and_then(|engine| engine.status().source_audition_error.clone()),
            source_monitor_scrub: &mut self.source_monitor_scrub,
            multicam_active_angle: &mut self.multicam_active_angle,
            multicam_view_open: &mut self.multicam_view_open,
            transcript_panel_open: &mut self.transcript_panel_open,
            transcript_scroll: &mut self.transcript_scroll,
            open_sequence_tabs: &mut self.open_sequence_tabs,
            nested_sequence_breadcrumbs: &mut self.nested_sequence_breadcrumbs,
        }
    }

    /// Build the shared [`PropPanelCtx`] and render one property-drawer group into
    /// `ui`, forwarding any produced action. Factored out so both the left drawer
    /// and the right drawer (which now hosts History) can render property groups
    /// from the same ctx construction rather than duplicating ~60 field bindings.
    fn draw_property_drawer_content(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Document,
        history: &CommandHistory,
        group: DrawerGroup,
    ) {
        if group == DrawerGroup::Transcript {
            if let Some((document_id, sequence_id, command)) =
                self.pending_transcript_command.take()
            {
                if document_id == doc.id
                    && doc
                        .timeline
                        .as_ref()
                        .and_then(|project| project.active_sequence)
                        == Some(sequence_id)
                {
                    panels::video::transcript::request_command(ui.ctx(), command);
                }
            }
        }
        let selected_node = self.selected_id.and_then(|id| doc.nodes.get(&id));
        let selection_count = doc.selection.node_ids.len();
        let selected_ids = doc.selection.node_ids.iter().cloned().collect::<Vec<_>>();
        // Keep the Edit History list live — recompute from the current undo/redo
        // stacks each frame so it never goes stale after an edit. Pull a deeper
        // slice than the old flat list needed so the commit graph shows real depth.
        self.history_graph = history.history_graph();
        self.history_current = history.current_node();
        let raster_color_range_target = self
            .raster_color_range
            .as_ref()
            .filter(|s| Some(s.node_id) == self.selected_id)
            .map(|s| s.target);
        let mut ctx = panels::PropPanelCtx {
            doc,
            active_tool: self.active_tool,
            fill_color: &mut self.fill_color,
            polygon_sides: &mut self.polygon_sides,
            star_points: &mut self.star_points,
            star_inner_ratio: &mut self.star_inner_ratio,
            rounded_rect_radius: &mut self.rounded_rect_radius,
            spiral_turns: &mut self.spiral_turns,
            spiral_inner_radius: &mut self.spiral_inner_radius,
            spiral_segs_per_turn: &mut self.spiral_segs_per_turn,
            selected_node,
            selected_id: self.selected_id,
            selection_count,
            selected_ids: &selected_ids,
            point_edit_node: self.point_edit_node,
            point_selected: &self.point_selected,
            prop_search: &mut self.prop_search,
            shear_x: &mut self.shear_x,
            shear_y: &mut self.shear_y,
            line_snap_45: &mut self.line_snap_45,
            color_guide_rule: &mut self.color_guide_rule,
            arc_start_angle: &mut self.arc_start_angle,
            arc_end_angle: &mut self.arc_end_angle,
            arc_open: &mut self.arc_open,
            grid_cols: &mut self.grid_cols,
            grid_rows: &mut self.grid_rows,
            polar_grid_rings: &mut self.polar_grid_rings,
            polar_grid_sectors: &mut self.polar_grid_sectors,
            polar_grid_inner_ratio: &mut self.polar_grid_inner_ratio,
            recolor_palette_input: &mut self.recolor_palette_input,
            magic_wand_attribute: &mut self.magic_wand_attribute,
            magic_wand_tolerance: &mut self.magic_wand_tolerance,
            eraser_radius: &mut self.eraser_radius,
            prop_spread: &mut self.prop_spread,
            prop_falloff_k: &mut self.prop_falloff_k,
            raster_mask_tolerance: &mut self.raster_mask_tolerance,
            raster_mask_contiguous: &mut self.raster_mask_contiguous,
            raster_color_range_target,
            rmbg_model_cached: self.rmbg_model_cached,
            composition_findings: &self.composition_findings,
            rhythm_findings: &self.rhythm_findings,
            branch_name_input: &mut self.branch_name_input,
            swatch_library_selected: &mut self.swatch_library_selected,
            graphic_style_name_input: &mut self.graphic_style_name_input,
            width_profile_name_input: &mut self.width_profile_name_input,
            grammar_rules: &self.grammar_rules,
            grammar_rule_name_input: &mut self.grammar_rule_name_input,
            grammar_rule_type_selected: &mut self.grammar_rule_type_selected,
            grammar_rule_params_input: &mut self.grammar_rule_params_input,
            grammar_check_results: &self.grammar_check_results,
            distance_results: &self.distance_results,
            action_names: &self.action_names,
            history_graph: &self.history_graph,
            history_current: self.history_current,
            history_last_saved: self
                .tabs
                .get(self.active_tab)
                .and_then(|t| t.last_saved_node),
            doc_export: &mut self.doc_export,
            bleed_mm_input: &mut self.bleed_mm_input,
            slug_mm_input: &mut self.slug_mm_input,
            construction_angle: &mut self.construction_angle,
            construction_x: &mut self.construction_x,
            construction_y: &mut self.construction_y,
            margin_top: &mut self.margin_top_input,
            margin_right: &mut self.margin_right_input,
            margin_bottom: &mut self.margin_bottom_input,
            margin_left: &mut self.margin_left_input,
            event_trigger_event: &mut self.event_trigger_event,
            event_trigger_action: &mut self.event_trigger_action,
            workspace_name_input: &mut self.workspace_name_input,
            media_ui: &mut self.media_pool_ui,
            engine_online: self.engine.is_some(),
            proxy_mode: self
                .engine
                .as_ref()
                .map(|b| b.proxy_mode)
                .unwrap_or_default(),
            // Inlined rather than `self.video_panel_ui()` because that borrows
            // all of `self`, which would collide with the disjoint `&mut self`
            // field borrows already held by this `PropPanelCtx` literal.
            video: VideoPanelUi {
                selection: &self.timeline_selection,
                selected_grade_op: &mut self.selected_grade_op,
                color_page_tab: &mut self.color_page_tab,
                scopes_panel_open: &mut self.scopes_panel_open,
                scope_kind: &mut self.scope_kind,
                open_graph: &mut self.open_graph,
                selected_graph_node: &mut self.selected_graph_node,
                node_canvas_active: &mut self.node_canvas_active,
                mixer_expanded_tracks: &mut self.mixer_expanded_tracks,
                keyframe_editor_target: &mut self.keyframe_editor_target,
                caption_edit_cue: &mut self.caption_edit_cue,
                export_dialog_open: &mut self.export_dialog_open,
                last_export_preset: &mut self.last_export_preset,
                playhead: self.playhead,
                source_marks: &mut self.source_marks,
                source_audition: self
                    .engine
                    .as_ref()
                    .and_then(|engine| engine.status().source_audition.clone()),
                source_audition_error: self
                    .engine
                    .as_ref()
                    .and_then(|engine| engine.status().source_audition_error.clone()),
                source_monitor_scrub: &mut self.source_monitor_scrub,
                multicam_active_angle: &mut self.multicam_active_angle,
                multicam_view_open: &mut self.multicam_view_open,
                transcript_panel_open: &mut self.transcript_panel_open,
                transcript_scroll: &mut self.transcript_scroll,
                open_sequence_tabs: &mut self.open_sequence_tabs,
                nested_sequence_breadcrumbs: &mut self.nested_sequence_breadcrumbs,
            },
            action: None,
            q: String::new(),
            forced_open: None,
        };
        if let Some(action) = panels::draw_drawer(ui, &mut ctx, group) {
            self.pending_panel_actions.push(action);
        }
    }

    /// Draw the full UI for one frame.
    ///
    /// Returns `true` if the document was modified this frame.
    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        view: &mut CanvasView,
        renderer: &mut PhotonicRenderer,
        mcp_running: bool,
        history: &mut CommandHistory,
    ) -> bool {
        let mut doc_modified = false;

        // ── Quit-from-welcome shortcut ────────────────────────────────────────
        // The quit prompt only runs in the editor (past the welcome early-return).
        // On the welcome screen there are no open documents to lose, so confirm the
        // pending quit immediately.
        if self.close_requested && self.show_welcome {
            self.close_requested = false;
            self.close_confirmed = true;
        }

        // ── First-launch recovery scan ────────────────────────────────────────
        // Surface any untitled-document autosaves left by a previous crash so the
        // restore/discard prompt can offer them.
        if !self.recovery_scanned {
            self.recovery_scanned = true;
            self.recovery_candidates = crate::app::autosave::recovery_dir()
                .map(|dir| {
                    std::fs::read_dir(&dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x == "photon"))
                        .collect()
                })
                .unwrap_or_default();
        }

        // ── Orphaned color-range preview guard ────────────────────────────────
        // A live color-range session previews directly in the document and is
        // controlled from the selected node's Raster Masking section. If the
        // selection moves off that node, discard the preview — otherwise an
        // uncommitted (and undo-invisible) mask would linger with no UI to
        // cancel it.
        if self
            .raster_color_range
            .as_ref()
            .is_some_and(|s| self.selected_id != Some(s.node_id))
        {
            self.cancel_raster_color_range(doc);
            doc_modified = true;
        }

        // ── Drag-and-drop image import ────────────────────────────────────────
        // Dropping image files onto the window places each as a raster layer
        // (same path as File → Place Image…). Native drops carry a filesystem
        // path; sandboxed/portal drops may only carry bytes — handle both.
        //
        // Platform caveat: winit 0.30 only emits DroppedFile on X11, Windows,
        // and macOS — its Wayland backend has no drag-and-drop support, so on
        // Wayland this handler never fires and File → Open/Place Image… is the
        // way in.
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        // In video mode, dropped media files import into the media pool
        // (05 §2) instead of placing raster layers; anything the pool doesn't
        // recognize falls through to the image path below.
        let mut media_drops: Vec<std::path::PathBuf> = Vec::new();
        for file in dropped {
            if self.mode == AppMode::Video {
                if let Some(path) = file
                    .path
                    .as_deref()
                    .filter(|p| panels::media_pool::guess_asset_kind(p).is_some())
                {
                    media_drops.push(path.to_path_buf());
                    continue;
                }
            }
            if let Some(path) = file.path.as_deref().filter(|p| is_image_path(p)) {
                let path = path.to_path_buf();
                self.place_image_file(doc, history, &path);
                doc_modified = true;
            } else if let Some(bytes) = &file.bytes {
                let name = std::path::PathBuf::from(&file.name);
                if is_image_path(&name) {
                    self.place_image_bytes(doc, history, bytes, Some(&name));
                    doc_modified = true;
                }
            }
        }
        if !media_drops.is_empty() {
            let bin = self.media_pool_ui.current_bin;
            let project_path = self.current_file.clone();
            // L0-first: stubs land on this frame; L1–L3 meta fills async (24 §2).
            let stubs = self
                .media_pool_ui
                .spawn_import(media_drops, bin, project_path);
            if !stubs.is_empty() {
                use photonic_core::timeline::ops;
                timeline::ops_bridge::ensure_project_and_sequence(
                    doc,
                    history,
                    photonic_core::timeline::FrameRate::FPS_30,
                );
                let auto_place = self.prefs.auto_place_import_on_timeline;
                let at = self.playhead;
                for asset in stubs {
                    let id = asset.id;
                    history.execute_discrete(Command::Timeline(ops::add_asset(asset)), doc);
                    // Proposal 213: CapCut-class default — land on the timeline.
                    if auto_place {
                        timeline::ops_bridge::insert_asset_at_first_fit(doc, history, id, at);
                    }
                }
                if auto_place
                    && !self.prefs.video_coach_dismissed
                    && self.prefs.video_coach_step == 0
                {
                    self.prefs.video_coach_step = 1;
                    self.prefs.save();
                }
                doc_modified = true;
            }
        }

        // ── Video engine upkeep ───────────────────────────────────────────────
        // Mirror the timeline into the engine's snapshot pair whenever the
        // history revision moved (see `app/engine.rs`), and apply L1–L3 meta
        // fills for imports already registered at L0 (24 §2).
        if let Some(bridge) = self.engine.as_mut() {
            bridge.sync_document(doc, history);
        }
        let meta_updates = self.media_pool_ui.drain_meta();
        if !meta_updates.is_empty() {
            use photonic_core::timeline::ops;
            let auto_proxy = doc
                .timeline
                .as_ref()
                .map(|p| p.settings.generate_proxies)
                .unwrap_or(false);
            let meta_asset_ids: Vec<_> = meta_updates.iter().map(|m| m.asset).collect();
            if let Some(project) = doc.timeline.as_ref() {
                let commands: Vec<_> = meta_updates
                    .into_iter()
                    .filter(|m| !m.waveform_only)
                    .filter_map(|m| {
                        ops::set_asset_meta(project, m.asset, m.probe, m.content_hash).ok()
                    })
                    .collect();
                for cmd in commands {
                    history.execute_discrete(Command::Timeline(cmd), doc);
                    doc_modified = true;
                }
            }
            // L7: after L1–L4 fill, auto-queue proxies when project policy says so
            // (24 §2 L7, G-15C). No Pending document mutation — in-flight is
            // session-only (`proxy_in_flight`); one Ready/Failed history entry
            // on completion. Manual "Build proxies" shares the same queue guard.
            if auto_proxy {
                let candidates: Vec<_> = doc
                    .timeline
                    .as_ref()
                    .map(|project| {
                        meta_asset_ids
                            .iter()
                            .filter_map(|id| project.media.assets.get(id).cloned())
                            .filter(|a| {
                                a.content_hash.is_some()
                                    && !self.media_pool_ui.proxy_job_in_flight(a.id)
                                    && photonic_video::media::should_auto_generate_proxy(
                                        a.kind,
                                        &a.source,
                                        a.proxy.as_ref(),
                                        true,
                                    )
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if !candidates.is_empty() {
                    self.media_pool_ui
                        .spawn_proxy_generation(candidates, self.current_file.clone());
                }
            }
        }
        let finished_proxies = self.media_pool_ui.drain_finished_proxies();
        if !finished_proxies.is_empty() {
            use photonic_core::timeline::ops;
            use photonic_core::timeline::ProxyOrigin;
            // Skip applying a Generated completion if the user attached a proxy
            // while the job was in flight (G-15A: never clobber Attached).
            for result in finished_proxies {
                let skip = doc
                    .timeline
                    .as_ref()
                    .and_then(|p| p.media.assets.get(&result.asset))
                    .and_then(|a| a.proxy.as_ref())
                    .is_some_and(|p| p.origin == ProxyOrigin::Attached);
                if skip {
                    continue;
                }
                let cmd = doc.timeline.as_ref().and_then(|project| {
                    ops::set_asset_proxy(project, result.asset, result.proxy).ok()
                });
                if let Some(cmd) = cmd {
                    history.execute_discrete(Command::Timeline(cmd), doc);
                    doc_modified = true;
                }
            }
        }

        // ── Direct Select entry seed (#164) ───────────────────────────────────
        // Edge-detect switching *into* the Direct Selection tool: when it becomes
        // active while a path object is selected, seed its point-edit state so
        // every anchor of that path shows up filled ("whole object selected")
        // without requiring an extra click. Edge-triggered (not every frame) so a
        // deliberate click-to-deselect isn't immediately undone.
        // Direct Select and its Proportional Move sub-variant share point-edit
        // state. Re-seed whenever the active tool changes into either one (a switch
        // between the two sub-variants re-seeds to the whole path; narrowing to a
        // single anchor is then done by clicking within the active sub-variant).
        let is_point_edit_tool = |t: Tool| matches!(t, Tool::DirectSelect | Tool::ProportionalMove);
        let entered_direct_select =
            is_point_edit_tool(self.active_tool) && self.last_tool != self.active_tool;
        // Central tool-lifecycle seam (#190): on a tool switch, fire the previous
        // tool's `on_deactivate` then the new tool's `on_activate`, so cross-tool
        // switch behaviour lives in one place (the DirectSelect seed below is a
        // candidate to migrate into `DirectSelectTool::on_activate`).
        if self.active_tool != self.last_tool {
            let (prev, cur) = (self.last_tool, self.active_tool);
            crate::tools::tool_for(prev).on_deactivate(self);
            crate::tools::tool_for(cur).on_activate(self);
        }
        self.last_tool = self.active_tool;
        if entered_direct_select {
            self.seed_direct_select_from_selection(doc);
        }

        // ── Apply the configured history-retention limits ─────────────────────
        // Cheap and idempotent when unchanged, so it's safe every frame. In
        // size-limited mode the byte cap is re-checked on a throttle (below)
        // rather than here, since measuring it serializes the history.
        let (max_steps, default_size) = self.prefs.history_limits();
        // A per-document cap (#195, set at new-file time) overrides the global
        // default for this file.
        let size_limit = doc
            .history_max_mb
            .map(|mb| (mb.max(0.1) * 1_048_576.0) as u64)
            .or(default_size);
        history.set_limits(max_steps, size_limit);
        if size_limit.is_some() {
            let now = ctx.input(|i| i.time);
            if now - self.last_history_size_check >= 1.5 {
                self.last_history_size_check = now;
                history.enforce_size();
                // Proactive warning (#197): warn *before* the cap starts dropping
                // steps. Fires once per breach of the ~85% soft threshold; re-arms
                // (`history_pressure_warned`) once pressure falls back under ~70%.
                if let Some(p) = history.size_pressure() {
                    if p >= 0.85 && !self.history_pressure_warned {
                        self.history_pressure_warned = true;
                        self.file_status = Some(format!(
                            "Project history is at {:.0}% of its {:.0} MB limit — raise it in \
                             Edit ▸ Behavior ▸ Project History before the oldest edits start dropping.",
                            (p * 100.0).min(100.0),
                            self.prefs.history_max_mb,
                        ));
                    } else if p < 0.70 {
                        self.history_pressure_warned = false;
                    }
                }
            }
        }
        // Final notice if the cap actually had to drop the oldest steps (so
        // trimming is never silent even when the proactive warning was dismissed).
        if let Some(msg) = history.take_limit_warning() {
            self.file_status = Some(msg);
        }

        // ── Gesture coalescing (#182) ─────────────────────────────────────────
        // While the pointer is down, collapse a continuous drag's per-tick edits
        // into a single undo step. Opened here (before any tool/panel handler can
        // call `history.execute` this frame) and closed on release in the
        // post-loop `any_released` block below. `begin_coalescing` is idempotent,
        // so it simply stays open across every frame of the gesture; the very
        // first `execute` of the gesture pushes an anchor and subsequent
        // same-target ticks fold into it. This fixes the fill/stroke color
        // picker (#180) and shields any future slider/handle that streams
        // `execute` from flooding the history.
        if ctx.input(|i| i.pointer.any_down()) {
            history.begin_coalescing();
        }

        // ── Command palette (Ctrl/Cmd+K) — drawn on top of everything ─────────
        // Handled before tool dispatch so a chosen command runs this frame.
        if self.command_palette(ctx, doc, history) {
            doc_modified = true;
        }

        // ── Tool-independent keyboard shortcuts (#192) ────────────────────────
        // Undo/redo, copy/paste, duplicate, select-all/deselect, flip,
        // group/ungroup, z-order and the view-preview/guide toggles must fire
        // regardless of the active tool. Dispatched here, before per-tool
        // handling, so a shortcut applies the same frame. Internally guarded by
        // `viewport_kb` so typing into a text field is unaffected.
        if self.handle_global_shortcuts(ctx, doc, history) {
            doc_modified = true;
        }

        // ── Poll an in-flight self-update check ───────────────────────────────
        if let Some(rx) = &self.update_rx {
            if let Ok(status) = rx.try_recv() {
                use crate::update::UpdateStatus;
                self.file_status = Some(match status {
                    UpdateStatus::UpToDate(v) => format!("Photonic is up to date (v{v})"),
                    UpdateStatus::Updated(v) => {
                        format!("Updated to v{v} — restart Photonic to apply")
                    }
                    UpdateStatus::Error(e) => format!("Update check failed: {e}"),
                });
                self.update_rx = None;
            } else {
                ctx.request_repaint(); // keep polling until the check returns
            }
        }

        // ── Poll an in-flight background-removal job ──────────────────────────
        if let Some(rx) = &self.rmbg_rx {
            if let Ok(result) = rx.try_recv() {
                self.rmbg_rx = None;
                let nid = self.rmbg_node.take();
                match (result, nid) {
                    (Ok(matte), Some(nid)) => {
                        self.rmbg_model_cached = true;
                        // A color-range preview started mid-inference would
                        // otherwise be baked into the undo record — discard it
                        // so the commit is built from committed state.
                        if self
                            .raster_color_range
                            .as_ref()
                            .is_some_and(|s| s.node_id == nid)
                        {
                            self.cancel_raster_color_range(doc);
                        }
                        if let Some(node) = doc.get_node(&nid) {
                            let mut updated = node.clone();
                            let mut applied = false;
                            if let SceneNodeKind::Raster(rn) = &mut updated.kind {
                                // No-op on size mismatch (image was replaced
                                // while the job ran) — then `applied` stays false.
                                let before = rn.mask.clone();
                                rn.set_foreground_matte(matte);
                                applied = rn.mask != before;
                            }
                            if applied {
                                history.execute(
                                    Command::UpdateNode {
                                        old: node.clone(),
                                        new: updated,
                                    },
                                    doc,
                                );
                                doc_modified = true;
                                self.file_status = Some(
                                    "Background removed — applied as a layer mask (undoable)"
                                        .into(),
                                );
                            } else {
                                self.file_status =
                                    Some("Background removal skipped: layer changed".into());
                            }
                        } else {
                            self.file_status =
                                Some("Background removal skipped: layer was deleted".into());
                        }
                    }
                    (Err(e), _) => {
                        self.file_status = Some(format!("Background removal failed: {e}"));
                    }
                    (Ok(_), None) => {}
                }
            } else {
                ctx.request_repaint(); // keep polling until the matte returns
            }
        }

        // ── Auto-check for a newer release, once per launch ───────────────────
        // Lightweight (no download): just asks GitHub for the latest version.
        // If a newer one exists, `update_available` drives a dismissable banner.
        if !self.update_checked_startup && self.prefs.auto_check_updates && self.update_rx.is_none()
        {
            self.update_checked_startup = true;
            self.update_check_rx = Some(crate::update::check_latest());
        }
        if let Some(rx) = &self.update_check_rx {
            match rx.try_recv() {
                Ok(check) => {
                    use crate::update::UpdateCheck;
                    if let UpdateCheck::Available(v) = check {
                        self.update_available = Some(v);
                    }
                    self.update_check_rx = None;
                }
                // The check thread ended without sending (panicked, or the thread
                // failed to spawn). Stop polling so we don't spin forever.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.update_check_rx = None;
                }
                // Still pending: wake a few times a second rather than repainting
                // every frame. `self_update::get_latest_release()` is a blocking
                // request with no timeout, so a slow/hung update check would
                // otherwise pin the main thread at 100% until it returns.
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
            }
        }

        // ── Update-available banner (dismissable, floats top-center) ──────────
        if let Some(ver) = self.update_available.clone() {
            let mut dismiss = false;
            let mut do_update = false;
            egui::Area::new(egui::Id::new("update_available_banner"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .fill(Color32::from_rgb(30, 41, 59))
                        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(59, 130, 246)))
                        .rounding(10.0)
                        .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(ph::ARROW_CIRCLE_UP)
                                        .size(18.0)
                                        .color(Color32::from_rgb(96, 165, 250)),
                                );
                                ui.label(
                                    RichText::new(format!("Photonic v{ver} is available"))
                                        .strong()
                                        .color(Color32::from_rgb(226, 232, 240)),
                                );
                                ui.add_space(8.0);
                                if ui
                                    .button(
                                        RichText::new(format!(
                                            "{} Update now",
                                            ph::DOWNLOAD_SIMPLE
                                        ))
                                        .color(Color32::WHITE),
                                    )
                                    .clicked()
                                {
                                    do_update = true;
                                }
                                if ui.button("Later").clicked() {
                                    dismiss = true;
                                }
                            });
                        });
                });
            if do_update {
                if self.update_rx.is_none() {
                    self.update_rx = Some(crate::update::check_and_update());
                    self.file_status = Some(format!(
                        "Downloading Photonic v{ver}… (current {})",
                        crate::update::CURRENT_VERSION
                    ));
                }
                self.update_available = None;
            } else if dismiss {
                self.update_available = None;
            }
        }

        // ── "What's New" after an update ──────────────────────────────────────
        // Once per launch, compare the running build to the last version this
        // user actually ran. If it moved forward, queue the notes for the gap.
        if !self.whats_new_checked {
            self.whats_new_checked = true;
            let cur = crate::update::CURRENT_VERSION;
            if self.prefs.last_seen_version.is_empty() {
                // Fresh install — record silently, never nag on first run.
                self.prefs.last_seen_version = cur.to_string();
                self.prefs.save();
            } else if self.prefs.last_seen_version != cur {
                let notes = crate::release_notes::since(&self.prefs.last_seen_version);
                self.prefs.last_seen_version = cur.to_string();
                self.prefs.save();
                if !notes.is_empty() {
                    self.whats_new_notes = notes;
                    self.show_whats_new = true;
                }
            }
        }
        if self.show_whats_new {
            self.draw_whats_new(ctx);
        }
        if self.show_mcp_modal {
            self.draw_mcp_modal(ctx, mcp_running);
        }

        // ── Pending crash reports (#59) ───────────────────────────────────────
        // Local capture is always on; this only governs *offering* to send. Scan
        // once per launch, then either ask for one-time consent (consent == None)
        // or surface a Report/Dismiss banner (consent == Some(true)).
        if !self.crash_reports_scanned {
            self.crash_reports_scanned = true;
            self.pending_crash_reports = photonic_core::diagnostics::pending_reports();
        }
        if !self.pending_crash_reports.is_empty() {
            self.draw_crash_report_prompt(ctx);
        }

        // ── Apply theme ───────────────────────────────────────────────────────
        // Install *both* palettes, then let the preference pick between them.
        //
        // `Context::set_visuals` writes only the slot for the theme that happens
        // to be active, and egui's `theme_preference` defaults to `System`. So
        // writing one palette left the other slot holding egui's stock visuals,
        // and on a light desktop the app came up in egui's default light theme no
        // matter what the preference said. Filling both slots makes the palette
        // correct whichever way the preference — or the desktop — resolves.
        ctx.set_visuals_of(egui::Theme::Dark, crate::theme::build_dark_theme());
        ctx.set_visuals_of(egui::Theme::Light, crate::theme::build_light_theme());
        ctx.set_theme(self.prefs.theme_mode.to_egui());
        // Mirror the *resolved* choice so the hand-picked colours elsewhere
        // (rulers, scrims, canvas overlays) follow a System resolution too.
        self.prefs.dark_mode = ctx.theme() == egui::Theme::Dark;
        // Apply the user's UI scale as a *zoom factor* composed on top of the
        // window's native scale factor — NOT as an absolute pixels-per-point.
        // Using an absolute ppp here decouples egui's layout/hit-testing from the
        // native scale factor egui-winit feeds in, so on any monitor whose scale
        // factor ≠ this value (HiDPI / fractional scaling) widgets get drawn at
        // one scale but hit-tested at another — the screen renders off-centre and
        // clicks miss. `set_zoom_factor` keeps layout, pointer mapping, screen
        // rect, and tessellation all coherent across resolutions.
        ctx.set_zoom_factor(self.prefs.ui_scale);

        // Lazily upload the embedded Photonic logo for the top toolbar (once).
        if self.logo_texture.is_none() {
            if let Ok(img) = photonic_core::raster::image::RasterImage::from_encoded(
                include_bytes!("../../assets/logo.png"),
            ) {
                let color = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width as usize, img.height as usize],
                    &img.pixels,
                );
                self.logo_texture =
                    Some(ctx.load_texture("photonic_logo", color, egui::TextureOptions::LINEAR));
            }
        }

        // ── Crash-recovery prompt (over welcome or editor) ───────────────────
        // Offer to restore untitled autosaves left by a previous session before
        // anything else. Re-enter cleanly next frame once resolved.
        if !self.recovery_candidates.is_empty()
            && self.draw_recovery_prompt(ctx, doc, view, history)
        {
            return doc_modified;
        }

        // ── Welcome screen (shown before the editor on first launch) ─────────
        if self.show_welcome {
            if let Some(action) = self.welcome.draw(ctx) {
                use crate::welcome::WelcomeAction;
                match action {
                    WelcomeAction::CreateNew(spec) => {
                        self.create_document_from_spec(doc, history, spec);
                        self.show_welcome = false;
                        doc_modified = true;
                    }
                    WelcomeAction::CreateNewVideo(spec) => {
                        // Video-mode counterpart of `CreateNew` above (04
                        // §1.2): a fresh document, THEN the timeline project
                        // it's paired with (§1.3) — same undo-reset discipline
                        // as `create_document_from_spec`, plus entering Video.
                        let crate::welcome::VideoProjectSpec {
                            name,
                            width,
                            height,
                            frame_rate,
                        } = spec;
                        *doc = photonic_core::Document::new(name, width, height);
                        history.reset();
                        self.fit_pending = true;
                        self.current_file = None;
                        self.selected_id = None;
                        self.ensure_timeline_project_with(
                            doc,
                            history,
                            width as u32,
                            height as u32,
                            frame_rate,
                        );
                        self.mode = AppMode::Video;
                        self.show_welcome = false;
                        doc_modified = true;
                    }
                    WelcomeAction::OpenFile(path) => match load_document(&path) {
                        Ok((loaded, hist_snap)) => {
                            self.welcome.add_recent(path.clone(), loaded.name.clone());
                            *doc = loaded;
                            apply_opened_history(history, hist_snap);
                            self.fit_pending = true;
                            self.current_file = native_project_path(&path);
                            self.selected_id = None;
                            self.show_welcome = false;
                            doc_modified = true;
                            // Auto-enter (04 §1.2): a project with a timeline
                            // always opens into video mode.
                            self.mode = if doc.timeline.is_some() {
                                AppMode::Video
                            } else {
                                AppMode::Vector
                            };
                        }
                        Err(e) => {
                            self.file_status = Some(format!("Open failed: {e}"));
                        }
                    },
                    WelcomeAction::AddDiskRoot => {
                        if let Some(dir) = run_file_dialog(|| rfd::FileDialog::new().pick_folder())
                        {
                            self.welcome.add_disk_root(dir);
                        }
                    }
                    WelcomeAction::OpenBrowse => {
                        if let Some(path) = run_file_dialog(|| {
                            rfd::FileDialog::new()
                                .add_filter("Photonic", &["photon"])
                                .add_filter("SVG", &["svg"])
                                .add_filter("Images", &IMAGE_EXTENSIONS)
                                .add_filter("All supported", &{
                                    let mut all = vec!["photon", "svg"];
                                    all.extend(IMAGE_EXTENSIONS);
                                    all
                                })
                                .pick_file()
                        }) {
                            // Opening a photo from the welcome screen starts a
                            // fresh artboard sized to it, with the image placed
                            // at the origin — like opening a photo in Photoshop.
                            if is_image_path(&path) {
                                *doc = Document::default_artboard();
                                history.reset();
                                self.current_file = None;
                                self.selected_id = None;
                                self.place_image_file(doc, history, &path);
                                if let Some(nid) = self.selected_id {
                                    let dims = doc.get_node(&nid).and_then(|n| match &n.kind {
                                        SceneNodeKind::Raster(rn) => {
                                            Some((rn.image.width as f64, rn.image.height as f64))
                                        }
                                        _ => None,
                                    });
                                    if let Some((w, h)) = dims {
                                        doc.width = w;
                                        doc.height = h;
                                        // The artboard is a spatial rect of its
                                        // own — size it to the photo as well,
                                        // or the visible board (and Crop to
                                        // Artboard) would still be A4.
                                        if let Some(ab) = doc.artboards.first_mut() {
                                            ab.x = 0.0;
                                            ab.y = 0.0;
                                            ab.width = w;
                                            ab.height = h;
                                        }
                                        if let Some(node) = doc.get_node_mut(&nid) {
                                            node.transform =
                                                photonic_core::Transform::translate(0.0, 0.0);
                                        }
                                    }
                                    doc.name = path
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| doc.name.clone());
                                }
                                self.fit_pending = true;
                                self.show_welcome = false;
                                doc_modified = true;
                            } else {
                                match load_document(&path) {
                                    Ok((loaded, hist_snap)) => {
                                        self.welcome.add_recent(path.clone(), loaded.name.clone());
                                        *doc = loaded;
                                        apply_opened_history(history, hist_snap);
                                        self.fit_pending = true;
                                        self.current_file = native_project_path(&path);
                                        self.selected_id = None;
                                        self.show_welcome = false;
                                        doc_modified = true;
                                        // Auto-enter (04 §1.2), same rule as
                                        // the WelcomeAction::OpenFile arm.
                                        self.mode = if doc.timeline.is_some() {
                                            AppMode::Video
                                        } else {
                                            AppMode::Vector
                                        };
                                    }
                                    Err(e) => {
                                        self.file_status = Some(format!("Open failed: {e}"));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            return doc_modified;
        }

        // ── Multi-document tabs ──────────────────────────────────────────────
        // Guarantee the active document has a tab slot (covers CLI-opened files
        // and the first editor frame), then apply any switch/close requested by
        // last frame's tab-bar UI before the canvas draws this frame.
        self.ensure_initial_tab(doc, history);
        // Auto-enter video mode (04 §1.2) for a CLI/double-click-opened
        // document that already has a timeline — runs exactly once, the
        // first frame after the initial tab lands.
        self.check_initial_auto_enter(doc);
        if let Some(target) = self.pending_tab_switch.take() {
            self.switch_tab(target, doc, history, view);
        }
        if let Some(idx) = self.pending_tab_close.take() {
            // Dirty tabs route through the Save/Discard/Cancel modal; clean tabs
            // close immediately.
            if self.tabs.get(idx).is_some_and(|t| t.dirty) {
                self.close_tab_prompt = Some(idx);
            } else {
                self.close_tab(idx, doc, history, view);
            }
        }

        // ── New document modal (File ▸ New) ──────────────────────────────────
        if self.draw_new_document_modal(ctx, doc, view, history) {
            doc_modified = true;
        }

        // ── Unsaved-changes guards (tab close + program quit) ────────────────
        doc_modified |= self.draw_unsaved_changes_modals(ctx, doc, view, history);

        // ── Export modal ─────────────────────────────────────────────────────
        self.draw_export_modal(ctx, doc);

        // ── Radial-menu fill/stroke color picker ──────────────────────────────
        doc_modified |= self.draw_color_popup(ctx, doc, history);

        // ── Simplify Path dialog ──────────────────────────────────────────────
        self.draw_simplify_dialog(ctx, doc, history);
        // ── Merge Vertices by Distance dialog ─────────────────────────────────
        self.draw_merge_vertices_dialog(ctx, doc, history);

        // ── Find / Replace Text dialog ────────────────────────────────────────
        self.draw_find_replace_text_dialog(ctx, doc, history);

        // ── Object/Layer Options modal ────────────────────────────────────────
        self.draw_object_options_dialog(ctx, doc, history);

        // ── Top toolbar ──────────────────────────────────────────────────────
        let toolbar_resp = egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // File toggle button — opens/closes the File drawer
                    let file_active = self.active_drawer == Some(DrawerKind::File);
                    if ui.selectable_label(file_active, "File").clicked() {
                        // Switching drawers away from Edit must flush prefs, else
                        // a just-changed setting (e.g. history limit) is lost.
                        if self.active_drawer == Some(DrawerKind::Edit) {
                            self.prefs.save();
                        }
                        self.active_drawer = if file_active {
                            None
                        } else {
                            Some(DrawerKind::File)
                        };
                        self.selected_drawer_option = None;
                    }

                    // Edit toggle button — opens/closes the Preferences drawer
                    let edit_active = self.active_drawer == Some(DrawerKind::Edit);
                    if ui.selectable_label(edit_active, "Edit").clicked() {
                        if edit_active {
                            self.prefs.save();
                            self.active_drawer = None;
                        } else {
                            self.active_drawer = Some(DrawerKind::Edit);
                        }
                        self.selected_drawer_option = None;
                    }

                    // Tools menu — lists all tools, lets user pin them to the sidebar
                    let tools_active = self.active_drawer == Some(DrawerKind::Tools);
                    if ui.selectable_label(tools_active, "Tools").clicked() {
                        if self.active_drawer == Some(DrawerKind::Edit) {
                            self.prefs.save();
                        }
                        self.active_drawer = if tools_active {
                            None
                        } else {
                            Some(DrawerKind::Tools)
                        };
                        self.selected_drawer_option = None;
                    }

                    // Video mode toggle (video-editor-module 04-ui-mode-timeline.md
                    // §1.2) — same selectable_label + active_drawer-flush idiom as
                    // File/Edit/Tools above. `enter_or_exit_video_mode` lazily
                    // creates `doc.timeline` on the way in (§1.3) and always issues
                    // the exit-pause seam on the way out (§7).
                    let video_active = self.mode == AppMode::Video;
                    let video_resp = ui.selectable_label(video_active, "Video");
                    if video_resp.clicked() {
                        if self.active_drawer == Some(DrawerKind::Edit) {
                            self.prefs.save();
                        }
                        self.enter_or_exit_video_mode(doc, history);
                    }
                    self.draw_video_hint_callout(ui.ctx(), video_resp.rect);

                    // Audit log toggle
                    if ui
                        .selectable_label(self.audit.panel_open, "Audit")
                        .clicked()
                    {
                        self.audit.panel_open = !self.audit.panel_open;
                    }

                    // Video export dialog toggle (05 §3) — video mode only.
                    // Toolbar entry for `export_dialog_open`; the floating dialog
                    // body is filled by the export-dialog (05) story. (The scopes
                    // panel toggle is owned by the Color Controls drawer instead,
                    // via `VideoPanelUi::scopes_panel_open`, per 07 §6.)
                    if self.mode == AppMode::Video
                        && ui
                            .selectable_label(self.export_dialog_open, "Export")
                            .on_hover_text("Export video (04 §4.1)")
                            .clicked()
                    {
                        self.export_dialog_open = !self.export_dialog_open;
                    }
                    if self.mode == AppMode::Video
                        && ui
                            .selectable_label(self.render_queue_panel_open, "Queue")
                            .on_hover_text("Render queue inspector (K-F1)")
                            .clicked()
                    {
                        self.render_queue_panel_open = !self.render_queue_panel_open;
                    }

                    // Global search (command palette) — tools + actions.
                    ui.separator();
                    self.global_search_ui(ui, doc, history);

                    // Diff overlay clear button (only visible when a diff is active)
                    if self.diff.overlay_active {
                        ui.separator();
                        if ui
                            .button(
                                RichText::new(format!("{} Clear Diff", ph::X))
                                    .small()
                                    .color(Color32::from_rgb(239, 68, 68)),
                            )
                            .on_hover_text("Clear diff highlights")
                            .clicked()
                        {
                            self.pending_panel_actions.push(PanelAction::ClearDiff);
                        }
                    }

                    ui.separator();
                    // Pass the file-status message into the toolbar so the zoom
                    // readout and status text share one right-aligned cluster
                    // instead of overlapping in the top-right corner.
                    panels::draw_toolbar(
                        ui,
                        &doc.name,
                        view.zoom,
                        self.file_status.as_deref(),
                        self.logo_texture.as_ref(),
                    );
                });
            });

        // ── Adaptive hotbar (always-on second top row) ───────────────────────
        self.draw_hotbar(ctx, doc);

        // Close drawer on Escape
        if viewport_kb(ctx) && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.active_drawer == Some(DrawerKind::Edit) {
                self.prefs.save();
            }
            self.active_drawer = None;
            self.selected_drawer_option = None;
        }

        let toolbar_rect = toolbar_resp.response.rect;
        doc_modified = self.draw_menu_drawer(ctx, doc, view, history, toolbar_rect, doc_modified);

        // ── Bottom status bar ────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("statusbar")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(concat!("Photonic v", env!("CARGO_PKG_VERSION"))).weak(),
                    );
                    // Isolation Mode indicator.
                    if let Some(iso_id) = self.isolated_group {
                        ui.separator();
                        let name = doc
                            .nodes
                            .get(&iso_id)
                            .map(|n| n.name.as_str())
                            .unwrap_or("Group");
                        ui.label(
                            RichText::new(format!("Isolation: {}  (Esc to exit)", name))
                                .color(egui::Color32::from_rgb(80, 160, 255))
                                .strong(),
                        );
                    }
                    ui.separator();
                    // Mode-aware status: vector editing shows tool/objects/zoom;
                    // video mode shows sequence/format/playhead — the vector
                    // tool + document node count are meaningless over a timeline.
                    if self.mode == AppMode::Video {
                        if let Some(line) = video_status_line(doc, self.playhead) {
                            ui.label(line);
                        }
                    } else {
                        let sel_info = self
                            .selected_id
                            .and_then(|id| doc.nodes.get(&id))
                            .map(|n| format!("  •  \"{}\" selected", n.name))
                            .unwrap_or_default();
                        ui.label(format!(
                            "{} {}  •  {} objects{}  •  {:.0}%",
                            self.active_tool.icon(),
                            self.active_tool.label(),
                            doc.node_count(),
                            sel_info,
                            view.zoom * 100.0,
                        ));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Clickable MCP status indicator → opens the MCP modal
                        // (status + restart, #170).
                        let (mcp_txt, mcp_col) = if mcp_running {
                            (
                                format!("MCP :7842 {}", ph::CHECK),
                                Color32::from_rgb(52, 211, 153),
                            )
                        } else {
                            (
                                format!("MCP offline {}", ph::X),
                                Color32::from_rgb(248, 113, 113),
                            )
                        };
                        if ui
                            .add(
                                egui::Button::new(RichText::new(mcp_txt).color(mcp_col))
                                    .frame(false),
                            )
                            .on_hover_text("MCP server — click for details and restart")
                            .clicked()
                        {
                            self.show_mcp_modal = true;
                        }
                        ui.separator();
                        // Console toggle
                        let label = if self.lua_console.visible {
                            format!("{} Hide Console", ph::TERMINAL)
                        } else {
                            format!("{} Console", ph::TERMINAL)
                        };
                        if ui
                            .selectable_label(self.lua_console.visible, label)
                            .clicked()
                        {
                            self.lua_console.visible = !self.lua_console.visible;
                        }
                    });
                });
            });

        // ── Document tab bar (sits just above the status bar) ─────────────────
        self.draw_tab_bar(ctx);

        // ── Left drawer rail (Canva-style) ────────────────────────────────────
        // A thin vertical strip with one phosphor-icon button per DrawerGroup —
        // Tools now lives here too (the old standalone tools panel is gone).
        // Clicking a group toggles its drawer; opening one closes any other.
        // A group with no content for the current context is DISABLED, and an
        // open drawer whose content disappears animates closed (and reappears
        // when the context returns) via `effective_open` — no state churn.
        let node_sel_count = doc.selection.node_ids.len();
        let clip_sel_count = self.timeline_selection.len();
        let effective_open = self
            .open_drawer
            .filter(|g| g.has_content(node_sel_count, clip_sel_count));
        // ── Rail / drawer card layout ─────────────────────────────────────────
        // Shared knobs for the floating rail + drawer "cards". Both use the same
        // corner radius, border, and vertical float; the rail stays flush with
        // the window on the left (square left corners) while its right side and
        // the drawer float. Tweak the look here — everything below derives from it.
        const CARD_ROUNDING: f32 = 8.0; // radius on every corner that rounds
        const CARD_FLOAT_Y: f32 = 6.0; // top/bottom gap so the cards float vertically
        const RAIL_ICON: f32 = 30.0; // rail icon button (square)
        const RAIL_PAD_X: f32 = 7.0; // rail inner left/right padding around the icons
        const RAIL_PAD_Y: f32 = 4.0; // rail inner top/bottom padding
        const RAIL_GAP: f32 = 4.0; // gap on the rail's right, before the drawer
        const DRAWER_GAP: f32 = 3.0; // gap on the drawer's left, after the rail
                                     // Gap on the drawer's right, off the canvas. Kept at 0 so the floating
                                     // card does not leave a pure-black window-fill strip between the left
                                     // drawer and the preview — that strip stacked with the monitor's
                                     // pillarbox and read as a stubborn "black box" when flipping tabs.
                                     // Top/bottom float (`CARD_FLOAT_Y`) still gives the card its rounded
                                     // corners against the window fill.
        const DRAWER_FLOAT_X: f32 = 0.0;
        const DRAWER_PAD_X: f32 = 10.0; // drawer inner left/right content gutter
        const DRAWER_PAD_Y: f32 = 8.0; // drawer inner top/bottom content gutter
                                       // Rail width is fully determined by its padding, icon size and right gap.
        const RAIL_WIDTH: f32 = RAIL_PAD_X + RAIL_ICON + RAIL_PAD_X + RAIL_GAP;

        // Style the rail to match the floating drawer: its left edge stays flush
        // with the window, but the right edge is rounded and it gets the same
        // top/bottom float, border, and corner radius, so rail + drawer read as a
        // matched pair of cards.
        let rail_frame = {
            let mut f = egui::Frame::side_top_panel(&ctx.style());
            // Symmetric left/right inner padding so the centre-aligned icons sit
            // visually centred within the card (the outer-right gap is separate).
            f.inner_margin = egui::Margin {
                left: RAIL_PAD_X,
                right: RAIL_PAD_X,
                top: RAIL_PAD_Y,
                bottom: RAIL_PAD_Y,
            };
            // Flush on the left (window edge), but float on the right, top and
            // bottom. The right gap is what makes the rounded right corners
            // actually read — with no gap they sat flush against the neighbour
            // at near-zero contrast, which is why the rounding looked absent.
            f.outer_margin = egui::Margin {
                left: 0.0,
                right: RAIL_GAP,
                top: CARD_FLOAT_Y + self.menu_drawer_height,
                bottom: CARD_FLOAT_Y,
            };
            // Round only the two right corners; the left edge is the window edge.
            f.rounding = egui::Rounding {
                nw: 0.0,
                ne: CARD_ROUNDING,
                sw: 0.0,
                se: CARD_ROUNDING,
            };
            // Same 1 px border as the drawer so the rounded right corners read the
            // same way (fill-vs-background alone was too low-contrast to show them).
            f.stroke = ctx.style().visuals.widgets.noninteractive.bg_stroke;
            f
        };
        egui::SidePanel::left("drawer_rail")
            .resizable(false)
            .exact_width(RAIL_WIDTH)
            .frame(rail_frame)
            // Drop egui's default panel separator — with the floating card look it
            // was the stray vertical line sitting in the gap beside the drawer.
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                // Centre the icon column within the rail card.
                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                    for group in DrawerGroup::all_for_mode(self.mode).iter().copied() {
                        let active = effective_open == Some(group);
                        let enabled = group.has_content(node_sel_count, clip_sel_count);
                        let resp = ui
                            .add_enabled(
                                enabled,
                                egui::Button::new(RichText::new(group.icon()).size(18.0))
                                    .min_size(egui::vec2(RAIL_ICON, RAIL_ICON))
                                    .selected(active),
                            )
                            .on_hover_text(group.title());
                        if resp.clicked() {
                            if self.open_drawer == Some(group) {
                                // Clicking the open group collapses the drawer.
                                self.open_drawer = None;
                            } else {
                                // Opening a group closes whatever else was open.
                                self.open_drawer = Some(group);
                                self.last_drawer_group = group;
                            }
                            // Persist the drawer state + any pending width change.
                            self.prefs.open_drawer = self.open_drawer;
                            self.prefs.save();
                        }
                        ui.add_space(4.0);
                    }
                });

                // ── Persistent active fill-color swatch (#172) ────────────────
                // Pinned to the bottom of the rail via a bottom-up layout so it
                // hugs the rail floor no matter how many group buttons show. This
                // is the always-visible readout of "what color my next fill will
                // be" — editing it opens egui's color popup on `fill_color` and
                // mirrors the change into `prefs.default_fill_color`, the mirror
                // image of the Tool Defaults handler (mod.rs ~2597) which edits
                // `prefs.default_fill_color` and mirrors back into `fill_color`.
                // Unlike that sibling this control also persists the default, but
                // it does so on interaction-end (see below) — never on every
                // dragged-slider frame — so a color pick doesn't thrash the disk.
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    // Keep the swatch a fixed, glanceable 26×26 that fits the
                    // 40 px rail rather than the default wide color button.
                    ui.spacing_mut().interact_size = egui::vec2(26.0, 26.0);
                    // `self.fill_color` is gamma-sRGB `[f32; 4]` (maps 1:1 into
                    // `Color`), so route it through the shared sRGBA picker to
                    // match the renderer instead of egui's linear `Rgba` control
                    // (issue #185).
                    let resp = crate::color_popup::ColorPopup::swatch_f32(ui, &mut self.fill_color)
                        .on_hover_text("Active fill color — click to change");
                    if resp.changed() {
                        // Mirror in-memory every frame (cheap) so the active
                        // color is always live, but only mark the persisted
                        // default dirty — the disk write is deferred.
                        self.prefs.default_fill_color = self.fill_color;
                        self.fill_swatch_dirty = true;
                    }
                    // Flush the single `prefs.save()` once the picker interaction
                    // settles (pointer released), matching the #171 recent-colors
                    // commit-on-release idiom. `ColorPopup::swatch_f32` drives an
                    // sRGBA picker popup whose `.changed()` fires on every drag frame;
                    // gating the write on pointer-release collapses dozens of
                    // synchronous serialize+fs::write cycles into one per edit.
                    if self.fill_swatch_dirty && ui.input(|i| i.pointer.any_released()) {
                        self.prefs.save();
                        self.fill_swatch_dirty = false;
                    }
                });
            });

        // ── Animated drawer host ──────────────────────────────────────────────
        // The width tweens 0 → target on open and target → 0 on close (~150ms,
        // ease-out, no overshoot). The open/closed STATE flips instantly (input
        // is never blocked on the tween); only the rendered width animates.
        // `animate_bool_with_time_and_easing` requests repaint while in flight.
        // Reduced-motion makes the transition instant.
        // Recompute after the rail click so opening/closing animates this frame.
        let effective_open = self
            .open_drawer
            .filter(|g| g.has_content(node_sel_count, clip_sel_count));
        let drawer_open = effective_open.is_some();
        let anim_time = if self.prefs.reduced_motion { 0.0 } else { 0.18 };
        let t = ctx.animate_bool_with_time_and_easing(
            egui::Id::new("drawer_width_anim"),
            drawer_open,
            anim_time,
            egui::emath::easing::cubic_out,
        );
        // Render the open group, or — during the close tween — the last one open.
        let render_group = effective_open.unwrap_or(self.last_drawer_group);
        let target_w = self.prefs.drawer_width.clamp(160.0, 420.0);
        if t > 0.001 {
            let fully_open = drawer_open && t >= 0.999;
            // Float the drawer as a rounded card, detached from the rail.
            // Prior work (#168 closed a dead band, #186 restored a small inner
            // gutter) treated the drawer as a flush-docked panel. Joseph wants
            // it to read as a floating card instead: a couple-pixel gap from
            // the rail on the left, a little breathing room from the canvas on
            // the right, and top/bottom padding so all four corners are visibly
            // rounded against the darker window background. The rail itself
            // stays flush with the window's left edge.
            //
            // `outer_margin` is the float (transparent, shows the window fill
            // behind); `inner_margin` is the content gutter (bumped on top so
            // the header clears the rounded top corners); a 1 px non-interactive
            // border makes the rounded edge crisp against the low-contrast gap.
            let drawer_frame = {
                let mut f = egui::Frame::side_top_panel(&ctx.style());
                f.inner_margin = egui::Margin {
                    left: DRAWER_PAD_X,
                    right: DRAWER_PAD_X,
                    top: DRAWER_PAD_Y,
                    bottom: DRAWER_PAD_Y,
                };
                f.outer_margin = egui::Margin {
                    left: DRAWER_GAP,
                    right: DRAWER_FLOAT_X,
                    top: CARD_FLOAT_Y + self.menu_drawer_height,
                    bottom: CARD_FLOAT_Y,
                };
                f.rounding = egui::Rounding::same(CARD_ROUNDING);
                f.stroke = ctx.style().visuals.widgets.noninteractive.bg_stroke;
                f
            };
            // Hold the panel at the user's width; content is never allowed to
            // widen it (see `pin_side_panel_width`).
            let drawer_id = egui::Id::new("properties");
            let dragging_width = pin_side_panel_width(
                ctx,
                drawer_id,
                if fully_open {
                    target_w
                } else {
                    (target_w * t).max(1.0)
                },
            );
            let mut panel = egui::SidePanel::left("properties")
                .frame(drawer_frame)
                // No default separator line — the card's own border defines its edge.
                .show_separator_line(false);
            panel = if fully_open {
                // Fully open: let the user drag-resize within range.
                panel
                    .resizable(true)
                    .min_width(160.0)
                    .max_width(420.0)
                    .default_width(target_w)
            } else {
                // Mid-tween: drive an exact eased width (no resize handle).
                panel.resizable(false).exact_width((target_w * t).max(1.0))
            };
            let resp = panel.show(ctx, |ui| {
                // Fill the panel. `Frame` sizes itself to its *content*, so a
                // drawer whose widest row is narrower than the panel painted a
                // card that stopped short of the panel's right edge — leaving a
                // wide unpainted band between the card and the canvas. (Before
                // the width was pinned this hid itself, because egui fed that
                // short frame rect back as the panel's next width and the panel
                // just crept narrower every frame instead.)
                // Use `max_rect` (not `available_width`) so the first widget in
                // the drawer can't shrink the reported width before we pin it.
                let fill = ui.max_rect().size();
                ui.set_min_size(fill);
                // Cross-fade the content with the slide (alpha tracks the eased
                // width factor) so the transition clearly reads as an animation
                // rather than a pop.
                ui.set_opacity(t);
                // Both axes scrollable, `auto_shrink` off on both. A scroll
                // area only bounds an axis it can actually scroll: a
                // `vertical()` one still reports its *content's* width, which is
                // the whole problem here. `auto_shrink` off on BOTH axes is what actually pins the
                // drawer's width. A `Frame` reports its content's size, and a
                // `SidePanel` re-reads that as its own width next frame — so a
                // single intrinsically-wide row (a long media filename plus its
                // metadata is ~570 px) drags the whole panel out to match,
                // overriding the width the user picked. A scroll area with
                // `auto_shrink[0] == false` reports the viewport width instead
                // of the content width, which severs that feedback loop; content
                // wider than the drawer now scrolls rather than widening it.
                // Disable drag-to-scroll while the speed-ramp handles are
                // being dragged — otherwise `ScrollArea::both` steals the
                // pointer and the curve never tracks the mouse.
                let speed_dragging = ui
                    .ctx()
                    .data(|d| d.get_temp::<bool>(egui::Id::new("speed_curve_dragging")))
                    .unwrap_or(false);
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new("speed_curve_dragging"), false);
                });
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .drag_to_scroll(!speed_dragging)
                    .show(ui, |ui| {
                        if render_group == DrawerGroup::Tools {
                            // Tools drawer: render the tool palette + apply selection.
                            if let Some(tool) = panels::draw_tools_panel(
                                ui,
                                self.active_tool,
                                &self.prefs.pinned_tools,
                            ) {
                                self.pen_points.clear();
                                self.pencil_points.clear();
                                self.lasso_points.clear();
                                self.isolated_group = None;
                                self.clear_point_edit();
                                self.active_tool = tool;
                                if tool != Tool::Select
                                    && tool != Tool::DirectSelect
                                    && tool != Tool::ProportionalMove
                                {
                                    self.selected_id = None;
                                    doc.selection.clear();
                                }
                            }
                            return;
                        }
                        self.draw_property_drawer_content(ui, doc, history, render_group);
                    });
            });
            // Capture a user resize of the fully-open drawer so it persists
            // (in-memory now; flushed to disk on the next toggle/close). Only a
            // real handle drag counts — otherwise a content-driven width bump
            // would be written back as if the user had asked for it.
            if fully_open && dragging_width {
                let w = resp.response.rect.width();
                if (w - self.prefs.drawer_width).abs() > 0.5 {
                    self.prefs.drawer_width = w;
                }
            }
        }

        // ── Right icon rail (mirror of the left drawer rail) ──────────────────
        // Flush with the window's right edge; its LEFT corners round and it floats
        // on the left/top/bottom. Icons toggle the right-side drawers (Layers, AI
        // Chat) that used to be stacked together in the always-on right panel.
        // Created before the right drawer so it stays the outermost (rightmost)
        // panel — the mirror of the left rail being the leftmost.
        let right_rail_frame = {
            let mut f = egui::Frame::side_top_panel(&ctx.style());
            f.inner_margin = egui::Margin {
                left: RAIL_PAD_X,
                right: RAIL_PAD_X,
                top: RAIL_PAD_Y,
                bottom: RAIL_PAD_Y,
            };
            f.outer_margin = egui::Margin {
                left: RAIL_GAP,
                right: 0.0,
                top: CARD_FLOAT_Y,
                bottom: CARD_FLOAT_Y,
            };
            // Round only the two left corners; the right edge is the window edge.
            f.rounding = egui::Rounding {
                nw: CARD_ROUNDING,
                ne: 0.0,
                sw: CARD_ROUNDING,
                se: 0.0,
            };
            f.stroke = ctx.style().visuals.widgets.noninteractive.bg_stroke;
            f
        };
        egui::SidePanel::right("right_rail")
            .resizable(false)
            .exact_width(RAIL_WIDTH)
            .frame(right_rail_frame)
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                    for group in RightDrawerGroup::all_for_mode(self.mode).iter().copied() {
                        // The mixer is the one group that does not fit a drawer
                        // (see `audio_mixer_window_open`); its rail button toggles
                        // the floating window instead, keeping the entry point
                        // exactly where it has always been.
                        let is_mixer = group == RightDrawerGroup::AudioMixer;
                        let active = if is_mixer {
                            self.audio_mixer_window_open
                        } else {
                            self.open_right_drawer == Some(group)
                        };
                        let resp = ui
                            .add(
                                egui::Button::new(RichText::new(group.icon()).size(18.0))
                                    .min_size(egui::vec2(RAIL_ICON, RAIL_ICON))
                                    .selected(active),
                            )
                            .on_hover_text(group.title());
                        if resp.clicked() {
                            if is_mixer {
                                self.audio_mixer_window_open = !active;
                            } else {
                                self.open_right_drawer = if active { None } else { Some(group) };
                                if let Some(g) = self.open_right_drawer {
                                    self.last_right_drawer_group = g;
                                }
                                self.prefs.open_right_drawer = self.open_right_drawer;
                                self.prefs.save();
                            }
                        }
                        ui.add_space(4.0);
                    }
                });
            });

        // ── Right animated drawer (mirror of the left drawer) ─────────────────
        let right_open = self.open_right_drawer;
        let right_drawer_open = right_open.is_some();
        let r_anim = if self.prefs.reduced_motion { 0.0 } else { 0.18 };
        let rt = ctx.animate_bool_with_time_and_easing(
            egui::Id::new("right_drawer_width_anim"),
            right_drawer_open,
            r_anim,
            egui::emath::easing::cubic_out,
        );
        let right_render_group = right_open.unwrap_or(self.last_right_drawer_group);
        let right_target_w = self.prefs.right_drawer_width.clamp(220.0, 480.0);
        if rt > 0.001 {
            let fully_open = right_drawer_open && rt >= 0.999;
            let right_drawer_frame = {
                let mut f = egui::Frame::side_top_panel(&ctx.style());
                f.inner_margin = egui::Margin {
                    left: DRAWER_PAD_X,
                    right: DRAWER_PAD_X,
                    top: DRAWER_PAD_Y,
                    bottom: DRAWER_PAD_Y,
                };
                // Mirror of the left drawer: the rail is on the RIGHT here and the
                // canvas on the LEFT, so the rail-side gap is on the right.
                f.outer_margin = egui::Margin {
                    left: DRAWER_FLOAT_X,
                    right: DRAWER_GAP,
                    top: CARD_FLOAT_Y,
                    bottom: CARD_FLOAT_Y,
                };
                f.rounding = egui::Rounding::same(CARD_ROUNDING);
                f.stroke = ctx.style().visuals.widgets.noninteractive.bg_stroke;
                f
            };
            let right_dragging_width = pin_side_panel_width(
                ctx,
                egui::Id::new("right_properties"),
                if fully_open {
                    right_target_w
                } else {
                    (right_target_w * rt).max(1.0)
                },
            );
            let mut panel = egui::SidePanel::right("right_properties")
                .frame(right_drawer_frame)
                .show_separator_line(false);
            panel = if fully_open {
                panel
                    .resizable(true)
                    .min_width(220.0)
                    .max_width(480.0)
                    .default_width(right_target_w)
            } else {
                panel
                    .resizable(false)
                    .exact_width((right_target_w * rt).max(1.0))
            };
            // Track history so a grade edit committed inside the drawer (via
            // SetGrade → CommandHistory) marks the document dirty.
            let right_rev_before = history.revision();
            let resp = panel.show(ctx, |ui| {
                // Fill the panel — see the left drawer for why.
                let fill = ui.max_rect().size();
                ui.set_min_size(fill);
                ui.set_opacity(rt);
                match right_render_group {
                    RightDrawerGroup::Layers => {
                        // The panel owns its own layout (scrolling tree + pinned
                        // footer), so it must not be wrapped in a ScrollArea.
                        if let Some(action) = panels::draw_layers_panel(
                            ui,
                            doc,
                            &mut self.selected_layer_ids,
                            self.selected_id,
                        ) {
                            self.pending_panel_actions.push(action);
                        }
                    }
                    RightDrawerGroup::Chat => {
                        self.draw_claude_tab(ui);
                    }
                    RightDrawerGroup::ColorControls => {
                        // Grade edits commit straight through `history`
                        // (SetGrade → CommandHistory); disjoint `self` field
                        // borrows keep this off `video_panel_ui`'s whole-self loan.
                        panels::video::color_page::draw_color_controls(
                            ui,
                            doc,
                            history,
                            &mut self.pending_panel_actions,
                            &self.timeline_selection,
                            &mut self.selected_grade_op,
                            &mut self.color_page_tab,
                            &mut self.scopes_panel_open,
                        );
                    }
                    // Hosted in a floating window instead — a stale
                    // `open_right_drawer` from an older build is migrated away
                    // in `PhotonicApp::new`, so this arm draws nothing.
                    RightDrawerGroup::AudioMixer => {}
                    RightDrawerGroup::History => {
                        egui::ScrollArea::vertical()
                            .id_salt("right_history_scroll")
                            .show(ui, |ui| {
                                self.draw_property_drawer_content(
                                    ui,
                                    doc,
                                    history,
                                    DrawerGroup::History,
                                );
                            });
                    }
                    // A group written by a newer build: normalized to the
                    // default at load, so this renders nothing rather than
                    // showing an empty right drawer.
                    RightDrawerGroup::Unknown => {}
                }
            });
            if fully_open && right_dragging_width {
                let w = resp.response.rect.width();
                if (w - self.prefs.right_drawer_width).abs() > 0.5 {
                    self.prefs.right_drawer_width = w;
                }
            }
            if history.revision() != right_rev_before {
                doc_modified = true;
            }
        }

        // ── Console panel ────────────────────────────────────────────────────
        // Changing the panel ID when toggling expanded forces egui to reset
        // its stored height to the new default_height.
        let (panel_id, default_h, min_h) = if self.lua_console.expanded {
            ("console_expanded", 480.0, 300.0)
        } else {
            ("console", 220.0, 120.0)
        };
        egui::TopBottomPanel::bottom(panel_id)
            .resizable(true)
            .default_height(default_h)
            .min_height(min_h)
            .show_animated(ctx, self.lua_console.visible, |ui| {
                self.draw_console(ui);
            });

        // ── Timeline panel (video mode only, 04 §1.1/§2) ───────────────────────
        // Docked below the console panel, above the (in video mode,
        // program-monitor) CentralPanel — panel registration order is
        // egui-stacking order, so this must land after the console panel's
        // `show_animated` and before `CentralPanel::show`.
        if self.mode == AppMode::Video {
            egui::TopBottomPanel::bottom("timeline")
                .resizable(true)
                .default_height(220.0)
                .min_height(120.0)
                .show(ctx, |ui| {
                    self.draw_timeline_panel(ui, doc, history);
                });
        }

        // ── Audio mixer (floating window, 09) ────────────────────────────────
        // A rack of 88 px channel strips plus the master bus needs far more
        // room than the right drawer can give, so the mixer floats. Foreground
        // order keeps it above every panel (it is a monitoring surface — it has
        // to stay legible while you drive the timeline underneath it), and both
        // axes scroll so a narrow app window clips nothing.
        if self.mode == AppMode::Video && self.audio_mixer_window_open {
            let mut open = true;
            egui::Window::new(format!("{}  Audio Mixer", ph::SLIDERS))
                .id(egui::Id::new("audio_mixer_window"))
                .order(egui::Order::Foreground)
                .open(&mut open)
                .collapsible(true)
                .resizable(true)
                .default_size(egui::vec2(620.0, 460.0))
                .min_width(260.0)
                .min_height(240.0)
                .default_pos(ctx.screen_rect().center() - egui::vec2(310.0, 230.0))
                .show(ctx, |ui| {
                    let mut vid = self.video_panel_ui();
                    panels::video::audio_mixer::draw_audio_mixer(ui, &mut vid);
                });
            self.audio_mixer_window_open = open;
        }

        // ── Audit panel (floating window) ────────────────────────────────────
        if self.audit.panel_open {
            panels::draw_audit_panel(
                ctx,
                &self.audit.log,
                &mut self.audit.panel_open,
                &mut self.audit.filter,
            );
        }

        // ── Video floating panels (04 §4.1) ──────────────────────────────────
        // The scopes panel (07 §6) and the video export dialog (05 §3) are
        // floating windows, not drawers. Gated on their session flags (both
        // default-off, so vector mode and untouched video mode are unchanged).
        if self.mode == AppMode::Video {
            if self.scopes_panel_open {
                // GPU scopes run over the engine's K-E2 scope tap (the clip's
                // post-grade / pre-fold texture, or the program pre-caption) via
                // `photonic_render::scopes` (07 §6 / 03 §3.6). The engine shares
                // the renderer's device (02 §1), so the same device/queue read it.
                let frame = self
                    .engine
                    .as_ref()
                    .and_then(|b| b.session().latest_frame());
                // K-E1: publish latest master-bus spectrum for the Spectrum scope.
                if let Some(spec) = self
                    .engine
                    .as_ref()
                    .and_then(|b| b.session().status().spectrum_db.clone())
                {
                    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ke1_spectrum_db"), spec));
                }
                let device = renderer.device_arc();
                let queue = renderer.queue_arc();
                let want = panels::video::color_page::draw_scopes_panel(
                    ctx,
                    &device,
                    &queue,
                    frame.as_deref(),
                    doc,
                    &self.timeline_selection,
                    &mut self.scopes_panel_open,
                    &mut self.scope_kind,
                );
                // Stateless resend: the engine echoes the tap it was asked for on
                // `EngineStatus`, so comparing against that sends one command per
                // real change instead of one per frame.
                if let Some(bridge) = self.engine.as_ref() {
                    if bridge.session().status().scope_tap != want {
                        bridge
                            .session()
                            .send(photonic_video::EngineCmd::SetScopeTap(want));
                    }
                }
            }
            // K-A6 Edit Duration floating form (position / in / out / duration +
            // ripple). Drawn next to the export dialog so both can share the
            // post-timeline borrow window on `doc`/`history`.
            panels::video::duration_dialog::draw_edit_duration_dialog(
                ctx,
                doc,
                history,
                &mut self.edit_duration_dialog,
            );
            if self.export_dialog_open {
                // Widened from the skeleton's `(ctx, vid)` stub — `VideoPanelUi`
                // carries no `doc`/history handle (unlike the timeline panel's
                // call site just above), and a real preset/format/range picker
                // needs both; same "necessary minimal call-site growth" move
                // already used for `draw_timeline_panel`. `vid`'s borrow of
                // `self` ends when this call returns, so reaching into
                // `self.engine` right after for the real `EngineCmd::Export`
                // send is a plain sequential borrow, not a conflict.
                let mut vid = self.video_panel_ui();
                let jobs =
                    panels::video::export_dialog::draw_export_dialog(ctx, &mut vid, doc, history);
                if !jobs.is_empty() {
                    if jobs.len() == 1 {
                        // Single job: the existing engine export path (progress
                        // on EngineStatus.export).
                        if let Some(bridge) = self.engine.as_ref() {
                            bridge
                                .session()
                                .send(photonic_video::EngineCmd::Export(Box::new(
                                    jobs.into_iter().next().unwrap(),
                                )));
                        }
                    } else if let Some(bridge) = self.engine.as_ref() {
                        // Multi-format / marker multi-export (K-F1/F2): freeze
                        // the project and drain via the shared render queue.
                        if let Ok(tools) = photonic_video::media::ffmpeg_locate::locate() {
                            self.render_queue.ensure_worker(bridge.gpu().clone(), tools);
                            if let Some(project) = doc.timeline.clone() {
                                for job in jobs {
                                    let label = job
                                        .output
                                        .file_name()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| "export".into());
                                    let _ = self.render_queue.enqueue(label, project.clone(), job);
                                }
                                // Surface the queue inspector when multi-job lands.
                                self.render_queue_panel_open = true;
                            }
                        }
                    }
                }
            }
            if self.render_queue_panel_open {
                panels::video::render_queue_panel::draw_render_queue_panel(
                    ctx,
                    &mut self.render_queue_panel_open,
                    &self.render_queue,
                );
            }
        }

        // ── Central canvas area ──────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                self.last_canvas_rect = Some(rect);
                {
                    let ppp = ui.ctx().pixels_per_point();
                    let x = (rect.min.x * ppp).floor().max(0.0) as u32;
                    let y = (rect.min.y * ppp).floor().max(0.0) as u32;
                    let w = (rect.width() * ppp).ceil().max(0.0) as u32;
                    let h = (rect.height() * ppp).ceil().max(0.0) as u32;
                    let px = Some((x, y, w, h));
                    // The host clips the document to this, but it only learns
                    // about it on the *next* frame — so the frame a panel opens,
                    // closes, or is dragged, the document is still clipped to the
                    // old viewport. Left alone that would be permanent, not
                    // transient: egui stops repainting the moment the UI settles,
                    // so the mis-clipped frame is the one left on screen. Asking
                    // for one more frame whenever the viewport moves guarantees
                    // the last frame drawn is always a correctly clipped one.
                    if px != self.last_canvas_rect_px {
                        ui.ctx().request_repaint();
                    }
                    self.last_canvas_rect_px = px;
                }
                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

                // ── Deferred fit-to-viewport on new/open ─────────────────────
                // Runs now that the real viewport `rect` is known, so artwork
                // lands properly scaled in the visible canvas (not the window).
                if self.fit_pending {
                    self.fit_pending = false;
                    fit_artboard_to_rect(view, rect, doc.artboards_bounds());
                    self.smooth.log_zoom_target = view.zoom.ln();
                }

                // ── Interactive raster brush / eraser ────────────────────────
                if matches!(self.active_tool, Tool::RasterBrush | Tool::RasterEraser) {
                    self.handle_raster_brush(&response, doc, view, history);
                }

                // ── Cursor coordinate overlay (Info Panel) ───────────────────
                // Vector-editing affordance only: in video mode this same canvas
                // rect is the program monitor, whose bottom edge holds the
                // transport/player controls (04 §3.2). Painting the X/Y readout
                // there covered the players — so it is drawn in Vector mode only.
                if let Some(cursor_screen) = ui.input(|i| i.pointer.hover_pos()) {
                    if self.mode == AppMode::Vector && rect.contains(cursor_screen) {
                        let (cx, cy) =
                            view.screen_to_canvas(cursor_screen.x as f64, cursor_screen.y as f64);
                        let coord_text = format!("  X: {:.1}  Y: {:.1}  ", cx, cy);
                        let fg_painter = ctx.layer_painter(egui::LayerId::new(
                            egui::Order::Foreground,
                            egui::Id::new("cursor_coords_overlay"),
                        ));
                        let text_color = if self.prefs.dark_mode {
                            egui::Color32::from_rgba_unmultiplied(220, 220, 220, 200)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(30, 30, 30, 200)
                        };
                        let bg_color = if self.prefs.dark_mode {
                            egui::Color32::from_rgba_unmultiplied(20, 20, 30, 160)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(240, 240, 250, 160)
                        };
                        let font = egui::FontId::monospace(11.0);
                        let text_pos = rect.min + egui::vec2(4.0, rect.height() - 20.0);
                        let galley = ctx.fonts(|f| f.layout_no_wrap(coord_text, font, text_color));
                        let text_rect = egui::Rect::from_min_size(text_pos, galley.size());
                        fg_painter.rect_filled(text_rect.expand(2.0), 2.0, bg_color);
                        fg_painter.galley(text_pos, galley, text_color);
                    }
                }

                // ── Video-mode program monitor (04 §1.1 point 3, §3) ─────────
                // D-02: same canvas rect, same fit/zoom/pan session state —
                // reused, not duplicated. Branches here and returns for the
                // rest of this frame's central-panel content: every block
                // below (raster/preview overlays, outline mode, grid,
                // gradient handles, the tool if-chain, and the space-pan/
                // arrow-nudge/WASD-pan blocks §5.2 calls out as colliding
                // with video-mode keys) is a *vector-canvas* concern and is
                // skipped this frame instead of being individually wrapped —
                // the same "gate vector-canvas input by mode, dispatch video.*
                // as a sibling block at the same call site" effect as one
                // early exit, at far lower risk than threading if/else
                // through the ~2000-line vector tool if-chain below.
                if self.mode == AppMode::Video {
                    debug_assert!(
                        doc.timeline.is_some(),
                        "AppMode::Video requires doc.timeline once entered (04 \
                         §1.3) — every entry path must call \
                         ensure_timeline_project[_with] first"
                    );
                    // Central-panel content state (08 §6.1): the node canvas
                    // replaces the program monitor while a composition is being
                    // edited; otherwise the monitor draws as before. Node-canvas
                    // entry/escape is wired by the node-editor (08) story.
                    if self.node_canvas_active {
                        let mut vid = self.video_panel_ui();
                        panels::video::node_editor::draw_node_canvas(
                            ui, rect, doc, history, &mut vid,
                        );
                    } else {
                        self.draw_video_monitor(ui, ctx, rect, doc, history);
                    }
                    return;
                }

                // ── Raster (pixel) layers ──────────────────────────────────────
                // Painted over the GPU-rendered vector layer as textured quads,
                // matching the headless export compositor. Skipped in outline mode
                // and in Pixel/Overprint Preview (those re-composite via the
                // headless render, so the live overlay would double up).
                if !self.outline_mode && !self.preview_active() {
                    let raster_painter = ui.painter_at(rect);
                    self.paint_raster_nodes(ctx, &raster_painter, doc, view);
                }

                // ── Isolation Mode: dim everything outside the isolated group ──
                self.paint_isolation_scrim(ui, doc, view, rect);

                // ── Pixel / Overprint Preview overlay (#22) ────────────────────
                // Overlay the active artboard with a nearest-sampled export-
                // resolution render so true aliasing/pixel snapping (Pixel
                // Preview) and overprint-ink multiply (Overprint Preview) show.
                if self.preview_active() {
                    let preview_painter = ui.painter_at(rect);
                    self.paint_preview_overlay(ctx, &preview_painter, doc, view);
                }

                // ── Outline Mode overlay ──────────────────────────────────────
                // Cover GPU-rendered geometry with a flat background, then draw
                // all visible path nodes as 1 px wireframe strokes.
                if self.outline_mode {
                    let painter = ui.painter_at(rect);
                    let bg = if self.prefs.dark_mode {
                        egui::Color32::from_rgb(28, 28, 40)
                    } else {
                        egui::Color32::WHITE
                    };
                    painter.rect_filled(rect, 0.0, bg);

                    // Draw artboard boundary.
                    let (ax0, ay0) = view.canvas_to_screen(0.0, 0.0);
                    let (ax1, ay1) = view.canvas_to_screen(doc.width, doc.height);
                    painter.rect_stroke(
                        egui::Rect::from_min_max(
                            egui::pos2(ax0 as f32, ay0 as f32),
                            egui::pos2(ax1 as f32, ay1 as f32),
                        ),
                        0.0,
                        egui::Stroke::new(1.0, egui::Color32::from_gray(128)),
                    );

                    // Draw each visible path node as a 1 px wireframe.
                    let outline_color = if self.prefs.dark_mode {
                        egui::Color32::from_rgb(180, 180, 210)
                    } else {
                        egui::Color32::from_rgb(30, 30, 60)
                    };
                    let outline_stroke = egui::Stroke::new(1.0, outline_color);
                    for node in doc.nodes.values() {
                        if !node.visible {
                            continue;
                        }
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            let subpaths = bez_to_screen_subpaths_xf(
                                &pn.path_data.to_bez_path(),
                                view,
                                &node.transform,
                            );
                            for (pts, closed) in subpaths {
                                if pts.len() >= 2 {
                                    painter.add(egui::Shape::Path(egui::epaint::PathShape {
                                        points: pts,
                                        closed,
                                        fill: egui::Color32::TRANSPARENT,
                                        stroke: egui::epaint::PathStroke::new(
                                            outline_stroke.width,
                                            outline_stroke.color,
                                        ),
                                    }));
                                }
                            }
                        }
                    }
                }

                // ── Simplify Path live preview overlay (#166) ─────────────────
                // While the Simplify dialog is open, paint the simplified result
                // as a non-destructive accent wireframe (plus anchor dots) over
                // the artwork so the tolerance can be judged before Apply. The
                // simplified PathData is cached per-tolerance, so Ramer-Douglas-
                // Peucker runs only when the tolerance changes, not every frame.
                if let Some(dlg) = self.simplify_dialog.as_mut() {
                    if let Some(node) = doc.nodes.get(&dlg.node_id) {
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            dlg.refresh(&pn.path_data);
                            if let Some(preview) = &dlg.preview {
                                let bez = preview.to_bez_path();
                                // Smooth wireframe: sample curves so fitted
                                // Béziers render as curves, not chords.
                                let subpaths =
                                    bez_to_screen_subpaths_xf(&bez, view, &node.transform);
                                if subpaths.iter().any(|(pts, _)| pts.len() >= 2) {
                                    let painter = ui.painter_at(rect);
                                    let accent = egui::Color32::from_rgb(110, 86, 207);
                                    for (pts, closed) in subpaths {
                                        if pts.len() >= 2 {
                                            painter.add(egui::Shape::Path(
                                                egui::epaint::PathShape {
                                                    points: pts,
                                                    closed,
                                                    fill: egui::Color32::TRANSPARENT,
                                                    stroke: egui::epaint::PathStroke::new(
                                                        1.5, accent,
                                                    ),
                                                },
                                            ));
                                        }
                                    }
                                    // Dots at real anchor points only (not every
                                    // sampled point along a curve).
                                    for p in anchor_screen_points_xf(&bez, view, &node.transform) {
                                        painter.circle_filled(p, 2.0, accent);
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Merge Vertices live preview overlay (#189) ────────────────
                // While the Merge Vertices dialog is open, paint the welded
                // result as a non-destructive accent wireframe (plus anchor dots)
                // over the artwork so the threshold can be judged before Apply.
                // The welded PathData is cached per-threshold, so welding runs
                // only when the threshold changes, not every frame.
                if let Some(dlg) = self.merge_vertices_dialog.as_mut() {
                    if let Some(node) = doc.nodes.get(&dlg.node_id) {
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            if dlg.preview.is_none() || dlg.cached_thr != dlg.threshold {
                                dlg.preview =
                                    Some(photonic_core::ops::merge::merge_vertices_by_distance(
                                        &pn.path_data,
                                        dlg.threshold,
                                    ));
                                dlg.cached_thr = dlg.threshold;
                            }
                            if let Some(preview) = &dlg.preview {
                                let subpaths = bez_to_screen_subpaths_xf(
                                    &preview.to_bez_path(),
                                    view,
                                    &node.transform,
                                );
                                if subpaths.iter().any(|(pts, _)| pts.len() >= 2) {
                                    let painter = ui.painter_at(rect);
                                    let accent = egui::Color32::from_rgb(86, 170, 207);
                                    for (pts, closed) in subpaths {
                                        if pts.len() >= 2 {
                                            painter.add(egui::Shape::Path(
                                                egui::epaint::PathShape {
                                                    points: pts.clone(),
                                                    closed,
                                                    fill: egui::Color32::TRANSPARENT,
                                                    stroke: egui::epaint::PathStroke::new(
                                                        1.5, accent,
                                                    ),
                                                },
                                            ));
                                            for p in &pts {
                                                painter.circle_filled(*p, 2.0, accent);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Grid overlay ─────────────────────────────────────────────
                if self.prefs.show_grid {
                    let grid_screen_size = self.prefs.grid_size as f64 * view.zoom;
                    if grid_screen_size >= 4.0 {
                        let painter = ui.painter_at(rect);
                        let [gr, gg, gb, ga] = self.prefs.grid_color;
                        let color =
                            egui::Color32::from(egui::Rgba::from_rgba_unmultiplied(gr, gg, gb, ga));
                        let stroke = egui::Stroke::new(1.0, color);
                        let g = self.prefs.grid_size as f64;
                        let (cx0, cy0) =
                            view.screen_to_canvas(rect.min.x as f64, rect.min.y as f64);
                        let (cx1, cy1) =
                            view.screen_to_canvas(rect.max.x as f64, rect.max.y as f64);
                        // Vertical lines
                        let mut cx = (cx0 / g).floor() * g;
                        while cx <= cx1 {
                            cx += g;
                            let (sx, _) = view.canvas_to_screen(cx, 0.0);
                            painter.line_segment(
                                [
                                    egui::pos2(sx as f32, rect.min.y),
                                    egui::pos2(sx as f32, rect.max.y),
                                ],
                                stroke,
                            );
                        }
                        // Horizontal lines
                        let mut cy = (cy0 / g).floor() * g;
                        while cy <= cy1 {
                            cy += g;
                            let (_, sy) = view.canvas_to_screen(0.0, cy);
                            painter.line_segment(
                                [
                                    egui::pos2(rect.min.x, sy as f32),
                                    egui::pos2(rect.max.x, sy as f32),
                                ],
                                stroke,
                            );
                        }
                    }
                }

                // ── Icon keyline template (#208) ─────────────────────────────
                // Draws the classic Material/Apple keyline safe-area shapes —
                // square, circle, portrait & landscape rects — centered on the
                // artboard, treating the whole artboard as the icon box. Gives
                // deterministic optical alignment for a set of icons.
                if self.prefs.show_keyline_grid {
                    let painter = ui.painter_at(rect);
                    let key_color = egui::Color32::from_rgba_unmultiplied(255, 120, 0, 150);
                    let ks = egui::Stroke::new(1.0, key_color);
                    let dw = doc.width;
                    let dh = doc.height;
                    let (cx, cy) = (dw / 2.0, dh / 2.0);
                    // Material keyline ratios on the 24-grid icon box (÷24).
                    let sq = 18.0 / 24.0; // square keyline
                    let circ = 20.0 / 24.0; // circle keyline diameter
                    let long = 20.0 / 24.0; // portrait/landscape long side
                    let short = 16.0 / 24.0; // portrait/landscape short side
                    let to_screen = |x: f64, y: f64| {
                        let (sx, sy) = view.canvas_to_screen(x, y);
                        egui::pos2(sx as f32, sy as f32)
                    };
                    let centered_rect = |fw: f64, fh: f64| {
                        let hw = dw * fw / 2.0;
                        let hh = dh * fh / 2.0;
                        egui::Rect::from_two_pos(
                            to_screen(cx - hw, cy - hh),
                            to_screen(cx + hw, cy + hh),
                        )
                    };
                    // Square keyline.
                    painter.rect_stroke(centered_rect(sq, sq), 0.0, ks);
                    // Portrait & landscape keylines.
                    painter.rect_stroke(centered_rect(short, long), 2.0, ks);
                    painter.rect_stroke(centered_rect(long, short), 2.0, ks);
                    // Circle keyline (use the smaller artboard dimension for radius).
                    let r = (dw.min(dh) * circ / 2.0 * view.zoom) as f32;
                    painter.circle_stroke(to_screen(cx, cy), r, ks);
                    // Center cross-hairs.
                    painter.line_segment(
                        [to_screen(cx, 0.0), to_screen(cx, dh)],
                        egui::Stroke::new(1.0, key_color.gamma_multiply(0.5)),
                    );
                    painter.line_segment(
                        [to_screen(0.0, cy), to_screen(dw, cy)],
                        egui::Stroke::new(1.0, key_color.gamma_multiply(0.5)),
                    );
                }

                // ── Ruler strips ─────────────────────────────────────────────
                if self.prefs.show_rulers {
                    let painter = ui.painter_at(rect);
                    let ruler_h = 18.0f32;
                    let bg = if self.prefs.dark_mode {
                        egui::Color32::from_rgb(19, 19, 31)
                    } else {
                        egui::Color32::from_rgb(234, 228, 255)
                    };
                    let tick_col = egui::Color32::from_gray(140);
                    // Ruler backgrounds
                    painter.rect_filled(
                        egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), ruler_h)),
                        0.0,
                        bg,
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_size(rect.min, egui::vec2(ruler_h, rect.height())),
                        0.0,
                        bg,
                    );
                    // Choose tick interval to keep ticks ~50px apart on screen
                    let raw = 50.0 / view.zoom;
                    let mag = 10.0f64.powf(raw.log10().floor());
                    let n = raw / mag;
                    let tick = if n < 2.0 {
                        mag
                    } else if n < 5.0 {
                        2.0 * mag
                    } else {
                        5.0 * mag
                    };
                    // Horizontal ruler ticks
                    let (cx0, _) = view.screen_to_canvas(rect.min.x as f64, 0.0);
                    let (cx1, _) = view.screen_to_canvas(rect.max.x as f64, 0.0);
                    let mut c = (cx0 / tick).floor() * tick;
                    while c <= cx1 {
                        let (sx, _) = view.canvas_to_screen(c, 0.0);
                        let sx = sx as f32;
                        if sx > rect.min.x + ruler_h {
                            painter.line_segment(
                                [
                                    egui::pos2(sx, rect.min.y + ruler_h - 5.0),
                                    egui::pos2(sx, rect.min.y + ruler_h),
                                ],
                                egui::Stroke::new(1.0, tick_col),
                            );
                            painter.text(
                                egui::pos2(sx + 2.0, rect.min.y + 2.0),
                                egui::Align2::LEFT_TOP,
                                self.format_ruler_value(c),
                                egui::FontId::proportional(9.0),
                                tick_col,
                            );
                        }
                        c += tick;
                    }
                    // Vertical ruler ticks
                    let (_, cy0) = view.screen_to_canvas(0.0, rect.min.y as f64);
                    let (_, cy1) = view.screen_to_canvas(0.0, rect.max.y as f64);
                    let mut c = (cy0 / tick).floor() * tick;
                    while c <= cy1 {
                        let (_, sy) = view.canvas_to_screen(0.0, c);
                        let sy = sy as f32;
                        if sy > rect.min.y + ruler_h {
                            painter.line_segment(
                                [
                                    egui::pos2(rect.min.x + ruler_h - 5.0, sy),
                                    egui::pos2(rect.min.x + ruler_h, sy),
                                ],
                                egui::Stroke::new(1.0, tick_col),
                            );
                            painter.text(
                                egui::pos2(rect.min.x + 1.0, sy + 1.0),
                                egui::Align2::LEFT_TOP,
                                self.format_ruler_value(c),
                                egui::FontId::proportional(8.0),
                                tick_col,
                            );
                        }
                        c += tick;
                    }
                }

                // ── Ruler interaction (guides, readout, unit selector) ───────
                self.handle_ruler_interaction(ui, rect, view, doc, history);

                // ── Guide overlay ─────────────────────────────────────────────
                // Render horizontal/vertical guide lines across the canvas.
                if self.guides_visible && !doc.guides.is_empty() {
                    let painter = ui.painter_at(rect);
                    for guide in &doc.guides {
                        let default_color = egui::Color32::from_rgba_unmultiplied(0, 200, 200, 180);
                        let color = guide
                            .color
                            .map(|[r, g, b, a]| {
                                egui::Color32::from_rgba_unmultiplied(
                                    (r * 255.0) as u8,
                                    (g * 255.0) as u8,
                                    (b * 255.0) as u8,
                                    (a * 255.0) as u8,
                                )
                            })
                            .unwrap_or(default_color);
                        let stroke = egui::Stroke::new(1.0, color);
                        if let Some(angle_deg) = guide.angle_degrees {
                            // Angled construction line: draw through (position_x, position_y) at given angle.
                            let (ox, oy) =
                                view.canvas_to_screen(guide.position_x, guide.position_y);
                            let angle_rad = angle_deg.to_radians();
                            let cos_a = angle_rad.cos() as f32;
                            let sin_a = angle_rad.sin() as f32;
                            // Extend far enough to reach any screen edge.
                            let ext = (rect.width() + rect.height()) * 2.0;
                            let p1 = egui::pos2(ox as f32 - cos_a * ext, oy as f32 - sin_a * ext);
                            let p2 = egui::pos2(ox as f32 + cos_a * ext, oy as f32 + sin_a * ext);
                            painter.line_segment([p1, p2], stroke);
                        } else {
                            match guide.orientation {
                                photonic_core::GuideOrientation::Horizontal => {
                                    let (_, sy) = view.canvas_to_screen(0.0, guide.position);
                                    let sy = sy as f32;
                                    if sy >= rect.min.y && sy <= rect.max.y {
                                        painter.line_segment(
                                            [
                                                egui::pos2(rect.min.x, sy),
                                                egui::pos2(rect.max.x, sy),
                                            ],
                                            stroke,
                                        );
                                    }
                                }
                                photonic_core::GuideOrientation::Vertical => {
                                    let (sx, _) = view.canvas_to_screen(guide.position, 0.0);
                                    let sx = sx as f32;
                                    if sx >= rect.min.x && sx <= rect.max.x {
                                        painter.line_segment(
                                            [
                                                egui::pos2(sx, rect.min.y),
                                                egui::pos2(sx, rect.max.y),
                                            ],
                                            stroke,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Smart guides (object snap) overlay ────────────────────────
                // While a move drag is snapping the selection to nearby nodes,
                // draw a dashed line across the canvas at each active alignment
                // plus a small pixel-distance label (#66). Cleared on release.
                if self.prefs.snap_show_guides {
                    if let Some(snap) = &self.last_snap_result {
                        let painter = ui.painter_at(rect);
                        let color = egui::Color32::from_rgb(255, 64, 160); // magenta
                        let stroke = egui::Stroke::new(1.0, color);
                        for guide in &snap.active {
                            match guide.axis {
                                crate::snap::SnapAxis::Vertical => {
                                    let (sx, _) = view.canvas_to_screen(guide.coord, 0.0);
                                    let sx = sx as f32;
                                    if sx >= rect.min.x && sx <= rect.max.x {
                                        painter.extend(egui::Shape::dashed_line(
                                            &[
                                                egui::pos2(sx, rect.min.y),
                                                egui::pos2(sx, rect.max.y),
                                            ],
                                            stroke,
                                            5.0,
                                            4.0,
                                        ));
                                    }
                                }
                                crate::snap::SnapAxis::Horizontal => {
                                    let (_, sy) = view.canvas_to_screen(0.0, guide.coord);
                                    let sy = sy as f32;
                                    if sy >= rect.min.y && sy <= rect.max.y {
                                        painter.extend(egui::Shape::dashed_line(
                                            &[
                                                egui::pos2(rect.min.x, sy),
                                                egui::pos2(rect.max.x, sy),
                                            ],
                                            stroke,
                                            5.0,
                                            4.0,
                                        ));
                                    }
                                }
                            }
                            // Pixel-distance label near the snap, placed at the
                            // mid-point of the guide on screen.
                            let dist_px = (guide.distance * view.zoom).round() as i64;
                            if dist_px > 0 {
                                let (lx, ly) = match guide.axis {
                                    crate::snap::SnapAxis::Vertical => {
                                        let (sx, _) = view.canvas_to_screen(guide.coord, 0.0);
                                        (sx as f32 + 4.0, rect.center().y)
                                    }
                                    crate::snap::SnapAxis::Horizontal => {
                                        let (_, sy) = view.canvas_to_screen(0.0, guide.coord);
                                        (rect.center().x, sy as f32 + 4.0)
                                    }
                                };
                                painter.text(
                                    egui::pos2(lx, ly),
                                    egui::Align2::LEFT_TOP,
                                    format!("{dist_px}px"),
                                    egui::FontId::proportional(10.0),
                                    color,
                                );
                            }
                        }
                        // Equal-spacing distribution hints (#66): two gap brackets
                        // (with end ticks) + the shared px value, in orange.
                        let scol = egui::Color32::from_rgb(255, 140, 0);
                        let sstroke = egui::Stroke::new(1.5, scol);
                        for sp in &snap.spacing {
                            let gap_px = (sp.gap * view.zoom).round() as i64;
                            for seg in [sp.seg1, sp.seg2] {
                                if sp.along_x {
                                    let (x0, y) = view.canvas_to_screen(seg.0, sp.perp);
                                    let (x1, _) = view.canvas_to_screen(seg.1, sp.perp);
                                    let (x0, x1, y) = (x0 as f32, x1 as f32, y as f32);
                                    painter.line_segment(
                                        [egui::pos2(x0, y), egui::pos2(x1, y)],
                                        sstroke,
                                    );
                                    for x in [x0, x1] {
                                        painter.line_segment(
                                            [egui::pos2(x, y - 4.0), egui::pos2(x, y + 4.0)],
                                            sstroke,
                                        );
                                    }
                                    if gap_px > 0 {
                                        painter.text(
                                            egui::pos2((x0 + x1) * 0.5, y - 6.0),
                                            egui::Align2::CENTER_BOTTOM,
                                            format!("{gap_px}px"),
                                            egui::FontId::proportional(10.0),
                                            scol,
                                        );
                                    }
                                } else {
                                    let (x, y0) = view.canvas_to_screen(sp.perp, seg.0);
                                    let (_, y1) = view.canvas_to_screen(sp.perp, seg.1);
                                    let (x, y0, y1) = (x as f32, y0 as f32, y1 as f32);
                                    painter.line_segment(
                                        [egui::pos2(x, y0), egui::pos2(x, y1)],
                                        sstroke,
                                    );
                                    for y in [y0, y1] {
                                        painter.line_segment(
                                            [egui::pos2(x - 4.0, y), egui::pos2(x + 4.0, y)],
                                            sstroke,
                                        );
                                    }
                                    if gap_px > 0 {
                                        painter.text(
                                            egui::pos2(x + 6.0, (y0 + y1) * 0.5),
                                            egui::Align2::LEFT_CENTER,
                                            format!("{gap_px}px"),
                                            egui::FontId::proportional(10.0),
                                            scol,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Artboard margin overlay ───────────────────────────────────
                if self.guides_visible
                    && (doc.margin_top > 0.0
                        || doc.margin_right > 0.0
                        || doc.margin_bottom > 0.0
                        || doc.margin_left > 0.0)
                {
                    let margin_painter = ui.painter_at(rect);
                    let margin_stroke = egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(100, 180, 255, 120),
                    );
                    let (ax0, ay0) = view.canvas_to_screen(0.0, 0.0);
                    let (ax1, ay1) = view.canvas_to_screen(doc.width, doc.height);
                    let mx0 = (ax0 + doc.margin_left * view.zoom) as f32;
                    let mx1 = (ax1 - doc.margin_right * view.zoom) as f32;
                    let my0 = (ay0 + doc.margin_top * view.zoom) as f32;
                    let my1 = (ay1 - doc.margin_bottom * view.zoom) as f32;
                    if mx0 < mx1 && my0 < my1 {
                        margin_painter.rect_stroke(
                            egui::Rect::from_min_max(egui::pos2(mx0, my0), egui::pos2(mx1, my1)),
                            0.0,
                            margin_stroke,
                        );
                    }
                }

                // ── Dimension annotation overlay ──────────────────────────────
                if self.guides_visible && !doc.dimensions.is_empty() {
                    let dim_painter = ui.painter_at(rect);
                    let dim_color = egui::Color32::from_rgba_unmultiplied(255, 160, 40, 220);
                    let dim_stroke = egui::Stroke::new(1.5, dim_color);
                    for dim in &doc.dimensions {
                        let (sx0, sy0) = view.canvas_to_screen(dim.from_x, dim.from_y);
                        let (sx1, sy1) = view.canvas_to_screen(dim.to_x, dim.to_y);

                        // Perpendicular unit for offset
                        let dx = (sx1 - sx0) as f32;
                        let dy = (sy1 - sy0) as f32;
                        let len = (dx * dx + dy * dy).sqrt().max(1.0);
                        let offset_px = (dim.label_offset * view.zoom) as f32;
                        let perp_x = -dy / len * offset_px;
                        let perp_y = dx / len * offset_px;

                        let p0 = egui::pos2(sx0 as f32 + perp_x, sy0 as f32 + perp_y);
                        let p1 = egui::pos2(sx1 as f32 + perp_x, sy1 as f32 + perp_y);

                        // Main dimension line
                        dim_painter.line_segment([p0, p1], dim_stroke);

                        // Extension tick marks
                        let tick = 6.0_f32;
                        let ux = dx / len;
                        let uy = dy / len;
                        dim_painter.line_segment(
                            [
                                egui::pos2(p0.x - uy * tick, p0.y + ux * tick),
                                egui::pos2(p0.x + uy * tick, p0.y - ux * tick),
                            ],
                            dim_stroke,
                        );
                        dim_painter.line_segment(
                            [
                                egui::pos2(p1.x - uy * tick, p1.y + ux * tick),
                                egui::pos2(p1.x + uy * tick, p1.y - ux * tick),
                            ],
                            dim_stroke,
                        );

                        // Distance label at midpoint
                        let mid = egui::pos2((p0.x + p1.x) / 2.0, (p0.y + p1.y) / 2.0);
                        let dist_text = format!("{:.1}", dim.distance());
                        dim_painter.text(
                            mid + egui::vec2(-perp_x * 0.5 - 4.0, -perp_y * 0.5 - 8.0),
                            egui::Align2::CENTER_CENTER,
                            &dist_text,
                            egui::FontId::proportional(11.0),
                            dim_color,
                        );
                    }
                }

                // ── Artboards: labels, select, drag, resize, rename, add/remove ─
                let mut over_artboard_label = false;
                {
                    let accent = egui::Color32::from_rgb(110, 86, 207);
                    // Snapshot (id, name, rect) so we can mutate `doc` afterward.
                    let boards: Vec<(photonic_core::ArtboardId, String, f64, f64, f64, f64)> = doc
                        .artboards
                        .iter()
                        .map(|a| (a.id, a.name.clone(), a.x, a.y, a.width, a.height))
                        .collect();
                    let active_id = doc.active_artboard.or_else(|| boards.first().map(|b| b.0));
                    let mut select: Option<photonic_core::ArtboardId> = None;
                    let mut start_rename: Option<(photonic_core::ArtboardId, String)> = None;

                    // Show the name + handles for every artboard, including a
                    // lone one (the name is always visible / editable).
                    if !boards.is_empty() {
                        let painter = ui.painter_at(rect);
                        for (i, (id, name, x, y, w, h)) in boards.iter().enumerate() {
                            let (sx0, sy0) = view.canvas_to_screen(*x, *y);
                            let (sx1, sy1) = view.canvas_to_screen(*x + *w, *y + *h);
                            let is_active = active_id == Some(*id);
                            let col = if is_active {
                                accent
                            } else {
                                egui::Color32::from_gray(140)
                            };

                            // Label: a drag handle (left of the name, shown on
                            // hover) moves the board + its artwork; the name
                            // selects / double-click renames (text cursor).
                            let renaming_this =
                                matches!(&self.artboard_rename, Some((rid, _)) if rid == id);
                            if !renaming_this {
                                let galley = painter.layout_no_wrap(
                                    name.clone(),
                                    egui::FontId::proportional(12.0),
                                    col,
                                );
                                let handle_w = 16.0_f32;
                                let name_pos = egui::pos2(sx0 as f32 + handle_w, sy0 as f32 - 19.0);
                                let name_rect = egui::Rect::from_min_size(name_pos, galley.size());
                                let handle_rect = egui::Rect::from_min_size(
                                    egui::pos2(sx0 as f32, sy0 as f32 - 20.0),
                                    egui::vec2(handle_w, 16.0),
                                );
                                let area = handle_rect.union(name_rect).expand(3.0);
                                let hovered_area = ui
                                    .input(|i| i.pointer.hover_pos())
                                    .is_some_and(|p| area.contains(p));

                                // Name → select / rename, with a text-edit cursor.
                                let nresp = ui.interact(
                                    name_rect.expand(2.0),
                                    ui.id().with(("ab_name", i)),
                                    egui::Sense::click(),
                                );
                                if nresp.hovered() {
                                    ctx.set_cursor_icon(egui::CursorIcon::Text);
                                    over_artboard_label = true;
                                }
                                if nresp.clicked() || nresp.double_clicked() {
                                    select = Some(*id);
                                    start_rename = Some((*id, name.clone()));
                                }

                                // Drag handle → move the board and its artwork.
                                let hresp = ui.interact(
                                    handle_rect,
                                    ui.id().with(("ab_drag", i)),
                                    egui::Sense::click_and_drag(),
                                );
                                if hresp.hovered() {
                                    ctx.set_cursor_icon(egui::CursorIcon::Grab);
                                    over_artboard_label = true;
                                }
                                if hresp.drag_started() {
                                    if let Some(p) = hresp.interact_pointer_pos() {
                                        let (cx, cy) =
                                            view.screen_to_canvas(p.x as f64, p.y as f64);
                                        self.artboard_pre = Some(doc.artboards.clone());
                                        self.artboard_drag = Some(ArtboardDrag {
                                            id: *id,
                                            grab_dx: cx - *x,
                                            grab_dy: cy - *y,
                                        });
                                        select = Some(*id);
                                    }
                                }

                                painter.galley(name_pos, galley, col);
                                let dragging_this =
                                    matches!(&self.artboard_drag, Some(d) if d.id == *id);
                                if hovered_area || is_active || dragging_this {
                                    painter.text(
                                        handle_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        ph::DOTS_SIX_VERTICAL,
                                        egui::FontId::proportional(14.0),
                                        col,
                                    );
                                }
                            }

                            // Active board: border + corner resize handles.
                            if is_active {
                                painter.rect_stroke(
                                    egui::Rect::from_min_max(
                                        egui::pos2(sx0 as f32, sy0 as f32),
                                        egui::pos2(sx1 as f32, sy1 as f32),
                                    ),
                                    0.0,
                                    egui::Stroke::new(1.5, accent),
                                );
                                let corners = [
                                    (sx0, sy0, 0u8),
                                    (sx1, sy0, 1u8),
                                    (sx0, sy1, 2u8),
                                    (sx1, sy1, 3u8),
                                ];
                                for (hx, hy, hidx) in corners {
                                    let hc = egui::pos2(hx as f32, hy as f32);
                                    let hrect =
                                        egui::Rect::from_center_size(hc, egui::vec2(11.0, 11.0));
                                    let hresp = ui.interact(
                                        hrect,
                                        ui.id().with(("ab_handle", i, hidx)),
                                        egui::Sense::click_and_drag(),
                                    );
                                    if hresp.hovered() {
                                        ctx.set_cursor_icon(if hidx == 0 || hidx == 3 {
                                            egui::CursorIcon::ResizeNwSe
                                        } else {
                                            egui::CursorIcon::ResizeNeSw
                                        });
                                        over_artboard_label = true;
                                    }
                                    if hresp.drag_started() {
                                        self.artboard_pre = Some(doc.artboards.clone());
                                        self.artboard_resize = Some((*id, hidx, *x, *y, *w, *h));
                                    }
                                    painter.rect_filled(hrect.shrink(2.5), 1.0, accent);
                                    painter.rect_stroke(
                                        hrect.shrink(2.5),
                                        1.0,
                                        egui::Stroke::new(1.0, egui::Color32::WHITE),
                                    );
                                }
                            }
                        }
                    }

                    // ── Apply an in-progress drag / resize ──────────────────────
                    let pointer = ui.input(|i| i.pointer.interact_pos());
                    let down = ui.input(|i| i.pointer.primary_down());
                    let mut end_artboard_drag = false;
                    if let Some(d) = self.artboard_drag.as_ref() {
                        if down {
                            if let Some(p) = pointer {
                                let (cx, cy) = view.screen_to_canvas(p.x as f64, p.y as f64);
                                let mut nx = self.snap(cx - d.grab_dx);
                                let mut ny = self.snap(cy - d.grab_dy);

                                // Alignment snapping: snap this board's left/centre/
                                // right (and top/middle/bottom) to other boards'
                                // matching edges when within ~8px on screen.
                                let (bw, bh) = boards
                                    .iter()
                                    .find(|b| b.0 == d.id)
                                    .map(|b| (b.4, b.5))
                                    .unwrap_or((100.0, 100.0));
                                let thresh = 8.0 / view.zoom.max(1e-6);
                                let mut guide_x: Option<f64> = None;
                                let mut guide_y: Option<f64> = None;
                                let mut best_dx: Option<f64> = None;
                                let mut best_dy: Option<f64> = None;
                                for b in boards.iter().filter(|b| b.0 != d.id) {
                                    let (ox, oy, ow, oh) = (b.2, b.3, b.4, b.5);
                                    for mx in [nx, nx + bw * 0.5, nx + bw] {
                                        for tx in [ox, ox + ow * 0.5, ox + ow] {
                                            let diff = tx - mx;
                                            if diff.abs() < thresh
                                                && best_dx
                                                    .is_none_or(|bb: f64| diff.abs() < bb.abs())
                                            {
                                                best_dx = Some(diff);
                                                guide_x = Some(tx);
                                            }
                                        }
                                    }
                                    for my in [ny, ny + bh * 0.5, ny + bh] {
                                        for ty in [oy, oy + oh * 0.5, oy + oh] {
                                            let diff = ty - my;
                                            if diff.abs() < thresh
                                                && best_dy
                                                    .is_none_or(|bb: f64| diff.abs() < bb.abs())
                                            {
                                                best_dy = Some(diff);
                                                guide_y = Some(ty);
                                            }
                                        }
                                    }
                                }
                                if let Some(ddx) = best_dx {
                                    nx += ddx;
                                }
                                if let Some(ddy) = best_dy {
                                    ny += ddy;
                                }

                                // Equal-distance (distribution) snapping: if the
                                // gap to a neighbour matches an existing gap
                                // between boards, lock to it (only when edge
                                // alignment didn't already claim that axis).
                                let others: Vec<(f64, f64, f64, f64)> = boards
                                    .iter()
                                    .filter(|b| b.0 != d.id)
                                    .map(|b| (b.2, b.3, b.4, b.5))
                                    .collect();
                                let ov_y = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| {
                                    a.1 < b.1 + b.3 && a.1 + a.3 > b.1
                                };
                                let ov_x = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| {
                                    a.0 < b.0 + b.2 && a.0 + a.2 > b.0
                                };
                                let mut dist_x: Vec<((f64, f64, f64, f64), (f64, f64, f64, f64))> =
                                    Vec::new();
                                let mut dist_y: Vec<((f64, f64, f64, f64), (f64, f64, f64, f64))> =
                                    Vec::new();

                                if best_dx.is_none() {
                                    let mut gaps: Vec<f64> = Vec::new();
                                    for i in 0..others.len() {
                                        for j in 0..others.len() {
                                            if i != j && ov_y(others[i], others[j]) {
                                                let g = others[j].0 - (others[i].0 + others[i].2);
                                                if g > 1.0 {
                                                    gaps.push(g);
                                                }
                                            }
                                        }
                                    }
                                    let mut best_adj: Option<f64> = None;
                                    let mut snap_g: Option<f64> = None;
                                    for &o in &others {
                                        if !ov_y((nx, ny, bw, bh), o) {
                                            continue;
                                        }
                                        for &g in &gaps {
                                            for t in [o.0 + o.2 + g, o.0 - g - bw] {
                                                let a = t - nx;
                                                if a.abs() < thresh
                                                    && best_adj
                                                        .is_none_or(|b: f64| a.abs() < b.abs())
                                                {
                                                    best_adj = Some(a);
                                                    snap_g = Some(g);
                                                }
                                            }
                                        }
                                    }
                                    if let Some(adj) = best_adj {
                                        nx += adj;
                                    }
                                    if let Some(g) = snap_g {
                                        let dn = (nx, ny, bw, bh);
                                        for &o in &others {
                                            if ov_y(dn, o) {
                                                if ((dn.0 - (o.0 + o.2)) - g).abs() < 0.6 {
                                                    dist_x.push((o, dn));
                                                }
                                                if ((o.0 - (dn.0 + dn.2)) - g).abs() < 0.6 {
                                                    dist_x.push((dn, o));
                                                }
                                            }
                                        }
                                        for i in 0..others.len() {
                                            for j in 0..others.len() {
                                                if i != j && ov_y(others[i], others[j]) {
                                                    let gg =
                                                        others[j].0 - (others[i].0 + others[i].2);
                                                    if (gg - g).abs() < 0.6 {
                                                        dist_x.push((others[i], others[j]));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if best_dy.is_none() {
                                    let mut gaps: Vec<f64> = Vec::new();
                                    for i in 0..others.len() {
                                        for j in 0..others.len() {
                                            if i != j && ov_x(others[i], others[j]) {
                                                let g = others[j].1 - (others[i].1 + others[i].3);
                                                if g > 1.0 {
                                                    gaps.push(g);
                                                }
                                            }
                                        }
                                    }
                                    let mut best_adj: Option<f64> = None;
                                    let mut snap_g: Option<f64> = None;
                                    for &o in &others {
                                        if !ov_x((nx, ny, bw, bh), o) {
                                            continue;
                                        }
                                        for &g in &gaps {
                                            for t in [o.1 + o.3 + g, o.1 - g - bh] {
                                                let a = t - ny;
                                                if a.abs() < thresh
                                                    && best_adj
                                                        .is_none_or(|b: f64| a.abs() < b.abs())
                                                {
                                                    best_adj = Some(a);
                                                    snap_g = Some(g);
                                                }
                                            }
                                        }
                                    }
                                    if let Some(adj) = best_adj {
                                        ny += adj;
                                    }
                                    if let Some(g) = snap_g {
                                        let dn = (nx, ny, bw, bh);
                                        for &o in &others {
                                            if ov_x(dn, o) {
                                                if ((dn.1 - (o.1 + o.3)) - g).abs() < 0.6 {
                                                    dist_y.push((o, dn));
                                                }
                                                if ((o.1 - (dn.1 + dn.3)) - g).abs() < 0.6 {
                                                    dist_y.push((dn, o));
                                                }
                                            }
                                        }
                                        for i in 0..others.len() {
                                            for j in 0..others.len() {
                                                if i != j && ov_x(others[i], others[j]) {
                                                    let gg =
                                                        others[j].1 - (others[i].1 + others[i].3);
                                                    if (gg - g).abs() < 0.6 {
                                                        dist_y.push((others[i], others[j]));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                if let Some(ab) = doc.artboards.iter_mut().find(|a| a.id == d.id) {
                                    ab.x = nx;
                                    ab.y = ny;
                                }

                                // Draw the alignment guide lines (full viewport).
                                let gp = ui.painter_at(rect);
                                let guide = egui::Color32::from_rgb(150, 128, 240);
                                if let Some(gx) = guide_x {
                                    let (sx, _) = view.canvas_to_screen(gx, 0.0);
                                    gp.line_segment(
                                        [
                                            egui::pos2(sx as f32, rect.top()),
                                            egui::pos2(sx as f32, rect.bottom()),
                                        ],
                                        egui::Stroke::new(1.0, guide),
                                    );
                                }
                                if let Some(gy) = guide_y {
                                    let (_, sy) = view.canvas_to_screen(0.0, gy);
                                    gp.line_segment(
                                        [
                                            egui::pos2(rect.left(), sy as f32),
                                            egui::pos2(rect.right(), sy as f32),
                                        ],
                                        egui::Stroke::new(1.0, guide),
                                    );
                                }
                                // Equal-distance measurements between matching boards.
                                for (l, r) in &dist_x {
                                    draw_h_gap(&gp, view, *l, *r, guide);
                                }
                                for (t, b) in &dist_y {
                                    draw_v_gap(&gp, view, *t, *b, guide);
                                }
                            }
                        } else {
                            end_artboard_drag = true;
                        }
                    }
                    if end_artboard_drag {
                        // Re-plan the live board move from its pre-drag state so
                        // the core planner moves its owned artwork in the same
                        // undo step as the artboard rect.
                        if let Some(d) = self.artboard_drag.take() {
                            if let Some(pre) = self.artboard_pre.take() {
                                let old_position = pre
                                    .iter()
                                    .find(|artboard| artboard.id == d.id)
                                    .map(|artboard| (artboard.x, artboard.y));
                                let new_position = doc
                                    .artboards
                                    .iter()
                                    .find(|artboard| artboard.id == d.id)
                                    .map(|artboard| (artboard.x, artboard.y));
                                if let (Some((old_x, old_y)), Some((new_x, new_y))) =
                                    (old_position, new_position)
                                {
                                    let dx = new_x - old_x;
                                    let dy = new_y - old_y;
                                    if dx != 0.0 || dy != 0.0 {
                                        doc.artboards = pre;
                                        if let Some(cmd) =
                                            artboard_ops::plan_move_artboard(doc, d.id, dx, dy)
                                        {
                                            history.execute(cmd, doc);
                                            doc_modified = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some((id, hidx, ox, oy, ow, oh)) = self.artboard_resize {
                        if down {
                            if let Some(p) = pointer {
                                let (cx, cy) = view.screen_to_canvas(p.x as f64, p.y as f64);
                                let (mut x0, mut y0, mut x1, mut y1) = (ox, oy, ox + ow, oy + oh);
                                match hidx {
                                    0 => {
                                        x0 = self.snap(cx);
                                        y0 = self.snap(cy);
                                    }
                                    1 => {
                                        x1 = self.snap(cx);
                                        y0 = self.snap(cy);
                                    }
                                    2 => {
                                        x0 = self.snap(cx);
                                        y1 = self.snap(cy);
                                    }
                                    _ => {
                                        x1 = self.snap(cx);
                                        y1 = self.snap(cy);
                                    }
                                }
                                if let Some(ab) = doc.artboards.iter_mut().find(|a| a.id == id) {
                                    ab.x = x0.min(x1);
                                    ab.y = y0.min(y1);
                                    ab.width = (x1 - x0).abs().max(16.0);
                                    ab.height = (y1 - y0).abs().max(16.0);
                                    doc_modified = true;
                                }
                            }
                        } else {
                            // Resize released — record it.
                            self.artboard_resize = None;
                            if let Some(pre) = self.artboard_pre.take() {
                                if pre != doc.artboards {
                                    history.execute(
                                        Command::SetArtboards {
                                            old: pre,
                                            new: doc.artboards.clone(),
                                        },
                                        doc,
                                    );
                                    doc_modified = true;
                                }
                            }
                        }
                    }

                    if let Some(id) = select {
                        doc.active_artboard = Some(id);
                        doc_modified = true;
                    }
                    if let Some(r) = start_rename {
                        self.artboard_pre = Some(doc.artboards.clone());
                        self.artboard_rename = Some(r);
                        self.artboard_rename_focus = true;
                    }

                    // ── Inline rename popup ─────────────────────────────────────
                    if let Some((rid, _)) = self.artboard_rename.clone() {
                        if let Some((bx, by)) =
                            boards.iter().find(|b| b.0 == rid).map(|b| (b.2, b.3))
                        {
                            let (sx0, sy0) = view.canvas_to_screen(bx, by);
                            let mut close: Option<bool> = None; // Some(true)=commit
                            egui::Area::new(ui.id().with(("ab_rename", rid)))
                                .fixed_pos(egui::pos2(sx0 as f32 + 16.0, sy0 as f32 - 24.0))
                                .order(egui::Order::Foreground)
                                .show(ctx, |ui| {
                                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                                        if let Some((_, buf)) = self.artboard_rename.as_mut() {
                                            let r = ui.add(
                                                egui::TextEdit::singleline(buf)
                                                    .desired_width(140.0),
                                            );
                                            // Focus once when opened (not every
                                            // frame) so clicking away can commit.
                                            if self.artboard_rename_focus {
                                                r.request_focus();
                                                self.artboard_rename_focus = false;
                                            }
                                            // Commit on focus loss (Enter or click
                                            // away); Escape cancels.
                                            let esc =
                                                ui.input(|i| i.key_pressed(egui::Key::Escape));
                                            if esc {
                                                close = Some(false);
                                            } else if r.lost_focus() {
                                                close = Some(true);
                                            }
                                        }
                                    });
                                });
                            match close {
                                Some(true) => {
                                    if let Some((id, buf)) = self.artboard_rename.take() {
                                        let name = buf.trim().to_string();
                                        if !name.is_empty() {
                                            if let Some(ab) =
                                                doc.artboards.iter_mut().find(|a| a.id == id)
                                            {
                                                ab.name = name;
                                                doc_modified = true;
                                            }
                                        }
                                    }
                                    // Record the rename.
                                    if let Some(pre) = self.artboard_pre.take() {
                                        if pre != doc.artboards {
                                            history.execute(
                                                Command::SetArtboards {
                                                    old: pre,
                                                    new: doc.artboards.clone(),
                                                },
                                                doc,
                                            );
                                        }
                                    }
                                }
                                Some(false) => {
                                    self.artboard_rename = None;
                                    self.artboard_pre = None;
                                }
                                None => {}
                            }
                        } else {
                            self.artboard_rename = None;
                        }
                    }

                    // Compact add/remove toolbar pinned to the viewport corner.
                    let mut do_add = false;
                    let mut do_duplicate = false;
                    let mut do_remove = false;
                    let duplicate_target = doc
                        .active_artboard
                        .or_else(|| doc.artboards.first().map(|artboard| artboard.id))
                        .and_then(|id| {
                            doc.artboards
                                .iter()
                                .find(|artboard| artboard.id == id)
                                .map(|artboard| (id, artboard.width))
                        });
                    egui::Area::new(ui.id().with("artboard_tools"))
                        // Sit above the cursor-coordinate readout (bottom-left).
                        .fixed_pos(egui::pos2(rect.left() + 12.0, rect.bottom() - 58.0))
                        .show(ctx, |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("Artboards: {}", boards.len()))
                                            .size(11.5),
                                    );
                                    if ui.add(egui::Button::new(ph::PLUS).small()).clicked() {
                                        do_add = true;
                                    }
                                    if ui
                                        .add_enabled(
                                            duplicate_target.is_some(),
                                            egui::Button::new("⧉").small(),
                                        )
                                        .on_hover_text("Duplicate artboard with its contents")
                                        .clicked()
                                    {
                                        do_duplicate = true;
                                    }
                                    if ui
                                        .add_enabled(
                                            boards.len() > 1,
                                            egui::Button::new(ph::MINUS).small(),
                                        )
                                        .clicked()
                                    {
                                        do_remove = true;
                                    }
                                });
                            });
                        });
                    if do_add {
                        let pre = doc.artboards.clone();
                        let (_, by0, bx1, _) = doc.artboards_bounds();
                        let (aw, ah) = doc
                            .active_artboard()
                            .map(|a| (a.width, a.height))
                            .unwrap_or((doc.width, doc.height));
                        let gap = (aw * 0.06).max(40.0);
                        let n = doc.artboards.len() + 1;
                        let ab = photonic_core::Artboard::new(
                            format!("Artboard {n}"),
                            bx1 + gap,
                            by0,
                            aw,
                            ah,
                        );
                        doc.add_artboard(ab);
                        history.execute(
                            Command::SetArtboards {
                                old: pre,
                                new: doc.artboards.clone(),
                            },
                            doc,
                        );
                        doc_modified = true;
                    }
                    if do_duplicate {
                        if let Some((id, width)) = duplicate_target {
                            if let Some((cmd, new_id)) = artboard_ops::plan_duplicate_artboard(
                                doc,
                                id,
                                width + 40.0,
                                0.0,
                                None,
                            ) {
                                history.execute(cmd, doc);
                                doc.active_artboard = Some(new_id);
                                doc_modified = true;
                            }
                        }
                    }
                    if do_remove {
                        if let Some(id) = doc.active_artboard {
                            let pre = doc.artboards.clone();
                            doc.remove_artboard(id);
                            if pre != doc.artboards {
                                history.execute(
                                    Command::SetArtboards {
                                        old: pre,
                                        new: doc.artboards.clone(),
                                    },
                                    doc,
                                );
                                doc_modified = true;
                            }
                        }
                    }
                }

                // While manipulating or hovering an artboard label/handle, don't
                // let the regular tools also act (or override the cursor).
                if over_artboard_label
                    || self.artboard_drag.is_some()
                    || self.artboard_resize.is_some()
                    || self.artboard_rename.is_some()
                {
                    return;
                }

                // ── Right-click radial wheel ──────────────────────────────────
                if response.secondary_clicked() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let (cx, cy) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                        // #187: In Direct Select, a right-click that lands on a
                        // directly-selected anchor must reach that tool's point-type
                        // context menu (Corner / Smooth-Curved / Round corner), which
                        // handle_direct_select_tool registers via response.context_menu.
                        // The radial wheel below early-returns on the same frame, so if
                        // we opened it here the menu closure would never run. Suppress
                        // the wheel when an anchor is under the click (using the same
                        // hit-test the tool uses) and fall through to the tool handler.
                        // Right-clicking empty canvas in Direct Select still opens the
                        // wheel, matching every other tool.
                        let ds_anchor_menu = matches!(
                            self.active_tool,
                            Tool::DirectSelect | Tool::ProportionalMove
                        ) && self.ds_anchor_at(cx, cy, doc, view).is_some();
                        if !ds_anchor_menu {
                            // `hit_test` returns the leaf under the cursor; groups are
                            // flattened in draw order. Resolve that leaf to the group
                            // the user perceives as the object (matching click-select),
                            // so a right-click on a group yields group actions
                            // (Ungroup / Ungroup All). Inside an isolated group we act
                            // on the raw leaf instead.
                            let hit = hit_test(doc, cx, cy, renderer).map(|id| {
                                if self.isolated_group.is_some() {
                                    id
                                } else {
                                    doc.outermost_group_of(&id).unwrap_or(id)
                                }
                            });
                            let wheel_ctx = match hit {
                                Some(id)
                                    if doc.selection.contains(&id) && doc.selection.count() > 1 =>
                                {
                                    WheelContext::MultiNode {
                                        node_ids: doc.selection.ids().copied().collect(),
                                    }
                                }
                                Some(id) => {
                                    let kind = match doc.get_node(&id).map(|n| &n.kind) {
                                        Some(SceneNodeKind::Group(_)) => WheelNodeKind::Group,
                                        Some(SceneNodeKind::Text(_)) => WheelNodeKind::Text,
                                        _ => WheelNodeKind::Path,
                                    };
                                    WheelContext::SingleNode {
                                        node_id: id,
                                        node_kind: kind,
                                    }
                                }
                                None if doc.selection.count() > 1 => WheelContext::MultiNode {
                                    node_ids: doc.selection.ids().copied().collect(),
                                },
                                _ => WheelContext::EmptyCanvas {
                                    canvas_x: cx,
                                    canvas_y: cy,
                                },
                            };
                            self.radial_wheel = Some(WheelState::new(
                                pos,
                                (cx, cy),
                                &wheel_ctx,
                                self.prefs.reduced_motion,
                            ));
                        }
                    }
                }

                // Update wheel hover, paint overlay, and handle interaction.
                // This block runs before any early-return tool handlers so the
                // wheel is always rendered while open.
                if self.radial_wheel.is_some() {
                    let now = ui.input(|i| i.time);

                    // Scroll wheel rotates between categories (carousel).
                    let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
                    if scroll_y != 0.0 {
                        if let Some(ref mut wheel) = self.radial_wheel {
                            if scroll_y > 0.0 {
                                wheel.prev_category(now);
                            } else {
                                wheel.next_category(now);
                            }
                        }
                    }

                    // Update hover position (ring segment + peek tabs)
                    if let Some(cursor) = ui.input(|i| i.pointer.hover_pos()) {
                        if let Some(ref mut wheel) = self.radial_wheel {
                            wheel.update_hover(cursor);
                        }
                    }

                    // Paint the overlay now — before any `return` can skip it
                    if let Some(ref wheel) = self.radial_wheel {
                        wheel.draw(ui.painter(), now);
                        // Keep animating the radial-wipe transition smoothly.
                        if wheel.is_animating(now) {
                            ui.ctx().request_repaint();
                        }
                    }

                    // Escape closes without selecting
                    if viewport_kb(ui.ctx()) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.radial_wheel = None;
                        return;
                    }

                    // Primary click: jump to a peeked category, fire a verb, or dismiss.
                    if response.clicked_by(egui::PointerButton::Primary) {
                        let on_peek = self
                            .radial_wheel
                            .as_ref()
                            .is_some_and(|w| w.peek_hovered.is_some());
                        if on_peek {
                            if let Some(ref mut wheel) = self.radial_wheel {
                                wheel.jump_peek(now);
                            }
                            return; // stay open on the new category
                        }
                        if let Some(wheel) = self.radial_wheel.take() {
                            if let Some(action) = wheel.hovered_action() {
                                let pa = PanelAction::from_wheel_action(
                                    action,
                                    wheel.canvas_pos,
                                    self.fill_color,
                                );
                                self.pending_panel_actions.push(pa);
                            }
                        }
                        return; // consume click — don't pass to tool handler
                    }

                    // Keep the wheel open during non-click interactions (pan, zoom)
                    return;
                }

                // ── Canvas pan: middle mouse drag ────────────────────────────
                if response.dragged_by(egui::PointerButton::Middle) {
                    // Closed-hand cursor signals the workspace is being grabbed/moved.
                    // Return early (like space+drag) so later tool-cursor logic
                    // doesn't overwrite Grabbing in the same frame.
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    let delta = response.drag_delta();
                    view.pan_x += delta.x as f64;
                    view.pan_y += delta.y as f64;
                    return;
                }

                // ── Canvas pan: alt + left drag ──────────────────────────────
                // Skipped for Shape Builder (alt = subtract) and for shape-creator
                // tools, where alt = draw-from-center (#10).
                if response.dragged_by(egui::PointerButton::Primary)
                    && ui.input(|i| i.modifiers.alt)
                    && self.active_tool != Tool::ShapeBuilder
                    && !self.active_tool.is_shape_creator()
                {
                    let delta = response.drag_delta();
                    view.pan_x += delta.x as f64;
                    view.pan_y += delta.y as f64;
                    return;
                }

                // ── Canvas pan: space + left drag ────────────────────────────────────
                let space_held = ui.input(|i| i.key_down(egui::Key::Space));
                if space_held {
                    let cursor = if response.dragged_by(egui::PointerButton::Primary) {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    };
                    ui.ctx().set_cursor_icon(cursor);
                    if response.dragged_by(egui::PointerButton::Primary) {
                        let delta = response.drag_delta();
                        view.pan_x += delta.x as f64;
                        view.pan_y += delta.y as f64;
                    }
                    return;
                }

                // ── Arrow-key nudge ───────────────────────────────────────────
                if viewport_kb(ctx) {
                    let shift = ctx.input(|i| i.modifiers.shift);
                    let dist = self.prefs.nudge_distance * if shift { 10.0 } else { 1.0 };
                    let (dx, dy) = ctx.input(|i| {
                        let mut dx = 0.0_f64;
                        let mut dy = 0.0_f64;
                        if i.key_pressed(egui::Key::ArrowLeft) {
                            dx -= dist;
                        }
                        if i.key_pressed(egui::Key::ArrowRight) {
                            dx += dist;
                        }
                        if i.key_pressed(egui::Key::ArrowUp) {
                            dy -= dist;
                        }
                        if i.key_pressed(egui::Key::ArrowDown) {
                            dy += dist;
                        }
                        (dx, dy)
                    });
                    if (dx.abs() > 1e-12 || dy.abs() > 1e-12) && !doc.selection.is_empty() {
                        use photonic_core::transform::Transform;
                        let sel_ids: Vec<NodeId> = doc.selection.ids().copied().collect();
                        let cmds: Vec<Command> = sel_ids
                            .iter()
                            .filter_map(|id| {
                                let node = doc.nodes.get(id)?;
                                let mut new_node = node.clone();
                                new_node.transform =
                                    new_node.transform.then(&Transform::translate(dx, dy));
                                Some(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                })
                            })
                            .collect();
                        if !cmds.is_empty() {
                            history.execute(Command::Batch(cmds), doc);
                            doc_modified = true;
                        }
                    }
                }

                let dt = ctx.input(|i| i.unstable_dt as f64).min(0.1);

                // ── Smooth zoom: lerp actual zoom toward log-space target ─────
                {
                    let target = self.smooth.log_zoom_target.exp();
                    if (view.zoom - target).abs() > 1e-5 {
                        let rate = 1.0 - (-22.0 * dt).exp();
                        let new_zoom = view.zoom + (target - view.zoom) * rate;
                        let factor = new_zoom / view.zoom;
                        let (px, py) = self.smooth.zoom_pivot;
                        view.zoom_at(factor, px, py);
                        ctx.request_repaint();
                    }
                }

                // ── Zoom: scroll accumulates into log-space target ────────────
                // Proportional Move claims the wheel while dragging an anchor
                // (spread / Shift+curve), so suppress zoom for that gesture.
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                let prop_move_scroll = self.active_tool == Tool::ProportionalMove
                    && response.dragged_by(egui::PointerButton::Primary);
                if scroll != 0.0 && response.hovered() && !prop_move_scroll {
                    let pivot = ui
                        .input(|i| i.pointer.hover_pos())
                        .unwrap_or(response.rect.center());
                    self.smooth.zoom_pivot = (pivot.x as f64, pivot.y as f64);
                    self.smooth.log_zoom_target += scroll as f64 * 0.001;
                    self.smooth.log_zoom_target = self
                        .smooth
                        .log_zoom_target
                        .clamp(0.01_f64.ln(), 64.0_f64.ln());
                }

                // ── WASD pan: velocity + exponential friction ─────────────────
                if viewport_kb(ctx) {
                    let (w, a, s, d) = ctx.input(|i| {
                        (
                            i.key_down(egui::Key::W),
                            i.key_down(egui::Key::A),
                            i.key_down(egui::Key::S),
                            i.key_down(egui::Key::D),
                        )
                    });
                    let accel = 2800.0 * dt;
                    let max_v = 900.0_f64;
                    if a {
                        self.smooth.pan_vel_x = (self.smooth.pan_vel_x + accel).min(max_v);
                    }
                    if d {
                        self.smooth.pan_vel_x = (self.smooth.pan_vel_x - accel).max(-max_v);
                    }
                    if w {
                        self.smooth.pan_vel_y = (self.smooth.pan_vel_y + accel).min(max_v);
                    }
                    if s {
                        self.smooth.pan_vel_y = (self.smooth.pan_vel_y - accel).max(-max_v);
                    }
                }
                let friction = (-10.0_f64 * dt).exp();
                self.smooth.pan_vel_x *= friction;
                self.smooth.pan_vel_y *= friction;
                if self.smooth.pan_vel_x.abs() > 0.5 || self.smooth.pan_vel_y.abs() > 0.5 {
                    view.pan_x += self.smooth.pan_vel_x * dt;
                    view.pan_y += self.smooth.pan_vel_y * dt;
                    ctx.request_repaint();
                }

                // ── Fit artboard: middle-click double-click ──────────────────
                if response.double_clicked_by(egui::PointerButton::Middle) {
                    view.fit_to_rect(
                        0.0,
                        0.0,
                        rect.width() as f64 * 0.8,
                        rect.height() as f64 * 0.8,
                    );
                    self.smooth.log_zoom_target = view.zoom.ln();
                }

                // ── Diff highlight overlay ────────────────────────────────────
                if self.diff.overlay_active {
                    for (node_id, category) in &self.diff.highlights {
                        if let Some(node) = doc.nodes.get(node_id) {
                            if let Some((cx0, cy0, cx1, cy1)) =
                                text_aware_canvas_bounds(node, renderer)
                            {
                                let (sx0, sy0) = view.canvas_to_screen(cx0, cy0);
                                let (sx1, sy1) = view.canvas_to_screen(cx1, cy1);
                                let rect = egui::Rect::from_min_max(
                                    egui::pos2(sx0 as f32, sy0 as f32),
                                    egui::pos2(sx1 as f32, sy1 as f32),
                                );
                                let (stroke_col, fill_col) = match category {
                                    DiffCategory::Added => (
                                        Color32::from_rgb(34, 197, 94),
                                        Color32::from_rgba_unmultiplied(34, 197, 94, 25),
                                    ),
                                    DiffCategory::Modified => (
                                        Color32::from_rgb(234, 179, 8),
                                        Color32::from_rgba_unmultiplied(234, 179, 8, 25),
                                    ),
                                    DiffCategory::Removed => unreachable!(),
                                };
                                ui.painter().rect_filled(rect, 2.0, fill_col);
                                ui.painter().rect_stroke(
                                    rect,
                                    2.0,
                                    egui::Stroke::new(2.0, stroke_col),
                                );
                            }
                        }
                    }
                    // Removed nodes use pre-computed canvas-space boxes.
                    let red_stroke = Color32::from_rgb(239, 68, 68);
                    let red_fill = Color32::from_rgba_unmultiplied(239, 68, 68, 25);
                    for &canvas_rect in &self.diff.removed_boxes {
                        let (sx0, sy0) = view
                            .canvas_to_screen(canvas_rect.min.x as f64, canvas_rect.min.y as f64);
                        let (sx1, sy1) = view
                            .canvas_to_screen(canvas_rect.max.x as f64, canvas_rect.max.y as f64);
                        let screen_rect = egui::Rect::from_min_max(
                            egui::pos2(sx0 as f32, sy0 as f32),
                            egui::pos2(sx1 as f32, sy1 as f32),
                        );
                        ui.painter().rect_filled(screen_rect, 2.0, red_fill);
                        ui.painter().rect_stroke(
                            screen_rect,
                            2.0,
                            egui::Stroke::new(2.0, red_stroke),
                        );
                    }
                }

                // ── On-canvas gradient handles ───────────────────────────────
                // Active whenever the movable fill popup is open (any tool). If
                // a handle is grabbed, it consumes the drag so the underlying
                // tool doesn't also act on it.
                if self.handle_gradient_on_canvas(
                    ui,
                    &response,
                    doc,
                    view,
                    &mut doc_modified,
                    history,
                ) {
                    return;
                }

                // ── Select tool ──────────────────────────────────────────────
                if self.active_tool == Tool::Select {
                    self.handle_select_tool(
                        ui,
                        &response,
                        doc,
                        view,
                        renderer,
                        &mut doc_modified,
                        history,
                    );
                    return;
                }

                // ── Direct Selection (point edit) tool ────────────────────────
                // Proportional Move is a sub-variant sharing the same handler; it
                // branches internally on `self.active_tool` for the anchor drag,
                // the falloff overlay, and scroll-wheel spread/curve control.
                if self.active_tool == Tool::DirectSelect
                    || self.active_tool == Tool::ProportionalMove
                {
                    self.handle_direct_select_tool(
                        ui,
                        &response,
                        doc,
                        view,
                        renderer,
                        &mut doc_modified,
                        history,
                    );
                    return;
                }

                // ── Pan tool ──────────────────────────────────────────────────
                if self.active_tool == Tool::Pan {
                    let cursor = if response.dragged_by(egui::PointerButton::Primary) {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    };
                    ui.ctx().set_cursor_icon(cursor);
                    if response.dragged_by(egui::PointerButton::Primary) {
                        let delta = response.drag_delta();
                        view.pan_x += delta.x as f64;
                        view.pan_y += delta.y as f64;
                    }
                    return;
                }

                // ── Pen tool ─────────────────────────────────────────────────
                if self.active_tool == Tool::Pen {
                    self.handle_pen_tool(ui, &response, doc, view, history, &mut doc_modified);
                    return;
                }

                // ── Shape Builder tool ────────────────────────────────────────
                if self.active_tool == Tool::ShapeBuilder {
                    self.handle_shape_builder_tool(
                        ui,
                        &response,
                        doc,
                        view,
                        renderer,
                        &mut doc_modified,
                        history,
                    );
                    return;
                }

                // ── Scissors tool ─────────────────────────────────────────────
                // Hover: show a blue dot at the nearest point on any path.
                // Click: split the nearest path at that point.
                if self.active_tool == Tool::Scissors {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                    if let Some(cursor) = ui.input(|i| i.pointer.hover_pos()) {
                        if rect.contains(cursor) {
                            let (cx, cy) = view.screen_to_canvas(cursor.x as f64, cursor.y as f64);

                            // Find the path node nearest to the cursor.
                            let mut best_node_id = None;
                            let mut best_dist = 20.0f64 / view.zoom; // 20px snap radius in canvas units
                            let mut best_cut = (cx, cy);

                            for node in doc.nodes.values() {
                                if !node.visible {
                                    continue;
                                }
                                let pn = match &node.kind {
                                    SceneNodeKind::Path(p) => p,
                                    _ => continue,
                                };
                                if pn.path_data.is_empty() {
                                    continue;
                                }

                                // Transform cursor to local space.
                                let inv = node.transform.to_kurbo().inverse();
                                let lpt = inv * kurbo::Point::new(cx, cy);

                                // Sample points on the path to find closest.
                                let samples = pn.path_data.sample_positions(64);
                                for &(sx, sy, _) in &samples {
                                    let d = ((sx - lpt.x).powi(2) + (sy - lpt.y).powi(2)).sqrt();
                                    if d < best_dist {
                                        // Transform the sample back to canvas space.
                                        let fwd = node.transform.to_kurbo();
                                        let sp = fwd * kurbo::Point::new(sx, sy);
                                        best_dist = d;
                                        best_node_id = Some(node.id);
                                        best_cut = (sp.x, sp.y);
                                    }
                                }
                            }

                            // Draw indicator dot at cut point.
                            if let Some(_nid) = best_node_id {
                                let painter = ctx.layer_painter(egui::LayerId::new(
                                    egui::Order::Foreground,
                                    egui::Id::new("scissors_indicator"),
                                ));
                                let (sx, sy) = view.canvas_to_screen(best_cut.0, best_cut.1);
                                painter.circle_filled(
                                    egui::pos2(sx as f32, sy as f32),
                                    5.0,
                                    egui::Color32::from_rgb(0, 140, 255),
                                );
                                painter.circle_stroke(
                                    egui::pos2(sx as f32, sy as f32),
                                    5.0,
                                    egui::Stroke::new(1.5, egui::Color32::WHITE),
                                );
                            }

                            // Click: split the path.
                            if response.clicked_by(egui::PointerButton::Primary) {
                                if let Some(nid) = best_node_id {
                                    if let Some(node) = doc.nodes.get(&nid) {
                                        let pn = match &node.kind {
                                            SceneNodeKind::Path(p) => p.clone(),
                                            _ => {
                                                return;
                                            }
                                        };
                                        let inv = node.transform.to_kurbo().inverse();
                                        let lpt = inv * kurbo::Point::new(cx, cy);

                                        if let Some((path_a, path_b)) =
                                            pn.path_data.split_at_point(lpt.x, lpt.y)
                                        {
                                            let layer_id = node.layer_id;
                                            let t = node.transform;
                                            let opacity = node.opacity;
                                            let blend_mode = node.blend_mode;
                                            let name_base = node.name.clone();

                                            let mut na = SceneNode::new(
                                                format!("{} (1/2)", name_base),
                                                layer_id,
                                                SceneNodeKind::Path(
                                                    photonic_core::node::PathNode {
                                                        path_data: path_a,
                                                        ..pn.clone()
                                                    },
                                                ),
                                            );
                                            na.transform = t;
                                            na.opacity = opacity;
                                            na.blend_mode = blend_mode;

                                            let mut nb = SceneNode::new(
                                                format!("{} (2/2)", name_base),
                                                layer_id,
                                                SceneNodeKind::Path(
                                                    photonic_core::node::PathNode {
                                                        path_data: path_b,
                                                        ..pn.clone()
                                                    },
                                                ),
                                            );
                                            nb.transform = t;
                                            nb.opacity = opacity;
                                            nb.blend_mode = blend_mode;

                                            let sel_a = na.id;
                                            let sel_b = nb.id;

                                            history.execute(
                                                Command::Batch(vec![
                                                    Command::RemoveNode { node_id: nid },
                                                    Command::AddNode {
                                                        node: na,
                                                        layer_id: Some(layer_id),
                                                    },
                                                    Command::AddNode {
                                                        node: nb,
                                                        layer_id: Some(layer_id),
                                                    },
                                                ]),
                                                doc,
                                            );
                                            doc.selection = photonic_core::Selection::from_ids(
                                                [sel_a, sel_b].iter().copied(),
                                            );
                                            doc_modified = true;
                                        }
                                    }
                                }
                                return;
                            }
                        }
                    }
                    return;
                }

                // ── Knife tool (freehand slice) ───────────────────────────────
                if self.active_tool == Tool::Knife {
                    self.handle_knife_tool(ui, &response, doc, view, &mut doc_modified, history);
                    return;
                }

                // ── Eraser tool (vector boolean subtract) ─────────────────────
                if self.active_tool == Tool::Eraser {
                    self.handle_eraser_tool(ui, &response, doc, view, &mut doc_modified, history);
                    return;
                }

                // ── Magic Wand tool ───────────────────────────────────────────
                if self.active_tool == Tool::MagicWand {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let (cx, cy) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                            // Find topmost visible unlocked node whose AABB contains click.
                            let hit = hit_test(doc, cx, cy, renderer);
                            if let Some(ref_id) = hit {
                                if let Some(ref_node) = doc.nodes.get(&ref_id).cloned() {
                                    let tolerance = self.magic_wand_tolerance as f32;
                                    let attr = self.magic_wand_attribute;
                                    let mut matched: Vec<NodeId> = Vec::new();
                                    for (nid, node) in &doc.nodes {
                                        let ok = match attr {
                                            SelectSameAttr::FillColor => {
                                                let ref_c = magic_wand_solid_fill(&ref_node);
                                                let cand_c = magic_wand_solid_fill(node);
                                                match (ref_c, cand_c) {
                                                    (Some(rc), Some(cc)) => {
                                                        magic_wand_color_dist(rc, cc) <= tolerance
                                                    }
                                                    (None, None) => true,
                                                    _ => false,
                                                }
                                            }
                                            SelectSameAttr::StrokeColor => {
                                                if let (
                                                    SceneNodeKind::Path(rp),
                                                    SceneNodeKind::Path(cp),
                                                ) = (&ref_node.kind, &node.kind)
                                                {
                                                    match (rp.stroke.enabled, cp.stroke.enabled) {
                                                        (true, true) => {
                                                            magic_wand_color_dist(
                                                                rp.stroke.color,
                                                                cp.stroke.color,
                                                            ) <= tolerance
                                                        }
                                                        (false, false) => true,
                                                        _ => false,
                                                    }
                                                } else {
                                                    false
                                                }
                                            }
                                            SelectSameAttr::StrokeWeight => {
                                                if let (
                                                    SceneNodeKind::Path(rp),
                                                    SceneNodeKind::Path(cp),
                                                ) = (&ref_node.kind, &node.kind)
                                                {
                                                    (rp.stroke.width - cp.stroke.width).abs()
                                                        <= tolerance as f64
                                                } else {
                                                    false
                                                }
                                            }
                                            SelectSameAttr::Opacity => {
                                                (ref_node.opacity - node.opacity).abs() <= tolerance
                                            }
                                            SelectSameAttr::BlendMode => {
                                                ref_node.blend_mode == node.blend_mode
                                            }
                                            SelectSameAttr::ObjectType => {
                                                std::mem::discriminant(&ref_node.kind)
                                                    == std::mem::discriminant(&node.kind)
                                            }
                                        };
                                        if ok {
                                            matched.push(*nid);
                                        }
                                    }
                                    doc.selection.clear();
                                    for nid in &matched {
                                        doc.selection.add(*nid);
                                    }
                                    self.selected_id = Some(ref_id);
                                    doc_modified = true;
                                }
                            }
                        }
                        return;
                    }
                }

                // ── Lasso tool ────────────────────────────────────────────────
                if self.active_tool == Tool::Lasso {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);

                    // Collect points while dragging.
                    if response.dragged_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            self.lasso_points.push(pos);
                        }
                    }

                    // Draw the lasso overlay while dragging.
                    if !self.lasso_points.is_empty() {
                        let painter = ctx.layer_painter(egui::LayerId::new(
                            egui::Order::Tooltip,
                            egui::Id::new("lasso_overlay"),
                        ));
                        let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(30, 120, 255));
                        let pts: Vec<egui::Pos2> = self.lasso_points.clone();
                        for w in pts.windows(2) {
                            painter.line_segment([w[0], w[1]], stroke);
                        }
                        // Close the lasso visually.
                        if pts.len() >= 2 {
                            painter.line_segment(
                                [*pts.last().unwrap(), pts[0]],
                                egui::Stroke::new(
                                    1.0,
                                    egui::Color32::from_rgba_premultiplied(30, 120, 255, 80),
                                ),
                            );
                        }
                    }

                    // On release: compute selection.
                    if response.drag_stopped() {
                        let pts = std::mem::take(&mut self.lasso_points);
                        if pts.len() >= 3 {
                            // Convert screen polygon to canvas coordinates.
                            let poly: Vec<[f64; 2]> = pts
                                .iter()
                                .map(|p| {
                                    let (cx, cy) = view.screen_to_canvas(p.x as f64, p.y as f64);
                                    [cx, cy]
                                })
                                .collect();

                            let additive = ui.input(|i| i.modifiers.shift);
                            if !additive {
                                doc.selection.clear();
                                self.selected_id = None;
                            }

                            // Collect matching IDs before mutating selection.
                            let to_select: Vec<NodeId> = doc
                                .nodes_in_draw_order()
                                .into_iter()
                                .filter(|n| !n.locked)
                                .filter_map(|node| {
                                    node_world_aabb_opt(node).and_then(|aabb| {
                                        let cx = (aabb.0 + aabb.2) / 2.0;
                                        let cy = (aabb.1 + aabb.3) / 2.0;
                                        if lasso_point_in_polygon(cx, cy, &poly) {
                                            Some(node.id)
                                        } else {
                                            None
                                        }
                                    })
                                })
                                .collect();
                            for nid in to_select {
                                doc.selection.add(nid);
                                if self.selected_id.is_none() {
                                    self.selected_id = Some(nid);
                                }
                            }
                            doc_modified = true;
                        }
                        return;
                    }

                    if response.dragged_by(egui::PointerButton::Primary) {
                        return;
                    }
                }

                // ── Pencil tool ───────────────────────────────────────────────
                if self.active_tool == Tool::Pencil {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);

                    // Collect canvas points while dragging, throttled to ~5 units apart.
                    if response.dragged_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let (cx, cy) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                            let should_add = match self.pencil_points.last() {
                                Some(&(lx, ly)) => {
                                    let dx = cx - lx;
                                    let dy = cy - ly;
                                    dx * dx + dy * dy >= 25.0 // 5 unit threshold
                                }
                                None => true,
                            };
                            if should_add {
                                self.pencil_points.push((cx, cy));
                            }
                        }
                    }

                    // Draw the preview stroke.
                    if self.pencil_points.len() >= 2 {
                        let painter = ctx.layer_painter(egui::LayerId::new(
                            egui::Order::Tooltip,
                            egui::Id::new("pencil_preview"),
                        ));
                        let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(80, 80, 200));
                        let screen_pts: Vec<egui::Pos2> = self
                            .pencil_points
                            .iter()
                            .map(|&(cx, cy)| {
                                let (sx, sy) = view.canvas_to_screen(cx, cy);
                                egui::pos2(sx as f32, sy as f32)
                            })
                            .collect();
                        for w in screen_pts.windows(2) {
                            painter.line_segment([w[0], w[1]], stroke);
                        }
                    }

                    // On release: build the path node.
                    if response.drag_stopped() {
                        let pts = std::mem::take(&mut self.pencil_points);
                        if pts.len() >= 2 {
                            // Build SVG path string from collected points.
                            let mut svg = format!("M {:.3} {:.3}", pts[0].0, pts[0].1);
                            for &(x, y) in &pts[1..] {
                                svg.push_str(&format!(" L {:.3} {:.3}", x, y));
                            }
                            if let Ok(path) = PathData::from_svg(&svg) {
                                let num = doc.node_count() + 1;
                                let stroke_arg = self.prefs.default_stroke_enabled.then_some({
                                    (
                                        self.prefs.default_stroke_color,
                                        self.prefs.default_stroke_width,
                                    )
                                });
                                let node =
                                    make_node(path, self.fill_color, stroke_arg, "Pencil", num);
                                let cmd = Command::AddNode {
                                    node,
                                    layer_id: None,
                                };
                                history.execute(cmd, doc);
                                doc_modified = true;
                            }
                        }
                        return;
                    }

                    if response.dragged_by(egui::PointerButton::Primary) {
                        return;
                    }
                }

                // ── Smooth tool ───────────────────────────────────────────────
                if self.active_tool == Tool::Smooth {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);

                    // On click (or drag end): smooth the hit-tested path node.
                    let should_smooth = response.clicked_by(egui::PointerButton::Primary)
                        || response.drag_stopped();

                    if should_smooth {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let (cx, cy) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                            if let Some(hit_id) = hit_test(doc, cx, cy, renderer) {
                                if let Some(node) = doc.nodes.get(&hit_id) {
                                    if let SceneNodeKind::Path(pn) = &node.kind {
                                        let smoothed = pn.path_data.smooth(0.25, 2);
                                        let mut new_node = node.clone();
                                        if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                            new_pn.path_data = smoothed;
                                        }
                                        history.execute(
                                            Command::UpdateNode {
                                                old: node.clone(),
                                                new: new_node,
                                            },
                                            doc,
                                        );
                                        doc_modified = true;
                                    }
                                }
                            }
                        }
                        return;
                    }
                    if response.dragged_by(egui::PointerButton::Primary) {
                        return;
                    }
                }

                // ── Width tool (interactive variable-width stroke editing) ────
                if self.active_tool == Tool::Width {
                    self.handle_width_tool(ui, &response, doc, view, &mut doc_modified, history);
                    return;
                }

                // ── Text tool ─────────────────────────────────────────────────
                if self.active_tool == Tool::Text {
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            use photonic_core::node::TextNode;
                            let (cx, cy) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                            let (cx, cy) = (self.snap(cx), self.snap(cy));
                            let [r, g, b, a] = self.fill_color;
                            let mut text_node = TextNode::new("Text");
                            text_node.fill = Fill::solid(Color { r, g, b, a });
                            let kind = SceneNodeKind::Text(text_node);
                            let num = doc.node_count() + 1;
                            let mut node =
                                SceneNode::new(format!("Text {}", num), Default::default(), kind);
                            node.transform = photonic_core::transform::Transform::translate(cx, cy);
                            self.tool_commit_add(node, doc, history, &mut doc_modified);
                        }
                    }
                    return;
                }

                // ── Shape creation tools ─────────────────────────────────────
                if !self.active_tool.is_shape_creator() {
                    return;
                }

                if response.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let (cx, cy) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                        self.drag_start_canvas = Some((self.snap(cx), self.snap(cy)));
                    }
                }

                if response.drag_stopped_by(egui::PointerButton::Primary) {
                    if let (Some((sx, sy)), Some(end_pos)) = (
                        self.drag_start_canvas.take(),
                        response.interact_pointer_pos(),
                    ) {
                        let (ex_raw, ey_raw) =
                            view.screen_to_canvas(end_pos.x as f64, end_pos.y as f64);
                        let (mut ex, mut ey) = (self.snap(ex_raw), self.snap(ey_raw));
                        let shift_held = ui.input(|i| i.modifiers.shift);
                        // Line tool: snap endpoint to nearest 45° angle when Snap45 is on or Shift held.
                        if self.active_tool == Tool::Line {
                            if self.line_snap_45 || shift_held {
                                let (snapped_ex, snapped_ey) = snap_line_to_45(sx, sy, ex, ey);
                                ex = snapped_ex;
                                ey = snapped_ey;
                            }
                        } else if shift_held {
                            // Other shape tools: Shift constrains the bounding box to
                            // 1:1 (square / circle / proportional).
                            let (cex, cey) = constrain_to_square(sx, sy, ex, ey);
                            ex = cex;
                            ey = cey;
                        }
                        // Alt: treat the drag start as the shape's center (#10).
                        let ((bsx, bsy), (bex, bey)) = if ui.input(|i| i.modifiers.alt) {
                            shape_corners_from_center(sx, sy, ex, ey)
                        } else {
                            ((sx, sy), (ex, ey))
                        };
                        if (ex - sx).abs() > 2.0 || (ey - sy).abs() > 2.0 {
                            if let Some(path) = self.build_shape(bsx, bsy, bex, bey) {
                                let stroke_arg = self.prefs.default_stroke_enabled.then_some({
                                    (
                                        self.prefs.default_stroke_color,
                                        self.prefs.default_stroke_width,
                                    )
                                });
                                let node = make_node(
                                    path,
                                    self.fill_color,
                                    stroke_arg,
                                    self.active_tool.label(),
                                    doc.node_count() + 1,
                                );
                                self.tool_commit_add(node, doc, history, &mut doc_modified);
                            }
                        }
                    }
                } else if self.drag_start_canvas.is_none()
                    && response.clicked_by(egui::PointerButton::Primary)
                {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let (cx, cy) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                        if let Some(path) =
                            self.build_shape(cx - 50.0, cy - 50.0, cx + 50.0, cy + 50.0)
                        {
                            let stroke_arg = self.prefs.default_stroke_enabled.then_some({
                                (
                                    self.prefs.default_stroke_color,
                                    self.prefs.default_stroke_width,
                                )
                            });
                            let node = make_node(
                                path,
                                self.fill_color,
                                stroke_arg,
                                self.active_tool.label(),
                                doc.node_count() + 1,
                            );
                            self.tool_commit_add(node, doc, history, &mut doc_modified);
                        }
                    }
                }

                // ── Shape preview while dragging ─────────────────────────────
                if let Some((sx, sy)) = self.drag_start_canvas {
                    let cursor = response
                        .interact_pointer_pos()
                        .or_else(|| ui.input(|i| i.pointer.hover_pos()));
                    if let Some(cursor) = cursor {
                        let (ex_raw, ey_raw) =
                            view.screen_to_canvas(cursor.x as f64, cursor.y as f64);
                        let shift_held = ui.input(|i| i.modifiers.shift);
                        let (ex, ey) = if self.active_tool == Tool::Line {
                            if self.line_snap_45 || shift_held {
                                snap_line_to_45(sx, sy, ex_raw, ey_raw)
                            } else {
                                (ex_raw, ey_raw)
                            }
                        } else if shift_held {
                            constrain_to_square(sx, sy, ex_raw, ey_raw)
                        } else {
                            (ex_raw, ey_raw)
                        };
                        // Alt: preview the shape centered on the drag start (#10).
                        let ((bsx, bsy), (bex, bey)) = if ui.input(|i| i.modifiers.alt) {
                            shape_corners_from_center(sx, sy, ex, ey)
                        } else {
                            ((sx, sy), (ex, ey))
                        };
                        if let Some(path) = self.build_shape(bsx, bsy, bex, bey) {
                            let pts = bez_to_screen_points(&path.to_bez_path(), view);
                            if pts.len() >= 2 {
                                let [fr, fg, fb, _] = self.fill_color;
                                let fill = Color32::from_rgba_unmultiplied(
                                    (fr * 255.0) as u8,
                                    (fg * 255.0) as u8,
                                    (fb * 255.0) as u8,
                                    40,
                                );
                                let stroke_color = Color32::from_rgb(110, 86, 207);
                                ui.painter().add(egui::Shape::Path(egui::epaint::PathShape {
                                    points: pts,
                                    closed: true,
                                    fill,
                                    stroke: egui::epaint::PathStroke::new(1.5, stroke_color),
                                }));
                            }
                        }
                    }
                }
            });
        doc_modified = self.process_panel_actions(ctx, doc, view, renderer, history, doc_modified);

        // #171: commit the picked Fill/Stroke color to the Recent list only once
        // the drag ends, so the intermediate colors dragged through the picker
        // don't flood the list. Discrete-click recolor paths (Recent swatch,
        // Color Guide, Recolor) fire on a frame where the pointer also releases,
        // so they still record exactly one color (record_recent_color dedups).
        if self.pending_recent_color.is_some() && ctx.input(|i| i.pointer.any_released()) {
            if let Some(c) = self.pending_recent_color.take() {
                doc.record_recent_color(c);
                doc_modified = true;
            }
        }

        // ── Close the gesture-coalescing window on pointer release (#182) ─────
        // Runs after this frame's edit handlers, so a final same-frame edit still
        // folds into the single undo step before the gesture is sealed. Between
        // gestures the history pushes each command normally.
        if ctx.input(|i| i.pointer.any_released()) {
            history.end_coalescing();
        }

        // ── Eyedropper overlay ────────────────────────────────────────────────
        if self.eyedropper.active() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);

            let (esc, raw_clicked, cursor) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::Escape),
                    i.pointer.primary_clicked(),
                    i.pointer.latest_pos(),
                )
            });
            // Discard the button's own release so it doesn't immediately sample.
            let clicked = if self.eyedropper.skip_click {
                if raw_clicked {
                    self.eyedropper.skip_click = false;
                }
                false
            } else {
                raw_clicked
            };

            if esc {
                self.eyedropper.cancel();
            } else {
                if let Some(pos) = cursor {
                    // Convert the egui cursor position (screen-space, relative to
                    // the egui viewport) to canvas coordinates and sample the
                    // topmost filled node in the document.  This is reliable on
                    // all platforms including Wayland — no screen capture needed.
                    let (cx, cy) = view.screen_to_canvas(pos.x as f64, pos.y as f64);
                    // The raster color-range target samples the raster layer's
                    // own pixels; every other target samples vector fills.
                    let raster_target: Option<NodeId> = match &self.eyedropper.target {
                        Some(EyedropperTarget::RasterColorRange { node_id }) => Some(*node_id),
                        _ => None,
                    };
                    let raster_sample =
                        raster_target.and_then(|nid| self.sample_raster_pixel(doc, nid, cx, cy));
                    let sampled = if raster_target.is_some() {
                        raster_sample.map(|(rgba, _)| {
                            [
                                rgba[0] as f32 / 255.0,
                                rgba[1] as f32 / 255.0,
                                rgba[2] as f32 / 255.0,
                                rgba[3] as f32 / 255.0,
                            ]
                        })
                    } else {
                        photonic_core::sample_fill_at(doc, cx, cy)
                    };

                    // Draw color preview badge near cursor
                    let preview_color = sampled
                        .map(|c| {
                            egui::Color32::from_rgba_unmultiplied(
                                (c[0] * 255.0) as u8,
                                (c[1] * 255.0) as u8,
                                (c[2] * 255.0) as u8,
                                (c[3] * 255.0) as u8,
                            )
                        })
                        .unwrap_or(egui::Color32::TRANSPARENT);

                    let painter = ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Tooltip,
                        egui::Id::new("eyedropper_preview"),
                    ));
                    let preview_rect = egui::Rect::from_min_size(
                        pos + egui::vec2(14.0, -28.0),
                        egui::vec2(28.0, 28.0),
                    );
                    painter.rect_filled(preview_rect, 4.0, preview_color);
                    painter.rect_stroke(
                        preview_rect,
                        4.0,
                        egui::Stroke::new(1.5, egui::Color32::WHITE),
                    );

                    if clicked {
                        if let Some(nid) = raster_target {
                            // Begin (or restart) the color-range mask-out
                            // session with the sampled pixel; a click outside
                            // the layer just cancels the eyedropper.
                            if let Some((rgba, seed)) = raster_sample {
                                self.begin_raster_color_range(doc, nid, rgba, seed);
                                doc_modified = true;
                            }
                        } else if let Some(rgba) = sampled {
                            let picked = photonic_core::Color {
                                r: rgba[0],
                                g: rgba[1],
                                b: rgba[2],
                                a: rgba[3],
                            };
                            self.apply_eyedropper_color(doc, history, picked, &mut doc_modified);
                        }
                        self.eyedropper.cancel();
                    }
                }

                // Full-screen invisible area to block other interactions
                egui::Area::new(egui::Id::new("eyedropper_overlay"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.allocate_rect(ctx.screen_rect(), egui::Sense::click());
                    });
            }
        }

        // ── Per-frame tab bookkeeping + autosave ─────────────────────────────
        // Refresh the active tab's title/dirty from this frame's edits, then run
        // the autosave timer (writes titled docs to disk + untitled docs to the
        // recovery folder when the interval elapses).
        self.sync_active_tab_meta(doc, doc_modified);
        self.run_autosave(ctx, doc, history);

        doc_modified
    }

    // ── Select tool handler ───────────────────────────────────────────────────

    // (Select / Pen / Shape Builder handlers moved to `mod tool_handlers`)

    // (Layer/group operations moved to `mod layer_ops`)
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Returns `true` when viewport keyboard shortcuts should be processed.
///
/// All tool handlers **must** gate every keyboard shortcut through this
/// check.  When any text widget (e.g. the AI chat box) has keyboard focus,
/// `egui::Context::wants_keyboard_input` returns `true` and we suppress
/// every viewport shortcut so typing never accidentally mutates the canvas.
fn viewport_kb(ctx: &egui::Context) -> bool {
    !ctx.wants_keyboard_input()
}

/// Force the width `egui` remembers for `panel_id` to `want`, and report whether
/// the user is currently dragging that panel's resize handle.
///
/// `SidePanel` derives its width from the rect it stored on the previous frame,
/// and that stored rect is the *frame* rect — which is the content's minimum
/// size, not the size we asked for. So any drawer whose widest row needs more
/// room than the panel currently has silently widens the panel, permanently.
/// Our drawers trip that on every single open, because the width tween renders
/// real content into a panel only a few pixels wide; the panel then settles at
/// whatever that group's content demanded rather than at the user's width, which
/// is why flipping between rail tabs appeared to resize the drawer at random.
///
/// Overwriting the stored width before egui reads it makes a deliberate drag the
/// only thing that can change the width. Returns `true` while such a drag is in
/// flight (the caller must leave the width alone, and only then persist it).
fn pin_side_panel_width(ctx: &egui::Context, panel_id: egui::Id, want: f32) -> bool {
    let resizing = ctx
        .read_response(panel_id.with("__resize"))
        .is_some_and(|r| r.dragged());
    if resizing {
        return true;
    }
    // Only the width of the stored rect is read back; its position is recomputed
    // from the available area each frame, so anchoring at `min.x` is fine for a
    // right-hand panel too.
    if let Some(state) = egui::containers::panel::PanelState::load(ctx, panel_id) {
        if (state.rect.width() - want).abs() > 0.01 {
            let mut rect = state.rect;
            rect.max.x = rect.min.x + want;
            ctx.data_mut(|d| {
                d.insert_persisted(panel_id, egui::containers::panel::PanelState { rect })
            });
        }
    }
    false
}

/// Flatten a kurbo `BezPath` into screen-space egui points, approximating
/// cubic and quadratic bezier segments with line segments.

// ── path geometry helpers moved to `mod geometry` (see geometry.rs) ──

// ─── Claude tab ───────────────────────────────────────────────────────────────

impl PhotonicApp {
    // ── "What's New" popup ─────────────────────────────────────────────────────

    // ── Export modal ─────────────────────────────────────────────────────────
}

// ── hit-testing & node helpers moved to `mod hit_test` (see hit_test.rs) ──
// ── chart/tiling demo generators moved to `mod demos` (see demos.rs) ──

/// Render a Keep-a-Changelog section body with light markdown: `### Foo`
/// becomes a small heading, `- item` / `* item` become bullets (nested by leading
/// indentation), and inline `**bold**`, `*italic*` / `_italic_`, `` `code` `` and
/// `[text](url)` links are formatted rather than shown with their literal markup.
/// (We still don't pull in a full markdown crate — just enough for our changelog.)
fn render_changelog_body(ui: &mut egui::Ui, body: &str) {
    const BASE: Color32 = Color32::from_rgb(203, 213, 225);
    for raw in body.lines() {
        // Keep leading whitespace to derive nesting; trim only the trailing edge.
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            ui.add_space(3.0);
            continue;
        }
        // Two leading spaces per nesting level (Keep-a-Changelog sub-bullets).
        let indent = line.len() - trimmed.len();
        let level = (indent / 2) as f32;

        if let Some(h) = trimmed.strip_prefix("### ") {
            ui.add_space(2.0);
            let w = ui.available_width();
            let job = inline_md_job(ui, h.trim(), Color32::from_rgb(226, 232, 240), true, w);
            ui.add(egui::Label::new(job));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            ui.horizontal_top(|ui| {
                ui.add_space(6.0 + level * 14.0);
                ui.label(RichText::new("•").color(Color32::from_rgb(96, 165, 250)));
                // Wrap to the width remaining to the right of the bullet + indent.
                let w = ui.available_width();
                let job = inline_md_job(ui, item.trim(), BASE, false, w);
                ui.add(egui::Label::new(job));
            });
        } else {
            let w = ui.available_width();
            let job = inline_md_job(ui, trimmed, BASE, false, w);
            ui.add(egui::Label::new(job));
        }
    }
}

/// Parse one line of inline markdown into a wrapped [`egui::text::LayoutJob`].
/// Handles `**bold**` (rendered as a brighter "strong" colour), `*italic*` /
/// `_italic_` (egui's synthetic slant), `` `code` `` (monospace on a faint chip)
/// and `[text](url)` (the link text, inline). Unmatched markers stay literal.
fn inline_md_job(
    ui: &egui::Ui,
    text: &str,
    base: Color32,
    base_strong: bool,
    wrap_width: f32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let body_font = egui::TextStyle::Body.resolve(ui.style());
    let mono_font = egui::TextStyle::Monospace.resolve(ui.style());
    let strong_col = Color32::from_rgb(236, 242, 250);
    let code_col = Color32::from_rgb(147, 197, 253);
    let code_bg = Color32::from_rgb(30, 30, 50);

    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;

    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut buf = String::new();
    let mut bold = base_strong;
    let mut italic = false;

    // Flush the plain-text accumulator as one styled run.
    let flush = |job: &mut LayoutJob, buf: &mut String, bold: bool, italic: bool| {
        if buf.is_empty() {
            return;
        }
        let mut fmt = TextFormat::simple(body_font.clone(), if bold { strong_col } else { base });
        fmt.italics = italic;
        job.append(buf, 0.0, fmt);
        buf.clear();
    };

    let mut i = 0usize;
    while i < n {
        let c = chars[i];
        if c == '`' {
            // Inline code up to the next backtick.
            if let Some(rel) = chars[i + 1..].iter().position(|&x| x == '`') {
                flush(&mut job, &mut buf, bold, italic);
                let end = i + 1 + rel;
                let code: String = chars[i + 1..end].iter().collect();
                let mut fmt = TextFormat::simple(mono_font.clone(), code_col);
                fmt.background = code_bg;
                job.append(&code, 0.0, fmt);
                i = end + 1;
                continue;
            }
        } else if c == '*' && i + 1 < n && chars[i + 1] == '*' {
            // Bold toggle.
            flush(&mut job, &mut buf, bold, italic);
            bold = !bold;
            i += 2;
            continue;
        } else if (c == '*' || c == '_') && is_emph_delim(&chars, i, italic) {
            // Italic toggle (word-boundary aware so `snake_case` is untouched).
            flush(&mut job, &mut buf, bold, italic);
            italic = !italic;
            i += 1;
            continue;
        } else if c == '[' {
            // Link `[text](url)` → keep the link text, drop the URL.
            if let Some(close_rel) = chars[i + 1..].iter().position(|&x| x == ']') {
                let close = i + 1 + close_rel;
                if close + 1 < n && chars[close + 1] == '(' {
                    if let Some(paren_rel) = chars[close + 2..].iter().position(|&x| x == ')') {
                        for ch in &chars[i + 1..close] {
                            buf.push(*ch);
                        }
                        i = close + 2 + paren_rel + 1;
                        continue;
                    }
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut job, &mut buf, bold, italic);
    job
}

/// Whether the `*`/`_` at `i` acts as an emphasis delimiter (rather than literal
/// punctuation or an intra-word underscore like `set_paint`). Opening markers
/// must be followed by a non-space; closing markers must follow a non-space; and
/// `_` additionally requires a word boundary on the outer side so identifiers are
/// never italicised.
fn is_emph_delim(chars: &[char], i: usize, in_italic: bool) -> bool {
    let c = chars[i];
    let prev = i.checked_sub(1).map(|p| chars[p]);
    let next = chars.get(i + 1).copied();
    if in_italic {
        let prev_ok = prev.is_some_and(|p| !p.is_whitespace());
        if c == '_' {
            prev_ok && next.is_none_or(|x| !x.is_alphanumeric())
        } else {
            prev_ok
        }
    } else {
        let next_ok = next.is_some_and(|x| !x.is_whitespace() && x != c);
        if c == '_' {
            next_ok && prev.is_none_or(|p| !p.is_alphanumeric())
        } else {
            next_ok
        }
    }
}

#[cfg(test)]
mod direct_select_geometry_tests {
    use super::*;
    use kurbo::BezPath;

    fn rect() -> BezPath {
        // Closed square 0,0 .. 100,100 (M,L,L,L,Z) — four straight corners.
        let mut b = BezPath::new();
        b.move_to((0.0, 0.0));
        b.line_to((100.0, 0.0));
        b.line_to((100.0, 100.0));
        b.line_to((0.0, 100.0));
        b.close_path();
        b
    }

    #[test]
    fn all_rect_corners_are_roundable() {
        let m = straight_corners(&rect());
        // Indices 0..=3 are the four anchors (MoveTo + 3 LineTo).
        assert_eq!(m.len(), 4, "expected 4 straight corners, got {}", m.len());
        for i in 0..4 {
            assert!(m.contains_key(&i), "anchor {i} should be a straight corner");
        }
    }

    #[test]
    fn rounding_one_corner_adds_a_quad_and_preserves_others() {
        let bez = rect();
        let sel: std::collections::HashSet<usize> = [1usize].into_iter().collect();
        let out = round_selected_corners(&bez, &sel, 10.0);
        // Exactly one quad segment is introduced for the single rounded corner.
        let quads = out
            .elements()
            .iter()
            .filter(|e| matches!(e, PathEl::QuadTo(_, _)))
            .count();
        assert_eq!(quads, 1, "one corner rounded → one quad arc");
        // Still a closed path.
        assert!(out
            .elements()
            .iter()
            .any(|e| matches!(e, PathEl::ClosePath)));
    }

    #[test]
    fn rounding_isolated_corner_rounds_past_half_edge() {
        // An isolated selected corner (neighbours not selected) must be allowed
        // to retreat past half its shorter edge — the old unconditional
        // `lin/2` clamp artificially capped it there (issue #165).
        let bez = rect();
        let sel: std::collections::HashSet<usize> = [1usize].into_iter().collect();
        let r = 60.0; // > half of the 100-unit edge
        let out = round_selected_corners(&bez, &sel, r);
        let els = out.elements();
        // Single fillet → single quad; the point before it is the retreat start.
        let qpos = els
            .iter()
            .position(|e| matches!(e, PathEl::QuadTo(_, _)))
            .expect("one rounded corner → one quad");
        let fs = match els[qpos - 1] {
            PathEl::LineTo(p) | PathEl::MoveTo(p) => p,
            _ => panic!("expected a line/move endpoint before the fillet quad"),
        };
        // Corner 1 sits at (100,0) with its incoming edge running from (0,0).
        let corner = Point::new(100.0, 0.0);
        let retreat = ((corner.x - fs.x).powi(2) + (corner.y - fs.y).powi(2)).sqrt();
        assert!(
            retreat > 50.0 + 1e-6,
            "isolated corner should round past half-edge, retreat was {retreat}"
        );
    }

    #[test]
    fn rounding_two_adjacent_corners_never_overlap() {
        // Two adjacent selected corners share an edge; their fillets must split
        // it (meet at most at the midpoint) rather than overrun each other.
        let bez = rect();
        let sel: std::collections::HashSet<usize> = [1usize, 2usize].into_iter().collect();
        // Large radius vs the 100-unit shared edge (100,0)->(100,100).
        let out = round_selected_corners(&bez, &sel, 90.0);
        let els: Vec<PathEl> = out.elements().to_vec();
        let quads: Vec<usize> = els
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, PathEl::QuadTo(_, _)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(quads.len(), 2, "two rounded corners → two quads");
        let fe1 = match els[quads[0]] {
            PathEl::QuadTo(_, p) => p, // corner 1 exit toward corner 2
            _ => unreachable!(),
        };
        let fs2 = match els[quads[1] - 1] {
            PathEl::LineTo(p) => p, // corner 2 entry coming from corner 1
            _ => panic!("expected a line endpoint before the second fillet quad"),
        };
        let c1 = Point::new(100.0, 0.0);
        let c2 = Point::new(100.0, 100.0);
        let rout = ((fe1.x - c1.x).powi(2) + (fe1.y - c1.y).powi(2)).sqrt();
        let rin = ((fs2.x - c2.x).powi(2) + (fs2.y - c2.y).powi(2)).sqrt();
        // No overlap ⟺ the two retreats along the shared edge sum to at most its
        // length; and corner 1's exit never passes corner 2's entry.
        assert!(
            rout + rin <= 100.0 + 1e-6,
            "adjacent fillets overlap on the shared edge: {rout} + {rin}"
        );
        assert!(
            fe1.y <= fs2.y + 1e-6,
            "corner 1 fillet crosses corner 2 fillet ({} vs {})",
            fe1.y,
            fs2.y
        );
    }

    #[test]
    fn rounding_non_adjacent_corners_round_independently() {
        // Opposite corners of the square share no edge, so each should round
        // freely (past half-edge) without the shared-edge 50/50 split.
        let bez = rect();
        let sel: std::collections::HashSet<usize> = [1usize, 3usize].into_iter().collect();
        let r = 60.0; // > half-edge; the old lin/2 clamp would have capped it
        let out = round_selected_corners(&bez, &sel, r);
        let els: Vec<PathEl> = out.elements().to_vec();
        let quads: Vec<usize> = els
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, PathEl::QuadTo(_, _)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(quads.len(), 2, "two rounded corners → two quads");
        for (&q, corner) in quads
            .iter()
            .zip([Point::new(100.0, 0.0), Point::new(0.0, 100.0)])
        {
            let fs = match els[q - 1] {
                PathEl::LineTo(p) | PathEl::MoveTo(p) => p,
                _ => panic!("expected a line/move endpoint before a fillet quad"),
            };
            let retreat = ((corner.x - fs.x).powi(2) + (corner.y - fs.y).powi(2)).sqrt();
            assert!(
                retreat > 50.0 + 1e-6,
                "non-adjacent corner should round independently past half-edge, retreat was {retreat}"
            );
        }
    }

    #[test]
    fn rounding_zero_radius_is_noop() {
        let bez = rect();
        let sel: std::collections::HashSet<usize> = [0, 1, 2, 3].into_iter().collect();
        let out = round_selected_corners(&bez, &sel, 0.0);
        assert_eq!(out.elements().len(), bez.elements().len());
    }

    #[test]
    fn curve_has_handles_line_does_not() {
        // M then a single CurveTo: anchor index 1 has an IN handle (c2); its
        // OUT handle is None (no following curve).
        let mut b = BezPath::new();
        b.move_to((0.0, 0.0));
        b.curve_to((10.0, 0.0), (20.0, 10.0), (20.0, 20.0));
        let els = b.elements();
        assert!(anchor_handle_point(els, 1, HandleKind::In).is_some());
        assert!(anchor_handle_point(els, 1, HandleKind::Out).is_none());
        // A pure rectangle anchor has neither handle.
        let r = rect();
        assert!(anchor_handle_point(r.elements(), 1, HandleKind::In).is_none());
    }

    #[test]
    fn set_handle_moves_only_target_when_not_mirrored() {
        let mut b = BezPath::new();
        b.move_to((0.0, 0.0));
        b.curve_to((10.0, 0.0), (20.0, 10.0), (20.0, 20.0));
        let out = bez_set_handle(&b, 1, HandleKind::In, Point::new(25.0, 5.0), false);
        if let PathEl::CurveTo(_, c2, p) = out.elements()[1] {
            assert_eq!(c2, Point::new(25.0, 5.0));
            assert_eq!(p, Point::new(20.0, 20.0), "endpoint must not move");
        } else {
            panic!("expected CurveTo");
        }
    }

    #[test]
    fn set_anchor_position_moves_endpoint() {
        let bez = rect();
        let out = bez_set_anchor_position(&bez, 1, 130.0, 5.0);
        // Anchor at element index 1 is the second vertex.
        if let PathEl::LineTo(p) = out.elements()[1] {
            assert_eq!(p, Point::new(130.0, 5.0));
        } else {
            panic!("expected LineTo at index 1");
        }
    }

    // A closed cubic "diamond": MoveTo + 4 CurveTo back to start + ClosePath.
    // The start point is listed twice (index 0 and the closing CurveTo at 4).
    fn closed_curve() -> BezPath {
        let mut b = BezPath::new();
        b.move_to((50.0, 0.0));
        b.curve_to((80.0, 20.0), (100.0, 30.0), (100.0, 50.0));
        b.curve_to((80.0, 80.0), (60.0, 100.0), (50.0, 100.0));
        b.curve_to((20.0, 80.0), (0.0, 70.0), (0.0, 50.0));
        b.curve_to((20.0, 20.0), (40.0, 0.0), (50.0, 0.0));
        b.close_path();
        b
    }

    #[test]
    fn seam_anchor_resolves_both_handles() {
        let b = closed_curve();
        // Logical start anchor (index 0): Out handle on element 1's c1, In
        // handle on the closing curve (element 4) c2 — the seam case.
        let (in_h, out_h) = anchor_handle_pair(&b, 0);
        assert!(
            out_h.is_some(),
            "start anchor should expose its outgoing handle"
        );
        assert!(
            in_h.is_some(),
            "start anchor should resolve its incoming handle across the seam"
        );
    }

    #[test]
    fn seam_smooth_mirror_actually_moves_opposite_handle() {
        let b = closed_curve();
        // Drag the start anchor's OUT handle with mirror on; the IN handle (which
        // lives on the closing element) must move to stay collinear.
        let before = anchor_handle_pair(&b, 0).0.unwrap().1;
        let out = bez_set_handle(&b, 0, HandleKind::Out, Point::new(70.0, -30.0), true);
        let after = anchor_handle_pair(&out, 0).0.unwrap().1;
        assert_ne!(before, after, "seam mirror must update the opposite handle");
    }

    #[test]
    fn cusp_is_not_detected_as_smooth() {
        // Two curves meeting at a 90° cusp (handles not collinear).
        let mut b = BezPath::new();
        b.move_to((0.0, 0.0));
        b.curve_to((10.0, 0.0), (20.0, 0.0), (30.0, 0.0)); // arrives along +x
        b.curve_to((30.0, 10.0), (30.0, 20.0), (30.0, 30.0)); // leaves along +y
                                                              // Anchor at index 1 (point 30,0) has In handle (20,0) and Out (30,10):
                                                              // directions are perpendicular → cusp, not smooth.
        assert!(!is_smooth_anchor(&b, 1), "perpendicular handles are a cusp");
    }

    #[test]
    fn collinear_handles_detected_as_smooth() {
        let mut b = BezPath::new();
        b.move_to((0.0, 0.0));
        b.curve_to((10.0, 0.0), (20.0, 0.0), (30.0, 0.0)); // in handle at (20,0)
        b.curve_to((40.0, 0.0), (50.0, 0.0), (60.0, 0.0)); // out handle at (40,0)
                                                           // At (30,0): in dir →(20,0)-(30,0)=(-1,0), out dir →(40,0)-(30,0)=(+1,0): opposite.
        assert!(
            is_smooth_anchor(&b, 1),
            "collinear opposite handles are smooth"
        );
    }
}

#[cfg(test)]
mod crash_report_url_tests {
    use super::*;

    fn report_with_backtrace(len: usize) -> photonic_core::CrashReport {
        // A backtrace full of bytes that each percent-encode to %XX (3 chars):
        // spaces, slashes, colons, newlines — exactly the worst case in release.
        let backtrace: String = " /:\n".repeat(len / 4 + 1);
        photonic_core::CrashReport {
            version: "9.9.9".to_string(),
            timestamp: "2026-06-30T00:00:00Z".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            panic_message: "boom".to_string(),
            location: Some("src/lib.rs:1:1".to_string()),
            backtrace,
        }
    }

    #[test]
    fn url_stays_under_github_limit_for_huge_backtrace() {
        // ~16 KB of all-%XX backtrace would encode to ~48 KB if unbounded.
        let report = report_with_backtrace(16_000);
        let url = issue_url_for_report(&report);
        assert!(
            url.len() <= 7000,
            "encoded URL must stay within the budget, got {}",
            url.len()
        );
        // Truncated bodies re-close the code fence via the trim note.
        let body = url.split("&body=").nth(1).unwrap();
        assert!(body.contains(&percent_encode("truncated to fit")));
    }

    #[test]
    fn small_report_is_not_truncated() {
        let report = report_with_backtrace(40);
        let url = issue_url_for_report(&report);
        assert!(url.len() <= 7000);
        let body = url.split("&body=").nth(1).unwrap();
        assert!(!body.contains(&percent_encode("truncated to fit")));
    }
}

/// One-line video-mode status bar content: active sequence, frame/format,
/// playhead timecode, and track counts — the video analogue of the vector
/// tool/objects/zoom line. `None` when there's no active sequence (degrades
/// gracefully; the 04 §1.3 invariant means video mode normally has one).
fn video_status_line(doc: &Document, playhead: photonic_core::timeline::Tick) -> Option<String> {
    let p = doc.timeline.as_ref()?;
    let seq = p.sequences.get(&p.active_sequence?)?;
    let fmt = seq.formats.get(seq.active_format)?;
    let fr = seq.frame_rate;
    let fps = fr.num as f64 / fr.den.max(1) as f64;
    let frame_idx = fr.frame_at(playhead).max(0);
    let fpsi = (fps.round() as i64).max(1);
    let ff = frame_idx % fpsi;
    let secs = frame_idx / fpsi;
    let tc = format!(
        "{:02}:{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60,
        ff
    );
    Some(format!(
        "{} {}  •  {}×{} {:.0}fps  •  {}  •  {}V / {}A",
        ph::FILM_SLATE,
        seq.name,
        fmt.width,
        fmt.height,
        fps,
        tc,
        seq.video_tracks.len(),
        seq.audio_tracks.len(),
    ))
}
