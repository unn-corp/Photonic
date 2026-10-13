//! Color-grading data model (07 §1, normative).
//!
//! `Clip.grade: Option<Grade>` holds an ordered corrector stack — the Resolve
//! node-page mental model, not a flat filter list. Every op's params animate
//! through the same [`AnimProps`](super::anim::AnimProps) machinery as clip
//! transforms and effects. Grade math (linear working space) lives in the engine
//! (`photonic-video`); this module is pure data plus the ASC CDL XML interchange
//! functions (07 §6.2), which are pure string in/out and satisfy core's no-I/O
//! rule.

use super::anim::{AnimProps, PropSet};
use super::ids::{AssetId, GradeOpId, GraphId, GraphNodeId, SharedLookId};
use super::prop_registry::PropTargetKind;
use super::unknown::UnknownTag;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// An ordered grade stack applied to a clip (or embedded in a `GraphOp::Grade`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grade {
    /// Ordered; user-reorderable. Default seed order is defined in 07 §4.4.
    pub ops: Vec<GradeOp>,
    /// Global grade bypass (color-page toggle); `false` = active.
    #[serde(default)]
    pub bypass: bool,
    /// Optional image graph evaluated at this scope's grade stage. Corrector
    /// nodes refer to `ops`, which remain the single editable op store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<GradeGraph>,
}

/// Project-wide reusable look. The grade executes after each linked clip's own
/// grade and before group post-grade; changes propagate to every linked shot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SharedLook {
    pub id: SharedLookId,
    pub name: String,
    pub grade: Grade,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<SharedLookVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_version: Option<uuid::Uuid>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SharedLookVersion {
    pub id: uuid::Uuid,
    pub name: String,
    pub grade: Grade,
}
impl SharedLook {
    pub fn sync_active_version(&mut self) {
        if let Some(active) = self.active_version {
            if let Some(version) = self.versions.iter_mut().find(|v| v.id == active) {
                version.grade = self.grade.clone();
            }
        }
    }
}

/// A clip's optional look stage. `Local` is the exact snapshot produced by
/// Make Independent, so detaching never shifts the look's render position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ClipLook {
    Shared(SharedLookId),
    Local(Box<Grade>),
}

/// Stage address within one selected clip's grading pipeline. Readback uses
/// this directly; writes must additionally pass the checked edit operations.
/// Shared stages must match that clip's current link; this is not an arbitrary
/// project-wide shared-look preview with an invented input picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GradeInspectionStage {
    #[default]
    Clip,
    LocalLook,
    GroupPre {
        id: super::GroupId,
    },
    GroupPost {
        id: super::GroupId,
    },
    SharedLook {
        id: SharedLookId,
    },
}

impl<'de> Deserialize<'de> for GradeInspectionStage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Serde unit variants accept ignored payload fields even with an enum's
        // deny_unknown_fields. Empty struct variants preserve the strict address
        // contract without changing the public unit-variant API.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Address {
            Clip {},
            LocalLook {},
            GroupPre { id: super::GroupId },
            GroupPost { id: super::GroupId },
            SharedLook { id: SharedLookId },
        }
        Ok(match Address::deserialize(deserializer)? {
            Address::Clip {} => Self::Clip,
            Address::LocalLook {} => Self::LocalLook,
            Address::GroupPre { id } => Self::GroupPre { id },
            Address::GroupPost { id } => Self::GroupPost { id },
            Address::SharedLook { id } => Self::SharedLook { id },
        })
    }
}

impl GradeInspectionStage {
    pub fn resolve_grade<'a>(
        self,
        project: &'a super::TimelineProject,
        clip: &'a super::Clip,
    ) -> Option<&'a Grade> {
        match self {
            Self::Clip => clip.grade.as_ref(),
            Self::LocalLook => match clip.look.as_ref()? {
                ClipLook::Local(grade) => Some(grade.as_ref()),
                _ => None,
            },
            Self::GroupPre { id } | Self::GroupPost { id } => {
                // A shared group has a different input for each member. Resolve
                // only the selected member's complete, well-formed ancestry.
                let mut owners = project.sequences.values().filter(|sequence| {
                    sequence
                        .tracks()
                        .any(|track| track.clips.iter().any(|c| c.id == clip.id))
                });
                let sequence = owners.next()?;
                if owners.next().is_some() {
                    return None;
                }
                let mut current = clip.group;
                let mut visited = std::collections::HashSet::new();
                let mut contains = false;
                while let Some(group) = current {
                    if !visited.insert(group) {
                        return None;
                    }
                    let node = sequence.groups.get(&group)?;
                    if !matches!(
                        node.kind,
                        super::GroupKind::Normal | super::GroupKind::AvLink
                    ) {
                        return None;
                    }
                    contains |= group == id;
                    current = node.parent;
                }
                if !contains {
                    return None;
                }
                let node = sequence.groups.get(&id)?;
                if matches!(self, Self::GroupPre { .. }) {
                    node.pre_grade.as_ref()
                } else {
                    node.post_grade.as_ref()
                }
            }
            Self::SharedLook { id } => match clip.look.as_ref()? {
                ClipLook::Shared(link) if *link == id => {
                    project.shared_looks.get(&id).map(|look| &look.grade)
                }
                _ => None,
            },
        }
    }
}

impl Grade {
    pub fn new() -> Self {
        Grade {
            ops: Vec::new(),
            bypass: false,
            graph: None,
        }
    }

    /// Enable serial graph evaluation for this existing stack. Idempotent so
    /// callers can expose conversion as one undoable whole-grade edit.
    pub fn convert_to_graph(&mut self) {
        if self.graph.is_none() {
            self.graph = Some(GradeGraph::from_stack(&self.ops));
        }
    }

    /// Insert a corrector at the graph output, either serially or as a new
    /// parallel branch over the current output. The operation is atomic.
    pub fn add_graph_corrector(&mut self, op: GradeOp, parallel: bool) -> Result<(), &'static str> {
        let mut graph = self.graph.clone().ok_or("grade has no graph")?;
        let mut ops = self.ops.clone();
        if ops.iter().any(|existing| existing.id == op.id) {
            return Err("grading graph corrector id is duplicated");
        }
        graph.append_corrector(op.id, parallel)?;
        ops.push(op);
        graph.validate(&ops)?;
        self.ops = ops;
        self.graph = Some(graph);
        Ok(())
    }

    /// Add a typed utility node. Image utilities become the grade output;
    /// matte utilities remain available for routing without changing pixels.
    /// An operator-only input request must not silently choose a graph branch.
    /// Qualifier key utilities reference an operator without being its image corrector.
    pub fn has_grade_input(&self, op: GradeOpId, node: Option<u32>) -> bool {
        match node {
            Some(node) => self.graph.as_ref().is_some_and(|graph| matches!(graph.nodes.get(&node), Some(GradeGraphNode::Corrector { op: candidate, .. } | GradeGraphNode::QualifierMatte { op: candidate, .. }) if *candidate == op)),
            None => self.has_unambiguous_corrector_input(op),
        }
    }

    /// Whether a raw matte output belongs to a valid, enabled graph and feeds
    /// its output. Disconnected alternatives are not rendered inspection taps.
    pub fn has_matte_output(&self, node: u32) -> bool {
        !self.bypass
            && self.graph.as_ref().is_some_and(|graph| {
                graph.validate(&self.ops).is_ok()
                    && graph
                        .nodes
                        .get(&node)
                        .is_some_and(|value| value.output_type() == GradeGraphPortType::Matte)
                    && graph
                        .reachable_nodes()
                        .is_ok_and(|reachable| reachable.contains(&node))
            })
    }

    /// Reachable native image-node output in the ordinary clip grade.
    pub fn has_image_output(&self, node: u32) -> bool {
        !self.bypass
            && self.graph.as_ref().is_some_and(|graph| {
                graph.validate(&self.ops).is_ok()
                    && graph
                        .nodes
                        .get(&node)
                        .is_some_and(|value| value.output_type() == GradeGraphPortType::Image)
                    && graph
                        .reachable_nodes()
                        .is_ok_and(|reachable| reachable.contains(&node))
            })
    }

    pub fn has_unambiguous_corrector_input(&self, op: GradeOpId) -> bool {
        self.graph.as_ref().is_none_or(|graph| {
            graph.nodes.values().filter(|node| matches!(node, GradeGraphNode::Corrector { op: candidate, .. } if *candidate == op)).count() == 1
        })
    }

    pub fn add_graph_utility(&mut self, node: GradeGraphNode) -> Result<u32, &'static str> {
        if !matches!(
            node,
            GradeGraphNode::WindowMatte { .. }
                | GradeGraphNode::BezierMatte { .. }
                | GradeGraphNode::AnimatedBezierMatte { .. }
                | GradeGraphNode::QualifierMatte { .. }
                | GradeGraphNode::KeyMixer { .. }
                | GradeGraphNode::MatteApply { .. }
                | GradeGraphNode::MatteRefine { .. }
        ) {
            return Err("expected a matte source, key mixer, matte refinement or matte apply node");
        }
        let mut graph = self.graph.clone().ok_or("grade has no graph")?;
        let after_max = graph
            .nodes
            .keys()
            .next_back()
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("grading graph has too many nodes")?;
        let id = graph.next_id.max(after_max);
        graph.next_id = id
            .checked_add(1)
            .ok_or("grading graph has too many nodes")?;
        let image = node.output_type() == GradeGraphPortType::Image;
        graph.nodes.insert(id, node);
        if image {
            graph
                .nodes
                .insert(graph.output, GradeGraphNode::Output { input: id });
        }
        graph.validate(&self.ops)?;
        self.graph = Some(graph);
        Ok(id)
    }

    /// Remove a graph node without leaving dangling ports. A corrector is
    /// replaced by its input; a mixer is replaced by its bottom branch. Nodes
    /// and operators that become unreachable are pruned together.
    pub fn remove_graph_node(&mut self, id: u32) -> Result<(), &'static str> {
        let mut graph = self.graph.clone().ok_or("grade has no graph")?;
        graph.validate(&self.ops)?;
        graph.remove_node(id)?;
        let used_ops: HashSet<_> = graph
            .nodes
            .values()
            .filter_map(|node| match node {
                GradeGraphNode::Corrector { op, .. }
                | GradeGraphNode::WindowMatte { op, .. }
                | GradeGraphNode::QualifierMatte { op, .. } => Some(*op),
                _ => None,
            })
            .collect();
        let ops: Vec<_> = self
            .ops
            .iter()
            .filter(|op| used_ops.contains(&op.id))
            .cloned()
            .collect();
        graph.validate(&ops)?;
        self.ops = ops;
        self.graph = Some(graph);
        Ok(())
    }
}

