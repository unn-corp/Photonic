//! Node editor (04 §4.1 / 08 §6.1), split across two surfaces:
//! - the left-rail `DrawerGroup::NodeEditor` drawer — add-node palette, selected
//!   node inspector, graph info ([`draw_node_editor_palette`]);
//! - the central-panel node canvas content state — the egui graph canvas +
//!   viewer inset that replaces the program monitor while a composition is being
//!   edited ([`draw_node_canvas`]).
//!
//! Both interiors are owned by 08-fusion-node-flows.md. Open-graph / selected-
//! node / active state lives on [`super::VideoPanelUi`]; all *view* state (pan/
//! zoom, in-progress wire/node drag, viewer pin, palette search) is session-only
//! and kept in egui temp memory, never in the document.
//!
//! ## Wiring discipline (DoD: every mutation → pure op → `CommandHistory`)
//! The central canvas owns `&mut Document` + `&mut CommandHistory` (like the
//! vector central panel, which commits directly), so it is the SOLE committer.
//! Every graph edit is built by a `photonic_core::timeline::graph_ops` /`ops`
//! pure function into a [`TimelineCmd`] and pushed through
//! [`CommandHistory::execute_discrete`] — the document is never mutated in place.
//! The left-rail palette cannot see history (drawers only get an immutable doc),
//! so it emits typed [`PaletteIntent`]s into egui memory that the canvas drains
//! and commits in the same frame (the left drawer draws before the central panel
//! in `PhotonicApp::draw`). This keeps a single `graph_ops → history` path.
//!
//! egui-snarl was evaluated and rejected: only egui-snarl 0.5 targets our egui
//! 0.29, and it wants to *own* the graph and mutate it in place, which fights the
//! rule that the core `NodeGraph` is authoritative and every edit flows through
//! `graph_ops` → undo. 08 §9 declares the data model UI-library-agnostic and the
//! canvas a swappable UI layer, so this is a first-class realization, not a
//! fallback — and it keeps zero new-dependency risk on the workspace build.

use super::VideoPanelUi;
use crate::panels::PropPanelCtx;
use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};
use photonic_core::history::{Command, CommandHistory};
use photonic_core::layer::BlendMode;
use photonic_core::timeline::prop_registry;
use photonic_core::timeline::{
    graph_ops, ops, EffectKind, FitMode, Grade, GraphEdge, GraphId, GraphNode, GraphNodeId,
    GraphOp, InPort, MaskShapeKind, NodePos, OutPort, PropTargetKind, PropValue, PropValueKind,
    SequenceId, TextGen, Tick, TimelineCmd, TrackId,
};
use photonic_core::Document;

// ── Layout constants (graph-space units, scaled by zoom on draw) ─────────────
const NODE_W: f32 = 168.0;
const HEADER_H: f32 = 26.0;
const PORT_ROW_H: f32 = 20.0;
const INLINE_ROW_H: f32 = 24.0;
const BODY_PAD: f32 = 8.0;
/// Socket radius in screen px (kept constant so sockets stay clickable at zoom).
const SOCKET_R: f32 = 5.5;
/// Grab radius (screen px) for port hit-testing.
const PORT_GRAB_R: f32 = 11.0;
const MIN_ZOOM: f32 = 0.35;
const MAX_ZOOM: f32 = 2.5;
/// DESIGN.md `success` (#64C87A) — the `Mask` port hue. Theme-independent
/// (functional data-coding, DESIGN.md §Components "Node-editor port sockets").
const MASK_GREEN: Color32 = Color32::from_rgb(0x64, 0xC8, 0x7A);

// ── Port types ───────────────────────────────────────────────────────────────

/// Visual port type (08 §3.1). `Value` is post-v1 (no v1 op emits one), so the
/// canvas only ever colors `Image`/`Mask`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PortType {
    Image,
    Mask,
}

impl PortType {
    /// DESIGN.md §Components: `Image` = primary accent, `Mask` = success green.
    fn color(self, ui: &Ui) -> Color32 {
        match self {
            PortType::Image => ui.visuals().selection.stroke.color,
            PortType::Mask => MASK_GREEN,
        }
    }
}

/// One port of a node: its index, type, and short label.
#[derive(Copy, Clone)]
struct Port {
    idx: u16,
    ty: PortType,
    label: &'static str,
}

const fn p(idx: u16, ty: PortType, label: &'static str) -> Port {
    Port { idx, ty, label }
}

/// Input and output ports of an op (08 §2 catalog). `Switch` is fixed at 4 inputs
/// in v1 (variable arity is post-v1). `Invert` is typed `Image` for coloring
/// though it is generic over Image/Mask (08 §2 note).
fn op_ports(op: &GraphOp) -> (Vec<Port>, Vec<Port>) {
    use PortType::{Image, Mask};
    let (i, o): (&[Port], &[Port]) = match op {
        GraphOp::Output => (&[p(0, Image, "in")], &[]),
        GraphOp::ClipIn
        | GraphOp::MediaIn { .. }
        | GraphOp::VectorIn { .. }
        | GraphOp::SolidColor => (&[], &[p(0, Image, "out")]),
        GraphOp::Merge { .. } => (&[p(0, Image, "a"), p(1, Image, "b")], &[p(0, Image, "out")]),
        GraphOp::Switch => (
            &[
                p(0, Image, "0"),
                p(1, Image, "1"),
                p(2, Image, "2"),
                p(3, Image, "3"),
            ],
            &[p(0, Image, "out")],
        ),
        GraphOp::Transform2D
        | GraphOp::Crop
        | GraphOp::Resize { .. }
        | GraphOp::Blur
        | GraphOp::Sharpen
        | GraphOp::Glow
        | GraphOp::ChromaKey
        | GraphOp::LumaKey
        | GraphOp::Grade { .. }
        | GraphOp::Lut { .. }
        | GraphOp::TimeOffset { .. } => (&[p(0, Image, "in")], &[p(0, Image, "out")]),
        GraphOp::MaskShape { .. } => (&[], &[p(0, Mask, "out")]),
        GraphOp::MaskFromMatte => (&[p(0, Image, "in")], &[p(0, Mask, "out")]),
        GraphOp::Invert => (&[p(0, Image, "in")], &[p(0, Image, "out")]),
        GraphOp::ChannelSplit => (
            &[p(0, Image, "in")],
            &[
                p(0, Mask, "r"),
                p(1, Mask, "g"),
                p(2, Mask, "b"),
                p(3, Mask, "a"),
            ],
        ),
        GraphOp::ChannelCombine => (
            &[
                p(0, Mask, "r"),
                p(1, Mask, "g"),
                p(2, Mask, "b"),
                p(3, Mask, "a"),
            ],
            &[p(0, Image, "out")],
        ),
        GraphOp::Text { .. } => (&[], &[p(0, Image, "out")]),
        GraphOp::Note { .. } => (&[], &[]),
        // Forward-compat (39 §2.2): an op this build does not understand is
        // drawn as an inert unary passthrough — one image in, one image out,
        // mirroring how it lowers in the engine. Non-editable, but movable and
        // deletable; never guessed into a known op's ports.
        GraphOp::Unknown(_) => (&[p(0, Image, "in")], &[p(0, Image, "out")]),
    };
    (i.to_vec(), o.to_vec())
}

// ── Add-node catalog (08 §6.2 families) ───────────────────────────────────────

/// A palette-addable node kind. Sources that need an asset/vector ref
/// (`MediaIn`/`VectorIn`/`Lut`) are omitted from the searchable palette — they
/// are placed with an asset context from the media pool, not conjured blank.
#[derive(Copy, Clone, PartialEq, Eq)]
enum NodeKind {
    ClipIn,
    Merge,
    Switch,
    Transform2D,
    Crop,
    Resize,
    Blur,
    Sharpen,
    Glow,
    ChromaKey,
    LumaKey,
    MaskShape,
    MaskFromMatte,
    Invert,
    ChannelSplit,
    ChannelCombine,
    Grade,
    SolidColor,
    Text,
    TimeOffset,
    Note,
}

/// Node families in palette order (08 §6.2).
#[derive(Copy, Clone, PartialEq, Eq)]
enum Family {
    Sources,
    Compositing,
    Filters,
    Keys,
    Masks,
    Color,
    Generators,
    Time,
    Utility,
}

impl Family {
    fn title(self) -> &'static str {
        match self {
            Family::Sources => "Sources",
            Family::Compositing => "Compositing",
            Family::Filters => "Filters",
            Family::Keys => "Keys",
            Family::Masks => "Masks",
            Family::Color => "Color",
            Family::Generators => "Generators",
            Family::Time => "Time",
            Family::Utility => "Utility",
        }
    }
    const ORDER: [Family; 9] = [
        Family::Sources,
        Family::Compositing,
        Family::Filters,
        Family::Keys,
        Family::Masks,
        Family::Color,
        Family::Generators,
        Family::Time,
        Family::Utility,
    ];
}

impl NodeKind {
    const ALL: [NodeKind; 21] = [
        NodeKind::ClipIn,
        NodeKind::Merge,
        NodeKind::Switch,
        NodeKind::Transform2D,
        NodeKind::Crop,
        NodeKind::Resize,
        NodeKind::Blur,
        NodeKind::Sharpen,
        NodeKind::Glow,
        NodeKind::ChromaKey,
        NodeKind::LumaKey,
        NodeKind::MaskShape,
        NodeKind::MaskFromMatte,
        NodeKind::Invert,
        NodeKind::ChannelSplit,
        NodeKind::ChannelCombine,
        NodeKind::Grade,
        NodeKind::SolidColor,
        NodeKind::Text,
        NodeKind::TimeOffset,
        NodeKind::Note,
    ];

    fn label(self) -> &'static str {
        match self {
            NodeKind::ClipIn => "Clip Input",
            NodeKind::Merge => "Merge",
            NodeKind::Switch => "Switch",
            NodeKind::Transform2D => "Transform",
            NodeKind::Crop => "Crop",
            NodeKind::Resize => "Resize",
            NodeKind::Blur => "Blur",
            NodeKind::Sharpen => "Sharpen",
            NodeKind::Glow => "Glow",
            NodeKind::ChromaKey => "Chroma Key",
            NodeKind::LumaKey => "Luma Key",
            NodeKind::MaskShape => "Mask Shape",
            NodeKind::MaskFromMatte => "Mask from Matte",
            NodeKind::Invert => "Invert",
            NodeKind::ChannelSplit => "Channel Split",
            NodeKind::ChannelCombine => "Channel Combine",
            NodeKind::Grade => "Grade",
            NodeKind::SolidColor => "Solid Color",
            NodeKind::Text => "Text",
            NodeKind::TimeOffset => "Time Offset",
            NodeKind::Note => "Note",
        }
    }

    fn family(self) -> Family {
        match self {
            NodeKind::ClipIn => Family::Sources,
            NodeKind::Merge | NodeKind::Switch => Family::Compositing,
            NodeKind::Transform2D
            | NodeKind::Crop
            | NodeKind::Resize
            | NodeKind::Blur
            | NodeKind::Sharpen
            | NodeKind::Glow => Family::Filters,
            NodeKind::ChromaKey | NodeKind::LumaKey => Family::Keys,
            NodeKind::MaskShape
            | NodeKind::MaskFromMatte
            | NodeKind::Invert
            | NodeKind::ChannelSplit
            | NodeKind::ChannelCombine => Family::Masks,
            NodeKind::Grade => Family::Color,
            NodeKind::SolidColor | NodeKind::Text => Family::Generators,
            NodeKind::TimeOffset => Family::Time,
            NodeKind::Note => Family::Utility,
        }
    }

    fn make_op(self) -> GraphOp {
        match self {
            NodeKind::ClipIn => GraphOp::ClipIn,
            NodeKind::Merge => GraphOp::Merge {
                mode: BlendMode::default(),
            },
            NodeKind::Switch => GraphOp::Switch,
            NodeKind::Transform2D => GraphOp::Transform2D,
            NodeKind::Crop => GraphOp::Crop,
            NodeKind::Resize => GraphOp::Resize {
                fit: FitMode::Contain,
            },
            NodeKind::Blur => GraphOp::Blur,
            NodeKind::Sharpen => GraphOp::Sharpen,
            NodeKind::Glow => GraphOp::Glow,
            NodeKind::ChromaKey => GraphOp::ChromaKey,
            NodeKind::LumaKey => GraphOp::LumaKey,
            NodeKind::MaskShape => GraphOp::MaskShape {
                shape: MaskShapeKind::Ellipse,
            },
            NodeKind::MaskFromMatte => GraphOp::MaskFromMatte,
            NodeKind::Invert => GraphOp::Invert,
            NodeKind::ChannelSplit => GraphOp::ChannelSplit,
            NodeKind::ChannelCombine => GraphOp::ChannelCombine,
            NodeKind::Grade => GraphOp::Grade {
                grade: Grade::default(),
            },
            NodeKind::SolidColor => GraphOp::SolidColor,
            NodeKind::Text => GraphOp::Text {
                text: TextGen::default(),
            },
            NodeKind::TimeOffset => GraphOp::TimeOffset { offset: Tick(0) },
            NodeKind::Note => GraphOp::Note {
                text: "Note".to_string(),
            },
        }
    }

    /// Whether this kind is offerable in the given graph context. `ClipIn` is
    /// illegal in the project graph (08 §5).
    fn allowed(self, is_project_graph: bool) -> bool {
        !(is_project_graph && matches!(self, NodeKind::ClipIn))
    }

    fn matches_query(self, q: &str) -> bool {
        q.is_empty()
            || self.label().to_lowercase().contains(q)
            || self.family().title().to_lowercase().contains(q)
    }
}

// ── Op display + params ───────────────────────────────────────────────────────

/// Short op label shown in a node header.
fn op_title(op: &GraphOp) -> String {
    match op {
        GraphOp::Output => "Output".into(),
        GraphOp::ClipIn => "Clip Input".into(),
        GraphOp::MediaIn { .. } => "Media Input".into(),
        GraphOp::VectorIn { .. } => "Vector Input".into(),
        GraphOp::SolidColor => "Solid Color".into(),
        GraphOp::Merge { mode } => format!("Merge · {}", blend_label(*mode)),
        GraphOp::Transform2D => "Transform".into(),
        GraphOp::Crop => "Crop".into(),
        GraphOp::Resize { .. } => "Resize".into(),
        GraphOp::Blur => "Blur".into(),
        GraphOp::Sharpen => "Sharpen".into(),
        GraphOp::Glow => "Glow".into(),
        GraphOp::ChromaKey => "Chroma Key".into(),
        GraphOp::LumaKey => "Luma Key".into(),
        GraphOp::MaskShape { .. } => "Mask Shape".into(),
        GraphOp::MaskFromMatte => "Mask from Matte".into(),
        GraphOp::Invert => "Invert".into(),
        GraphOp::ChannelSplit => "Channel Split".into(),
        GraphOp::ChannelCombine => "Channel Combine".into(),
        GraphOp::Grade { .. } => "Grade".into(),
        GraphOp::Lut { .. } => "LUT".into(),
        GraphOp::Text { .. } => "Text".into(),
        GraphOp::TimeOffset { .. } => "Time Offset".into(),
        GraphOp::Switch => "Switch".into(),
        GraphOp::Note { .. } => "Note".into(),
        // Forward-compat (39 §2.2): show the preserved op tag verbatim so the
        // user sees exactly what a newer build wrote; the node is non-editable.
        GraphOp::Unknown(_) => op.unknown_tag().unwrap_or("Unknown").to_string(),
    }
}

fn blend_label(mode: BlendMode) -> &'static str {
    // Short form for the 26 modes; only the common few are spelled out compactly.
    match mode {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
        BlendMode::Darken => "Darken",
        BlendMode::Lighten => "Lighten",
        BlendMode::ColorDodge => "Dodge",
        BlendMode::ColorBurn => "Burn",
        BlendMode::HardLight => "Hard Light",
        BlendMode::SoftLight => "Soft Light",
        BlendMode::Difference => "Difference",
        BlendMode::Exclusion => "Exclusion",
        BlendMode::Hue => "Hue",
        BlendMode::Saturation => "Saturation",
        BlendMode::Color => "Color",
        BlendMode::Luminosity => "Luminosity",
        BlendMode::LinearDodge => "Lin Dodge",
        BlendMode::LinearBurn => "Lin Burn",
        BlendMode::Subtract => "Subtract",
        BlendMode::Divide => "Divide",
        BlendMode::VividLight => "Vivid Light",
        BlendMode::LinearLight => "Lin Light",
        BlendMode::PinLight => "Pin Light",
        BlendMode::HardMix => "Hard Mix",
        BlendMode::DarkerColor => "Darker",
        BlendMode::LighterColor => "Lighter",
    }
}

/// One editable parameter row: registry-style descriptor for the inspector /
/// inline widgets. Editing routes through `graph_ops::set_node_param` (the only
/// core op that mutates node content), so this covers the *animatable param
/// bag* only — structural op fields (blend mode, offset, asset refs, embedded
/// grade) have no edit op yet and are shown read-only by the inspector.
#[derive(Clone, Copy)]
struct ParamDesc {
    path: &'static str,
    label: &'static str,
    kind: PropValueKind,
    range: Option<(f64, f64)>,
}

