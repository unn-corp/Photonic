//! Node graphs (01 §8): one arena for all user graphs (per-clip compositions
//! and the project graph).
//!
//! Data-model graphs are *descriptions*: the [`GraphOp`] catalog carries
//! structural/reference data (asset ids, shape kind, node arity) and animatable
//! knobs live in [`GraphNode::params`]; evaluation semantics, port types, and
//! caching live in the engine (02/08). This module owns the arena shape and the
//! cycle-check invariant (edge insertion must fail on a cycle, never panic).

use super::anim::{AnimProps, PropSet};
use super::effect_kind::EffectParams;
use super::grade::Grade;
use super::ids::{AssetId, GraphId, GraphNodeId};
use super::media::VectorRef;
use super::prop_registry::PropTargetKind;
use super::time::Tick;
use crate::layer::BlendMode;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// An output port index on a node. `0` is the primary output; multi-output ops
/// (`ChannelSplit`) use `1..` per their catalog row (08 §2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutPort(pub u16);

/// An input port index on a node. `0` is the primary input; multi-input ops
/// (`Merge` a/b, `ChannelCombine` r/g/b/a) use `1..`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InPort(pub u16);

impl OutPort {
    pub const PRIMARY: OutPort = OutPort(0);
}
impl InPort {
    pub const PRIMARY: InPort = InPort(0);
    /// `Merge` top input.
    pub const A: InPort = InPort(0);
    /// `Merge` bottom input.
    pub const B: InPort = InPort(1);
}

/// A directed edge feeding one node's output into another's input.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: (GraphNodeId, OutPort),
    pub to: (GraphNodeId, InPort),
}

/// Editor position of a node on the canvas (persisted; undoable per 08 §8).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodePos {
    pub x: f32,
    pub y: f32,
}

/// Where a standalone `MediaIn` samples its source time (08 §2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeSource {
    /// Advance with the host clip's local time.
    Local,
    /// Sample at the sequence tick directly (default).
    #[default]
    Sequence,
}

/// Resize fit mode (08 §2 `Resize`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    Stretch,
    Contain,
    Cover,
}

/// Mask-shape generator shape (08 §2 `MaskShape`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum MaskShapeKind {
    Ellipse,
    Rect,
    /// Polygon vertices are static in v1 (normalized coords).
    Polygon {
        vertices: Vec<[f32; 2]>,
    },
}

/// Static styled-text payload for a `Text` generator node (08 §2). A subset of
/// `CaptionStyle` fields; the string may be keyframed via params in a later phase
/// but is carried structurally here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextGen {
    pub text: String,
    pub font_family: String,
    pub font_size: f32,
    pub fill: crate::Color,
    pub position: [f32; 2],
}

impl Default for TextGen {
    fn default() -> Self {
        TextGen {
            text: String::new(),
            font_family: "sans-serif".to_string(),
            font_size: 48.0,
            fill: crate::Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            position: [0.5, 0.5],
        }
    }
}

/// The v1 node catalog (08 §2), as data only. Animatable knobs (opacity, blur
/// radius, blend opacity, etc.) live in [`GraphNode::params`]; each variant here
/// carries only the structural/reference data the compiler needs to lower it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GraphOp {
    /// Terminal; exactly one per graph.
    Output,
    /// The clip's source after trim/speed — legal only in a per-clip composition.
    ClipIn,
    MediaIn {
        asset: AssetId,
        #[serde(default)]
        time_source: TimeSource,
    },
    VectorIn {
        vref: VectorRef,
    },
    /// `color` is animatable (in params); no structural payload.
    SolidColor,
    /// `mode`/`opacity` are animatable (in params). Binary only (08 §3.2).
    Merge {
        #[serde(default)]
        mode: BlendMode,
    },
    Transform2D,
    Crop,
    Resize {
        fit: FitMode,
    },
    Blur,
    Sharpen,
    Glow,
    ChromaKey,
    LumaKey,
    MaskShape {
        shape: MaskShapeKind,
    },
    MaskFromMatte,
    Invert,
    ChannelSplit,
    ChannelCombine,
    /// Embeds a full grade stack (07); same type `Clip.grade` uses.
    Grade {
        grade: Grade,
    },
    Lut {
        asset: AssetId,
    },
    Text {
        text: TextGen,
    },
    /// v1 recommends treating `offset` as a per-instance constant.
    TimeOffset {
        offset: Tick,
    },
    /// `selected` is animatable (in params); resolved at compile time.
    Switch,
    /// Pure canvas annotation; no ports, never compiled.
    Note {
        text: String,
    },
    /// Forward-compat (39 §2.2): an op this build does not know. The whole
    /// object — `op` tag and payload — is retained verbatim and re-emitted
    /// unchanged. Lowers to passthrough of its primary input (an inert unary
    /// filter), never guessed. Declared last so serde tries known tags first.
    #[serde(untagged)]
    Unknown(serde_json::Map<String, serde_json::Value>),
}