/// A small typed image graph for serial and parallel corrections. Node ids are
/// stable within the grade; the graph never changes the enclosing scope order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradeGraph {
    #[serde(deserialize_with = "deserialize_grade_graph_nodes")]
    pub nodes: BTreeMap<u32, GradeGraphNode>,
    pub output: u32,
    /// Allocation cursor; retained across deletions so stale node IDs are not
    /// accidentally reused. Older graphs default to zero and recover on add.
    #[serde(default)]
    pub next_id: u32,
}

// JSON object keys arrive as strings. Internally tagged timeline commands
// buffer nested maps in Serde Content, losing serde_json's integer-key adapter.
// Accept both representations so a GradeGraph survives embedded undo history.
fn deserialize_grade_graph_nodes<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<u32, GradeGraphNode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Key {
        Number(u32),
        Text(String),
    }
    struct Nodes;
    impl<'de> serde::de::Visitor<'de> for Nodes {
        type Value = BTreeMap<u32, GradeGraphNode>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a map of numeric grading node IDs")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut nodes = BTreeMap::new();
            while let Some((key, node)) = map.next_entry::<Key, GradeGraphNode>()? {
                let id = match key {
                    Key::Number(id) => id,
                    Key::Text(text) => text.parse::<u32>().map_err(serde::de::Error::custom)?,
                };
                if nodes.insert(id, node).is_some() {
                    return Err(serde::de::Error::custom("duplicate grading graph node ID"));
                }
            }
            Ok(nodes)
        }
    }
    deserializer.deserialize_map(Nodes)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GradeGraphNode {
    Input,
    Corrector {
        input: u32,
        op: GradeOpId,
        label: String,
    },
    LayerMixer {
        top: u32,
        bottom: u32,
        opacity: f32,
        label: String,
    },
    /// A qualifier key only; its CDL does not contribute to the matte.
    QualifierMatte {
        input: u32,
        op: GradeOpId,
        label: String,
    },
    /// Geometry-only window key. RGB parameters and source alpha do not affect it.
    WindowMatte {
        input: u32,
        op: GradeOpId,
        label: String,
    },
    /// Closed cubic geometry key, independent of source color and alpha.
    BezierMatte {
        input: u32,
        window: super::BezierWindow,
        label: String,
    },
    /// Fixed-topology keyframed path. Separate variant prevents old readers
    /// from silently ignoring animation on an otherwise static window.
    AnimatedBezierMatte {
        input: u32,
        track: super::BezierWindowTrack,
        label: String,
    },
    MatteRefine {
        input: u32,
        #[serde(default)]
        refinement: GradeMatteRefinement,
        label: String,
    },
    KeyMixer {
        top: u32,
        bottom: u32,
        mode: GradeKeyMixMode,
        label: String,
    },
    /// Apply corrected straight color using an unassociated matte weight.
    MatteApply {
        original: u32,
        corrected: u32,
        matte: u32,
        label: String,
    },
    Output {
        input: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeKeyMixMode {
    Union,
    Intersect,
    Subtract,
    Multiply,
}

/// Spatial key refinement. Grow and Gaussian sigma are fractions of the
/// logical frame's shorter dimension; denoise is a 3x3 processing-pixel median.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GradeMatteRefinement {
    pub denoise: bool,
    pub grow: f32,
    pub blur: f32,
    pub matte_levels: [f32; 2],
}

impl GradeMatteRefinement {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.grow.is_finite()
            || !(-0.02..=0.02).contains(&self.grow)
            || !self.blur.is_finite()
            || !(0.0..=0.02).contains(&self.blur)
            || self
                .matte_levels
                .iter()
                .any(|value| !value.is_finite() || !(0.0..1.0).contains(value))
            || self.matte_levels[0] + self.matte_levels[1] >= 1.0
        {
            return Err("matte refinement requires finite grow within ±2%, blur within 0..=2%, and valid clean thresholds");
        }
        Ok(())
    }

    pub fn is_neutral(&self) -> bool {
        !self.denoise && self.grow == 0.0 && self.blur == 0.0 && self.matte_levels == [0.0; 2]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradeGraphPortType {
    Image,
    Matte,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GradeGraphInputPort {
    pub node: u32,
    pub kind: GradeGraphPortType,
    pub label: &'static str,
}

impl GradeGraphNode {
    pub fn output_type(&self) -> GradeGraphPortType {
        match self {
            Self::WindowMatte { .. }
            | Self::BezierMatte { .. }
            | Self::AnimatedBezierMatte { .. }
            | Self::QualifierMatte { .. }
            | Self::KeyMixer { .. }
            | Self::MatteRefine { .. } => GradeGraphPortType::Matte,
            _ => GradeGraphPortType::Image,
        }
    }

    /// Fixed-size ports avoid per-node allocation during frame validation.
    pub fn input_ports(&self) -> [Option<GradeGraphInputPort>; 3] {
        use GradeGraphPortType::{Image, Matte};
        let port = |node, kind, label| Some(GradeGraphInputPort { node, kind, label });
        match self {
            Self::Input => [None; 3],
            Self::Corrector { input, .. }
            | Self::WindowMatte { input, .. }
            | Self::BezierMatte { input, .. }
            | Self::AnimatedBezierMatte { input, .. }
            | Self::QualifierMatte { input, .. }
            | Self::Output { input } => [port(*input, Image, "image"), None, None],
            Self::LayerMixer { top, bottom, .. } => [
                port(*top, Image, "top"),
                port(*bottom, Image, "bottom"),
                None,
            ],
            Self::MatteRefine { input, .. } => [port(*input, Matte, "matte"), None, None],
            Self::KeyMixer { top, bottom, .. } => [
                port(*top, Matte, "top"),
                port(*bottom, Matte, "bottom"),
                None,
            ],
            Self::MatteApply {
                original,
                corrected,
                matte,
                ..
            } => [
                port(*original, Image, "original"),
                port(*corrected, Image, "corrected"),
                port(*matte, Matte, "matte"),
            ],
        }
    }

    pub fn set_input(&mut self, port: &str, source: u32) -> Result<(), &'static str> {
        let slot = match self {
            Self::Corrector { input, .. }
            | Self::WindowMatte { input, .. }
            | Self::BezierMatte { input, .. }
            | Self::AnimatedBezierMatte { input, .. }
            | Self::QualifierMatte { input, .. }
            | Self::Output { input }
                if port == "image" =>
            {
                input
            }
            Self::MatteRefine { input, .. } if port == "matte" => input,
            Self::LayerMixer { top, .. } | Self::KeyMixer { top, .. } if port == "top" => top,
            Self::LayerMixer { bottom, .. } | Self::KeyMixer { bottom, .. } if port == "bottom" => {
                bottom
            }
            Self::MatteApply { original, .. } if port == "original" => original,
            Self::MatteApply { corrected, .. } if port == "corrected" => corrected,
            Self::MatteApply { matte, .. } if port == "matte" => matte,
            _ => return Err("grading graph input port does not exist"),
        };
        *slot = source;
        Ok(())
    }
}

impl GradeGraph {
    /// Convert an ordered stack to a serial graph without changing op ids,
    /// animation, masks, or order. A bypassed grade remains bypassed.
    pub fn from_stack(ops: &[GradeOp]) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(0, GradeGraphNode::Input);
        let mut previous = 0;
        for (index, op) in ops.iter().enumerate() {
            let id = index as u32 + 1;
            nodes.insert(
                id,
                GradeGraphNode::Corrector {
                    input: previous,
                    op: op.id,
                    label: String::new(),
                },
            );
            previous = id;
        }
        let output = previous + 1;
        nodes.insert(output, GradeGraphNode::Output { input: previous });
        Self {
            nodes,
            output,
            next_id: output.saturating_add(1),
        }
    }

    fn append_corrector(&mut self, op: GradeOpId, parallel: bool) -> Result<(), &'static str> {
        let upstream = match self.nodes.get(&self.output) {
            Some(GradeGraphNode::Output { input }) => *input,
            _ => return Err("grading graph has no output node"),
        };
        let after_max = self
            .nodes
            .keys()
            .next_back()
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("grading graph has too many nodes")?;
        let next = self.next_id.max(after_max);
        let input = if parallel {
            self.nodes
                .iter()
                .find_map(|(id, node)| matches!(node, GradeGraphNode::Input).then_some(*id))
                .ok_or("grading graph has no input node")?
        } else {
            upstream
        };
        let mixer_id = if parallel {
            Some(
                next.checked_add(1)
                    .ok_or("grading graph has too many nodes")?,
            )
        } else {
            None
        };
        self.nodes.insert(
            next,
            GradeGraphNode::Corrector {
                input,
                op,
                label: String::new(),
            },
        );
        let new_output = if let Some(mixer_id) = mixer_id {
            self.nodes.insert(
                mixer_id,
                GradeGraphNode::LayerMixer {
                    top: next,
                    bottom: upstream,
                    opacity: 0.5,
                    label: String::new(),
                },
            );
            mixer_id
        } else {
            next
        };
        self.nodes
            .insert(self.output, GradeGraphNode::Output { input: new_output });
        self.next_id = new_output.checked_add(1).unwrap_or(u32::MAX);
        Ok(())
    }

    fn remove_node(&mut self, id: u32) -> Result<(), &'static str> {
        let reachable_before = self.reachable_nodes()?;
        let replacement = match self.nodes.get(&id) {
            Some(
                GradeGraphNode::Corrector { input, .. } | GradeGraphNode::MatteRefine { input, .. },
            ) => *input,
            Some(
                GradeGraphNode::LayerMixer { bottom, .. } | GradeGraphNode::KeyMixer { bottom, .. },
            ) => *bottom,
            Some(GradeGraphNode::MatteApply { original, .. }) => *original,
            Some(
                GradeGraphNode::WindowMatte { .. }
                | GradeGraphNode::BezierMatte { .. }
                | GradeGraphNode::AnimatedBezierMatte { .. }
                | GradeGraphNode::QualifierMatte { .. },
            ) => {
                if self.nodes.values().any(|node| {
                    node.input_ports()
                        .into_iter()
                        .flatten()
                        .any(|port| port.node == id)
                }) {
                    return Err("disconnect the matte source before removing it");
                }
                0 // Unconnected source: no port will use this replacement.
            }
            Some(GradeGraphNode::Input | GradeGraphNode::Output { .. }) => {
                return Err("cannot remove grading graph input or output")
            }
            None => return Err("grading graph node does not exist"),
        };
        for node in self.nodes.values_mut() {
            for port in node.input_ports().into_iter().flatten() {
                if port.node == id {
                    node.set_input(port.label, replacement)?;
                }
            }
        }
        self.nodes.remove(&id);
        let reachable_after = self.reachable_nodes()?;
        // Keep nodes that were already disconnected before this edit: they may
        // be parked alternatives. Only prune the branch this removal orphaned.
        self.nodes.retain(|node_id, _| {
            !reachable_before.contains(node_id) || reachable_after.contains(node_id)
        });
        Ok(())
    }

    fn reachable_nodes(&self) -> Result<HashSet<u32>, &'static str> {
        let mut reachable = HashSet::new();
        let mut stack = vec![self.output];
        while let Some(current) = stack.pop() {
            if !reachable.insert(current) {
                continue;
            }
            let node = self
                .nodes
                .get(&current)
                .ok_or("grading graph references a missing node")?;
            stack.extend(
                node.input_ports()
                    .into_iter()
                    .flatten()
                    .map(|port| port.node),
            );
        }
        Ok(reachable)
    }

    /// Validate image/matte connections and cycles, including parked nodes.
    pub fn validate(&self, ops: &[GradeOp]) -> Result<(), &'static str> {
        let mut op_ids = HashSet::new();
        if ops.iter().any(|op| !op_ids.insert(op.id)) {
            return Err("grading graph corrector id is duplicated");
        }
        fn visit(
            graph: &GradeGraph,
            id: u32,
            ops: &[GradeOp],
            visiting: &mut HashSet<u32>,
            visited: &mut HashSet<u32>,
        ) -> Result<(), &'static str> {
            if visited.contains(&id) {
                return Ok(());
            }
            if !visiting.insert(id) {
                return Err("grading graph contains a cycle");
            }
            let node = graph
                .nodes
                .get(&id)
                .ok_or("grading graph references a missing node")?;
            match node {
                GradeGraphNode::Corrector { op, .. }
                | GradeGraphNode::WindowMatte { op, .. }
                | GradeGraphNode::QualifierMatte { op, .. } => {
                    let corrector = ops
                        .iter()
                        .find(|candidate| candidate.id == *op)
                        .ok_or("grading graph references a missing corrector")?;
                    if matches!(node, GradeGraphNode::QualifierMatte { .. })
                        && (corrector.kind != GradeOpKind::HslQualifier
                            || !matches!(corrector.params.base, GradeOpParams::HslQualifier { .. }))
                    {
                        return Err("matte source requires an HSL qualifier");
                    }
                }
                GradeGraphNode::MatteRefine { refinement, .. } => refinement.validate()?,
                GradeGraphNode::BezierMatte { window, .. } => window.validate()?,
                GradeGraphNode::AnimatedBezierMatte { track, .. } => track.validate()?,
                GradeGraphNode::LayerMixer { opacity, .. }
                    if !opacity.is_finite() || !(0.0..=1.0).contains(opacity) =>
                {
                    return Err("grading graph mixer opacity is out of range")
                }
                _ => {}
            }
            for port in node.input_ports().into_iter().flatten() {
                let source = graph
                    .nodes
                    .get(&port.node)
                    .ok_or("grading graph references a missing node")?;
                if source.output_type() != port.kind {
                    return Err(match port.kind {
                        GradeGraphPortType::Image => "image input cannot receive a matte",
                        GradeGraphPortType::Matte => "matte input cannot receive an image",
                    });
                }
                visit(graph, port.node, ops, visiting, visited)?;
            }
            visiting.remove(&id);
            visited.insert(id);
            Ok(())
        }
        if !matches!(
            self.nodes.get(&self.output),
            Some(GradeGraphNode::Output { .. })
        ) {
            return Err("grading graph has no output node");
        }
        let mut visited = HashSet::new();
        for id in self.nodes.keys().copied() {
            visit(self, id, ops, &mut HashSet::new(), &mut visited)?;
        }
        Ok(())
    }
}