/// Editable param rows for an op. Effect-family ops draw from the shared
/// `prop_registry` (guaranteed to match the engine's `Effect` params); the rest
/// use small hand-authored lists (the graph-node param surface is an open map,
/// 08 §6.4, so extra paths validate leniently).
fn op_params(op: &GraphOp) -> Vec<ParamDesc> {
    fn from_registry(kind: PropTargetKind) -> Vec<ParamDesc> {
        prop_registry::entries(kind)
            .iter()
            .map(|e| ParamDesc {
                path: e.path,
                label: pretty_path(e.path),
                kind: e.kind,
                range: e.range,
            })
            .collect()
    }
    fn f(path: &'static str, label: &'static str, lo: f64, hi: f64) -> ParamDesc {
        ParamDesc {
            path,
            label,
            kind: PropValueKind::Float,
            range: Some((lo, hi)),
        }
    }
    match op {
        GraphOp::Blur => from_registry(PropTargetKind::Effect(EffectKind::Blur)),
        GraphOp::Sharpen => from_registry(PropTargetKind::Effect(EffectKind::Sharpen)),
        GraphOp::Glow => from_registry(PropTargetKind::Effect(EffectKind::Glow)),
        GraphOp::ChromaKey => from_registry(PropTargetKind::Effect(EffectKind::ChromaKey)),
        GraphOp::LumaKey => from_registry(PropTargetKind::Effect(EffectKind::LumaKey)),
        GraphOp::MaskShape { .. } => {
            from_registry(PropTargetKind::Effect(EffectKind::MaskShapeGen))
        }
        GraphOp::Transform2D => from_registry(PropTargetKind::ClipTransform)
            .into_iter()
            .filter(|param| param.path != "transform.opacity")
            .collect(),
        GraphOp::Merge { .. } => vec![f("params.opacity", "Opacity", 0.0, 1.0)],
        GraphOp::SolidColor => vec![ParamDesc {
            path: "params.color",
            label: "Color",
            kind: PropValueKind::Color,
            range: None,
        }],
        GraphOp::Crop => vec![
            f("params.left", "Left", 0.0, 1.0),
            f("params.top", "Top", 0.0, 1.0),
            f("params.right", "Right", 0.0, 1.0),
            f("params.bottom", "Bottom", 0.0, 1.0),
        ],
        GraphOp::Resize { .. } => vec![
            f("params.width", "Width", 1.0, 8192.0),
            f("params.height", "Height", 1.0, 8192.0),
        ],
        GraphOp::Switch => vec![ParamDesc {
            path: "params.selected",
            label: "Selected",
            kind: PropValueKind::Enum,
            range: Some((0.0, 3.0)),
        }],
        _ => vec![],
    }
}

/// The single most load-bearing inline param for a node body (08 §6.4), if any.
fn primary_inline_param(op: &GraphOp) -> Option<ParamDesc> {
    let params = op_params(op);
    let pick = match op {
        GraphOp::Blur => "params.radius",
        GraphOp::Glow => "params.intensity",
        GraphOp::Sharpen => "params.amount",
        GraphOp::Merge { .. } => "params.opacity",
        GraphOp::ChromaKey => "params.tolerance",
        GraphOp::LumaKey => "params.threshold",
        _ => return None,
    };
    params.into_iter().find(|d| d.path == pick)
}

/// `params.edge_softness` → `Edge softness`.
fn pretty_path(path: &str) -> &'static str {
    // Return a stable &'static label for known paths; fall back to the raw tail.
    match path {
        "params.radius" => "Radius",
        "params.amount" => "Amount",
        "params.threshold" => "Threshold",
        "params.intensity" => "Intensity",
        "params.tint" => "Tint",
        "params.key_color" => "Key color",
        "params.tolerance" => "Tolerance",
        "params.edge_softness" => "Edge softness",
        "params.spill_suppress" => "Spill suppress",
        "params.softness" => "Softness",
        "params.invert" => "Invert",
        "params.center_x" => "Center X",
        "params.center_y" => "Center Y",
        "params.size_x" => "Size X",
        "params.size_y" => "Size Y",
        "params.rotation" => "Rotation",
        "transform.rotation" => "Rotation (rad)",
        "params.feather" => "Feather",
        "transform.x" => "Position X",
        "transform.y" => "Position Y",
        "transform.scale_x" => "Scale X",
        "transform.scale_y" => "Scale Y",
        "transform.anchor_x" => "Anchor X",
        "transform.anchor_y" => "Anchor Y",
        "transform.opacity" => "Opacity",
        _ => "Value",
    }
}

/// Display the compiler's implicit value without materializing parameters just
/// because the user opened a node. Stored values (including zero) always win.
fn node_param_value(doc: &Document, node: &GraphNode, d: &ParamDesc) -> PropValue {
    if let Some(value) = node.params.base.0.get(d.path) {
        return *value;
    }
    let effect = match node.op {
        GraphOp::Blur => Some(EffectKind::Blur),
        GraphOp::Sharpen => Some(EffectKind::Sharpen),
        GraphOp::Glow => Some(EffectKind::Glow),
        GraphOp::ChromaKey => Some(EffectKind::ChromaKey),
        GraphOp::LumaKey => Some(EffectKind::LumaKey),
        GraphOp::MaskShape { .. } => Some(EffectKind::MaskShapeGen),
        _ => None,
    };
    if let Some(kind) = effect {
        if let Some(value) = photonic_core::timeline::effect_manifest::manifest(kind.effect_id())
            .and_then(|manifest| manifest.params.iter().find(|param| param.path == d.path))
        {
            return value.default;
        }
    }

    match (&node.op, d.path) {
        (GraphOp::Merge { .. }, "params.opacity") => return PropValue::Float(1.0),
        (GraphOp::SolidColor, "params.color") => {
            return PropValue::Color(photonic_core::Color::BLACK)
        }
        (GraphOp::Transform2D, path) => {
            let defaults = photonic_core::timeline::ClipTransform::default();
            let value = match path {
                "transform.x" => defaults.x,
                "transform.y" => defaults.y,
                "transform.scale_x" => defaults.scale_x,
                "transform.scale_y" => defaults.scale_y,
                "transform.rotation" => defaults.rotation,
                "transform.anchor_x" => defaults.anchor_x,
                "transform.anchor_y" => defaults.anchor_y,
                _ => return default_value(d),
            };
            return PropValue::Float(value);
        }
        (GraphOp::Resize { .. }, path @ ("params.width" | "params.height")) => {
            if let Some(format) = doc
                .timeline
                .as_ref()
                .and_then(|project| {
                    project
                        .active_sequence
                        .and_then(|id| project.sequences.get(&id))
                })
                .and_then(|seq| {
                    seq.formats
                        .get(seq.active_format.min(seq.formats.len().saturating_sub(1)))
                })
            {
                return PropValue::Float(if path == "params.width" {
                    format.width
                } else {
                    format.height
                } as f64);
            }
        }
        _ => {}
    }
    default_value(d)
}

/// Generic fallback for controls whose compiler default is neutral.
fn default_value(d: &ParamDesc) -> PropValue {
    match d.kind {
        PropValueKind::Float => {
            let v = match d.range {
                Some((lo, hi)) if (lo..=hi).contains(&0.0) => 0.0,
                Some((lo, _)) => lo,
                None => 0.0,
            };
            PropValue::Float(v)
        }
        PropValueKind::Vec2 => PropValue::Vec2([0.0, 0.0]),
        PropValueKind::Color => PropValue::Color(photonic_core::Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        }),
        PropValueKind::Bool => PropValue::Bool(false),
        PropValueKind::Enum => PropValue::Enum(0),
    }
}

// ── Pure geometry (unit-tested) ───────────────────────────────────────────────

/// Graph-space → screen-space.
fn g2s(g: Pos2, origin: Pos2, pan: Vec2, zoom: f32) -> Pos2 {
    origin + pan + (g.to_vec2() * zoom)
}

/// Screen-space → graph-space.
fn s2g(s: Pos2, origin: Pos2, pan: Vec2, zoom: f32) -> Pos2 {
    (((s - origin) - pan) / zoom).to_pos2()
}

/// Node body size in graph units (must match what [`draw_node`] paints so
/// hit-testing lines up).
fn node_body_size(op: &GraphOp) -> Vec2 {
    let (ins, outs) = op_ports(op);
    let rows = ins.len().max(outs.len()) as f32;
    let inline = if primary_inline_param(op).is_some() {
        INLINE_ROW_H
    } else {
        0.0
    };
    let h = HEADER_H + rows * PORT_ROW_H + inline + BODY_PAD;
    Vec2::new(NODE_W, h.max(HEADER_H + PORT_ROW_H))
}

/// Graph-space center of a port socket. Inputs sit on the left edge, outputs on
/// the right edge, evenly distributed down the port band under the header.
fn port_center_graph(pos: NodePos, op: &GraphOp, is_input: bool, idx: u16) -> Pos2 {
    let size = node_body_size(op);
    let (ins, outs) = op_ports(op);
    let count = if is_input { ins.len() } else { outs.len() }.max(1) as f32;
    let x = if is_input { pos.x } else { pos.x + size.x };
    let y = pos.y + HEADER_H + (idx as f32 + 0.5) * (PORT_ROW_H.min((size.y - HEADER_H) / count));
    Pos2::new(x, y)
}

/// A port reference for hit-testing / wiring.
#[derive(Copy, Clone, PartialEq, Eq)]
struct PortRef {
    node: GraphNodeId,
    is_input: bool,
    idx: u16,
}