impl GraphOp {
    /// True for ops with no input ports (sources / generators, 08 §3.3).
    ///
    /// `Unknown` falls through to `false` (no wildcard needed): it is treated
    /// as a unary filter, i.e. passthrough of its primary input — the correct
    /// inert default for an op whose arity this build cannot know (39 §2.2).
    pub fn is_source(&self) -> bool {
        matches!(
            self,
            GraphOp::ClipIn
                | GraphOp::MediaIn { .. }
                | GraphOp::VectorIn { .. }
                | GraphOp::SolidColor
                | GraphOp::MaskShape { .. }
                | GraphOp::Text { .. }
                | GraphOp::Note { .. }
        )
    }

    /// The preserved `op` tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(&self) -> Option<&str> {
        match self {
            GraphOp::Unknown(map) => map.get("op").and_then(|v| v.as_str()),
            _ => None,
        }
    }

    /// True if this is a forward-compat variant this build does not understand.
    pub fn is_unknown(&self) -> bool {
        matches!(self, GraphOp::Unknown(_))
    }
}

/// One node in a graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: GraphNodeId,
    pub op: GraphOp,
    pub params: AnimProps<GraphNodeParams>,
}

impl GraphNode {
    pub fn new(op: GraphOp) -> Self {
        GraphNode {
            id: GraphNodeId::new(),
            op,
            params: AnimProps::new(GraphNodeParams::default()),
        }
    }
}

/// Node params — an open `EffectParams` map (08 §6.4: node param surface is
/// open, validated leniently). A thin newtype so it can carry a distinct
/// `PropSet` target from `EffectParams`' generic bound while reusing its storage.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GraphNodeParams(pub EffectParams);

impl PropSet for GraphNodeParams {
    const TARGET_KIND: PropTargetKind = PropTargetKind::GraphNode;
}

/// One arena entry: a per-clip composition or the project graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeGraph {
    pub id: GraphId,
    pub name: String,
    pub nodes: HashMap<GraphNodeId, GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Exactly one `Output` node (01 §8).
    pub output: GraphNodeId,
    pub ui: HashMap<GraphNodeId, NodePos>,
}

impl NodeGraph {
    /// A fresh graph seeded `ClipIn → Output` (the fixed v1 composition seed,
    /// 08 §4). Returns the graph plus the `ClipIn` node id.
    pub fn new_clip_composition(name: impl Into<String>) -> (Self, GraphNodeId) {
        let clip_in = GraphNode::new(GraphOp::ClipIn);
        let output = GraphNode::new(GraphOp::Output);
        let clip_in_id = clip_in.id;
        let output_id = output.id;
        let mut nodes = HashMap::new();
        nodes.insert(clip_in_id, clip_in);
        nodes.insert(output_id, output);
        let mut ui = HashMap::new();
        ui.insert(clip_in_id, NodePos { x: 0.0, y: 0.0 });
        ui.insert(output_id, NodePos { x: 240.0, y: 0.0 });
        let graph = NodeGraph {
            id: GraphId::new(),
            name: name.into(),
            nodes,
            edges: vec![GraphEdge {
                from: (clip_in_id, OutPort::PRIMARY),
                to: (output_id, InPort::PRIMARY),
            }],
            output: output_id,
            ui,
        };
        (graph, clip_in_id)
    }

    /// An empty project graph seeded with just an `Output` node (no `ClipIn` —
    /// illegal in the project graph, 08 §5).
    pub fn new_project_graph(name: impl Into<String>) -> Self {
        let output = GraphNode::new(GraphOp::Output);
        let output_id = output.id;
        let mut nodes = HashMap::new();
        nodes.insert(output_id, output);
        let mut ui = HashMap::new();
        ui.insert(output_id, NodePos { x: 0.0, y: 0.0 });
        NodeGraph {
            id: GraphId::new(),
            name: name.into(),
            nodes,
            edges: Vec::new(),
            output: output_id,
            ui,
        }
    }