impl Default for Grade {
    fn default() -> Self {
        Grade::new()
    }
}

#[cfg(test)]
mod graph_tests {
    use super::*;

    #[test]
    fn inspection_stage_json_is_explicit_strict_and_round_trips() {
        let id = SharedLookId::new();
        for stage in [
            GradeInspectionStage::Clip,
            GradeInspectionStage::LocalLook,
            GradeInspectionStage::GroupPre {
                id: super::super::GroupId::new(),
            },
            GradeInspectionStage::GroupPost {
                id: super::super::GroupId::new(),
            },
            GradeInspectionStage::SharedLook { id },
        ] {
            let json = serde_json::to_value(stage).unwrap();
            assert_eq!(
                serde_json::from_value::<GradeInspectionStage>(json).unwrap(),
                stage
            );
        }
        assert_eq!(GradeInspectionStage::default(), GradeInspectionStage::Clip);
        for json in [
            serde_json::json!({"kind":"shared_look"}),
            serde_json::json!({"kind":"shared_look","id":"bad"}),
            serde_json::json!({"kind":"local_look","id":id}),
            serde_json::json!({"kind":"clip","extra":true}),
            serde_json::json!({"kind":"group"}),
            serde_json::json!({"kind":"group_pre"}),
            serde_json::json!({"kind":"group_post","id":"bad"}),
            serde_json::json!({"kind":"group_pre","id":id,"extra":true}),
        ] {
            assert!(
                serde_json::from_value::<GradeInspectionStage>(json.clone()).is_err(),
                "{json}"
            );
        }
    }