/// Nearest port to a screen point within [`PORT_GRAB_R`], searching every node's
/// sockets. Returns the port and its graph-space center.
fn hit_port(
    graph: &photonic_core::timeline::NodeGraph,
    origin: Pos2,
    pan: Vec2,
    zoom: f32,
    screen: Pos2,
) -> Option<PortRef> {
    let mut best: Option<(f32, PortRef)> = None;
    for (id, node) in &graph.nodes {
        let (ins, outs) = op_ports(&node.op);
        let pos = graph
            .ui
            .get(id)
            .copied()
            .unwrap_or(NodePos { x: 0.0, y: 0.0 });
        for (is_input, ports) in [(true, &ins), (false, &outs)] {
            for port in ports.iter() {
                let c = g2s(
                    port_center_graph(pos, &node.op, is_input, port.idx),
                    origin,
                    pan,
                    zoom,
                );
                let d = c.distance(screen);
                if d <= PORT_GRAB_R && best.as_ref().map(|(bd, _)| d < *bd).unwrap_or(true) {
                    best = Some((
                        d,
                        PortRef {
                            node: *id,
                            is_input,
                            idx: port.idx,
                        },
                    ));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Topmost node whose body rect contains a screen point (nodes higher in the map
/// win ties arbitrarily but deterministically by not overlapping in practice).
fn hit_node(
    graph: &photonic_core::timeline::NodeGraph,
    origin: Pos2,
    pan: Vec2,
    zoom: f32,
    screen: Pos2,
) -> Option<GraphNodeId> {
    let g = s2g(screen, origin, pan, zoom);
    let mut hit = None;
    for (id, node) in &graph.nodes {
        let pos = graph
            .ui
            .get(id)
            .copied()
            .unwrap_or(NodePos { x: 0.0, y: 0.0 });
        let size = node_body_size(&node.op);
        let r = Rect::from_min_size(Pos2::new(pos.x, pos.y), Vec2::new(size.x, size.y));
        if r.contains(g) {
            hit = Some(*id);
        }
    }
    hit
}

/// Port type of a specific port (for wire coloring / compatibility).
fn port_type(op: &GraphOp, is_input: bool, idx: u16) -> Option<PortType> {
    let (ins, outs) = op_ports(op);
    let ports = if is_input { ins } else { outs };
    ports.iter().find(|p| p.idx == idx).map(|p| p.ty)
}

// ── Session view state (egui memory; never touches the document) ──────────────

#[derive(Clone, Copy)]
struct View {
    pan: Vec2,
    zoom: f32,
}
impl Default for View {
    fn default() -> Self {
        View {
            pan: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

/// What the pointer is currently doing on the canvas (persisted across frames
/// for the duration of a drag).
#[derive(Clone)]
enum Interaction {
    Pan,
    DragNode {
        node: GraphNodeId,
        start: NodePos,
        accum: Vec2,
    },
    Wire {
        from: GraphNodeId,
        out_port: u16,
        original: Option<GraphEdge>,
    },
}

/// Typed edit request the left-rail palette hands to the canvas (which owns
/// history). Drained + committed each frame.
#[derive(Clone)]
enum PaletteIntent {
    AddNode(NodeKind),
    SetTimeOffset {
        node: GraphNodeId,
        old: Tick,
        new: Tick,
    },
    SetMaskShape {
        node: GraphNodeId,
        old: MaskShapeKind,
        new: MaskShapeKind,
    },
    SetText {
        node: GraphNodeId,
        old: TextGen,
        new: TextGen,
    },
    SetParam {
        node: GraphNodeId,
        path: String,
        value: PropValue,
    },
    Connect {
        from: GraphNodeId,
        to: GraphNodeId,
        in_port: u16,
    },
    RemoveNode(GraphNodeId),
}

#[derive(Clone, Default)]
struct PaletteIntents(Vec<PaletteIntent>);

fn view_id(gid: GraphId) -> Id {
    Id::new(("photonic_node_view", gid))
}
fn interaction_id() -> Id {
    Id::new("photonic_node_interaction")
}
fn menu_pos_id() -> Id {
    Id::new("photonic_node_menu_pos")
}
fn pin_id(gid: GraphId) -> Id {
    Id::new(("photonic_node_pin", gid))
}
fn flash_id() -> Id {
    Id::new("photonic_node_flash")
}
fn palette_intents_id() -> Id {
    Id::new("photonic_node_palette_intents")
}
fn palette_search_id() -> Id {
    Id::new("photonic_node_palette_search")
}

fn push_palette_intent(ui: &Ui, intent: PaletteIntent) {
    ui.data_mut(|d| {
        let mut v = d
            .get_temp::<PaletteIntents>(palette_intents_id())
            .unwrap_or_default();
        v.0.push(intent);
        d.insert_temp(palette_intents_id(), v);
    });
}

fn drain_palette_intents(ui: &Ui) -> Vec<PaletteIntent> {
    ui.data_mut(|d| {
        let v = d
            .get_temp::<PaletteIntents>(palette_intents_id())
            .unwrap_or_default();
        d.insert_temp(palette_intents_id(), PaletteIntents::default());
        v.0
    })
}

fn set_flash(ui: &Ui, msg: impl Into<String>) {
    let until = ui.input(|i| i.time) + 2.2;
    ui.data_mut(|d| d.insert_temp(flash_id(), (msg.into(), until)));
}

// ── Command building helpers (all through graph_ops/ops → history) ────────────

/// Commit one timeline command as a single undo step.
fn commit(history: &mut CommandHistory, doc: &mut Document, cmd: TimelineCmd) {
    history.execute_discrete(Command::Timeline(cmd), doc);
}

/// Commit several commands as ONE undo step.
fn commit_batch(history: &mut CommandHistory, doc: &mut Document, cmds: Vec<TimelineCmd>) {
    match cmds.len() {
        0 => {}
        1 => commit(history, doc, cmds.into_iter().next().unwrap()),
        _ => {
            let batch = cmds.into_iter().map(Command::Timeline).collect();
            history.execute_discrete(Command::Batch(batch), doc);
        }
    }
}

/// Build a `SetNodeParam` command that sets one path on a node's param bag,
/// cloning the current bag from the live document.
fn set_time_offset_cmd(
    doc: &Document,
    gid: GraphId,
    node: GraphNodeId,
    old: Tick,
    new: Tick,
) -> Option<TimelineCmd> {
    let p = doc.timeline.as_ref()?;
    let n = p.graphs.get(&gid)?.nodes.get(&node)?;
    if !matches!(n.op, GraphOp::TimeOffset{offset} if offset==old) {
        return None;
    }
    graph_ops::set_time_offset(p, gid, node, new).ok().flatten()
}

fn set_mask_shape_cmd(
    doc: &Document,
    gid: GraphId,
    node: GraphNodeId,
    old: &MaskShapeKind,
    new: MaskShapeKind,
) -> Option<TimelineCmd> {
    let p = doc.timeline.as_ref()?;
    let n = p.graphs.get(&gid)?.nodes.get(&node)?;
    if !matches!(&n.op,GraphOp::MaskShape{shape} if shape==old) {
        return None;
    }
    graph_ops::set_mask_shape(p, gid, node, new).ok().flatten()
}

fn set_text_cmd(
    doc: &Document,
    gid: GraphId,
    node: GraphNodeId,
    old: &TextGen,
    new: TextGen,
) -> Option<TimelineCmd> {
    let p = doc.timeline.as_ref()?;
    let n = p.graphs.get(&gid)?.nodes.get(&node)?;
    if !matches!(&n.op,GraphOp::Text{text} if text==old) {
        return None;
    }
    graph_ops::set_text(p, gid, node, new).ok().flatten()
}

fn set_param_cmd(
    doc: &Document,
    gid: GraphId,
    node: GraphNodeId,
    path: &str,
    value: PropValue,
) -> Option<TimelineCmd> {
    let p = doc.timeline.as_ref()?;
    let g = p.graphs.get(&gid)?;
    let n = g.nodes.get(&node)?;
    let mut bag = n.params.base.clone();
    bag.0.set(path, value);
    graph_ops::set_node_param(p, gid, node, bag).ok()
}

/// Connect `from`'s primary output into `to`'s `in_port`, replacing any edge
/// already on that input, refusing cycles (08 §6.6). Returns the batch or `Err`
/// for a cycle.
fn connect_cmds(
    doc: &Document,
    gid: GraphId,
    from: GraphNodeId,
    out_port: u16,
    to: GraphNodeId,
    in_port: u16,
) -> Result<Vec<TimelineCmd>, ()> {
    let p = match doc.timeline.as_ref() {
        Some(p) => p,
        None => return Err(()),
    };
    let edge = GraphEdge {
        from: (from, OutPort(out_port)),
        to: (to, InPort(in_port)),
    };
    Ok(graph_ops::connect_edge(p, gid, edge.from, edge.to)
        .map_err(|_| ())?
        .into_iter()
        .collect())
}

fn input_wire(
    graph: &photonic_core::timeline::NodeGraph,
    node: GraphNodeId,
    port: u16,
) -> Option<Interaction> {
    graph
        .edges
        .iter()
        .find(|edge| edge.to == (node, InPort(port)))
        .copied()
        .map(|edge| Interaction::Wire {
            from: edge.from.0,
            out_port: edge.from.1 .0,
            original: Some(edge),
        })
}

#[allow(clippy::too_many_arguments)]
fn wire_drop_cmds(
    doc: &Document,
    gid: GraphId,
    from: GraphNodeId,
    out_port: u16,
    original: Option<GraphEdge>,
    target: Option<(GraphNodeId, u16)>,
    blank: bool,
) -> Result<Vec<TimelineCmd>, ops::EditError> {
    let project = doc.timeline.as_ref().ok_or(ops::EditError::NoProject)?;
    if let Some((node, port)) = target {
        Ok(graph_ops::rewire_edge(
            project,
            gid,
            original,
            (from, OutPort(out_port)),
            (node, InPort(port)),
        )?
        .into_iter()
        .collect())
    } else if blank {
        original
            .map(|edge| graph_ops::remove_edge(project, gid, edge))
            .transpose()
            .map(|command| command.into_iter().collect())
    } else {
        Ok(Vec::new())
    }
}

fn graph_connection_error(error: &ops::EditError) -> String {
    match error {
        ops::EditError::TrackLocked => "Track locked: graph is read-only".into(),
        ops::EditError::WouldCreateCycle => "Can't connect: this would create a cycle".into(),
        ops::EditError::InvalidGraphConnection(reason) => reason.clone(),
        _ => "Connection no longer available".into(),
    }
}

/// Locate the sequence + track that own a clip (for opening its composition).
fn find_clip_location(
    doc: &Document,
    clip: photonic_core::timeline::ClipId,
) -> Option<(SequenceId, TrackId)> {
    let p = doc.timeline.as_ref()?;
    for (sid, seq) in &p.sequences {
        for t in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            if t.clips.iter().any(|c| c.id == clip) {
                return Some((*sid, t.id));
            }
        }
    }
    None
}

/// Extract the id of the graph an `AddGraph` command carries (to open it after
/// committing a create-composition batch).
fn added_graph_id(cmds: &[TimelineCmd]) -> Option<GraphId> {
    cmds.iter().find_map(|c| match c {
        TimelineCmd::AddGraph { graph } => Some(graph.id),
        _ => None,
    })
}

// ── Central canvas ─────────────────────────────────────────────────────────────

/// One rung of the node-canvas Esc cancel ladder (41 §3 R-6). A single Esc press
/// discharges exactly one, in priority order, so the gesture unwinds one step at a
/// time and an in-flight wire is cancelled — never orphaned (R-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EscLevel {
    /// A wire drag is in flight → cancel just the wire.
    CancelWire,
    /// No wire, but a node is selected → clear just the selection.
    ClearSelection,
    /// Nothing pending → leave the canvas for the timeline.
    CloseCanvas,
}

/// Which ladder rung a single Esc press discharges, given the current canvas
/// state. Pure so the ordering is unit-testable without an egui context.
pub(crate) fn esc_level(wire_in_flight: bool, has_selection: bool) -> EscLevel {
    if wire_in_flight {
        EscLevel::CancelWire
    } else if has_selection {
        EscLevel::ClearSelection
    } else {
        EscLevel::CloseCanvas
    }
}

/// Central-panel node canvas content state (08 §6.1), drawn in place of the
/// program monitor while [`VideoPanelUi::node_canvas_active`] is set. Owns
/// `&mut Document` + `&mut CommandHistory` so it can commit graph edits directly
/// (the sole committer; the palette hands it intents). The "Back to Timeline"
/// escape (button + `Esc`) clears `node_canvas_active`.
pub(crate) fn draw_node_canvas(
    ui: &mut Ui,
    rect: Rect,
    doc: &mut Document,
    history: &mut CommandHistory,
    vid: &mut VideoPanelUi,
) {
    let gid = *vid.open_graph;
    let Some(gid) = gid else {
        draw_no_graph_placeholder(ui, rect, doc, history, vid);
        return;
    };
    // Snapshot the graph so rendering/hit-testing hold no borrow on `doc` while
    // commits mutate it.
    let Some(graph) = doc
        .timeline
        .as_ref()
        .and_then(|p| p.graphs.get(&gid))
        .cloned()
    else {
        // Open-graph pointer is stale (graph pruned); drop back to timeline.
        *vid.open_graph = None;
        *vid.node_canvas_active = false;
        return;
    };
    let is_project_graph = doc.timeline.as_ref().and_then(|p| p.project_graph) == Some(gid);
    begin_graph_canvas_pass(ui, graph_interaction_scope(doc, gid));

    let full = rect;
    let mut pending: Vec<TimelineCmd> = Vec::new();
    let mut batches: Vec<Vec<TimelineCmd>> = Vec::new();

    // ── Top bar: graph switcher + back button + node/edge counts ─────────────
    let top_h = 30.0;
    let top_rect = Rect::from_min_size(full.min, Vec2::new(full.width(), top_h));
    draw_top_bar(
        ui,
        top_rect,
        doc,
        vid,
        &graph,
        gid,
        is_project_graph,
        &mut batches,
    );

    // ── Split: canvas (left) + viewer inset (right) ──────────────────────────
    let body = Rect::from_min_max(Pos2::new(full.min.x, top_rect.max.y + 2.0), full.max);
    let ratio_id = Id::new(("photonic_node_viewer_ratio", gid));
    let ratio: f32 = ui
        .data_mut(|d| *d.get_temp_mut_or_default::<f32>(ratio_id))
        .clamp(0.0, 1.0);
    let ratio = if ratio == 0.0 { 0.7 } else { ratio };
    let split_x = body.min.x + body.width() * ratio;
    let canvas_rect = Rect::from_min_max(body.min, Pos2::new(split_x - 4.0, body.max.y));
    let viewer_rect = Rect::from_min_max(Pos2::new(split_x + 4.0, body.min.y), body.max);

    // Resizable split handle.
    let handle = Rect::from_min_max(
        Pos2::new(split_x - 4.0, body.min.y),
        Pos2::new(split_x + 4.0, body.max.y),
    );
    let handle_resp = ui.interact(handle, Id::new(("photonic_node_split", gid)), Sense::drag());
    if handle_resp.dragged() {
        let new_ratio = ((ui
            .input(|i| i.pointer.hover_pos().map(|p| p.x))
            .unwrap_or(split_x)
            - body.min.x)
            / body.width())
        .clamp(0.35, 0.85);
        ui.data_mut(|d| d.insert_temp(ratio_id, new_ratio));
    }
    if handle_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    ui.painter().rect_filled(
        handle,
        0.0,
        ui.visuals().widgets.noninteractive.bg_stroke.color,
    );

    // ── View state + canvas interaction ──────────────────────────────────────
    let mut view: View = ui.data(|d| d.get_temp(view_id(gid))).unwrap_or_default();
    let resp = ui.interact(
        canvas_rect,
        Id::new(("photonic_node_canvas", gid)),
        Sense::click_and_drag(),
    );
    // Clicking the canvas gives it keyboard focus, which is what `handle_keyboard`
    // gates on (41 §3 R-5: never gate key handling on pointer position).
    if resp.clicked() || resp.drag_started() {
        resp.request_focus();
    }
    // Own arrow/Tab/Delete/Esc on the *focused* canvas across frames. Without an
    // EventFilter, egui's focus navigation turns the first Tab/Arrow into a focus
    // move — stealing canvas focus so the second press never reaches
    // `handle_keyboard` (41 §3 R-4/R-5). `tab: true` because the canvas owns Tab as
    // its node-cycle key; `escape: true` because it also owns Esc for the R-6
    // cancel ladder below — with `escape: false` egui would clear canvas focus on
    // the same Esc, so the ladder's `contains_focus` gate could never fire.
    if resp.has_focus() {
        ui.ctx().memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
    // Esc discharges one rung of the R-6 cancel ladder per press, gated on canvas
    // focus so it can't tear the canvas down from a menu or text field. `consume_key`
    // so the same Esc isn't also seen by other panels that frame. Never `return`s
    // early — the frame must still paint (08 §6.1). This replaces the old
    // unconditional top-of-fn `key_pressed(Escape)`, which fired app-wide and
    // abandoned an in-flight wire instead of cancelling it.
    handle_canvas_escape(
        ui,
        resp.has_focus(),
        &mut *vid.selected_graph_node,
        &mut *vid.node_canvas_active,
    );
    let origin = canvas_rect.min;

    handle_canvas_input(
        ui,
        &resp,
        canvas_rect,
        &graph,
        gid,
        &mut view,
        doc,
        &mut pending,
        &mut batches,
        vid,
    );

    // Keyboard nav (13 §16 / 08 §6.2): Tab cycles, arrows nudge, Delete removes.
    handle_keyboard(ui, &resp, &graph, gid, doc, vid, &mut pending);

    // ── Paint the canvas ─────────────────────────────────────────────────────
    let painter = ui.painter_at(canvas_rect);
    painter.rect_filled(canvas_rect, 0.0, ui.visuals().extreme_bg_color);
    draw_grid(&painter, canvas_rect, origin, view);

    // In-progress node drag offset (applied visually before commit-on-release).
    let drag_state: Option<Interaction> = ui.data(|d| d.get_temp(interaction_id()));
    let dragging_node = match &drag_state {
        Some(Interaction::DragNode { node, start, accum }) => {
            Some((*node, *start, *accum / view.zoom))
        }
        _ => None,
    };

    // Edges first (under nodes).
    for e in &graph.edges {
        let (Some(fp), Some(tp)) = (graph.nodes.get(&e.from.0), graph.nodes.get(&e.to.0)) else {
            continue;
        };
        let from_pos = node_draw_pos(&graph, e.from.0, dragging_node);
        let to_pos = node_draw_pos(&graph, e.to.0, dragging_node);
        let a = g2s(
            port_center_graph(from_pos, &fp.op, false, e.from.1 .0),
            origin,
            view.pan,
            view.zoom,
        );
        let b = g2s(
            port_center_graph(to_pos, &tp.op, true, e.to.1 .0),
            origin,
            view.pan,
            view.zoom,
        );
        let ty = port_type(&fp.op, false, e.from.1 .0).unwrap_or(PortType::Image);
        draw_wire(&painter, a, b, ty.color(ui), 2.0);
    }

    // In-progress wire preview.
    if let Some(Interaction::Wire { from, out_port, .. }) = &drag_state {
        if let (Some(node), Some(ptr)) =
            (graph.nodes.get(from), ui.input(|i| i.pointer.hover_pos()))
        {
            let from_pos = node_draw_pos(&graph, *from, dragging_node);
            let a = g2s(
                port_center_graph(from_pos, &node.op, false, *out_port),
                origin,
                view.pan,
                view.zoom,
            );
            let ty = port_type(&node.op, false, *out_port).unwrap_or(PortType::Image);
            draw_wire(&painter, a, ptr, ty.color(ui), 2.0);
        }
    }

    // Nodes on top.
    let pin: Option<GraphNodeId> = ui.data(|d| d.get_temp(pin_id(gid)));
    let mut node_ids: Vec<GraphNodeId> = graph.nodes.keys().copied().collect();
    node_ids.sort(); // deterministic paint order
    for id in &node_ids {
        let node = &graph.nodes[id];
        let pos = node_draw_pos(&graph, *id, dragging_node);
        draw_node(
            ui,
            &painter,
            origin,
            view,
            canvas_rect,
            node,
            pos,
            *vid.selected_graph_node == Some(*id),
            pin == Some(*id),
            &graph,
            doc,
            gid,
            &mut pending,
        );
    }

    // ── Viewer inset ─────────────────────────────────────────────────────────
    draw_viewer(ui, viewer_rect, &graph, gid, pin);

    // ── Transient flash (cycle refusal etc.) ─────────────────────────────────
    draw_flash(ui, canvas_rect);

    // ── Drain palette intents, commit everything ─────────────────────────────
    for intent in drain_palette_intents(ui) {
        apply_palette_intent(
            ui,
            doc,
            gid,
            &graph,
            canvas_rect,
            view,
            intent,
            &mut pending,
            &mut batches,
        );
    }

    ui.data_mut(|d| d.insert_temp(view_id(gid), view));

    for cmd in pending {
        commit(history, doc, cmd);
    }
    for batch in batches {
        commit_batch(history, doc, batch);
    }
}

fn handle_canvas_escape(
    ui: &Ui,
    focused: bool,
    selected: &mut Option<GraphNodeId>,
    active: &mut bool,
) {
    if focused && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        let wire = matches!(
            ui.data(|data| data.get_temp::<Interaction>(interaction_id())),
            Some(Interaction::Wire { .. })
        );
        match esc_level(wire, selected.is_some()) {
            EscLevel::CancelWire => {
                ui.data_mut(|data| data.remove::<Interaction>(interaction_id()))
            }
            EscLevel::ClearSelection => *selected = None,
            EscLevel::CloseCanvas => *active = false,
        }
    }
}

fn graph_interaction_scope(doc: &Document, gid: GraphId) -> Id {
    Id::new(("node_canvas_scope", doc.id, gid))
}

fn begin_graph_canvas_pass(ui: &Ui, scope: Id) {
    let pass = ui.ctx().cumulative_pass_nr();
    let previous = ui.data(|data| data.get_temp::<(Id, u64)>(Id::new("node_canvas_visible_pass")));
    if previous.is_none_or(|(previous_graph, previous_pass)| {
        previous_graph != scope || previous_pass.saturating_add(1) < pass
    }) {
        ui.data_mut(|data| data.remove::<Interaction>(interaction_id()));
    }
    ui.data_mut(|data| data.insert_temp(Id::new("node_canvas_visible_pass"), (scope, pass)));
}

/// Effective draw position for a node, applying an in-progress drag offset.
fn node_draw_pos(
    graph: &photonic_core::timeline::NodeGraph,
    id: GraphNodeId,
    dragging: Option<(GraphNodeId, NodePos, Vec2)>,
) -> NodePos {
    let base = graph
        .ui
        .get(&id)
        .copied()
        .unwrap_or(NodePos { x: 0.0, y: 0.0 });
    if let Some((dn, start, off)) = dragging {
        if dn == id {
            return NodePos {
                x: start.x + off.x,
                y: start.y + off.y,
            };
        }
    }
    base
}

#[allow(clippy::too_many_arguments)]
fn handle_canvas_input(
    ui: &Ui,
    resp: &Response,
    canvas_rect: Rect,
    graph: &photonic_core::timeline::NodeGraph,
    gid: GraphId,
    view: &mut View,
    doc: &Document,
    pending: &mut Vec<TimelineCmd>,
    batches: &mut Vec<Vec<TimelineCmd>>,
    vid: &mut VideoPanelUi,
) {
    let origin = canvas_rect.min;
    let editable = graph_is_editable(doc, gid);
    if !editable
        && !matches!(
            ui.data(|d| d.get_temp::<Interaction>(interaction_id())),
            Some(Interaction::Pan)
        )
    {
        ui.data_mut(|d| d.remove::<Interaction>(interaction_id()));
    }

    // Zoom on scroll, anchored at the cursor.
    if resp.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            if let Some(ptr) = ui.input(|i| i.pointer.hover_pos()) {
                let before = s2g(ptr, origin, view.pan, view.zoom);
                view.zoom = (view.zoom * (1.0 + scroll * 0.0015)).clamp(MIN_ZOOM, MAX_ZOOM);
                let after = s2g(ptr, origin, view.pan, view.zoom);
                view.pan += (after - before) * view.zoom;
            }
        }
    }

    // Classify a starting drag.
    if resp.drag_started() {
        if let Some(press) = ui
            .input(|input| input.pointer.press_origin())
            .or_else(|| resp.interact_pointer_pos())
        {
            let interaction =
                if let Some(port) = hit_port(graph, origin, view.pan, view.zoom, press) {
                    if !editable {
                        None
                    } else if port.is_input {
                        // Keep the original edge until a drop is committed, so Escape
                        // and interrupted drags cannot orphan a connection.
                        input_wire(graph, port.node, port.idx)
                    } else {
                        Some(Interaction::Wire {
                            from: port.node,
                            out_port: port.idx,
                            original: None,
                        })
                    }
                } else if let Some(node) = hit_node(graph, origin, view.pan, view.zoom, press) {
                    *vid.selected_graph_node = Some(node);
                    let start = graph
                        .ui
                        .get(&node)
                        .copied()
                        .unwrap_or(NodePos { x: 0.0, y: 0.0 });
                    editable.then_some(Interaction::DragNode {
                        node,
                        start,
                        accum: Vec2::ZERO,
                    })
                } else {
                    Some(Interaction::Pan)
                };
            if let Some(i) = interaction {
                ui.data_mut(|d| d.insert_temp(interaction_id(), i));
            }
        }
    }

    // Apply an ongoing drag.
    if resp.dragged() {
        let delta = resp.drag_delta();
        let cur: Option<Interaction> = ui.data(|d| d.get_temp(interaction_id()));
        match cur {
            Some(Interaction::Pan) => view.pan += delta,
            Some(Interaction::DragNode { node, start, accum }) => {
                ui.data_mut(|d| {
                    d.insert_temp(
                        interaction_id(),
                        Interaction::DragNode {
                            node,
                            start,
                            accum: accum + delta,
                        },
                    )
                });
            }
            _ => {}
        }
    }

    // Finalize on release.
    if resp.drag_stopped() {
        let cur: Option<Interaction> = ui.data(|d| d.get_temp(interaction_id()));
        match cur {
            Some(Interaction::DragNode { node, start, accum }) => {
                let off = accum / view.zoom;
                let new = NodePos {
                    x: start.x + off.x,
                    y: start.y + off.y,
                };
                if (new.x - start.x).abs() > 0.01 || (new.y - start.y).abs() > 0.01 {
                    if let Some(p) = doc.timeline.as_ref() {
                        if let Ok(cmd) = graph_ops::move_node(p, gid, node, new) {
                            pending.push(cmd);
                        }
                    }
                }
            }
            Some(Interaction::Wire {
                from,
                out_port,
                original,
            }) => {
                if let Some(drop) = resp.interact_pointer_pos() {
                    let port = hit_port(graph, origin, view.pan, view.zoom, drop);
                    let target = port
                        .filter(|port| port.is_input)
                        .map(|port| (port.node, port.idx));
                    let blank = canvas_rect.contains(drop)
                        && port.is_none()
                        && hit_node(graph, origin, view.pan, view.zoom, drop).is_none();
                    match wire_drop_cmds(doc, gid, from, out_port, original, target, blank) {
                        Ok(cmds) => batches.push(cmds),
                        Err(error) => set_flash(ui, graph_connection_error(&error)),
                    }
                }
            }
            _ => {}
        }
        ui.data_mut(|d| d.remove::<Interaction>(interaction_id()));
    }

    // Click: disconnect an input socket, or deselect on empty.
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(port) = hit_port(graph, origin, view.pan, view.zoom, pos) {
                if port.is_input {
                    if let Some(edge) = graph
                        .edges
                        .iter()
                        .find(|e| e.to == (port.node, InPort(port.idx)))
                        .copied()
                    {
                        pending.extend(remove_edge_cmd(doc, gid, edge));
                    }
                }
            } else {
                *vid.selected_graph_node = hit_node(graph, origin, view.pan, view.zoom, pos);
            }
        }
    }

    // Right-click context menu (add node on empty, node ops on a node).
    if resp.secondary_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            ui.data_mut(|d| d.insert_temp(menu_pos_id(), pos));
        }
    }
    let menu_pos: Option<Pos2> = ui.data(|d| d.get_temp(menu_pos_id()));
    resp.context_menu(|ui| {
        let pos = menu_pos.unwrap_or(canvas_rect.center());
        let is_pg = doc.timeline.as_ref().and_then(|p| p.project_graph) == Some(gid);
        if let Some(node) = hit_node(graph, canvas_rect.min, view.pan, view.zoom, pos) {
            node_context_menu(ui, graph, gid, node, doc, vid, pending);
        } else {
            let drop_graph = s2g(pos, canvas_rect.min, view.pan, view.zoom);
            ui.label(egui::RichText::new("Add node").small().weak());
            ui.separator();
            add_node_menu(ui, is_pg, |kind| {
                let node = GraphNode::new(kind.make_op());
                pending.extend(add_node_cmd(
                    doc,
                    gid,
                    node,
                    NodePos {
                        x: drop_graph.x,
                        y: drop_graph.y,
                    },
                ));
            });
        }
    });
}

/// Grouped Add-Node submenus, one per family (08 §6.2).
fn add_node_menu(ui: &mut Ui, is_project_graph: bool, mut on_pick: impl FnMut(NodeKind)) {
    for fam in Family::ORDER {
        let kinds: Vec<NodeKind> = NodeKind::ALL
            .iter()
            .copied()
            .filter(|k| k.family() == fam && k.allowed(is_project_graph))
            .collect();
        if kinds.is_empty() {
            continue;
        }
        ui.menu_button(fam.title(), |ui| {
            for k in kinds {
                if ui.button(k.label()).clicked() {
                    on_pick(k);
                    ui.close_menu();
                }
            }
        });
    }
}

fn node_context_menu(
    ui: &mut Ui,
    graph: &photonic_core::timeline::NodeGraph,
    gid: GraphId,
    node: GraphNodeId,
    doc: &Document,
    vid: &mut VideoPanelUi,
    pending: &mut Vec<TimelineCmd>,
) {
    let is_output = graph
        .nodes
        .get(&node)
        .map(|n| matches!(n.op, GraphOp::Output))
        .unwrap_or(false);
    if ui.button("Pin to viewer").clicked() {
        ui.data_mut(|d| d.insert_temp(pin_id(gid), node));
        ui.close_menu();
    }
    if ui.button("Clear viewer pin").clicked() {
        ui.data_mut(|d| d.remove::<GraphNodeId>(pin_id(gid)));
        ui.close_menu();
    }
    ui.separator();
    ui.add_enabled_ui(!is_output && graph_is_editable(doc, gid), |ui| {
        if ui.button("Delete node").clicked() {
            if let Some(p) = doc.timeline.as_ref() {
                if let Ok(cmd) = graph_ops::remove_node(p, gid, node) {
                    pending.push(cmd);
                    if *vid.selected_graph_node == Some(node) {
                        *vid.selected_graph_node = None;
                    }
                }
            }
            ui.close_menu();
        }
    });
    if is_output {
        ui.label(
            egui::RichText::new("Output can't be removed")
                .small()
                .weak(),
        );
    }
}

fn handle_keyboard(
    ui: &Ui,
    canvas_resp: &Response,
    graph: &photonic_core::timeline::NodeGraph,
    gid: GraphId,
    doc: &Document,
    vid: &mut VideoPanelUi,
    pending: &mut Vec<TimelineCmd>,
) {
    // Only when the canvas holds keyboard focus, so Tab/arrows/Delete don't
    // collide with the still-live timeline panel below (04 §5.2 key collisions).
    //
    // This was previously gated on `rect_contains_pointer`, which made every
    // keyboard shortcut require the mouse to be hovering the canvas — i.e. the
    // keyboard path existed but was unreachable without a pointer. 41 §3 R-5
    // forbids gating key handling on pointer position; focus is the correct
    // gate, and the canvas takes focus on click (see the `interact` call site).
    if !canvas_resp.has_focus() {
        return;
    }
    // Tab cycles selection through nodes ordered by (y, x).
    if ui.input(|i| i.key_pressed(egui::Key::Tab)) {
        let mut ids: Vec<GraphNodeId> = graph.nodes.keys().copied().collect();
        ids.sort_by(|a, b| {
            let pa = graph
                .ui
                .get(a)
                .copied()
                .unwrap_or(NodePos { x: 0.0, y: 0.0 });
            let pb = graph
                .ui
                .get(b)
                .copied()
                .unwrap_or(NodePos { x: 0.0, y: 0.0 });
            (pa.y, pa.x)
                .partial_cmp(&(pb.y, pb.x))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if !ids.is_empty() {
            let next = match *vid.selected_graph_node {
                Some(cur) => {
                    let i = ids.iter().position(|x| *x == cur).unwrap_or(0);
                    ids[(i + 1) % ids.len()]
                }
                None => ids[0],
            };
            *vid.selected_graph_node = Some(next);
        }
    }
    let Some(sel) = *vid.selected_graph_node else {
        return;
    };
    // Arrow nudge (one undoable step per press).
    let mut delta = Vec2::ZERO;
    ui.input(|i| {
        if i.key_pressed(egui::Key::ArrowLeft) {
            delta.x -= 8.0;
        }
        if i.key_pressed(egui::Key::ArrowRight) {
            delta.x += 8.0;
        }
        if i.key_pressed(egui::Key::ArrowUp) {
            delta.y -= 8.0;
        }
        if i.key_pressed(egui::Key::ArrowDown) {
            delta.y += 8.0;
        }
    });
    if delta != Vec2::ZERO {
        if let Some(p) = doc.timeline.as_ref() {
            let cur = p
                .graphs
                .get(&gid)
                .and_then(|g| g.ui.get(&sel))
                .copied()
                .unwrap_or(NodePos { x: 0.0, y: 0.0 });
            let new = NodePos {
                x: cur.x + delta.x,
                y: cur.y + delta.y,
            };
            if let Ok(cmd) = graph_ops::move_node(p, gid, sel, new) {
                pending.push(cmd);
            }
        }
    }
    // Delete removes the selected node (never the Output).
    if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
        let is_output = graph
            .nodes
            .get(&sel)
            .map(|n| matches!(n.op, GraphOp::Output))
            .unwrap_or(false);
        if !is_output {
            if let Some(p) = doc.timeline.as_ref() {
                if let Ok(cmd) = graph_ops::remove_node(p, gid, sel) {
                    pending.push(cmd);
                    *vid.selected_graph_node = None;
                }
            }
        }
    }
}

// ── Painting ──────────────────────────────────────────────────────────────────

fn draw_grid(painter: &egui::Painter, rect: Rect, origin: Pos2, view: View) {
    let step = 32.0 * view.zoom;
    if step < 6.0 {
        return;
    }
    let col = painter
        .ctx()
        .style()
        .visuals
        .widgets
        .noninteractive
        .bg_stroke
        .color;
    let faint = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 40);
    let stroke = Stroke::new(1.0, faint);
    let ox = (origin.x + view.pan.x).rem_euclid(step);
    let oy = (origin.y + view.pan.y).rem_euclid(step);
    let mut x = rect.min.x + ox - step;
    while x < rect.max.x {
        painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], stroke);
        x += step;
    }
    let mut y = rect.min.y + oy - step;
    while y < rect.max.y {
        painter.line_segment([Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)], stroke);
        y += step;
    }
}