    /// Deep-clone this graph under a fresh [`GraphId`] with fresh node ids,
    /// remapping every edge and ui entry (08 §4 copy/paste-composition rule:
    /// paste must not alias two clips to one arena graph). The `output` pointer
    /// is remapped too.
    pub fn deep_clone_fresh_ids(&self) -> Self {
        let mut id_map: HashMap<GraphNodeId, GraphNodeId> = HashMap::new();
        let mut nodes = HashMap::new();
        for (old_id, node) in &self.nodes {
            let new_id = GraphNodeId::new();
            id_map.insert(*old_id, new_id);
            let mut cloned = node.clone();
            cloned.id = new_id;
            nodes.insert(new_id, cloned);
        }
        let map = |id: GraphNodeId| *id_map.get(&id).unwrap_or(&id);
        let edges = self
            .edges
            .iter()
            .map(|e| GraphEdge {
                from: (map(e.from.0), e.from.1),
                to: (map(e.to.0), e.to.1),
            })
            .collect();
        let ui = self.ui.iter().map(|(k, v)| (map(*k), *v)).collect();
        NodeGraph {
            id: GraphId::new(),
            name: self.name.clone(),
            nodes,
            edges,
            output: map(self.output),
            ui,
        }
    }

    /// True if adding an edge `from → to` would create a cycle — i.e. `from` is
    /// already reachable from `to` through existing edges (01 §8: edge insertion
    /// cycle-checks; the edit op fails, never panics).
    pub fn would_create_cycle(&self, from: GraphNodeId, to: GraphNodeId) -> bool {
        if from == to {
            return true;
        }
        // Can we reach `from` starting at `to`? If so, adding from→to closes a loop.
        let mut stack = vec![to];
        let mut seen = HashSet::new();
        while let Some(n) = stack.pop() {
            if n == from {
                return true;
            }
            if !seen.insert(n) {
                continue;
            }
            for e in &self.edges {
                if e.from.0 == n {
                    stack.push(e.to.0);
                }
            }
        }
        false
    }

    /// Whether the current edge set already contains a cycle (defensive check
    /// used by validation/tests; well-formed graphs never do).
    pub fn has_cycle(&self) -> bool {
        // Kahn's algorithm: a DAG fully reduces to zero remaining nodes.
        let mut indeg: HashMap<GraphNodeId, usize> = self.nodes.keys().map(|k| (*k, 0)).collect();
        for e in &self.edges {
            *indeg.entry(e.to.0).or_insert(0) += 1;
        }
        let mut queue: Vec<GraphNodeId> = indeg
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(k, _)| *k)
            .collect();
        let mut removed = 0usize;
        while let Some(n) = queue.pop() {
            removed += 1;
            for e in &self.edges {
                if e.from.0 == n {
                    if let Some(d) = indeg.get_mut(&e.to.0) {
                        *d -= 1;
                        if *d == 0 {
                            queue.push(e.to.0);
                        }
                    }
                }
            }
        }
        removed != self.nodes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_composition_seed_is_clipin_to_output() {
        let (g, clip_in) = NodeGraph::new_clip_composition("comp");
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].from.0, clip_in);
        assert_eq!(g.edges[0].to.0, g.output);
        assert!(!g.has_cycle());
    }

    #[test]
    fn cycle_check_refuses_back_edge() {
        let (mut g, clip_in) = NodeGraph::new_clip_composition("comp");
        // A → clip_in would cycle since clip_in → output already; but test the
        // direct case: output → clip_in closes the loop.
        assert!(g.would_create_cycle(g.output, clip_in));
        // A brand-new disconnected node cannot cycle.
        let blur = GraphNode::new(GraphOp::Blur);
        let blur_id = blur.id;
        g.nodes.insert(blur_id, blur);
        assert!(!g.would_create_cycle(clip_in, blur_id));
    }

    #[test]
    fn deep_clone_gives_fresh_ids_and_remaps_edges() {
        let (g, _) = NodeGraph::new_clip_composition("comp");
        let c = g.deep_clone_fresh_ids();
        assert_ne!(c.id, g.id);
        // No node id is shared.
        for k in c.nodes.keys() {
            assert!(!g.nodes.contains_key(k), "node id leaked into clone");
        }
        // Edge endpoints all resolve to nodes in the clone.
        for e in &c.edges {
            assert!(c.nodes.contains_key(&e.from.0));
            assert!(c.nodes.contains_key(&e.to.0));
        }
        assert!(c.nodes.contains_key(&c.output));
    }

    #[test]
    fn graph_roundtrip_serde() {
        let (g, _) = NodeGraph::new_clip_composition("comp");
        let j = serde_json::to_string(&g).unwrap();
        let back: NodeGraph = serde_json::from_str(&j).unwrap();
        assert_eq!(g, back);
    }
}