    #[test]
    fn group_inspection_resolves_only_valid_member_ancestry() {
        use super::super::{
            Clip, ClipSource, FrameRate, GroupKind, GroupNode, Sequence, Tick, TimelineProject,
            Track, TrackKind,
        };
        let mut p = TimelineProject::new();
        let mut sequence = Sequence::new("members", FrameRate::FPS_30, 16, 16);
        let seq = sequence.id;
        let mut root = GroupNode::new(GroupKind::Normal);
        root.pre_grade = Some(Grade::new());
        root.post_grade = Some(Grade {
            bypass: true,
            ..Grade::new()
        });
        let root_id = root.id;
        let mut leaf = GroupNode::new(GroupKind::Normal);
        let leaf_id = leaf.id;
        leaf.parent = Some(root_id);
        let mut clip = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
        clip.group = Some(leaf_id);
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(clip);
        sequence.video_tracks.push(track);
        sequence.groups.insert(root_id, root);
        sequence.groups.insert(leaf_id, leaf);
        p.insert_sequence(sequence);
        let stage = GradeInspectionStage::GroupPre { id: root_id };
        let member = |p: &TimelineProject| p.sequences[&seq].video_tracks[0].clips[0].clone();
        assert!(!stage.resolve_grade(&p, &member(&p)).unwrap().bypass);
        assert!(
            GradeInspectionStage::GroupPost { id: root_id }
                .resolve_grade(&p, &member(&p))
                .unwrap()
                .bypass
        );
        assert!(GradeInspectionStage::GroupPre { id: leaf_id }
            .resolve_grade(&p, &member(&p))
            .is_none());
        let mut detached = p.clone();
        detached.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].group = None;
        assert!(stage.resolve_grade(&detached, &member(&detached)).is_none());
        let mut reparented = p.clone();
        reparented
            .sequences
            .get_mut(&seq)
            .unwrap()
            .groups
            .get_mut(&leaf_id)
            .unwrap()
            .parent = None;
        assert!(stage
            .resolve_grade(&reparented, &member(&reparented))
            .is_none());
        let mut cycle = p.clone();
        cycle
            .sequences
            .get_mut(&seq)
            .unwrap()
            .groups
            .get_mut(&root_id)
            .unwrap()
            .parent = Some(leaf_id);
        assert!(stage.resolve_grade(&cycle, &member(&cycle)).is_none());
        let mut missing = p.clone();
        missing
            .sequences
            .get_mut(&seq)
            .unwrap()
            .groups
            .get_mut(&root_id)
            .unwrap()
            .parent = Some(super::super::GroupId::new());
        assert!(stage.resolve_grade(&missing, &member(&missing)).is_none());
        let mut duplicate = p.clone();
        let mut other = Sequence::new("duplicate", FrameRate::FPS_30, 16, 16);
        other.video_tracks = duplicate.sequences[&seq].video_tracks.clone();
        duplicate.insert_sequence(other);
        assert!(stage
            .resolve_grade(&duplicate, &member(&duplicate))
            .is_none());
    }

    #[test]
    fn graph_node_key_decoder_rejects_invalid_and_duplicate_ids() {
        for nodes in [
            r#"{"-1":{"kind":"input"}}"#,
            r#"{"4294967296":{"kind":"input"}}"#,
            r#"{"x":{"kind":"input"}}"#,
            r#"{"0":{"kind":"input"},"00":{"kind":"input"}}"#,
        ] {
            let json = format!(r#"{{"nodes":{nodes},"output":0}}"#);
            assert!(serde_json::from_str::<GradeGraph>(&json).is_err(), "{json}");
        }
    }

    #[test]
    fn window_matte_graph_is_typed_atomic_serializable_and_retains_its_operator() {
        let mut grade = Grade::new();
        let op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 0.0 },
        );
        let op_id = op.id;
        grade.ops.push(op);
        grade.convert_to_graph();
        let key = grade
            .add_graph_utility(GradeGraphNode::WindowMatte {
                input: 0,
                op: op_id,
                label: "Window".into(),
            })
            .unwrap();
        let before = grade.clone();
        assert_eq!(
            grade.graph.as_ref().unwrap().nodes[&key].output_type(),
            GradeGraphPortType::Matte
        );
        assert!(grade
            .add_graph_utility(GradeGraphNode::WindowMatte {
                input: key,
                op: op_id,
                label: String::new()
            })
            .is_err());
        assert_eq!(grade, before);
        assert!(grade
            .add_graph_utility(GradeGraphNode::WindowMatte {
                input: 0,
                op: GradeOpId::new(),
                label: String::new()
            })
            .is_err());
        assert_eq!(grade, before);
        let mixed = grade
            .add_graph_utility(GradeGraphNode::KeyMixer {
                top: key,
                bottom: key,
                mode: GradeKeyMixMode::Union,
                label: String::new(),
            })
            .unwrap();
        assert!(grade.remove_graph_node(key).is_err());
        grade.remove_graph_node(1).unwrap();
        assert_eq!(
            grade.ops.len(),
            1,
            "removing the RGB corrector must retain the window's operator"
        );
        let apply = grade
            .add_graph_utility(GradeGraphNode::MatteApply {
                original: 0,
                corrected: 0,
                matte: mixed,
                label: String::new(),
            })
            .unwrap();
        assert!(grade.has_matte_output(key));
        let round: Grade = serde_json::from_str(&serde_json::to_string(&grade).unwrap()).unwrap();
        assert_eq!(round, grade);
        assert!(round.graph.unwrap().nodes.contains_key(&apply));
    }

    #[test]
    fn typed_matte_utilities_round_trip_and_reject_wrong_ports_atomically() {
        let mut grade = Grade::new();
        let exposure = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        let exposure_id = exposure.id;
        let qualifier = GradeOp::new(
            GradeOpKind::HslQualifier,
            GradeOpParams::HslQualifier {
                hue: [0.0, 1.0],
                sat: [0.0, 1.0],
                lum: [0.0, 1.0],
                softness: 0.0,
                correction: CdlParams::identity(),
                keys: vec![],
                matte_levels: [0.0, 0.0],
            },
        );
        let qualifier_id = qualifier.id;
        grade.ops.extend([exposure, qualifier]);
        grade.convert_to_graph();
        let before = grade.clone();
        assert!(grade
            .add_graph_utility(GradeGraphNode::QualifierMatte {
                input: 1,
                op: exposure_id,
                label: String::new()
            })
            .is_err());
        assert_eq!(grade, before);
        let matte = grade
            .add_graph_utility(GradeGraphNode::QualifierMatte {
                input: 1,
                op: qualifier_id,
                label: "Key".into(),
            })
            .unwrap();
        assert!(
            grade.has_unambiguous_corrector_input(qualifier_id),
            "a key-only utility is not a second image corrector"
        );
        let key = grade
            .add_graph_utility(GradeGraphNode::KeyMixer {
                top: matte,
                bottom: matte,
                mode: GradeKeyMixMode::Subtract,
                label: "Exclude".into(),
            })
            .unwrap();
        let apply = grade
            .add_graph_utility(GradeGraphNode::MatteApply {
                original: 1,
                corrected: 2,
                matte: key,
                label: "Apply".into(),
            })
            .unwrap();
        let graph = grade.graph.as_ref().unwrap();
        assert_eq!(graph.validate(&grade.ops), Ok(()));
        assert_eq!(graph.nodes[&matte].output_type(), GradeGraphPortType::Matte);
        assert_eq!(graph.nodes[&apply].output_type(), GradeGraphPortType::Image);
        let ports = graph.nodes[&apply].input_ports();
        assert_eq!(ports[2].unwrap().kind, GradeGraphPortType::Matte);
        assert_eq!(
            serde_json::from_value::<Grade>(serde_json::to_value(&grade).unwrap()).unwrap(),
            grade
        );
        let mut bad = graph.clone();
        bad.nodes
            .get_mut(&apply)
            .unwrap()
            .set_input("matte", 1)
            .unwrap();
        assert_eq!(
            bad.validate(&grade.ops),
            Err("matte input cannot receive an image")
        );
        let mut bad = graph.clone();
        bad.nodes
            .get_mut(&graph.output)
            .unwrap()
            .set_input("image", matte)
            .unwrap();
        assert_eq!(
            bad.validate(&grade.ops),
            Err("image input cannot receive a matte")
        );
        let mut bad = graph.clone();
        bad.nodes
            .get_mut(&matte)
            .unwrap()
            .set_input("image", apply)
            .unwrap();
        assert_eq!(
            bad.validate(&grade.ops),
            Err("grading graph contains a cycle")
        );
        let mut bad = graph.clone();
        bad.nodes.insert(
            999,
            GradeGraphNode::Corrector {
                input: 1000,
                op: exposure_id,
                label: String::new(),
            },
        );
        assert_eq!(
            bad.validate(&grade.ops),
            Err("grading graph references a missing node")
        );
        let before = grade.clone();
        assert!(grade.remove_graph_node(matte).is_err());
        assert_eq!(grade, before);
        grade.remove_graph_node(apply).unwrap();
        assert_eq!(grade.graph.as_ref().unwrap().validate(&grade.ops), Ok(()));
        assert!(matches!(
            grade.graph.as_ref().unwrap().nodes[&grade.graph.as_ref().unwrap().output],
            GradeGraphNode::Output { input: 1 }
        ));
    }

    #[test]
    fn matte_refinement_validation_is_atomic_and_removal_reconnects_consumers() {
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::HslQualifier,
            GradeOpParams::HslQualifier {
                hue: [0.0, 1.0],
                sat: [0.0, 1.0],
                lum: [0.0, 1.0],
                softness: 0.0,
                correction: CdlParams::identity(),
                keys: vec![],
                matte_levels: [0.0; 2],
            },
        ));
        let op = grade.ops[0].id;
        grade.convert_to_graph();
        let matte = grade
            .add_graph_utility(GradeGraphNode::QualifierMatte {
                input: 0,
                op,
                label: String::new(),
            })
            .unwrap();
        let before = grade.clone();
        for refinement in [
            GradeMatteRefinement {
                grow: 0.021,
                ..Default::default()
            },
            GradeMatteRefinement {
                matte_levels: [0.5; 2],
                ..Default::default()
            },
        ] {
            assert!(grade
                .add_graph_utility(GradeGraphNode::MatteRefine {
                    input: matte,
                    refinement,
                    label: String::new()
                })
                .is_err());
            assert_eq!(grade, before);
        }
        assert!(grade
            .add_graph_utility(GradeGraphNode::MatteRefine {
                input: 0,
                refinement: Default::default(),
                label: String::new()
            })
            .is_err());
        assert_eq!(grade, before);
        let refined = grade
            .add_graph_utility(GradeGraphNode::MatteRefine {
                input: matte,
                refinement: GradeMatteRefinement {
                    denoise: true,
                    grow: -0.01,
                    blur: 0.005,
                    matte_levels: [0.1, 0.2],
                },
                label: "Refine".into(),
            })
            .unwrap();
        let mix = grade
            .add_graph_utility(GradeGraphNode::KeyMixer {
                top: refined,
                bottom: matte,
                mode: GradeKeyMixMode::Multiply,
                label: String::new(),
            })
            .unwrap();
        assert_eq!(
            serde_json::from_value::<Grade>(serde_json::to_value(&grade).unwrap()).unwrap(),
            grade
        );
        grade.remove_graph_node(refined).unwrap();
        assert!(
            matches!(grade.graph.as_ref().unwrap().nodes[&mix], GradeGraphNode::KeyMixer { top, .. } if top == matte)
        );
        assert_eq!(grade.graph.as_ref().unwrap().validate(&grade.ops), Ok(()));
    }

    #[test]
    fn ordered_stack_converts_to_valid_serial_graph_and_round_trips() {
        let ops = vec![
            GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops: 1.0 },
            ),
            GradeOp::new(
                GradeOpKind::Contrast,
                GradeOpParams::Contrast {
                    pivot: 0.5,
                    amount: 1.2,
                },
            ),
        ];
        let graph = GradeGraph::from_stack(&ops);
        assert_eq!(graph.validate(&ops), Ok(()));
        let grade = Grade {
            ops,
            bypass: false,
            graph: Some(graph),
        };
        let json = serde_json::to_string(&grade).unwrap();
        assert_eq!(serde_json::from_str::<Grade>(&json).unwrap(), grade);
    }

    #[test]
    fn graph_rejects_missing_corrector_and_cycle() {
        let op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        let mut graph = GradeGraph::from_stack(std::slice::from_ref(&op));
        assert_eq!(
            graph.validate(&[]),
            Err("grading graph references a missing corrector")
        );
        graph.nodes.insert(
            1,
            GradeGraphNode::Corrector {
                input: 1,
                op: op.id,
                label: String::new(),
            },
        );
        assert_eq!(graph.validate(&[op]), Err("grading graph contains a cycle"));
    }

    #[test]
    fn graph_adds_serial_and_parallel_correctors_atomically() {
        let first = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        let second = GradeOp::new(
            GradeOpKind::Contrast,
            GradeOpParams::Contrast {
                pivot: 0.5,
                amount: 1.2,
            },
        );
        let third = GradeOp::new(
            GradeOpKind::LinearOffset,
            GradeOpParams::LinearOffset {
                rgb: [0.1, 0.0, 0.0],
            },
        );
        let mut grade = Grade {
            ops: vec![first],
            bypass: false,
            graph: None,
        };
        grade.convert_to_graph();
        grade.add_graph_corrector(second, false).unwrap();
        grade.add_graph_corrector(third.clone(), true).unwrap();
        let graph = grade.graph.as_ref().unwrap();
        assert_eq!(graph.validate(&grade.ops), Ok(()));
        let mixer = graph
            .nodes
            .values()
            .find_map(|node| match node {
                GradeGraphNode::LayerMixer {
                    top,
                    bottom,
                    opacity,
                    ..
                } => Some((*top, *bottom, *opacity)),
                _ => None,
            })
            .unwrap();
        assert_eq!(mixer.2, 0.5);
        assert!(
            matches!(graph.nodes[&mixer.0], GradeGraphNode::Corrector { op, .. } if op == third.id)
        );
        let before = grade.clone();
        assert!(grade.add_graph_corrector(third, false).is_err());
        assert_eq!(grade, before);
    }

    #[test]
    fn graph_removal_rewires_serial_nodes_and_prunes_parallel_branch() {
        let first = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        let second = GradeOp::new(
            GradeOpKind::Contrast,
            GradeOpParams::Contrast {
                pivot: 0.5,
                amount: 1.2,
            },
        );
        let branch = GradeOp::new(
            GradeOpKind::LinearOffset,
            GradeOpParams::LinearOffset {
                rgb: [0.1, 0.0, 0.0],
            },
        );
        let mut grade = Grade {
            ops: vec![first.clone(), second.clone()],
            bypass: false,
            graph: None,
        };
        grade.convert_to_graph();
        let second_node = grade
            .graph
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(id, node)| {
                matches!(node, GradeGraphNode::Corrector { op, .. } if *op == second.id)
                    .then_some(*id)
            })
            .unwrap();
        grade.remove_graph_node(second_node).unwrap();
        assert_eq!(
            grade.ops.iter().map(|op| op.id).collect::<Vec<_>>(),
            vec![first.id]
        );
        let previous_cursor = grade.graph.as_ref().unwrap().next_id;
        grade.add_graph_corrector(branch.clone(), true).unwrap();
        let graph = grade.graph.as_ref().unwrap();
        let mixer_id = graph
            .nodes
            .iter()
            .find_map(|(id, node)| matches!(node, GradeGraphNode::LayerMixer { .. }).then_some(*id))
            .unwrap();
        assert!(mixer_id >= previous_cursor);
        let parked = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: -0.5 },
        );
        grade.ops.push(parked.clone());
        grade.graph.as_mut().unwrap().nodes.insert(
            99,
            GradeGraphNode::Corrector {
                input: 0,
                op: parked.id,
                label: "Parked look".into(),
            },
        );
        grade.graph.as_mut().unwrap().next_id = 100;
        grade.remove_graph_node(mixer_id).unwrap();
        assert_eq!(
            grade.ops.iter().map(|op| op.id).collect::<Vec<_>>(),
            vec![first.id, parked.id]
        );
        assert!(grade.graph.as_ref().unwrap().nodes.contains_key(&99));
        assert!(grade
            .graph
            .as_ref()
            .unwrap()
            .nodes
            .values()
            .all(|node| !matches!(node, GradeGraphNode::LayerMixer { .. })));
        assert_eq!(grade.graph.as_ref().unwrap().validate(&grade.ops), Ok(()));
        let before = grade.clone();
        assert!(grade.remove_graph_node(0).is_err());
        assert_eq!(grade, before);
    }
}