/// A left→right S-curve wire, sampled to a polyline (avoids CubicBezierShape API
/// churn across egui versions).
fn draw_wire(painter: &egui::Painter, a: Pos2, b: Pos2, color: Color32, width: f32) {
    let dx = ((b.x - a.x).abs() * 0.5).max(40.0);
    let c1 = Pos2::new(a.x + dx, a.y);
    let c2 = Pos2::new(b.x - dx, b.y);
    let n = 24;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            cubic(a, c1, c2, b, t)
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(width, color)));
}

fn cubic(p0: Pos2, p1: Pos2, p2: Pos2, p3: Pos2, t: f32) -> Pos2 {
    let u = 1.0 - t;
    let w0 = u * u * u;
    let w1 = 3.0 * u * u * t;
    let w2 = 3.0 * u * t * t;
    let w3 = t * t * t;
    Pos2::new(
        w0 * p0.x + w1 * p1.x + w2 * p2.x + w3 * p3.x,
        w0 * p0.y + w1 * p1.y + w2 * p2.y + w3 * p3.y,
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_node(
    ui: &mut Ui,
    painter: &egui::Painter,
    origin: Pos2,
    view: View,
    canvas_rect: Rect,
    node: &GraphNode,
    pos: NodePos,
    selected: bool,
    pinned: bool,
    graph: &photonic_core::timeline::NodeGraph,
    doc: &Document,
    gid: GraphId,
    pending: &mut Vec<TimelineCmd>,
) {
    let size = node_body_size(&node.op);
    let min = g2s(Pos2::new(pos.x, pos.y), origin, view.pan, view.zoom);
    let rect = Rect::from_min_size(min, size * view.zoom);
    if !rect.intersects(canvas_rect) {
        return;
    }
    let v = ui.visuals();
    let rounding = egui::Rounding::same(5.0);
    // Body.
    painter.rect_filled(rect, rounding, v.widgets.inactive.bg_fill);
    let border = if selected {
        Stroke::new(2.0, v.selection.stroke.color)
    } else {
        Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color)
    };
    painter.rect_stroke(rect, rounding, border);
    // Header band.
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_H * view.zoom));
    painter.rect_filled(
        header,
        egui::Rounding {
            nw: 5.0,
            ne: 5.0,
            sw: 0.0,
            se: 0.0,
        },
        if selected {
            v.selection.bg_fill
        } else {
            v.widgets.noninteractive.weak_bg_fill
        },
    );
    if view.zoom > 0.5 {
        painter.text(
            header.left_center() + Vec2::new(8.0, 0.0),
            Align2::LEFT_CENTER,
            op_title(&node.op),
            FontId::proportional((12.0 * view.zoom).clamp(8.0, 15.0)),
            v.text_color(),
        );
    }

    // Diagnostic + keyframe badges (top-right of header).
    let mut badge_x = header.max.x - 10.0 * view.zoom;
    if !node.params.tracks.is_empty() {
        // Keyframe diamond (08 §6.5).
        draw_diamond(
            painter,
            Pos2::new(badge_x, header.center().y),
            4.0 * view.zoom,
            v.selection.stroke.color,
        );
        badge_x -= 12.0 * view.zoom;
    }
    if let Some(msg) = node_diagnostic(&node.op, node.id, graph) {
        painter.text(
            Pos2::new(badge_x, header.center().y),
            Align2::CENTER_CENTER,
            "!",
            FontId::proportional(13.0 * view.zoom),
            v.error_fg_color,
        );
        // Tooltip via a hidden interact rect.
        let br = Rect::from_center_size(Pos2::new(badge_x, header.center().y), Vec2::splat(14.0));
        ui.interact(br, Id::new(("nd_diag", node.id)), Sense::hover())
            .on_hover_text(msg);
    }
    if pinned {
        painter.text(
            rect.right_top() + Vec2::new(-6.0, 4.0 + HEADER_H * view.zoom),
            Align2::RIGHT_TOP,
            "◉ viewer",
            FontId::proportional(9.0 * view.zoom),
            v.selection.stroke.color,
        );
    }

    // Ports.
    let (ins, outs) = op_ports(&node.op);
    for (is_input, ports) in [(true, &ins), (false, &outs)] {
        for port in ports {
            let c = g2s(
                port_center_graph(pos, &node.op, is_input, port.idx),
                origin,
                view.pan,
                view.zoom,
            );
            let connected = graph.edges.iter().any(|e| {
                if is_input {
                    e.to == (node.id, InPort(port.idx))
                } else {
                    e.from == (node.id, OutPort(port.idx))
                }
            });
            let col = port.ty.color(ui);
            if connected {
                painter.circle_filled(c, SOCKET_R, col);
            } else {
                painter.circle_filled(c, SOCKET_R, v.extreme_bg_color);
                painter.circle_stroke(c, SOCKET_R, Stroke::new(1.5, col));
            }
            if view.zoom > 0.75 {
                let lx = if is_input { c.x + 9.0 } else { c.x - 9.0 };
                let align = if is_input {
                    Align2::LEFT_CENTER
                } else {
                    Align2::RIGHT_CENTER
                };
                painter.text(
                    Pos2::new(lx, c.y),
                    align,
                    port.label,
                    FontId::proportional(9.0 * view.zoom),
                    v.weak_text_color(),
                );
            }
        }
    }

    // Inline primary param (editable when the node is drawn large enough).
    if view.zoom >= 0.75 {
        if let Some(desc) = primary_inline_param(&node.op) {
            let rows = ins.len().max(outs.len()) as f32;
            let inline_top = rect.min.y + (HEADER_H + rows * PORT_ROW_H) * view.zoom;
            let inline_rect = Rect::from_min_size(
                Pos2::new(rect.min.x + 6.0, inline_top + 2.0),
                Vec2::new(
                    rect.width() - 12.0,
                    (INLINE_ROW_H * view.zoom - 4.0).max(16.0),
                ),
            );
            draw_inline_param(ui, inline_rect, node, &desc, doc, gid, pending);
        }
    }
}

fn draw_diamond(painter: &egui::Painter, c: Pos2, r: f32, color: Color32) {
    let pts = vec![
        Pos2::new(c.x, c.y - r),
        Pos2::new(c.x + r, c.y),
        Pos2::new(c.x, c.y + r),
        Pos2::new(c.x - r, c.y),
    ];
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// A lightweight, data-only local diagnostic (08 §3.3 / §6.6): a required input
/// left unwired. `Output`'s missing input is the "composition unsatisfied" case.
fn node_diagnostic(
    op: &GraphOp,
    id: GraphNodeId,
    graph: &photonic_core::timeline::NodeGraph,
) -> Option<String> {
    let (ins, _) = op_ports(op);
    if ins.is_empty() {
        return None;
    }
    // Only flag the primary input for the "unsatisfied" hint; `Merge`/`Switch`
    // tolerate partial wiring (08 §3.3 defaults) so don't nag on those.
    let flag_all = matches!(op, GraphOp::Output);
    let missing: Vec<&str> = ins
        .iter()
        .filter(|port| {
            (flag_all || port.idx == 0)
                && !graph.edges.iter().any(|e| e.to == (id, InPort(port.idx)))
        })
        .map(|p| p.label)
        .collect();
    if missing.is_empty() {
        None
    } else if matches!(op, GraphOp::Output) {
        Some("Output has no input — composition falls back to the clip's default chain".into())
    } else {
        Some(format!(
            "Input '{}' is unwired (defaults apply)",
            missing.join(", ")
        ))
    }
}

/// Compact inline param editor placed on a node body via [`Ui::put`]. Only the
/// `Float`/`Enum` kinds (the ones [`primary_inline_param`] ever returns) render
/// inline; commit fires on drag release / discrete change so one gesture is one
/// undo step (SetNodeParam does not coalesce, 08 §8 / core `GraphCmd::coalesce`).
fn draw_inline_param(
    ui: &mut Ui,
    rect: Rect,
    node: &GraphNode,
    desc: &ParamDesc,
    doc: &Document,
    gid: GraphId,
    pending: &mut Vec<TimelineCmd>,
) {
    let cur = node_param_value(doc, node, desc);
    let prefix = format!("{}: ", desc.label);
    if !graph_is_editable(doc, gid) {
        let text = match cur {
            PropValue::Float(value) => format!("{prefix}{value:.2}"),
            PropValue::Enum(value) => format!("{prefix}{value}"),
            _ => prefix,
        };
        ui.put(rect, egui::Label::new(egui::RichText::new(text).weak()))
            .on_hover_text("Track locked: graph is read-only");
        return;
    }
    match desc.kind {
        PropValueKind::Float => {
            let v = match cur {
                PropValue::Float(f) => f,
                _ => 0.0,
            };
            let key = node_number_draft_id(ui, doc, node.id, desc.path);
            let (_, edited) = node_number_control(ui, key, v, |ui, value| {
                ui.put(rect, node_drag_value(value, desc).prefix(prefix))
            });
            if let Some(v) = edited {
                if let Some(cmd) = set_param_cmd(doc, gid, node.id, desc.path, PropValue::Float(v))
                {
                    pending.push(cmd);
                }
            }
        }
        PropValueKind::Enum => {
            let n = match cur {
                PropValue::Enum(e) => e as f64,
                _ => 0.0,
            };
            let key = node_number_draft_id(ui, doc, node.id, desc.path);
            let (_, edited) = node_number_control(ui, key, n, |ui, value| {
                ui.put(rect, node_drag_value(value, desc).prefix(prefix))
            });
            if let Some(n) = edited {
                let value = PropValue::Enum(n.round().max(0.0) as u32);
                if let Some(cmd) = set_param_cmd(doc, gid, node.id, desc.path, value) {
                    pending.push(cmd);
                }
            }
        }
        _ => {}
    }
}

/// Commit-on-release: one undo step per drag gesture, immediate on typed/discrete
/// changes.
#[cfg(test)]
fn commit_worthy(resp: &Response) -> bool {
    resp.drag_stopped() || (resp.changed() && !resp.dragged())
}

fn draw_viewer(
    ui: &Ui,
    rect: Rect,
    graph: &photonic_core::timeline::NodeGraph,
    _gid: GraphId,
    pin: Option<GraphNodeId>,
) {
    let painter = ui.painter_at(rect);
    let v = ui.visuals();
    painter.rect_filled(rect, egui::Rounding::same(4.0), v.extreme_bg_color);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(4.0),
        Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color),
    );
    // Header.
    painter.text(
        rect.min + Vec2::new(8.0, 8.0),
        Align2::LEFT_TOP,
        "Viewer",
        FontId::proportional(11.0),
        v.weak_text_color(),
    );
    let target = pin
        .and_then(|id| graph.nodes.get(&id).map(|n| op_title(&n.op)))
        .unwrap_or_else(|| "Output".to_string());
    let pinned = pin.is_some();
    painter.text(
        rect.min + Vec2::new(8.0, 24.0),
        Align2::LEFT_TOP,
        if pinned {
            format!("● pinned: {target}")
        } else {
            format!("● {target} (true output)")
        },
        FontId::proportional(11.0),
        if pinned {
            v.selection.stroke.color
        } else {
            v.text_color()
        },
    );
    // Placeholder composited frame region (live readback is engine-owned, 08 §6.7).
    let frame = Rect::from_min_max(
        rect.min + Vec2::new(8.0, 44.0),
        rect.max - Vec2::new(8.0, 8.0),
    );
    if frame.width() > 10.0 && frame.height() > 10.0 {
        painter.rect_filled(frame, egui::Rounding::same(3.0), v.faint_bg_color);
        painter.text(
            frame.center(),
            Align2::CENTER_CENTER,
            "composed output\n(scrub in the timeline below)",
            FontId::proportional(10.0),
            v.weak_text_color(),
        );
    }
}

fn draw_flash(ui: &Ui, rect: Rect) {
    let now = ui.input(|i| i.time);
    let flash: Option<(String, f64)> = ui.data(|d| d.get_temp(flash_id()));
    if let Some((msg, until)) = flash {
        if now < until {
            let painter = ui.painter_at(rect);
            let v = ui.visuals();
            let pos = Pos2::new(rect.center().x, rect.min.y + 14.0);
            let galley = painter.layout_no_wrap(msg, FontId::proportional(12.0), v.error_fg_color);
            let bg = Rect::from_center_size(pos, galley.size() + Vec2::new(16.0, 8.0));
            painter.rect_filled(bg, egui::Rounding::same(4.0), v.extreme_bg_color);
            painter.rect_stroke(
                bg,
                egui::Rounding::same(4.0),
                Stroke::new(1.0, v.error_fg_color),
            );
            painter.galley(
                Pos2::new(bg.min.x + 8.0, bg.min.y + 4.0),
                galley,
                v.error_fg_color,
            );
            ui.ctx().request_repaint();
        }
    }
}

/// Placeholder shown when no graph is open: offers the two entry points that can
/// create/open a graph from within this panel (project graph + selected clip's
/// composition), both via real `ops` commands.
fn draw_no_graph_placeholder(
    ui: &mut Ui,
    rect: Rect,
    doc: &mut Document,
    history: &mut CommandHistory,
    vid: &mut VideoPanelUi,
) {
    let mut open_project = false;
    let mut open_clip = false;
    let has_selection = !vid.selection.is_empty();
    ui.painter_at(rect)
        .rect_filled(rect, 0.0, ui.visuals().panel_fill);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(24.0))
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    child.add_space(40.0);
    child.label(egui::RichText::new("Node Editor").heading());
    child.add_space(6.0);
    child.label(
        egui::RichText::new("Open a clip's composition or the project graph to start compositing.")
            .weak(),
    );
    child.add_space(16.0);
    if child
        .add_enabled(
            has_selection,
            egui::Button::new("Open composition for selected clip"),
        )
        .clicked()
    {
        open_clip = true;
    }
    child.add_space(4.0);
    if child.button("Open project graph").clicked() {
        open_project = true;
    }
    child.add_space(10.0);
    if child.button("Back to timeline").clicked() {
        *vid.node_canvas_active = false;
    }

    if open_project {
        let existing = doc.timeline.as_ref().and_then(|p| p.project_graph);
        if let Some(g) = existing {
            *vid.open_graph = Some(g);
        } else if let Some(p) = doc.timeline.as_ref() {
            let cmds = ops::set_project_graph(p, None);
            let new_id = added_graph_id(&cmds);
            commit_batch(history, doc, cmds);
            *vid.open_graph =
                new_id.or_else(|| doc.timeline.as_ref().and_then(|p| p.project_graph));
        }
    }
    if open_clip {
        if let Some(&clip) = vid.selection.first() {
            // Reuse an existing composition if the clip already has one.
            let existing = doc.timeline.as_ref().and_then(|p| {
                p.sequences.values().find_map(|s| {
                    s.video_tracks
                        .iter()
                        .chain(s.audio_tracks.iter())
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == clip)
                        .and_then(|c| c.composition)
                })
            });
            if let Some(g) = existing {
                *vid.open_graph = Some(g);
            } else if let Some((sid, tid)) = find_clip_location(doc, clip) {
                if let Some(p) = doc.timeline.as_ref() {
                    if let Ok(cmds) = ops::create_clip_composition(p, sid, tid, clip) {
                        let new_id = added_graph_id(&cmds);
                        commit_batch(history, doc, cmds);
                        *vid.open_graph = new_id;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_top_bar(
    ui: &mut Ui,
    rect: Rect,
    doc: &mut Document,
    vid: &mut VideoPanelUi,
    graph: &photonic_core::timeline::NodeGraph,
    gid: GraphId,
    is_project_graph: bool,
    _batches: &mut [Vec<TimelineCmd>],
) {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child
        .painter()
        .rect_filled(rect, 0.0, child.visuals().panel_fill);
    if child.button("← Back to Timeline").clicked() {
        *vid.node_canvas_active = false;
    }
    child.separator();
    // Segmented control: which graph is open.
    let clip_name = clip_name_for_graph(doc, gid);
    let clip_label = clip_name
        .clone()
        .map(|n| format!("Clip: {n}"))
        .unwrap_or_else(|| "Clip".to_string());
    let clip_sel = !is_project_graph;
    if child
        .selectable_label(clip_sel, clip_label)
        .on_hover_text("The open per-clip composition")
        .clicked()
    {
        // Switch to a clip composition: prefer the currently-open clip graph; if
        // the project graph is open, switch to the selected clip's composition.
        if is_project_graph {
            if let Some(&clip) = vid.selection.first() {
                if let Some(g) = clip_composition_id(doc, clip) {
                    *vid.open_graph = Some(g);
                }
            }
        }
    }
    let pg = doc.timeline.as_ref().and_then(|p| p.project_graph);
    if child
        .selectable_label(is_project_graph, "Project Graph")
        .on_hover_text("The document-level output graph (08 §5)")
        .clicked()
    {
        if let Some(g) = pg {
            *vid.open_graph = Some(g);
        }
    }
    child.separator();
    child.label(
        egui::RichText::new(format!(
            "{} nodes · {} edges",
            graph.nodes.len(),
            graph.edges.len()
        ))
        .weak()
        .small(),
    );
}

fn clip_name_for_graph(doc: &Document, gid: GraphId) -> Option<String> {
    let p = doc.timeline.as_ref()?;
    for s in p.sequences.values() {
        for t in s.video_tracks.iter().chain(s.audio_tracks.iter()) {
            for c in &t.clips {
                if c.composition == Some(gid) {
                    return Some(c.name.clone());
                }
            }
        }
    }
    None
}

fn graph_is_editable(doc: &Document, gid: GraphId) -> bool {
    doc.timeline
        .as_ref()
        .is_some_and(|project| graph_ops::ensure_editable(project, gid).is_ok())
}

fn add_node_cmd(
    doc: &Document,
    gid: GraphId,
    node: GraphNode,
    pos: NodePos,
) -> Option<TimelineCmd> {
    graph_ops::add_node(doc.timeline.as_ref()?, gid, node, pos).ok()
}

fn remove_edge_cmd(doc: &Document, gid: GraphId, edge: GraphEdge) -> Option<TimelineCmd> {
    graph_ops::remove_edge(doc.timeline.as_ref()?, gid, edge).ok()
}

fn clip_composition_id(doc: &Document, clip: photonic_core::timeline::ClipId) -> Option<GraphId> {
    let p = doc.timeline.as_ref()?;
    p.sequences.values().find_map(|s| {
        s.video_tracks
            .iter()
            .chain(s.audio_tracks.iter())
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == clip)
            .and_then(|c| c.composition)
    })
}

#[allow(clippy::too_many_arguments)]
fn apply_palette_intent(
    _ui: &Ui,
    doc: &Document,
    gid: GraphId,
    graph: &photonic_core::timeline::NodeGraph,
    canvas_rect: Rect,
    view: View,
    intent: PaletteIntent,
    pending: &mut Vec<TimelineCmd>,
    batches: &mut Vec<Vec<TimelineCmd>>,
) {
    match intent {
        PaletteIntent::AddNode(kind) => {
            // Drop new palette nodes near the canvas center in graph space.
            let center = s2g(canvas_rect.center(), canvas_rect.min, view.pan, view.zoom);
            // Nudge by node count so successive adds don't stack exactly.
            let n = graph.nodes.len() as f32;
            let pos = NodePos {
                x: center.x - NODE_W * 0.5 + (n % 4.0) * 18.0,
                y: center.y + (n % 4.0) * 18.0,
            };
            let node = GraphNode::new(kind.make_op());
            pending.extend(add_node_cmd(doc, gid, node, pos));
        }
        PaletteIntent::SetTimeOffset { node, old, new } => {
            if !pending.iter().any(|cmd|matches!(cmd,TimelineCmd::GraphEdit(photonic_core::timeline::GraphCmd::SetTimeOffset{node:n,..}) if *n==node)) {
                pending.extend(set_time_offset_cmd(doc,gid,node,old,new));
            }
        }
        PaletteIntent::SetMaskShape{node,old,new}=>{
            if !pending.iter().any(|cmd|matches!(cmd,TimelineCmd::GraphEdit(photonic_core::timeline::GraphCmd::SetMaskShape{node:n,..}) if *n==node)) {pending.extend(set_mask_shape_cmd(doc,gid,node,&old,new));}
        }
        PaletteIntent::SetText {node,old,new} => {
            if !pending.iter().any(|cmd|matches!(cmd,TimelineCmd::GraphEdit(photonic_core::timeline::GraphCmd::SetText{node:n,..}) if *n==node)) {
                pending.extend(set_text_cmd(doc,gid,node,&old,new));
            }
        }
        PaletteIntent::SetParam { node, path, value } => {
            if let Some(cmd) = set_param_cmd(doc, gid, node, &path, value) {
                pending.push(cmd);
            }
        }
        PaletteIntent::Connect { from, to, in_port } => {
            if let Ok(cmds) = connect_cmds(doc, gid, from, 0, to, in_port) {
                batches.push(cmds);
            }
        }
        PaletteIntent::RemoveNode(node) => {
            if let Some(p) = doc.timeline.as_ref() {
                if let Ok(cmd) = graph_ops::remove_node(p, gid, node) {
                    pending.push(cmd);
                }
            }
        }
    }
}

// ── Left-rail palette + inspector drawer ───────────────────────────────────────

/// Left-rail Node Editor drawer: palette + selected-node inspector + graph info.
/// NOT the graph canvas (that is [`draw_node_canvas`]). Emits typed
/// [`PaletteIntent`]s into egui memory that the canvas drains + commits.
pub(crate) fn draw_node_editor_palette(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let Some(gid) = *ctx.video.open_graph else {
        ui.label(egui::RichText::new("NODE EDITOR").small().weak());
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "No composition open. Enter the node editor to open the project \
                                 graph or a clip's composition.",
            )
            .weak(),
        );
        ui.add_space(6.0);
        if ui.button("Open node editor").clicked() {
            *ctx.video.node_canvas_active = true;
        }
        return;
    };
    let Some(graph) = ctx
        .doc
        .timeline
        .as_ref()
        .and_then(|p| p.graphs.get(&gid))
        .cloned()
    else {
        ui.label(egui::RichText::new("Graph unavailable").weak());
        return;
    };
    let is_project_graph = ctx.doc.timeline.as_ref().and_then(|p| p.project_graph) == Some(gid);

    // ── Graph info header ────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("NODE EDITOR").small().weak());
        if !*ctx.video.node_canvas_active && ui.small_button("Edit").clicked() {
            *ctx.video.node_canvas_active = true;
        }
    });
    ui.label(
        egui::RichText::new(if is_project_graph {
            "Project graph".to_string()
        } else {
            clip_name_for_graph(ctx.doc, gid)
                .map(|n| format!("Composition · {n}"))
                .unwrap_or_else(|| "Composition".to_string())
        })
        .strong(),
    );
    ui.label(
        egui::RichText::new(format!(
            "{} nodes · {} edges",
            graph.nodes.len(),
            graph.edges.len()
        ))
        .weak()
        .small(),
    );
    ui.separator();

    let editable = graph_is_editable(ctx.doc, gid);
    if !editable {
        ui.label("Read-only: a clip using this graph is on a locked track.");
    }
    ui.add_enabled_ui(editable, |ui| {
        // ── Add-node search palette (08 §6.2) ────────────────────────────────────
        egui::CollapsingHeader::new("Add node")
            .default_open(true)
            .show(ui, |ui| {
                let mut q: String = ui
                    .data(|d| d.get_temp::<String>(palette_search_id()))
                    .unwrap_or_default();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut q)
                        .hint_text("Search nodes…")
                        .desired_width(f32::INFINITY),
                );
                if resp.changed() {
                    ui.data_mut(|d| d.insert_temp(palette_search_id(), q.clone()));
                }
                let ql = q.to_lowercase();
                egui::ScrollArea::vertical()
                    .id_salt("node_palette_list")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for fam in Family::ORDER {
                            let kinds: Vec<NodeKind> = NodeKind::ALL
                                .iter()
                                .copied()
                                .filter(|k| {
                                    k.family() == fam
                                        && k.allowed(is_project_graph)
                                        && k.matches_query(&ql)
                                })
                                .collect();
                            if kinds.is_empty() {
                                continue;
                            }
                            ui.label(
                                egui::RichText::new(fam.title().to_uppercase())
                                    .small()
                                    .weak(),
                            );
                            for k in kinds {
                                if ui
                                    .add(
                                        egui::Button::new(k.label())
                                            .min_size(Vec2::new(ui.available_width(), 0.0)),
                                    )
                                    .clicked()
                                {
                                    push_palette_intent(ui, PaletteIntent::AddNode(k));
                                    *ctx.video.node_canvas_active = true;
                                }
                            }
                            ui.add_space(2.0);
                        }
                    });
            });

        ui.separator();

        // ── Selected-node inspector (08 §6.4) ────────────────────────────────────
        let sel = *ctx.video.selected_graph_node;
        match sel.and_then(|id| graph.nodes.get(&id)) {
            None => {
                ui.label(egui::RichText::new("Select a node to edit its parameters.").weak());
            }
            Some(node) => {
                draw_inspector(ui, ctx, gid, &graph, node);
            }
        }
    });
}

#[derive(Clone)]
struct GraphTimeDraft {
    base: Tick,
    text: String,
}

fn graph_time_draft(stored: Option<GraphTimeDraft>, offset: Tick) -> GraphTimeDraft {
    stored
        .filter(|draft| draft.base == offset)
        .unwrap_or_else(|| GraphTimeDraft {
            base: offset,
            text: offset.0.to_string(),
        })
}