/// Static spatial cleanup of a qualifier's combined key/window, before source
/// coverage is applied. Lengths are fractions of the shorter logical dimension;
/// denoise is a 3x3 processing-pixel median. Native Managed only.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GradeQualifierSpatial {
    pub denoise: bool,
    pub grow: f32,
    pub blur: f32,
}

impl GradeQualifierSpatial {
    pub fn refinement(self) -> GradeMatteRefinement {
        GradeMatteRefinement {
            denoise: self.denoise,
            grow: self.grow,
            blur: self.blur,
            matte_levels: [0.0; 2],
        }
    }
    pub fn validate(self) -> Result<(), &'static str> {
        self.refinement().validate()
    }
    pub fn is_neutral(self) -> bool {
        self.refinement().is_neutral()
    }
}

/// One corrector in the stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradeOp {
    pub id: GradeOpId,
    /// Per-op bypass.
    #[serde(default = "crate::timeline::grade::default_true")]
    pub enabled: bool,
    /// Discriminant, immutable after creation.
    pub kind: GradeOpKind,
    /// `base + PropertyTrack` keyframes (01 §6 convention). Operator scalars
    /// use `params.`; power-window scalars share these tracks under `mask.`.
    pub params: AnimProps<GradeOpParams>,
    /// Spatial/qualifier mask; `None` = full frame (§4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<GradeMask>,
    /// Optional static cleanup; absent/neutral retains the original evaluator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifier_spatial: Option<GradeQualifierSpatial>,
}

pub(crate) fn default_true() -> bool {
    true
}

impl GradeOp {
    /// Scalar animation base, including normalized power-window geometry.
    /// Window lanes share the corrector's existing tracks with a `mask.` prefix.
    pub fn scalar_property(&self, path: &str) -> Option<f32> {
        self.params
            .base
            .scalar_property(path)
            .or_else(|| self.mask.as_ref()?.scalar_property(path))
    }

    pub fn has_mask_animation(&self) -> bool {
        self.params.tracks.iter().any(|track| {
            !track.orphaned
                && !track.keyframes.is_empty()
                && self
                    .mask
                    .as_ref()
                    .and_then(|mask| mask.scalar_property(track.property.as_str()))
                    .is_some()
        })
    }

    /// Sample geometry in the same tick domain as this corrector's parameters.
    /// Missing/non-power masks retain dormant lanes; no mask is created implicitly.
    pub fn mask_at(&self, tick: super::Tick) -> Result<Option<GradeMask>, &'static str> {
        let mut mask = self.mask.clone();
        if !self.has_mask_animation() {
            return Ok(mask);
        }
        let Some(mask_value) = mask.as_mut() else {
            return Ok(mask);
        };
        for track in &self.params.tracks {
            if track.orphaned || track.keyframes.is_empty() {
                continue;
            }
            let Some(slot) = mask_value.scalar_property_mut(track.property.as_str()) else {
                continue;
            };
            let base = super::PropValue::Float(f64::from(*slot));
            let super::PropValue::Float(value) = super::anim::eval(track, &base, tick) else {
                return Err("window animation requires scalar Float values");
            };
            if !value.is_finite() || !(value as f32).is_finite() {
                return Err("window animation produced a nonfinite value");
            }
            if super::prop_registry::resolve(self.kind.target_kind(), track.property.as_str())
                .and_then(|entry| entry.range)
                .is_some_and(|(low, high)| !(low..=high).contains(&value))
            {
                return Err("window animation exceeded its registered scalar range");
            }
            *slot = value as f32;
        }
        if let GradeMask::PowerWindow {
            center,
            size,
            rotation,
            softness,
            ..
        } = mask_value
        {
            if center.iter().chain(size.iter()).any(|v| !v.is_finite())
                || size.iter().any(|v| *v <= 0.0)
                || !rotation.is_finite()
                || !softness.is_finite()
                || *softness < 0.0
            {
                return Err("animated window requires finite geometry, positive sizes and nonnegative feather");
            }
        }
        Ok(mask)
    }

    /// Construct an op with default params for its kind.
    pub fn new(kind: GradeOpKind, params: GradeOpParams) -> Self {
        GradeOp {
            id: GradeOpId::new(),
            enabled: true,
            kind,
            params: AnimProps::new(params),
            mask: None,
            qualifier_spatial: None,
        }
    }
}

/// Grade-op discriminant. Additive-only; never remove or renumber a variant
/// (07 §1). Serde uses snake_case tags matching `GradeOpParams`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GradeOpKind {
    Exposure,
    /// Add per-channel scene-linear light; a distinct versioned operator so
    /// existing CDL/wheel documents retain their prior arithmetic.
    LinearOffset,
    /// Independent red/green/blue printer-light points; 12 points = one stop.
    PrinterLights,
    /// Hue-preserving scene-linear highlight compression above a knee.
    HighlightRolloff,
    /// Linked scene-linear saturation with selective vibrance.
    SaturationVibrance,
    Contrast,
    WhiteBalance,
    /// Explicit approximate Planckian-white Bradford adaptation in Kelvin.
    WhiteBalanceCct,
    /// Linked scene-linear exposure selected by relative-luminance tonal ranges.
    TonalExposure,
    Cdl,
    Wheels,
    Curves,
    HslQualifier,
    Lut3d,
    /// Forward-compat (39 §2.2): a variant this build does not know. The
    /// original serialized tag is preserved verbatim and re-emitted on save.
    /// Declared last so serde tries the known snake_case tags first.
    #[serde(untagged)]
    Unknown(UnknownTag),
}

impl GradeOpKind {
    pub fn target_kind(self) -> PropTargetKind {
        PropTargetKind::GradeOp(self)
    }

    /// The preserved tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(self) -> Option<UnknownTag> {
        match self {
            GradeOpKind::Unknown(t) => Some(t),
            _ => None,
        }
    }

    /// True if this is a forward-compat variant this build does not understand.
    pub fn is_unknown(self) -> bool {
        matches!(self, GradeOpKind::Unknown(_))
    }
}

/// 3D-LUT interpolation quality (07 §3.8, §6.5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LutInterp {
    /// Shipped baseline.
    Trilinear,
    /// Behind a quality toggle.
    Tetrahedral,
}

/// ASC CDL slope/offset/power/sat, per-channel. Used both as the `Cdl` op's
/// params and inline as `HslQualifier.correction` (07 §1 — deliberately NOT a
/// boxed recursive `GradeOpParams`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CdlParams {
    pub slope: [f32; 3],
    pub offset: [f32; 3],
    pub power: [f32; 3],
    pub sat: f32,
}

impl CdlParams {
    /// The identity correction (no-op).
    pub fn identity() -> Self {
        CdlParams {
            slope: [1.0; 3],
            offset: [0.0; 3],
            power: [1.0; 3],
            sat: 1.0,
        }
    }
}

impl Default for CdlParams {
    fn default() -> Self {
        CdlParams::identity()
    }
}

/// Per-kind grade parameters. Animatable scalar fields use the surrounding
/// `AnimProps` (07 §1); sampled qualifier regions remain authored data. Forward-compat is
/// carried by the `#[serde(other)] Unknown` catch-all: a newer binary's op kind
/// loads inert and flagged, never dropped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GradeOpParams {
    Exposure {
        stops: f32,
    },
    LinearOffset {
        rgb: [f32; 3],
    },
    PrinterLights {
        points: [f32; 3],
    },
    HighlightRolloff {
        knee: f32,
        strength: f32,
    },
    SaturationVibrance {
        saturation: f32,
        vibrance: f32,
    },
    Contrast {
        pivot: f32,
        amount: f32,
    },
    WhiteBalance {
        temp: f32,
        tint: f32,
    },
    TonalExposure {
        stops: [f32; 3],
        boundaries: [f32; 2],
        softness: f32,
    },
    WhiteBalanceCct {
        source_kelvin: f32,
        target_kelvin: f32,
    },
    Cdl {
        slope: [f32; 3],
        offset: [f32; 3],
        power: [f32; 3],
        sat: f32,
    },
    Wheels {
        lift: [f32; 3],
        gamma: [f32; 3],
        gain: [f32; 3],
        sat: f32,
    },
    Curves {
        master: Vec<(f32, f32)>,
        red: Vec<(f32, f32)>,
        green: Vec<(f32, f32)>,
        blue: Vec<(f32, f32)>,
        hue_vs_hue: Vec<(f32, f32)>,
        hue_vs_sat: Vec<(f32, f32)>,
        /// Optional new curve families. Empty means disabled; omit on save so
        /// legacy grades retain their serialized shape and look.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        hue_vs_luma: Vec<(f32, f32)>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        luma_vs_sat: Vec<(f32, f32)>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sat_vs_sat: Vec<(f32, f32)>,
    },
    HslQualifier {
        hue: [f32; 2],
        sat: [f32; 2],
        lum: [f32; 2],
        softness: f32,
        correction: CdlParams,
        /// Additional disjoint inclusions and sampled exclusions. Empty in
        /// legacy projects, preserving their serialized shape and output.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<QualifierKey>,
        /// Low/high matte thresholds. Neutral values keep legacy output.
        #[serde(default, skip_serializing_if = "matte_levels_neutral")]
        matte_levels: [f32; 2],
    },
    Lut3d {
        asset: AssetId,
        intensity: f32,
        interp: LutInterp,
    },
    /// Forward-compat (39 §2.2): an op kind this build does not understand. The
    /// whole object — `kind` tag and payload — is retained verbatim and
    /// re-emitted unchanged (the old `#[serde(other)]` unit variant destroyed
    /// the payload and rewrote the tag). Loads inert + flagged in UI, never
    /// dropped. Declared last so serde tries the known tags first.
    #[serde(untagged)]
    Unknown(serde_json::Map<String, serde_json::Value>),
}

fn matte_levels_neutral(levels: &[f32; 2]) -> bool {
    *levels == [0.0, 0.0]
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualifierKeyMode {
    Add,
    Subtract,
}

/// Maximum additional HSL key regions supported by the GPU qualifier kernel.
/// Loaded grades above the limit are diagnosed and bypassed, never truncated.
pub const MAX_QUALIFIER_KEYS: usize = 16;

/// One sampled HSL neighborhood. Hue bounds may extend across the 0/1 seam;
/// saturation and luminance remain in the normalized interval.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QualifierKey {
    pub mode: QualifierKeyMode,
    pub hue: [f32; 2],
    pub sat: [f32; 2],
    pub lum: [f32; 2],
    pub softness: f32,
}

impl QualifierKey {
    /// The executable domain shared by edit validation and render resolution.
    pub fn is_valid(&self) -> bool {
        self.hue.iter().all(|value| value.is_finite())
            && self.hue[0] >= -1.0
            && self.hue[1] <= 2.0
            && self.hue[0] <= self.hue[1]
            && self.sat.iter().all(|value| (0.0..=1.0).contains(value))
            && self.sat[0] <= self.sat[1]
            && self.lum.iter().all(|value| (0.0..=1.0).contains(value))
            && self.lum[0] <= self.lum[1]
            && self.softness.is_finite()
            && (0.0..=1.0).contains(&self.softness)
    }
}

impl GradeOpParams {
    /// The op kind this param payload corresponds to, or `None` for `Unknown`.
    pub fn kind(&self) -> Option<GradeOpKind> {
        Some(match self {
            GradeOpParams::Exposure { .. } => GradeOpKind::Exposure,
            GradeOpParams::LinearOffset { .. } => GradeOpKind::LinearOffset,
            GradeOpParams::PrinterLights { .. } => GradeOpKind::PrinterLights,
            GradeOpParams::HighlightRolloff { .. } => GradeOpKind::HighlightRolloff,
            GradeOpParams::SaturationVibrance { .. } => GradeOpKind::SaturationVibrance,
            GradeOpParams::Contrast { .. } => GradeOpKind::Contrast,
            GradeOpParams::WhiteBalance { .. } => GradeOpKind::WhiteBalance,
            GradeOpParams::WhiteBalanceCct { .. } => GradeOpKind::WhiteBalanceCct,
            GradeOpParams::TonalExposure { .. } => GradeOpKind::TonalExposure,
            GradeOpParams::Cdl { .. } => GradeOpKind::Cdl,
            GradeOpParams::Wheels { .. } => GradeOpKind::Wheels,
            GradeOpParams::Curves { .. } => GradeOpKind::Curves,
            GradeOpParams::HslQualifier { .. } => GradeOpKind::HslQualifier,
            GradeOpParams::Lut3d { .. } => GradeOpKind::Lut3d,
            GradeOpParams::Unknown(_) => return None,
        })
    }

    /// The preserved `kind` tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(&self) -> Option<&str> {
        match self {
            GradeOpParams::Unknown(map) => map.get("kind").and_then(|v| v.as_str()),
            _ => None,
        }
    }

    /// True if this is a forward-compat variant this build does not understand.
    pub fn is_unknown(&self) -> bool {
        matches!(self, GradeOpParams::Unknown(_))
    }
}

impl PropSet for GradeOpParams {
    // Like `EffectParams`, grade params span multiple kinds; the concrete kind
    // drives per-path validation via `prop_registry`, so the generic bound uses
    // the lenient `GraphNode` target for its single const.
    const TARGET_KIND: PropTargetKind = PropTargetKind::GraphNode;
}

/// A grade mask restricting an op to part of the frame (§4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape_kind", rename_all = "snake_case")]
pub enum GradeMask {
    PowerWindow {
        shape: WindowShape,
        center: [f32; 2],
        size: [f32; 2],
        rotation: f32,
        softness: f32,
        invert: bool,
    },
    /// Stretch/forward-compat (07 §4.2): not required for CAP-015's v1 exit.
    RotoMatte { source: MaskRef, invert: bool },
}

impl GradeMask {
    pub fn scalar_property(&self, path: &str) -> Option<f32> {
        let mut copy = self.clone();
        copy.scalar_property_mut(path).copied()
    }
    pub fn scalar_property_mut(&mut self, path: &str) -> Option<&mut f32> {
        let Self::PowerWindow {
            shape,
            center,
            size,
            rotation,
            softness,
            ..
        } = self
        else {
            return None;
        };
        match path {
            "mask.center[0]" => Some(&mut center[0]),
            "mask.center[1]" => Some(&mut center[1]),
            "mask.size[0]" if *shape != WindowShape::Gradient => Some(&mut size[0]),
            "mask.size[1]" => Some(&mut size[1]),
            "mask.rotation" => Some(rotation),
            "mask.softness" if *shape != WindowShape::Gradient => Some(softness),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowShape {
    Ellipse,
    Rectangle,
    /// Directional transition across the window's local Y axis.
    Gradient,
}

/// Reference to a mask-producing source for `GradeMask::RotoMatte` (07 §4.2).
///
/// The spec leaves `MaskRef`'s concrete shape open (no roto tool exists yet). We
/// define a minimal forward-compat enum covering the two named v1 candidates: an
/// automatic subject matte (`photonic-matte`) and a mask-typed node in a
/// composition graph. See the deviation note in the P2 report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MaskRef {
    /// Auto subject cutout via `photonic-matte` (07 §4.2 v1 candidate).
    Matte,
    /// A `Mask`-typed output of a node in a composition graph.
    GraphNode { graph: GraphId, node: GraphNodeId },
}

// ── ASC CDL XML interchange (07 §6.2) ───────────────────────────────────────

/// Error parsing ASC CDL XML.
#[derive(Debug, Clone, PartialEq)]
pub enum CdlXmlError {
    /// The document was not well-formed XML.
    Malformed(String),
    /// A `Slope`/`Offset`/`Power` element did not contain three floats, or a
    /// `Saturation` was not a single float.
    BadNumbers(String),
    /// No `<ColorCorrection>` element was found.
    NoColorCorrection,
}

impl std::fmt::Display for CdlXmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdlXmlError::Malformed(s) => write!(f, "malformed CDL XML: {s}"),
            CdlXmlError::BadNumbers(s) => write!(f, "bad CDL numbers: {s}"),
            CdlXmlError::NoColorCorrection => write!(f, "no <ColorCorrection> element"),
        }
    }
}

impl std::error::Error for CdlXmlError {}

fn parse_triple(s: &str) -> Result<[f32; 3], CdlXmlError> {
    let parts: Vec<f32> = s
        .split_whitespace()
        .map(|p| p.parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|e| CdlXmlError::BadNumbers(e.to_string()))?;
    if parts.len() != 3 {
        return Err(CdlXmlError::BadNumbers(format!(
            "expected 3 values, got {}",
            parts.len()
        )));
    }
    Ok([parts[0], parts[1], parts[2]])
}