fn draw_graph_time_editor(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    gid: GraphId,
    node: GraphNodeId,
    offset: Tick,
) {
    let key = graph_interaction_scope(ctx.doc, gid).with(("time_offset_draft", node));
    let mut draft = graph_time_draft(ui.data(|d| d.get_temp(key)), offset);
    ui.label("Offset (integer ticks)");
    ui.add(egui::TextEdit::singleline(&mut draft.text).desired_width(ui.available_width()));
    ui.label(
        egui::RichText::new("705,600,000 ticks = 1 second. Positive reads earlier frames.")
            .small()
            .weak(),
    );
    let parsed = draft.text.trim().parse::<i64>().map(Tick);
    if let Ok(value) = parsed {
        ui.label(format!("{:.6} seconds", value.as_seconds_f64()));
    } else {
        ui.colored_label(
            ui.visuals().error_fg_color,
            "Enter a signed 64-bit integer.",
        );
    }
    let editable = ctx
        .doc
        .timeline
        .as_ref()
        .is_some_and(|p| graph_ops::ensure_editable(p, gid).is_ok());
    if !editable {
        ui.label("Track locked: graph is read-only");
    }
    let changed = parsed.as_ref().is_ok_and(|value| *value != offset);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(editable && changed, egui::Button::new("Apply offset"))
            .clicked()
        {
            if let Ok(new) = parsed {
                push_palette_intent(
                    ui,
                    PaletteIntent::SetTimeOffset {
                        node,
                        old: offset,
                        new,
                    },
                );
                *ctx.video.node_canvas_active = true;
            }
        }
        if ui
            .add_enabled(
                draft.text != offset.0.to_string(),
                egui::Button::new("Revert draft"),
            )
            .clicked()
        {
            draft.text = offset.0.to_string();
        }
    });
    ui.data_mut(|d| d.insert_temp(key, draft));
}

#[derive(Clone)]
struct GraphShapeDraft {
    base: MaskShapeKind,
    edited: MaskShapeKind,
}

fn draw_graph_shape_fields(ui: &mut Ui, shape: &mut MaskShapeKind) {
    let old_kind = match shape {
        MaskShapeKind::Ellipse => 0,
        MaskShapeKind::Rect => 1,
        MaskShapeKind::Polygon { .. } => 2,
    };
    let mut kind = old_kind;
    egui::ComboBox::from_id_salt("shape_kind")
        .selected_text(["Ellipse", "Rectangle", "Polygon"][kind])
        .show_ui(ui, |ui| {
            for (index, name) in ["Ellipse", "Rectangle", "Polygon"].iter().enumerate() {
                ui.selectable_value(&mut kind, index, *name);
            }
        });
    if kind != old_kind {
        *shape = match kind {
            0 => MaskShapeKind::Ellipse,
            1 => MaskShapeKind::Rect,
            _ => MaskShapeKind::Polygon {
                vertices: vec![[0., 0.], [1., 0.], [0.5, 1.]],
            },
        };
    }
    if let MaskShapeKind::Polygon { vertices } = shape {
        ui.label("Static normalized vertices · even-odd fill");
        ui.label("Feather softens inward; size is the local half-extent.");
        let mut remove = None;
        egui::ScrollArea::vertical()
            .id_salt("polygon_vertices")
            .max_height(160.0)
            .show(ui, |ui| {
                let can_remove = vertices.len() > 3;
                for (index, vertex) in vertices.iter_mut().take(128).enumerate() {
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(format!("{}", index + 1));
                            for v in vertex {
                                ui.add(
                                    egui::DragValue::new(v)
                                        .range(0.0..=1.0)
                                        .clamp_existing_to_range(false)
                                        .speed(0.01),
                                );
                            }
                            if ui.add_enabled(can_remove, egui::Button::new("−")).clicked() {
                                remove = Some(index);
                            }
                        });
                    });
                }
            });
        if let Some(index) = remove {
            vertices.remove(index);
        }
        if ui
            .add_enabled(vertices.len() < 128, egui::Button::new("Add vertex"))
            .clicked()
        {
            vertices.push([0.5, 0.5]);
        }
    }
}

fn draw_graph_shape_editor(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    gid: GraphId,
    node: GraphNodeId,
    shape: &MaskShapeKind,
) {
    let key = graph_interaction_scope(ctx.doc, gid).with(("shape_draft", node));
    let mut draft = ui
        .data(|d| d.get_temp::<GraphShapeDraft>(key))
        .filter(|d| d.base == *shape)
        .unwrap_or_else(|| GraphShapeDraft {
            base: shape.clone(),
            edited: shape.clone(),
        });
    draw_graph_shape_fields(ui, &mut draft.edited);
    let error = draft.edited.validate_native().err();
    if let Some(error) = error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    let editable = ctx
        .doc
        .timeline
        .as_ref()
        .is_some_and(|p| graph_ops::ensure_editable(p, gid).is_ok());
    if !editable {
        ui.label("Track locked: graph is read-only");
    }
    let changed = draft.edited != *shape;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                editable && changed && error.is_none(),
                egui::Button::new("Apply shape"),
            )
            .clicked()
        {
            push_palette_intent(
                ui,
                PaletteIntent::SetMaskShape {
                    node,
                    old: shape.clone(),
                    new: draft.edited.clone(),
                },
            );
            *ctx.video.node_canvas_active = true;
        }
        if ui
            .add_enabled(changed, egui::Button::new("Revert draft"))
            .clicked()
        {
            draft.edited = shape.clone();
        }
    });
    ui.label(
        egui::RichText::new("Polygon rendering: Native Managed only.")
            .small()
            .weak(),
    );
    ui.data_mut(|d| d.insert_temp(key, draft));
}

#[derive(Clone)]
struct GraphTextDraft {
    base: TextGen,
    edited: TextGen,
}

fn draw_graph_text_fields(ui: &mut Ui, text: &mut TextGen) {
    ui.add(egui::TextEdit::multiline(&mut text.text).desired_rows(3));
    ui.horizontal(|ui| {
        ui.label("Font");
        ui.text_edit_singleline(&mut text.font_family);
    });
    ui.horizontal(|ui| {
        ui.label("Size");
        ui.add(
            egui::DragValue::new(&mut text.font_size)
                .range(1.0..=512.0)
                .clamp_existing_to_range(false)
                .speed(0.5),
        );
    });
    let c = &mut text.fill;
    let mut rgba = [c.r, c.g, c.b, c.a];
    if ui.color_edit_button_rgba_unmultiplied(&mut rgba).changed() {
        *c = photonic_core::Color::new(rgba[0], rgba[1], rgba[2], rgba[3]);
    }
    ui.horizontal(|ui| {
        ui.label("Position");
        for v in &mut text.position {
            ui.add(
                egui::DragValue::new(v)
                    .range(0.0..=1.0)
                    .clamp_existing_to_range(false)
                    .speed(0.01),
            );
        }
    });
}

fn draw_graph_text_editor(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    gid: GraphId,
    node: GraphNodeId,
    text: &TextGen,
) {
    let key = graph_interaction_scope(ctx.doc, gid).with(("text_draft", node));
    let mut draft = ui
        .data(|d| d.get_temp::<GraphTextDraft>(key))
        .filter(|d| d.base == *text)
        .unwrap_or_else(|| GraphTextDraft {
            base: text.clone(),
            edited: text.clone(),
        });
    ui.label("Static text · managed scene graphic");
    ui.label(
        egui::RichText::new("Native Managed rendering; Legacy text nodes remain unqualified.")
            .small()
            .weak(),
    );
    draw_graph_text_fields(ui, &mut draft.edited);
    let error = draft.edited.validate_native().err();
    if let Some(error) = error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    let editable = ctx
        .doc
        .timeline
        .as_ref()
        .is_some_and(|p| graph_ops::ensure_editable(p, gid).is_ok());
    if !editable {
        ui.label("Track locked: graph is read-only");
    }
    let changed = draft.edited != *text;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                editable && changed && error.is_none(),
                egui::Button::new("Apply text"),
            )
            .clicked()
        {
            push_palette_intent(
                ui,
                PaletteIntent::SetText {
                    node,
                    old: text.clone(),
                    new: draft.edited.clone(),
                },
            );
            *ctx.video.node_canvas_active = true;
        }
        if ui
            .add_enabled(changed, egui::Button::new("Revert draft"))
            .clicked()
        {
            draft.edited = text.clone();
        }
    });
    ui.data_mut(|d| d.insert_temp(key, draft));
}

fn draw_inspector(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    gid: GraphId,
    graph: &photonic_core::timeline::NodeGraph,
    node: &GraphNode,
) {
    ui.label(egui::RichText::new(op_title(&node.op)).strong());
    if !node.params.tracks.is_empty() {
        ui.label(
            egui::RichText::new(format!("◆ {} animated param(s)", node.params.tracks.len()))
                .small()
                .color(ui.visuals().selection.stroke.color),
        );
    }

    if let GraphOp::MaskShape { shape } = &node.op {
        draw_graph_shape_editor(ui, ctx, gid, node.id, shape);
    }
    if let GraphOp::TimeOffset { offset } = node.op {
        draw_graph_time_editor(ui, ctx, gid, node.id, offset);
    }
    let params = op_params(&node.op);
    if let GraphOp::Text { text } = &node.op {
        draw_graph_text_editor(ui, ctx, gid, node.id, text);
    } else if params.is_empty() {
        ui.label(
            egui::RichText::new(structural_note(&node.op))
                .weak()
                .small(),
        );
    } else {
        for desc in &params {
            draw_inspector_param(ui, ctx, gid, node, desc);
        }
    }

    ui.add_space(6.0);
    ui.separator();

    // A11y / no-pointer wiring fallback (08 §6.2): connect this node's output to
    // another node's primary input without a drag.
    let (_, outs) = op_ports(&node.op);
    if !outs.is_empty() {
        egui::CollapsingHeader::new("Connect output → node")
            .default_open(false)
            .show(ui, |ui| {
                for (id, other) in &graph.nodes {
                    if *id == node.id {
                        continue;
                    }
                    let (ins, _) = op_ports(&other.op);
                    if ins.is_empty() {
                        continue;
                    }
                    if ui.button(format!("→ {}", op_title(&other.op))).clicked() {
                        push_palette_intent(
                            ui,
                            PaletteIntent::Connect {
                                from: node.id,
                                to: *id,
                                in_port: 0,
                            },
                        );
                        *ctx.video.node_canvas_active = true;
                    }
                }
            });
    }

    // Delete (never the Output node).
    if !matches!(node.op, GraphOp::Output) && ui.button("Delete node").clicked() {
        push_palette_intent(ui, PaletteIntent::RemoveNode(node.id));
        *ctx.video.selected_graph_node = None;
        *ctx.video.node_canvas_active = true;
    }
}

fn node_number_draft_id(ui: &Ui, doc: &Document, node: GraphNodeId, path: &str) -> Id {
    ui.id().with(("node_number_draft", doc.id, node, path))
}

#[derive(Clone, Copy)]
struct NodeNumberDraft {
    original: f64,
    value: f64,
    pass: u64,
    widget: Id,
}

/// Keep an in-progress drag outside the document and commit it once on release.
/// Escape, a hidden control, a lock change or a concurrent value edit cancels it.
fn node_number_control(
    ui: &mut Ui,
    key: Id,
    original: f64,
    draw: impl FnOnce(&mut Ui, &mut f64) -> Response,
) -> (Response, Option<f64>) {
    let pass = ui.ctx().cumulative_pass_nr();
    let previous = ui.data(|data| data.get_temp::<NodeNumberDraft>(key));
    let (escape, down, released) = ui.input(|input| {
        (
            input.key_pressed(egui::Key::Escape),
            input.pointer.primary_down(),
            input.pointer.button_released(egui::PointerButton::Primary),
        )
    });
    let cancelled = escape
        || !ui.is_enabled()
        || previous.is_some_and(|draft| {
            draft.original.to_bits() != original.to_bits()
                || draft.pass.saturating_add(1) < pass
                || (!down && !released)
        });
    if cancelled {
        if previous.is_some_and(|draft| ui.ctx().dragged_id() == Some(draft.widget)) {
            ui.ctx().stop_dragging();
        }
        ui.data_mut(|data| data.remove::<NodeNumberDraft>(key));
    }
    let mut value = if cancelled {
        original
    } else {
        previous.map_or(original, |draft| draft.value)
    };
    let response = draw(ui, &mut value);
    if cancelled {
        return (response, None);
    }
    if (response.dragged() || (response.changed() && down)) && !released {
        let widget = ui.ctx().dragged_id().unwrap_or(response.id);
        ui.data_mut(|data| {
            data.insert_temp(
                key,
                NodeNumberDraft {
                    original,
                    value,
                    pass,
                    widget,
                },
            )
        });
        return (response, None);
    }
    let stopped = (released && previous.is_some())
        || response.drag_stopped()
        || previous.is_some_and(|draft| ui.ctx().drag_stopped_id() == Some(draft.widget));
    let changed = response.changed() || stopped;
    if changed {
        ui.data_mut(|data| data.remove::<NodeNumberDraft>(key));
        let result = (value.to_bits() != original.to_bits()).then_some(value);
        return (response, result);
    }
    (response, None)
}

fn node_drag_value<'a>(value: &'a mut f64, desc: &ParamDesc) -> egui::DragValue<'a> {
    let speed = if desc.kind == PropValueKind::Enum {
        0.25
    } else if desc.path == "transform.rotation" {
        0.01
    } else {
        desc.range.map_or(1.0, |(lo, hi)| (hi - lo) / 300.0)
    };
    let mut widget = egui::DragValue::new(value)
        .speed(speed)
        .clamp_existing_to_range(false)
        .update_while_editing(false);
    if let Some((lo, hi)) = desc.range {
        widget = widget.range(lo..=hi);
    }
    widget
}

fn draw_float_param(ui: &mut Ui, value: &mut f64, desc: &ParamDesc) -> Response {
    match desc.range {
        Some((lo, hi)) => {
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 64.0).clamp(24.0, 120.0);
                let slider = ui.add(
                    egui::Slider::new(value, lo..=hi)
                        .show_value(false)
                        .clamping(egui::SliderClamping::Edits),
                );
                let number = ui.add(node_drag_value(value, desc));
                slider.union(number)
            })
            .inner
        }
        None => ui.add(node_drag_value(value, desc)),
    }
}

fn draw_inspector_param(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    gid: GraphId,
    node: &GraphNode,
    desc: &ParamDesc,
) {
    let cur = node_param_value(ctx.doc, node, desc);
    ui.horizontal(|ui| {
        ui.label(desc.label);
        let _ = gid;
        match desc.kind {
            PropValueKind::Float => {
                let v = match cur {
                    PropValue::Float(f) => f,
                    _ => 0.0,
                };
                let key = node_number_draft_id(ui, ctx.doc, node.id, desc.path);
                let (_, edited) =
                    node_number_control(ui, key, v, |ui, value| draw_float_param(ui, value, desc));
                if let Some(v) = edited {
                    push_palette_intent(
                        ui,
                        PaletteIntent::SetParam {
                            node: node.id,
                            path: desc.path.to_string(),
                            value: PropValue::Float(v),
                        },
                    );
                    *ctx.video.node_canvas_active = true;
                }
            }
            PropValueKind::Color => {
                let mut rgba = match cur {
                    PropValue::Color(c) => [c.r, c.g, c.b, c.a],
                    _ => [1.0, 1.0, 1.0, 1.0],
                };
                if ui.color_edit_button_rgba_unmultiplied(&mut rgba).changed() {
                    push_palette_intent(
                        ui,
                        PaletteIntent::SetParam {
                            node: node.id,
                            path: desc.path.to_string(),
                            value: PropValue::Color(photonic_core::Color {
                                r: rgba[0],
                                g: rgba[1],
                                b: rgba[2],
                                a: rgba[3],
                            }),
                        },
                    );
                    *ctx.video.node_canvas_active = true;
                }
            }
            PropValueKind::Bool => {
                let mut b = matches!(cur, PropValue::Bool(true));
                if ui.checkbox(&mut b, "").changed() {
                    push_palette_intent(
                        ui,
                        PaletteIntent::SetParam {
                            node: node.id,
                            path: desc.path.to_string(),
                            value: PropValue::Bool(b),
                        },
                    );
                    *ctx.video.node_canvas_active = true;
                }
            }
            PropValueKind::Enum => {
                let n = match cur {
                    PropValue::Enum(e) => e as f64,
                    _ => 0.0,
                };
                let key = node_number_draft_id(ui, ctx.doc, node.id, desc.path);
                let (_, edited) = node_number_control(ui, key, n, |ui, value| {
                    ui.add(node_drag_value(value, desc))
                });
                if let Some(n) = edited {
                    push_palette_intent(
                        ui,
                        PaletteIntent::SetParam {
                            node: node.id,
                            path: desc.path.to_string(),
                            value: PropValue::Enum(n.round().max(0.0) as u32),
                        },
                    );
                    *ctx.video.node_canvas_active = true;
                }
            }
            PropValueKind::Vec2 => {
                ui.label(egui::RichText::new("vec2").weak());
            }
        }
    });
}