/// Parse ASC CDL XML — either a single `<ColorCorrection>` (`.cdl`) or a
/// `<ColorCorrectionCollection>` of several (`.ccc`) — into `(id, params)` pairs.
/// A missing SOP/Sat child defaults to the identity for that component.
pub fn parse_cdl_xml(s: &str) -> Result<Vec<(String, CdlParams)>, CdlXmlError> {
    let doc = roxmltree::Document::parse(s).map_err(|e| CdlXmlError::Malformed(e.to_string()))?;
    let mut out = Vec::new();
    for cc in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "ColorCorrection")
    {
        let id = cc.attribute("id").unwrap_or("").to_string();
        let mut params = CdlParams::identity();
        if let Some(sop) = cc
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "SOPNode")
        {
            for child in sop.children().filter(|n| n.is_element()) {
                let text = child.text().unwrap_or("").trim();
                match child.tag_name().name() {
                    "Slope" => params.slope = parse_triple(text)?,
                    "Offset" => params.offset = parse_triple(text)?,
                    "Power" => params.power = parse_triple(text)?,
                    _ => {}
                }
            }
        }
        if let Some(sat) = cc
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "SatNode")
        {
            if let Some(satn) = sat
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "Saturation")
            {
                let text = satn.text().unwrap_or("").trim();
                params.sat = text
                    .parse::<f32>()
                    .map_err(|e| CdlXmlError::BadNumbers(e.to_string()))?;
            }
        }
        out.push((id, params));
    }
    if out.is_empty() {
        return Err(CdlXmlError::NoColorCorrection);
    }
    Ok(out)
}

/// Write `(id, params)` pairs as ASC CDL XML. A single entry emits a bare
/// `<ColorCorrection>` (`.cdl`); multiple entries emit a
/// `<ColorCorrectionCollection>` (`.ccc`). Round-trips through [`parse_cdl_xml`].
pub fn write_cdl_xml(entries: &[(String, CdlParams)]) -> String {
    fn triple(v: &[f32; 3]) -> String {
        format!("{} {} {}", v[0], v[1], v[2])
    }
    fn one(id: &str, p: &CdlParams, indent: &str) -> String {
        let id_attr = if id.is_empty() {
            String::new()
        } else {
            format!(" id=\"{id}\"")
        };
        format!(
            "{i}<ColorCorrection{id_attr}>\n\
             {i}  <SOPNode>\n\
             {i}    <Slope>{}</Slope>\n\
             {i}    <Offset>{}</Offset>\n\
             {i}    <Power>{}</Power>\n\
             {i}  </SOPNode>\n\
             {i}  <SatNode>\n\
             {i}    <Saturation>{}</Saturation>\n\
             {i}  </SatNode>\n\
             {i}</ColorCorrection>\n",
            triple(&p.slope),
            triple(&p.offset),
            triple(&p.power),
            p.sat,
            i = indent
        )
    }

    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    if entries.len() == 1 {
        s.push_str(&one(&entries[0].0, &entries[0].1, ""));
    } else {
        s.push_str("<ColorCorrectionCollection>\n");
        for (id, p) in entries {
            s.push_str(&one(id, p, "  "));
        }
        s.push_str("</ColorCorrectionCollection>\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualifier_add_subtract_keys_roundtrip_without_changing_legacy_shape() {
        let legacy = GradeOpParams::HslQualifier {
            hue: [0.0, 1.0],
            sat: [0.0, 1.0],
            lum: [0.0, 1.0],
            softness: 0.1,
            correction: CdlParams::default(),
            keys: Vec::new(),
            matte_levels: [0.0, 0.0],
        };
        let legacy_json = serde_json::to_value(&legacy).unwrap();
        assert!(legacy_json.get("keys").is_none());
        assert!(legacy_json.get("matte_levels").is_none());
        assert_eq!(
            serde_json::from_value::<GradeOpParams>(legacy_json).unwrap(),
            legacy
        );

        let mut authored = legacy;
        if let GradeOpParams::HslQualifier { keys, .. } = &mut authored {
            keys.push(QualifierKey {
                mode: QualifierKeyMode::Subtract,
                hue: [-0.04, 0.07],
                sat: [0.4, 1.0],
                lum: [0.2, 0.8],
                softness: 0.05,
            });
        }
        if let GradeOpParams::HslQualifier { matte_levels, .. } = &mut authored {
            *matte_levels = [0.1, 0.2];
        }
        let encoded = serde_json::to_value(&authored).unwrap();
        assert_eq!(encoded["keys"][0]["mode"], "subtract");
        assert_eq!(
            encoded["matte_levels"],
            serde_json::to_value([0.1_f32, 0.2_f32]).unwrap()
        );
        assert_eq!(
            serde_json::from_value::<GradeOpParams>(encoded).unwrap(),
            authored
        );
    }

    #[test]
    fn gradient_window_roundtrips_without_changing_legacy_window_tags() {
        let mask = GradeMask::PowerWindow {
            shape: WindowShape::Gradient,
            center: [0.5, 0.5],
            size: [0.25, 0.3],
            rotation: 0.2,
            softness: 0.1,
            invert: false,
        };
        let value = serde_json::to_value(&mask).unwrap();
        assert_eq!(value["shape"], "gradient");
        assert_eq!(serde_json::from_value::<GradeMask>(value).unwrap(), mask);
        assert_eq!(
            serde_json::to_value(WindowShape::Ellipse).unwrap(),
            "ellipse"
        );
        assert_eq!(
            serde_json::to_value(WindowShape::Rectangle).unwrap(),
            "rectangle"
        );
    }

    #[test]
    fn legacy_curves_omit_new_optional_families_on_roundtrip() {
        let legacy = serde_json::json!({
            "kind": "curves",
            "master": [[0.0, 0.0], [1.0, 1.0]],
            "red": [], "green": [], "blue": [],
            "hue_vs_hue": [], "hue_vs_sat": []
        });
        let parsed: GradeOpParams = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), legacy);
        if let GradeOpParams::Curves {
            hue_vs_luma,
            luma_vs_sat,
            sat_vs_sat,
            ..
        } = parsed
        {
            assert!(hue_vs_luma.is_empty());
            assert!(luma_vs_sat.is_empty());
            assert!(sat_vs_sat.is_empty());
        } else {
            panic!("legacy curve payload changed kind");
        }
    }

    #[test]
    fn grade_op_params_forward_compat_unknown() {
        // A payload tagged with an op kind this build doesn't know loads as
        // Unknown, preserving the WHOLE object verbatim (39 §2.2 rule 1) — the
        // old `#[serde(other)]` unit variant destroyed the payload.
        let json = r#"{"kind":"future_op","wild":[1,2,3]}"#;
        let p: GradeOpParams = serde_json::from_str(json).unwrap();
        assert!(p.is_unknown());
        assert_eq!(p.unknown_tag(), Some("future_op"));
        assert_eq!(p.kind(), None);
        // Re-serializes value-equal to the input (payload retained, not dropped).
        let back: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        let orig: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(back, orig);
    }

    #[test]
    fn grade_op_kind_unknown_preserves_tag() {
        let k: GradeOpKind = serde_json::from_str("\"bloom\"").unwrap();
        assert!(k.is_unknown());
        assert_eq!(k.unknown_tag().unwrap().as_str(), "bloom");
        assert_eq!(serde_json::to_string(&k).unwrap(), "\"bloom\"");
        for (k, tag) in [
            (GradeOpKind::Exposure, "\"exposure\""),
            (GradeOpKind::Cdl, "\"cdl\""),
            (GradeOpKind::Lut3d, "\"lut3d\""),
        ] {
            assert_eq!(serde_json::to_string(&k).unwrap(), tag);
            let back: GradeOpKind = serde_json::from_str(tag).unwrap();
            assert_eq!(back, k);
            assert!(!back.is_unknown());
        }
    }

    #[test]
    fn grade_op_params_malformed_known_falls_to_unknown_with_known_tag() {
        // serde's per-variant untagged fallback is greedy: a KNOWN kind with a
        // malformed field degrades to Unknown at the serde layer (this
        // regressed the old `#[serde(other)]` unit variant, which errored). The
        // document-level integrity guard lives in `load::finalize_load`, which
        // rejects a retained Unknown whose tag is a KNOWN catalog tag. Here we
        // pin the serde-layer behaviour that feeds that guard.
        let bad = r#"{"kind":"exposure","stops":"NOPE"}"#;
        let p: GradeOpParams = serde_json::from_str(bad).unwrap();
        assert!(p.is_unknown());
        assert_eq!(p.unknown_tag(), Some("exposure"));
    }

    #[test]
    fn grade_op_params_roundtrip_each_kind() {
        let samples = vec![
            GradeOpParams::Exposure { stops: 1.5 },
            GradeOpParams::LinearOffset {
                rgb: [0.1, 0.0, -0.1],
            },
            GradeOpParams::PrinterLights {
                points: [3.0, 0.0, -3.0],
            },
            GradeOpParams::HighlightRolloff {
                knee: 1.0,
                strength: 0.75,
            },
            GradeOpParams::SaturationVibrance {
                saturation: 1.2,
                vibrance: 0.3,
            },
            GradeOpParams::Cdl {
                slope: [1.1, 1.0, 0.9],
                offset: [0.0, 0.01, -0.02],
                power: [1.0, 1.0, 1.0],
                sat: 1.2,
            },
            GradeOpParams::Lut3d {
                asset: AssetId::new(),
                intensity: 0.8,
                interp: LutInterp::Tetrahedral,
            },
        ];
        for s in samples {
            let j = serde_json::to_string(&s).unwrap();
            let back: GradeOpParams = serde_json::from_str(&j).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn cdl_xml_round_trips() {
        let entries = vec![(
            "shot_010".to_string(),
            CdlParams {
                slope: [1.2, 1.0, 0.8],
                offset: [0.0, 0.05, -0.05],
                power: [0.9, 1.0, 1.1],
                sat: 1.1,
            },
        )];
        let xml = write_cdl_xml(&entries);
        let parsed = parse_cdl_xml(&xml).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "shot_010");
        let p = parsed[0].1;
        let e = entries[0].1;
        for i in 0..3 {
            assert!((p.slope[i] - e.slope[i]).abs() < 1e-5);
            assert!((p.offset[i] - e.offset[i]).abs() < 1e-5);
            assert!((p.power[i] - e.power[i]).abs() < 1e-5);
        }
        assert!((p.sat - e.sat).abs() < 1e-5);
    }

    #[test]
    fn ccc_collection_round_trips() {
        let entries = vec![
            ("a".to_string(), CdlParams::identity()),
            (
                "b".to_string(),
                CdlParams {
                    slope: [2.0, 2.0, 2.0],
                    offset: [0.0; 3],
                    power: [1.0; 3],
                    sat: 1.0,
                },
            ),
        ];
        let xml = write_cdl_xml(&entries);
        assert!(xml.contains("ColorCorrectionCollection"));
        let parsed = parse_cdl_xml(&xml).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].0, "b");
        assert!((parsed[1].1.slope[0] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn parse_rejects_non_cdl() {
        assert!(matches!(
            parse_cdl_xml("<foo/>"),
            Err(CdlXmlError::NoColorCorrection)
        ));
        assert!(matches!(
            parse_cdl_xml("not xml <<"),
            Err(CdlXmlError::Malformed(_))
        ));
    }
}

/// Tonal pivots are stops relative to linear Y=0.18, with one shared half-width.
/// Ordered boundaries guarantee nonnegative partition-of-unity weights.
pub fn validate_tonal_exposure(
    stops: [f32; 3],
    boundaries: [f32; 2],
    softness: f32,
) -> Result<(), &'static str> {
    if !stops
        .iter()
        .all(|v| v.is_finite() && (-8.0..=8.0).contains(v))
        || !boundaries
            .iter()
            .all(|v| v.is_finite() && (-12.0..=12.0).contains(v))
        || boundaries[0] >= boundaries[1]
        || !softness.is_finite()
        || !(0.01..=6.0).contains(&softness)
    {
        return Err("tonal exposure requires stops in -8..=8, ordered boundaries in -12..=12, and softness in 0.01..=6");
    }
    Ok(())
}

impl GradeOpParams {
    /// Shared scalar path mapping for rendering and generic grade animation.
    pub fn scalar_property_mut(&mut self, path: &str) -> Option<&mut f32> {
        use GradeOpParams::*;
        match self {
            Exposure { stops } => (path == "params.stops").then_some(stops),
            LinearOffset { rgb } => match path {
                "params.rgb[0]" => Some(&mut rgb[0]),
                "params.rgb[1]" => Some(&mut rgb[1]),
                "params.rgb[2]" => Some(&mut rgb[2]),
                _ => None,
            },
            PrinterLights { points } => match path {
                "params.points[0]" => Some(&mut points[0]),
                "params.points[1]" => Some(&mut points[1]),
                "params.points[2]" => Some(&mut points[2]),
                _ => None,
            },
            HighlightRolloff { knee, strength } => match path {
                "params.knee" => Some(knee),
                "params.strength" => Some(strength),
                _ => None,
            },
            SaturationVibrance {
                saturation,
                vibrance,
            } => match path {
                "params.saturation" => Some(saturation),
                "params.vibrance" => Some(vibrance),
                _ => None,
            },
            Contrast { pivot, amount } => match path {
                "params.pivot" => Some(pivot),
                "params.amount" => Some(amount),
                _ => None,
            },
            TonalExposure {
                stops,
                boundaries,
                softness,
            } => match path {
                "params.stops[0]" => Some(&mut stops[0]),
                "params.stops[1]" => Some(&mut stops[1]),
                "params.stops[2]" => Some(&mut stops[2]),
                "params.boundaries[0]" => Some(&mut boundaries[0]),
                "params.boundaries[1]" => Some(&mut boundaries[1]),
                "params.softness" => Some(softness),
                _ => None,
            },
            WhiteBalanceCct {
                source_kelvin,
                target_kelvin,
            } => match path {
                "params.source_kelvin" => Some(source_kelvin),
                "params.target_kelvin" => Some(target_kelvin),
                _ => None,
            },
            WhiteBalance { temp, tint } => match path {
                "params.temp" => Some(temp),
                "params.tint" => Some(tint),
                _ => None,
            },
            Cdl {
                slope,
                offset,
                power,
                sat,
            } => match path {
                "params.slope[0]" => Some(&mut slope[0]),
                "params.slope[1]" => Some(&mut slope[1]),
                "params.slope[2]" => Some(&mut slope[2]),
                "params.offset[0]" => Some(&mut offset[0]),
                "params.offset[1]" => Some(&mut offset[1]),
                "params.offset[2]" => Some(&mut offset[2]),
                "params.power[0]" => Some(&mut power[0]),
                "params.power[1]" => Some(&mut power[1]),
                "params.power[2]" => Some(&mut power[2]),
                "params.sat" => Some(sat),
                _ => None,
            },
            Wheels {
                lift,
                gamma,
                gain,
                sat,
            } => match path {
                "params.lift[0]" => Some(&mut lift[0]),
                "params.lift[1]" => Some(&mut lift[1]),
                "params.lift[2]" => Some(&mut lift[2]),
                "params.gamma[0]" => Some(&mut gamma[0]),
                "params.gamma[1]" => Some(&mut gamma[1]),
                "params.gamma[2]" => Some(&mut gamma[2]),
                "params.gain[0]" => Some(&mut gain[0]),
                "params.gain[1]" => Some(&mut gain[1]),
                "params.gain[2]" => Some(&mut gain[2]),
                "params.sat" => Some(sat),
                _ => None,
            },
            HslQualifier {
                softness,
                correction,
                ..
            } => match path {
                "params.softness" => Some(softness),
                "params.correction.sat" => Some(&mut correction.sat),
                _ => None,
            },
            Lut3d { intensity, .. } => (path == "params.intensity").then_some(intensity),
            Curves { .. } | Unknown(_) => None,
        }
    }
    /// Current authored scalar value. Structural vector/unknown parameters are
    /// not exposed by the scalar registry. Called by UI, never per pixel.
    pub fn scalar_property(&self, path: &str) -> Option<f32> {
        self.clone().scalar_property_mut(path).copied()
    }
}

#[cfg(test)]
mod window_animation_tests {
    use super::*;
    use crate::timeline::{Interp, Keyframe, PropPath, PropValue, Tick};
    fn op() -> GradeOp {
        let mut op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        op.mask = Some(GradeMask::PowerWindow {
            shape: WindowShape::Ellipse,
            center: [0.25, 0.25],
            size: [0.1, 0.2],
            rotation: 0.0,
            softness: 0.2,
            invert: false,
        });
        op
    }
    fn lane(op: &mut GradeOp, path: &str, a: f64, b: f64) {
        let track = op.params.track_mut(&PropPath::new(path));
        for (at, v) in [(0, a), (100, b)] {
            track.insert_keyframe(Keyframe::new(Tick(at), PropValue::Float(v), Interp::Linear));
        }
    }
    #[test]
    fn window_animation_samples_registered_geometry_without_mutating_base() {
        let mut op = op();
        for (path, a, b) in [
            ("mask.center[0]", 0.25, 0.75),
            ("mask.center[1]", 0.25, 0.5),
            ("mask.size[0]", 0.1, 0.3),
            ("mask.size[1]", 0.2, 0.4),
            ("mask.rotation", 0.0, 1.0),
            ("mask.softness", 0.2, 0.8),
        ] {
            lane(&mut op, path, a, b);
            assert!(crate::timeline::prop_registry::resolve(op.kind.target_kind(), path).is_some());
        }
        let json = serde_json::to_string(&op).unwrap();
        let loaded: GradeOp = serde_json::from_str(&json).unwrap();
        assert!(loaded.has_mask_animation());
        let mask = loaded.mask_at(Tick(50)).unwrap().unwrap();
        for (path, value) in [
            ("mask.center[0]", 0.5),
            ("mask.center[1]", 0.375),
            ("mask.size[0]", 0.2),
            ("mask.size[1]", 0.3),
            ("mask.rotation", 0.5),
            ("mask.softness", 0.5),
        ] {
            assert!((mask.scalar_property(path).unwrap() - value).abs() < 1e-6);
        }
        assert_eq!(serde_json::to_string(&op).unwrap(), json);
        assert_eq!(
            loaded
                .mask_at(Tick(-1))
                .unwrap()
                .unwrap()
                .scalar_property("mask.center[0]"),
            Some(0.25)
        );
        assert_eq!(
            loaded
                .mask_at(Tick(200))
                .unwrap()
                .unwrap()
                .scalar_property("mask.center[0]"),
            Some(0.75)
        );
    }
    #[test]
    fn window_animation_rejects_invalid_samples_and_preserves_dormant_lanes() {
        for (path, value) in [
            ("mask.size[0]", 0.0),
            ("mask.size[1]", -0.2),
            ("mask.softness", -0.1),
            ("mask.softness", 1.1),
            ("mask.center[0]", f64::MAX),
        ] {
            let mut op = op();
            lane(&mut op, path, value, value);
            assert!(op.mask_at(Tick(50)).is_err(), "{path}");
        }
        let mut op = op();
        lane(&mut op, "mask.softness", 0.0, 1.0);
        let count = op.params.tracks.len();
        op.mask = None;
        assert!(!op.has_mask_animation());
        assert_eq!(op.mask_at(Tick(50)).unwrap(), None);
        assert_eq!(op.params.tracks.len(), count);
        op.mask = Some(GradeMask::PowerWindow {
            shape: WindowShape::Gradient,
            center: [0.5; 2],
            size: [0.2; 2],
            rotation: 0.0,
            softness: 0.0,
            invert: false,
        });
        assert!(!op.has_mask_animation());
        assert_eq!(op.scalar_property("mask.softness"), None);
        lane(&mut op, "mask.size[1]", 0.2, 0.6);
        assert!(op.has_mask_animation());
        assert!(
            (op.mask_at(Tick(50))
                .unwrap()
                .unwrap()
                .scalar_property("mask.size[1]")
                .unwrap()
                - 0.4)
                .abs()
                < 1e-6
        );
    }
}