/// Read-only note for ops whose editable surface is structural (no `set_node_param`
/// path) — honest about what the inspector can and can't touch in v1.
fn structural_note(op: &GraphOp) -> &'static str {
    match op {
        GraphOp::Output => "Terminal node — no parameters.",
        GraphOp::ClipIn => "Binds to the clip's trimmed source — no parameters.",
        GraphOp::MediaIn { .. } => "Source: media asset (set from the media pool).",
        GraphOp::VectorIn { .. } => "Source: vector document.",
        GraphOp::Merge { .. } => "Blend mode is structural; opacity is editable above.",
        GraphOp::Grade { .. } => "Grade stack — edit in the Color page.",
        GraphOp::Lut { .. } => "LUT asset reference.",
        GraphOp::Text { .. } => "Styled text — structural payload.",
        GraphOp::TimeOffset { .. } => "Time offset is structural (per-instance constant).",
        GraphOp::Note { .. } => "Canvas annotation.",
        GraphOp::MaskFromMatte => "Auto subject cutout — no parameters (v1).",
        GraphOp::ChannelSplit | GraphOp::ChannelCombine => "Channel routing — no parameters.",
        GraphOp::Invert => "No parameters.",
        _ => "No editable parameters.",
    }
}

// ── Tests (pure geometry + catalog logic) ──────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Pos2, b: Pos2) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    #[test]
    fn graph_text_inspector_view_preserves_unqualified_stored_values() {
        let ctx = egui::Context::default();
        let mut text = TextGen {
            text: "Read only".into(),
            font_size: 900.0,
            position: [-0.2, 1.2],
            fill: photonic_core::Color::new(-0.1, 2.0, 0.5, 0.5),
            ..Default::default()
        };
        let before = text.clone();
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| draw_graph_text_fields(ui, &mut text));
            });
            assert_eq!(text, before);
        }
    }

    #[test]
    fn graph_text_inspector_commit_is_discrete_and_stale_safe() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, Sequence, TimelineProject, Track, TrackKind,
        };
        let (mut g, node) = photonic_core::timeline::NodeGraph::new_clip_composition("Text");
        let gid = g.id;
        let original = TextGen {
            text: "Original".into(),
            ..Default::default()
        };
        g.nodes.get_mut(&node).unwrap().op = GraphOp::Text {
            text: original.clone(),
        };
        let mut p = TimelineProject::new();
        let mut seq = Sequence::new("Text", FrameRate::FPS_30, 128, 64);
        let sid = seq.id;
        let mut track = Track::new(TrackKind::Video, "V1");
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::BLACK,
            },
            Tick(0),
            Tick(100),
        );
        clip.composition = Some(gid);
        track.clips.push(clip);
        seq.video_tracks.push(track);
        p.insert_sequence(seq);
        p.graphs.insert(gid, g);
        let mut doc = Document::new("Text", 128.0, 64.0);
        doc.timeline = Some(p);
        let before = serde_json::to_value(&doc).unwrap();
        let mut history = CommandHistory::default();
        let next = TextGen {
            text: "Edited".into(),
            ..original.clone()
        };
        assert!(set_text_cmd(&doc, gid, node, &original, original.clone()).is_none());
        let cmd = set_text_cmd(&doc, gid, node, &original, next.clone()).unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        assert_eq!(history.revision(), 1);
        assert!(
            set_text_cmd(&doc, gid, node, &original, next.clone()).is_none(),
            "stale draft refused"
        );
        assert!(
            set_text_cmd(&doc, gid, node, &next, next.clone()).is_none(),
            "repeat is no-op"
        );
        history.undo(&mut doc);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&sid)
            .unwrap()
            .video_tracks[0]
            .locked = true;
        assert!(set_text_cmd(&doc, gid, node, &original, next).is_none());
    }

    #[test]
    fn graph_shape_fields_do_not_clamp_imported_vertices() {
        let ctx = egui::Context::default();
        let mut shape = MaskShapeKind::Polygon {
            vertices: vec![[-0.1, 0.0], [1.1, 0.0], [0.5, 1.2]],
        };
        let before = shape.clone();
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| draw_graph_shape_fields(ui, &mut shape));
            });
            assert_eq!(shape, before);
        }
    }

    #[test]
    fn graph_shape_inspector_commit_is_discrete_and_stale_safe() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, Sequence, TimelineProject, Track, TrackKind,
        };
        let (mut g, node) = photonic_core::timeline::NodeGraph::new_clip_composition("Text");
        let gid = g.id;
        let original = MaskShapeKind::Ellipse;
        g.nodes.get_mut(&node).unwrap().op = GraphOp::MaskShape {
            shape: original.clone(),
        };
        let mut p = TimelineProject::new();
        let mut seq = Sequence::new("Text", FrameRate::FPS_30, 128, 64);
        let sid = seq.id;
        let mut track = Track::new(TrackKind::Video, "V1");
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::BLACK,
            },
            Tick(0),
            Tick(100),
        );
        clip.composition = Some(gid);
        track.clips.push(clip);
        seq.video_tracks.push(track);
        p.insert_sequence(seq);
        p.graphs.insert(gid, g);
        let mut doc = Document::new("Text", 128.0, 64.0);
        doc.timeline = Some(p);
        let before = serde_json::to_value(&doc).unwrap();
        let mut history = CommandHistory::default();
        let next = MaskShapeKind::Polygon {
            vertices: vec![[0., 0.], [1., 0.], [0.5, 1.]],
        };
        assert!(set_mask_shape_cmd(&doc, gid, node, &original, original.clone()).is_none());
        let cmd = set_mask_shape_cmd(&doc, gid, node, &original, next.clone()).unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        assert_eq!(history.revision(), 1);
        assert!(
            set_mask_shape_cmd(&doc, gid, node, &original, next.clone()).is_none(),
            "stale draft refused"
        );
        assert!(
            set_mask_shape_cmd(&doc, gid, node, &next, next.clone()).is_none(),
            "repeat is no-op"
        );
        history.undo(&mut doc);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&sid)
            .unwrap()
            .video_tracks[0]
            .locked = true;
        assert!(set_mask_shape_cmd(&doc, gid, node, &original, next).is_none());
    }

    #[test]
    fn graph_time_offset_inspector_commit_is_discrete_and_stale_safe() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, Sequence, TimelineProject, Track, TrackKind,
        };
        let (mut g, node) = photonic_core::timeline::NodeGraph::new_clip_composition("Text");
        let gid = g.id;
        let original = Tick(0);
        g.nodes.get_mut(&node).unwrap().op = GraphOp::TimeOffset { offset: original };
        let mut p = TimelineProject::new();
        let mut seq = Sequence::new("Text", FrameRate::FPS_30, 128, 64);
        let sid = seq.id;
        let mut track = Track::new(TrackKind::Video, "V1");
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::BLACK,
            },
            Tick(0),
            Tick(100),
        );
        clip.composition = Some(gid);
        track.clips.push(clip);
        seq.video_tracks.push(track);
        p.insert_sequence(seq);
        p.graphs.insert(gid, g);
        let mut doc = Document::new("Text", 128.0, 64.0);
        doc.timeline = Some(p);
        let before = serde_json::to_value(&doc).unwrap();
        let mut history = CommandHistory::default();
        let next = Tick(-70_560_000);
        assert!(set_time_offset_cmd(&doc, gid, node, original, original).is_none());
        let cmd = set_time_offset_cmd(&doc, gid, node, original, next).unwrap();
        let serialized = serde_json::to_value(&cmd).unwrap();
        let cmd: TimelineCmd = serde_json::from_value(serialized).unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        assert_eq!(history.revision(), 1);
        assert!(
            set_time_offset_cmd(&doc, gid, node, original, next).is_none(),
            "stale draft refused"
        );
        assert!(
            set_time_offset_cmd(&doc, gid, node, next, next).is_none(),
            "repeat is no-op"
        );
        history.undo(&mut doc);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&sid)
            .unwrap()
            .video_tracks[0]
            .locked = true;
        assert!(set_time_offset_cmd(&doc, gid, node, original, next).is_none());
    }

    #[test]
    fn graph_time_offset_drafts_preserve_exact_ticks_and_reset_on_undo() {
        let original = Tick(i64::MAX);
        let mut draft = graph_time_draft(None, original);
        assert_eq!(draft.text.parse::<i64>().unwrap(), i64::MAX);
        draft.text = "unfinished -".into();
        assert_eq!(
            graph_time_draft(Some(draft.clone()), original).text,
            "unfinished -"
        );
        let reset = graph_time_draft(Some(draft), Tick(0));
        assert_eq!(reset.text, "0");
        assert_eq!(reset.base, Tick(0));
    }

    #[test]
    fn locked_graph_helpers_refuse_palette_canvas_and_inspector_edits() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, Sequence, Tick, TimelineProject, Track, TrackKind,
        };
        let (graph, source) =
            photonic_core::timeline::NodeGraph::new_clip_composition("Locked composition");
        let gid = graph.id;
        let edge = graph.edges[0];
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Sequence", FrameRate::FPS_30, 1920, 1080);
        let seq = sequence.id;
        let mut track = Track::new(TrackKind::Video, "Locked");
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::RED,
            },
            Tick(0),
            Tick(100),
        );
        clip.composition = Some(gid);
        track.clips.push(clip);
        track.locked = true;
        sequence.video_tracks.push(track);
        project.insert_sequence(sequence);
        project.graphs.insert(gid, graph);
        let mut doc = Document::new("Graph lock", 32.0, 32.0);
        doc.timeline = Some(project);
        let history = CommandHistory::default();
        let before = serde_json::to_value(&doc).unwrap();
        assert!(add_node_cmd(
            &doc,
            gid,
            GraphNode::new(GraphOp::Blur),
            NodePos { x: 0.0, y: 0.0 }
        )
        .is_none());
        assert!(remove_edge_cmd(&doc, gid, edge).is_none());
        assert!(set_param_cmd(&doc, gid, source, "params.radius", PropValue::Float(4.0)).is_none());
        assert!(connect_cmds(&doc, gid, edge.from.0, 0, edge.to.0, 0).is_err());
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert_eq!(history.revision(), 0);
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&seq)
            .unwrap()
            .video_tracks[0]
            .locked = false;
        assert!(add_node_cmd(
            &doc,
            gid,
            GraphNode::new(GraphOp::Blur),
            NodePos { x: 0.0, y: 0.0 }
        )
        .is_some());
        assert!(remove_edge_cmd(&doc, gid, edge).is_some());
        assert!(set_param_cmd(&doc, gid, source, "params.radius", PropValue::Float(4.0)).is_some());
        assert!(connect_cmds(&doc, gid, edge.from.0, 0, edge.to.0, 0).is_ok());
    }

    fn wire_fixture() -> (Document, GraphId, GraphNodeId, GraphNodeId) {
        let (mut graph, source) =
            photonic_core::timeline::NodeGraph::new_clip_composition("Deferred wire");
        let blur = GraphNode::new(GraphOp::Blur);
        let blur_id = blur.id;
        graph.nodes.insert(blur_id, blur);
        graph.edges = vec![
            GraphEdge {
                from: (source, OutPort::PRIMARY),
                to: (blur_id, InPort::PRIMARY),
            },
            GraphEdge {
                from: (blur_id, OutPort::PRIMARY),
                to: (graph.output, InPort::PRIMARY),
            },
        ];
        let gid = graph.id;
        let mut project = photonic_core::timeline::TimelineProject::new();
        project.graphs.insert(gid, graph);
        let mut document = Document::new("Deferred wire", 32.0, 32.0);
        document.timeline = Some(project);
        (document, gid, source, blur_id)
    }

    #[test]
    fn input_wire_is_deferred_and_valid_drop_is_one_undoable_command() {
        let (mut doc, gid, source, blur) = wire_fixture();
        let graph = &doc.timeline.as_ref().unwrap().graphs[&gid];
        let output = graph.output;
        let original = graph.edges[0];
        let before = serde_json::to_value(&doc).unwrap();
        assert!(
            matches!(input_wire(graph,blur,0),Some(Interaction::Wire {original:Some(edge),..}) if edge==original)
        );
        assert_eq!(
            serde_json::to_value(&doc).unwrap(),
            before,
            "starting a wire must not detach it"
        );
        assert!(
            wire_drop_cmds(&doc, gid, source, 0, Some(original), None, false)
                .unwrap()
                .is_empty(),
            "cancel/outside drop leaves the origin connected"
        );
        assert!(wire_drop_cmds(&doc, gid, source, 0, None, None, true)
            .unwrap()
            .is_empty());
        assert!(wire_drop_cmds(
            &doc,
            gid,
            source,
            0,
            Some(original),
            Some((source, 0)),
            false
        )
        .is_err());
        let commands = wire_drop_cmds(
            &doc,
            gid,
            source,
            0,
            Some(original),
            Some((output, 0)),
            false,
        )
        .unwrap();
        assert_eq!(commands.len(), 1);
        let mut history = CommandHistory::default();
        commit_batch(&mut history, &mut doc, commands);
        assert_eq!(history.revision(), 1);
        let after = serde_json::to_value(&doc).unwrap();
        assert_ne!(after, before);
        assert_eq!(
            doc.timeline.as_ref().unwrap().graphs[&gid].edges,
            vec![GraphEdge {
                from: (source, OutPort::PRIMARY),
                to: (output, InPort::PRIMARY)
            }]
        );
        assert!(history.undo(&mut doc));
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert!(history.redo(&mut doc));
        assert_eq!(serde_json::to_value(&doc).unwrap(), after);
        assert!(history.undo(&mut doc));
        let disconnect = wire_drop_cmds(&doc, gid, source, 0, Some(original), None, true).unwrap();
        assert_eq!(disconnect.len(), 1);
        commit_batch(&mut history, &mut doc, disconnect);
        assert!(!doc.timeline.as_ref().unwrap().graphs[&gid]
            .edges
            .contains(&original));
        assert!(history.undo(&mut doc));
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }

    #[test]
    fn hidden_or_changed_graph_cancels_transient_wire_without_mutation() {
        let (doc, gid, _, blur) = wire_fixture();
        let graph = &doc.timeline.as_ref().unwrap().graphs[&gid];
        let before = serde_json::to_value(&doc).unwrap();
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                begin_graph_canvas_pass(ui, graph_interaction_scope(&doc, gid));
                ui.data_mut(|data| {
                    data.insert_temp(interaction_id(), input_wire(graph, blur, 0).unwrap())
                });
            });
        });
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                begin_graph_canvas_pass(ui, graph_interaction_scope(&doc, gid));
                assert!(ui
                    .data(|data| data.get_temp::<Interaction>(interaction_id()))
                    .is_some());
            });
        });
        let _ = ctx.run(Default::default(), |_| {});
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                begin_graph_canvas_pass(ui, graph_interaction_scope(&doc, gid));
                assert!(ui
                    .data(|data| data.get_temp::<Interaction>(interaction_id()))
                    .is_none());
                ui.data_mut(|data| {
                    data.insert_temp(interaction_id(), input_wire(graph, blur, 0).unwrap())
                });
            });
        });
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                begin_graph_canvas_pass(ui, graph_interaction_scope(&doc, GraphId::new()));
                assert!(ui
                    .data(|data| data.get_temp::<Interaction>(interaction_id()))
                    .is_none());
            });
        });
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }

    #[test]
    fn interaction_drafts_are_scoped_to_document_identity() {
        let first = Document::new("First", 32.0, 32.0);
        let second = Document::new("Second", 32.0, 32.0);
        let graph = GraphId::new();
        let node = GraphNodeId::new();
        assert_ne!(
            graph_interaction_scope(&first, graph),
            graph_interaction_scope(&second, graph)
        );
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert_ne!(
                    node_number_draft_id(ui, &first, node, "params.opacity"),
                    node_number_draft_id(ui, &second, node, "params.opacity")
                );
            });
        });
    }

    #[test]
    fn escape_cancels_the_deferred_input_wire_before_selection_or_canvas() {
        let (doc, gid, _, blur) = wire_fixture();
        let graph = &doc.timeline.as_ref().unwrap().graphs[&gid];
        let before = serde_json::to_value(&doc).unwrap();
        let ctx = egui::Context::default();
        let mut selected = Some(blur);
        let mut active = true;
        let key = || egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key()],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.data_mut(|data| {
                        data.insert_temp(interaction_id(), input_wire(graph, blur, 0).unwrap())
                    });
                    handle_canvas_escape(ui, false, &mut selected, &mut active);
                    assert!(ui
                        .data(|data| data.get_temp::<Interaction>(interaction_id()))
                        .is_some());
                    handle_canvas_escape(ui, true, &mut selected, &mut active);
                    assert!(ui
                        .data(|data| data.get_temp::<Interaction>(interaction_id()))
                        .is_none());
                });
            },
        );
        assert_eq!(selected, Some(blur));
        assert!(active);
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key()],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    handle_canvas_escape(ui, true, &mut selected, &mut active)
                });
            },
        );
        assert!(selected.is_none());
        assert!(active);
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key()],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    handle_canvas_escape(ui, true, &mut selected, &mut active)
                });
            },
        );
        assert!(!active);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }

    #[test]
    fn esc_ladder_unwinds_in_order_without_history_entries() {
        // One Esc press = one rung, in priority order: wire → selection → close.
        assert_eq!(esc_level(true, true), EscLevel::CancelWire);
        assert_eq!(esc_level(true, false), EscLevel::CancelWire);
        assert_eq!(esc_level(false, true), EscLevel::ClearSelection);
        assert_eq!(esc_level(false, false), EscLevel::CloseCanvas);

        // The ladder is pure state / UI manipulation — none of its effects commit
        // a command, so the undo revision is untouched across the whole
        // selection→canvas-closed progression (41 §8 item 5).
        let history = CommandHistory::default();
        let rev = history.revision();
        for (wire, sel) in [(true, true), (false, true), (false, false)] {
            let _ = esc_level(wire, sel);
        }
        assert_eq!(
            history.revision(),
            rev,
            "Esc cancel ladder must not add a history entry"
        );
    }

    #[test]
    fn g2s_s2g_roundtrip() {
        let origin = Pos2::new(100.0, 50.0);
        let pan = Vec2::new(-30.0, 12.0);
        let zoom = 1.4;
        for g in [
            Pos2::new(0.0, 0.0),
            Pos2::new(240.0, -80.0),
            Pos2::new(17.5, 999.0),
        ] {
            let s = g2s(g, origin, pan, zoom);
            let back = s2g(s, origin, pan, zoom);
            assert!(approx(g, back), "roundtrip failed for {g:?} -> {back:?}");
        }
    }

    #[test]
    fn zoom_of_one_is_offset_only() {
        let origin = Pos2::new(10.0, 10.0);
        let pan = Vec2::new(5.0, 7.0);
        let g = Pos2::new(3.0, 4.0);
        assert!(approx(g2s(g, origin, pan, 1.0), Pos2::new(18.0, 21.0)));
    }

    #[test]
    fn displayed_port_counts_match_shared_graph_edit_validation() {
        for kind in NodeKind::ALL {
            let op = kind.make_op();
            let (inputs, outputs) = op_ports(&op);
            assert_eq!(
                (inputs.len() as u16, outputs.len() as u16),
                op.port_counts(),
                "{op:?}"
            );
        }
        assert_eq!(GraphOp::Output.port_counts(), (1, 0));
    }

    #[test]
    fn merge_has_two_inputs_one_output() {
        let (ins, outs) = op_ports(&GraphOp::Merge {
            mode: BlendMode::Normal,
        });
        assert_eq!(ins.len(), 2);
        assert_eq!(outs.len(), 1);
        assert_eq!(ins[0].label, "a");
        assert_eq!(ins[1].label, "b");
    }

    #[test]
    fn channel_split_outputs_are_masks() {
        let (ins, outs) = op_ports(&GraphOp::ChannelSplit);
        assert_eq!(ins.len(), 1);
        assert_eq!(outs.len(), 4);
        assert!(outs.iter().all(|p| p.ty == PortType::Mask));
        assert_eq!(ins[0].ty, PortType::Image);
    }

    #[test]
    fn output_and_source_arities() {
        assert_eq!(op_ports(&GraphOp::Output).1.len(), 0);
        assert_eq!(op_ports(&GraphOp::Output).0.len(), 1);
        assert_eq!(op_ports(&GraphOp::ClipIn).0.len(), 0);
        assert_eq!(op_ports(&GraphOp::SolidColor).1.len(), 1);
        assert_eq!(
            op_ports(&GraphOp::Note {
                text: String::new()
            })
            .0
            .len(),
            0
        );
        assert_eq!(
            op_ports(&GraphOp::Note {
                text: String::new()
            })
            .1
            .len(),
            0
        );
    }

    #[test]
    fn input_ports_sit_left_outputs_right() {
        let op = GraphOp::Blur;
        let pos = NodePos { x: 100.0, y: 200.0 };
        let inp = port_center_graph(pos, &op, true, 0);
        let outp = port_center_graph(pos, &op, false, 0);
        assert!((inp.x - 100.0).abs() < 1e-3, "input on left edge");
        assert!(
            (outp.x - (100.0 + NODE_W)).abs() < 1e-3,
            "output on right edge"
        );
        assert!(inp.y > pos.y + HEADER_H, "ports sit below the header");
    }

    #[test]
    fn hit_port_finds_nearest_socket() {
        let (mut graph, clip_in) = photonic_core::timeline::NodeGraph::new_clip_composition("t");
        // Position is seeded by new_clip_composition; grab the ClipIn output.
        let pos = graph.ui[&clip_in];
        let out_center = port_center_graph(pos, &GraphOp::ClipIn, false, 0);
        let origin = Pos2::ZERO;
        let pan = Vec2::ZERO;
        let hit = hit_port(
            &graph,
            origin,
            pan,
            1.0,
            Pos2::new(out_center.x, out_center.y),
        );
        assert!(hit.is_some());
        let h = hit.unwrap();
        assert_eq!(h.node, clip_in);
        assert!(!h.is_input);
        // A point far from any socket misses.
        assert!(hit_port(&graph, origin, pan, 1.0, Pos2::new(9000.0, 9000.0)).is_none());
        // Keep `graph` mutable-typed usage honest.
        graph.name.clear();
    }

    #[test]
    fn palette_families_partition_every_kind() {
        // Every catalog kind belongs to exactly one family in ORDER.
        for k in NodeKind::ALL {
            let fam = k.family();
            assert!(Family::ORDER.contains(&fam));
        }
        // ClipIn is filtered out of the project graph.
        assert!(!NodeKind::ClipIn.allowed(true));
        assert!(NodeKind::ClipIn.allowed(false));
        assert!(NodeKind::Merge.allowed(true));
    }

    #[test]
    fn search_matches_label_and_family() {
        assert!(NodeKind::ChromaKey.matches_query("chroma"));
        assert!(NodeKind::ChromaKey.matches_query("key")); // family "Keys"
        assert!(NodeKind::Blur.matches_query("")); // empty matches all
        assert!(!NodeKind::Blur.matches_query("zzz"));
    }

    #[test]
    fn effect_params_come_from_registry() {
        let glow = op_params(&GraphOp::Glow);
        assert!(glow.iter().any(|d| d.path == "params.radius"));
        assert!(glow.iter().any(|d| d.path == "params.intensity"));
        assert!(glow.iter().any(|d| matches!(d.kind, PropValueKind::Color))); // tint
                                                                              // Merge exposes an editable opacity even though it's not in the registry.
        let merge = op_params(&GraphOp::Merge {
            mode: BlendMode::Normal,
        });
        assert_eq!(merge.len(), 1);
        assert_eq!(merge[0].path, "params.opacity");
    }

    fn number_frame(
        context: &egui::Context,
        desc: &ParamDesc,
        events: Vec<egui::Event>,
        stored: &mut f64,
    ) -> (Rect, bool) {
        let mut rect = Rect::NOTHING;
        let mut edit = None;
        let original = *stored;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 200.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (response, value) = node_number_control(
                        ui,
                        Id::new("test_node_number"),
                        original,
                        |ui, current| draw_float_param(ui, current, desc),
                    );
                    rect = response.rect;
                    edit = value;
                });
            },
        );
        if let Some(value) = edit {
            *stored = value;
        }
        (rect, edit.is_some())
    }

    fn pointer_event(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn number_drag_commits_once_and_keeps_intermediate_values_out_of_document() {
        for (op, path, initial) in [
            (GraphOp::Transform2D, "transform.x", -240.0),
            (
                GraphOp::Merge {
                    mode: BlendMode::Normal,
                },
                "params.opacity",
                0.5,
            ),
        ] {
            let desc = op_params(&op).into_iter().find(|d| d.path == path).unwrap();
            let ctx = egui::Context::default();
            let mut stored = initial;
            let (rect, _) = number_frame(&ctx, &desc, vec![], &mut stored);
            let start = if desc.range.is_some() {
                Pos2::new(rect.left() + 30.0, rect.center().y)
            } else {
                rect.center()
            };
            let finish = start + Vec2::new(25.0, 0.0);
            let (_, edited) = number_frame(
                &ctx,
                &desc,
                vec![egui::Event::PointerMoved(start), pointer_event(start, true)],
                &mut stored,
            );
            assert!(!edited);
            let (_, edited) = number_frame(
                &ctx,
                &desc,
                vec![egui::Event::PointerMoved(finish)],
                &mut stored,
            );
            assert!(!edited);
            assert_eq!(stored, initial);
            let (_, edited) =
                number_frame(&ctx, &desc, vec![pointer_event(finish, false)], &mut stored);
            assert!(edited, "{path}: drag must commit on release");
            assert_ne!(stored, initial);
            let committed = stored;
            assert!(!number_frame(&ctx, &desc, vec![], &mut stored).1);
            assert_eq!(stored, committed);
        }
    }

    #[test]
    fn number_drag_escape_or_concurrent_value_change_cancels_the_pending_edit() {
        for escape in [true, false] {
            let desc = op_params(&GraphOp::Transform2D)
                .into_iter()
                .find(|d| d.path == "transform.x")
                .unwrap();
            let ctx = egui::Context::default();
            let mut stored = -240.0;
            let (rect, _) = number_frame(&ctx, &desc, vec![], &mut stored);
            let start = rect.center();
            let end = start + Vec2::new(30.0, 0.0);
            number_frame(
                &ctx,
                &desc,
                vec![egui::Event::PointerMoved(start), pointer_event(start, true)],
                &mut stored,
            );
            number_frame(
                &ctx,
                &desc,
                vec![egui::Event::PointerMoved(end)],
                &mut stored,
            );
            let events = if escape {
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }]
            } else {
                stored = 777.0;
                vec![]
            };
            assert!(!number_frame(&ctx, &desc, events, &mut stored).1);
            assert!(!number_frame(&ctx, &desc, vec![pointer_event(end, false)], &mut stored).1);
            assert_eq!(stored, if escape { -240.0 } else { 777.0 });
        }
    }

    #[test]
    fn node_number_text_entry_commits_once_or_cancels_with_escape() {
        for (op, path, initial, text, expected) in [
            (GraphOp::Transform2D, "transform.x", -240.0, "-120", -120.0),
            (
                GraphOp::Merge {
                    mode: BlendMode::Normal,
                },
                "params.opacity",
                0.5,
                "0.75",
                0.75,
            ),
        ] {
            for cancel in [false, true] {
                let desc = op_params(&op).into_iter().find(|d| d.path == path).unwrap();
                let ctx = egui::Context::default();
                let mut stored = initial;
                let (rect, _) = number_frame(&ctx, &desc, vec![], &mut stored);
                let click = if desc.range.is_some() {
                    Pos2::new(rect.right() - 10.0, rect.center().y)
                } else {
                    rect.center()
                };
                number_frame(
                    &ctx,
                    &desc,
                    vec![egui::Event::PointerMoved(click), pointer_event(click, true)],
                    &mut stored,
                );
                number_frame(&ctx, &desc, vec![pointer_event(click, false)], &mut stored);
                assert!(
                    !number_frame(
                        &ctx,
                        &desc,
                        vec![egui::Event::Text(text.to_string())],
                        &mut stored
                    )
                    .1
                );
                assert_eq!(stored, initial, "typing stays local until confirmation");
                let key = if cancel {
                    egui::Key::Escape
                } else {
                    egui::Key::Enter
                };
                let (_, edited) = number_frame(
                    &ctx,
                    &desc,
                    vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                    &mut stored,
                );
                assert_eq!(edited, !cancel, "{path}, cancel={cancel}");
                assert_eq!(stored, if cancel { initial } else { expected });
                assert!(!number_frame(&ctx, &desc, vec![], &mut stored).1);
            }
        }
    }

    #[test]
    fn hidden_number_control_discards_an_interrupted_drag() {
        let desc = op_params(&GraphOp::Transform2D)
            .into_iter()
            .find(|d| d.path == "transform.x")
            .unwrap();
        let ctx = egui::Context::default();
        let mut stored = -240.0;
        let (rect, _) = number_frame(&ctx, &desc, vec![], &mut stored);
        let start = rect.center();
        let end = start + Vec2::new(30.0, 0.0);
        number_frame(
            &ctx,
            &desc,
            vec![egui::Event::PointerMoved(start), pointer_event(start, true)],
            &mut stored,
        );
        number_frame(
            &ctx,
            &desc,
            vec![egui::Event::PointerMoved(end)],
            &mut stored,
        );
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        assert!(!number_frame(&ctx, &desc, vec![pointer_event(end, false)], &mut stored).1);
        assert_eq!(stored, -240.0);
    }

    #[test]
    fn merely_drawing_node_numbers_does_not_clamp_or_emit_edits() {
        for (op, path, initial) in [
            (GraphOp::Transform2D, "transform.x", -240.0),
            (GraphOp::Transform2D, "transform.y", 500.0),
            (
                GraphOp::Transform2D,
                "transform.rotation",
                std::f64::consts::PI,
            ),
            (
                GraphOp::Merge {
                    mode: BlendMode::Normal,
                },
                "params.opacity",
                1.25,
            ),
            (
                GraphOp::Merge {
                    mode: BlendMode::Normal,
                },
                "params.opacity",
                -0.25,
            ),
        ] {
            let desc = op_params(&op).into_iter().find(|d| d.path == path).unwrap();
            let context = egui::Context::default();
            let mut value = initial;
            let mut changed = false;
            let _ = context.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = draw_float_param(ui, &mut value, &desc);
                    changed = commit_worthy(&response);
                });
            });
            assert_eq!(value, initial, "viewing {path} changed the stored value");
            assert!(!changed, "viewing {path} emitted an edit");
        }
    }

    #[test]
    fn node_drag_widgets_preserve_loaded_values_until_an_edit() {
        assert_eq!(pretty_path("transform.rotation"), "Rotation (rad)");
        for (op, path, initial) in [
            (GraphOp::Switch, "params.selected", 7.0),
            (
                GraphOp::Merge {
                    mode: BlendMode::Normal,
                },
                "params.opacity",
                1.25,
            ),
            (GraphOp::Transform2D, "transform.x", -240.0),
        ] {
            let desc = op_params(&op).into_iter().find(|d| d.path == path).unwrap();
            let context = egui::Context::default();
            let mut value = initial;
            let mut changed = false;
            let _ = context.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    changed = commit_worthy(&ui.add(node_drag_value(&mut value, &desc)));
                });
            });
            assert_eq!(value, initial);
            assert!(!changed);
        }
    }

    #[test]
    fn unstored_node_controls_match_engine_defaults_without_edits() {
        use photonic_core::timeline::{EffectParams, FrameRate, Sequence, TimelineProject};
        let mut doc = Document::new("Defaults", 32.0, 32.0);
        let mut project = TimelineProject::new();
        let sequence = Sequence::new("Vertical", FrameRate::FPS_30, 1080, 1920);
        let id = sequence.id;
        project.insert_sequence(sequence);
        project.active_sequence = Some(id);
        doc.timeline = Some(project);
        let before = serde_json::to_value(&doc).unwrap();
        for (op, path, expected) in [
            (
                GraphOp::Merge {
                    mode: BlendMode::Normal,
                },
                "params.opacity",
                PropValue::Float(1.0),
            ),
            (
                GraphOp::SolidColor,
                "params.color",
                PropValue::Color(photonic_core::Color::BLACK),
            ),
            (
                GraphOp::Transform2D,
                "transform.scale_x",
                PropValue::Float(1.0),
            ),
            (
                GraphOp::Transform2D,
                "transform.scale_y",
                PropValue::Float(1.0),
            ),
            (
                GraphOp::Resize {
                    fit: FitMode::Contain,
                },
                "params.width",
                PropValue::Float(1080.0),
            ),
            (
                GraphOp::Resize {
                    fit: FitMode::Contain,
                },
                "params.height",
                PropValue::Float(1920.0),
            ),
            (GraphOp::Crop, "params.left", PropValue::Float(0.0)),
            (GraphOp::Switch, "params.selected", PropValue::Enum(0)),
        ] {
            let node = GraphNode::new(op);
            let param = op_params(&node.op)
                .into_iter()
                .find(|d| d.path == path)
                .unwrap();
            assert_eq!(node_param_value(&doc, &node, &param), expected, "{path}");
            assert!(node.params.base.0.entries.is_empty());
        }
        assert!(!op_params(&GraphOp::Transform2D).iter().any(|d| d.path == "transform.opacity"), "Transform graph nodes currently lower only their affine, so do not advertise inert opacity");
        for (op, kind) in [
            (GraphOp::Blur, EffectKind::Blur),
            (GraphOp::Sharpen, EffectKind::Sharpen),
            (GraphOp::Glow, EffectKind::Glow),
            (GraphOp::ChromaKey, EffectKind::ChromaKey),
            (GraphOp::LumaKey, EffectKind::LumaKey),
            (
                GraphOp::MaskShape {
                    shape: MaskShapeKind::Ellipse,
                },
                EffectKind::MaskShapeGen,
            ),
        ] {
            let node = GraphNode::new(op);
            let expected = EffectParams::seed(PropTargetKind::Effect(kind));
            for param in op_params(&node.op) {
                assert_eq!(
                    Some(node_param_value(&doc, &node, &param)),
                    expected.get(param.path).copied(),
                    "{:?}/{}",
                    node.op,
                    param.path
                );
            }
            assert!(node.params.base.0.entries.is_empty());
        }
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }

    #[test]
    fn stored_node_zero_values_are_not_replaced_by_implicit_defaults() {
        let doc = Document::new("Stored defaults", 32.0, 32.0);
        let mut node = GraphNode::new(GraphOp::Merge {
            mode: BlendMode::Normal,
        });
        let param = op_params(&node.op)[0];
        node.params.base.0.set(param.path, PropValue::Float(0.0));
        let before = node.clone();
        assert_eq!(node_param_value(&doc, &node, &param), PropValue::Float(0.0));
        assert_eq!(node, before);
    }

    #[test]
    fn primary_inline_is_a_real_param() {
        let d = primary_inline_param(&GraphOp::Blur).expect("blur has an inline param");
        let all = op_params(&GraphOp::Blur);
        assert!(all.iter().any(|x| x.path == d.path));
        assert!(primary_inline_param(&GraphOp::Output).is_none());
    }

    #[test]
    fn node_body_grows_with_port_count() {
        let small = node_body_size(&GraphOp::Output).y; // 1 port
        let big = node_body_size(&GraphOp::ChannelCombine).y; // 4 inputs
        assert!(big > small);
    }

    #[test]
    fn added_graph_id_extracts_from_batch() {
        let cmds =
            ops::set_project_graph(&photonic_core::timeline::TimelineProject::default(), None);
        let id = added_graph_id(&cmds);
        assert!(id.is_some());
    }
}
