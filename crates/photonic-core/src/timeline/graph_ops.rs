//! Pure node-graph edit ops (08 §6.6, §8), mirroring [`super::ops`].
//!
//! Each returns `Result<TimelineCmd, EditError>` wrapping a `GraphEdit(GraphCmd)`.
//! Cycle refusal is enforced here at edit time (`add_edge` returns
//! `WouldCreateCycle` — 01 §8: "edge insertion cycle-checks; the edit op fails,
//! never panics"), so no invalid edge is ever representable. Port-type checking
//! lives in the engine/UI (08 §3.1); this layer owns structural validity.

use super::commands::{GraphCmd, TimelineCmd};
use super::graph::{GraphEdge, GraphNode, GraphNodeParams, InPort, NodePos, OutPort};
use super::ids::{GraphId, GraphNodeId};
use super::ops::EditError;
use super::sequence::TimelineProject;
use super::time::Tick;

/// Editing a graph shared by any locked clip would mutate that clip indirectly.
/// Project-only and unreferenced graphs remain editable; reading is unrestricted.
pub fn ensure_editable(p: &TimelineProject, id: GraphId) -> Result<(), EditError> {
    if !p.graphs.contains_key(&id) {
        return Err(EditError::NoGraph(id));
    }
    if p.sequences.values().any(|sequence| {
        sequence
            .video_tracks
            .iter()
            .chain(sequence.audio_tracks.iter())
            .any(|track| {
                track.locked && track.clips.iter().any(|clip| clip.composition == Some(id))
            })
    }) {
        return Err(EditError::TrackLocked);
    }
    Ok(())
}

fn graph(p: &TimelineProject, id: GraphId) -> Result<&super::graph::NodeGraph, EditError> {
    ensure_editable(p, id)?;
    p.graphs.get(&id).ok_or(EditError::NoGraph(id))
}

pub fn add_node(
    p: &TimelineProject,
    graph_id: GraphId,
    node: GraphNode,
    pos: NodePos,
) -> Result<TimelineCmd, EditError> {
    ensure_editable(p, graph_id)?;
    Ok(TimelineCmd::GraphEdit(GraphCmd::AddNode {
        graph: graph_id,
        node: Box::new(node),
        pos: Some(pos),
        edges: Vec::new(),
        restore_edges: None,
    }))
}

pub fn remove_node(
    p: &TimelineProject,
    graph_id: GraphId,
    node_id: GraphNodeId,
) -> Result<TimelineCmd, EditError> {
    let g = graph(p, graph_id)?;
    let node = g
        .nodes
        .get(&node_id)
        .ok_or(EditError::NoGraph(graph_id))?
        .clone();
    let edges: Vec<GraphEdge> = g
        .edges
        .iter()
        .filter(|e| e.from.0 == node_id || e.to.0 == node_id)
        .copied()
        .collect();
    Ok(TimelineCmd::GraphEdit(GraphCmd::RemoveNode {
        graph: graph_id,
        node: Box::new(node),
        pos: g.ui.get(&node_id).copied(),
        edges,
        restore_edges: Some(g.edges.clone()),
    }))
}

fn validate_connection(
    g: &super::graph::NodeGraph,
    from: (GraphNodeId, OutPort),
    to: (GraphNodeId, InPort),
) -> Result<(), EditError> {
    let source = g.nodes.get(&from.0).ok_or(EditError::NoGraph(g.id))?;
    let target = g.nodes.get(&to.0).ok_or(EditError::NoGraph(g.id))?;
    if g.would_create_cycle(from.0, to.0) {
        return Err(EditError::WouldCreateCycle);
    }
    if from.1 .0 >= source.op.port_counts().1 || to.1 .0 >= target.op.port_counts().0 {
        return Err(EditError::InvalidGraphConnection(
            "source or destination port does not exist on this node".into(),
        ));
    }
    Ok(())
}

/// Add to an empty input. UI/MCP reconnection uses `connect_edge` below.
pub fn add_edge(
    p: &TimelineProject,
    graph_id: GraphId,
    from: (GraphNodeId, OutPort),
    to: (GraphNodeId, InPort),
) -> Result<TimelineCmd, EditError> {
    let g = graph(p, graph_id)?;
    validate_connection(g, from, to)?;
    if g.edges.iter().any(|edge| edge.to == to) {
        return Err(EditError::InvalidGraphConnection(
            "input is already connected".into(),
        ));
    }
    Ok(TimelineCmd::GraphEdit(GraphCmd::AddEdge {
        graph: graph_id,
        edge: GraphEdge { from, to },
    }))
}

/// Connect or replace one input atomically. Repeating the same connection is
/// a no-op, and undo restores the exact original edge ordering.
pub fn connect_edge(
    p: &TimelineProject,
    graph_id: GraphId,
    from: (GraphNodeId, OutPort),
    to: (GraphNodeId, InPort),
) -> Result<Option<TimelineCmd>, EditError> {
    rewire_edge(p, graph_id, None, from, to)
}

/// Finish moving an existing wire, without removing it before a valid drop.
/// Stale drag origins and locked owners are rejected without a command.
pub fn rewire_edge(
    p: &TimelineProject,
    graph_id: GraphId,
    detached: Option<GraphEdge>,
    from: (GraphNodeId, OutPort),
    to: (GraphNodeId, InPort),
) -> Result<Option<TimelineCmd>, EditError> {
    let g = graph(p, graph_id)?;
    validate_connection(g, from, to)?;
    if detached.is_some_and(|edge| edge.from != from || !g.edges.contains(&edge)) {
        return Err(EditError::InvalidGraphConnection(
            "the original wire changed while dragging".into(),
        ));
    }
    let edge = GraphEdge { from, to };
    let mut new = Vec::with_capacity(g.edges.len() + 1);
    let mut inserted = false;
    for old in &g.edges {
        if old.to == to {
            if !inserted {
                new.push(edge);
                inserted = true;
            }
        } else if Some(*old) != detached {
            new.push(*old);
        }
    }
    if !inserted {
        new.push(edge);
    }
    if new == g.edges {
        return Ok(None);
    }
    Ok(Some(TimelineCmd::GraphEdit(GraphCmd::SetEdges {
        graph: graph_id,
        old: g.edges.clone(),
        new,
    })))
}

pub fn remove_edge(
    p: &TimelineProject,
    graph_id: GraphId,
    edge: GraphEdge,
) -> Result<TimelineCmd, EditError> {
    let g = graph(p, graph_id)?;
    if !g.edges.contains(&edge) {
        return Err(EditError::IndexOutOfRange);
    }
    Ok(TimelineCmd::GraphEdit(GraphCmd::SetEdges {
        graph: graph_id,
        old: g.edges.clone(),
        new: g
            .edges
            .iter()
            .copied()
            .filter(|current| current != &edge)
            .collect(),
    }))
}

/// Replace only a Text node's static payload, preserving ids, edges, params and UI.
pub fn set_text(
    p: &TimelineProject,
    graph_id: GraphId,
    node_id: GraphNodeId,
    new: super::graph::TextGen,
) -> Result<Option<TimelineCmd>, EditError> {
    let g = graph(p, graph_id)?;
    let node = g.nodes.get(&node_id).ok_or(EditError::NoGraph(graph_id))?;
    let super::graph::GraphOp::Text { text: old } = &node.op else {
        return Err(EditError::InvalidGraphConnection(
            "target node is not Text".into(),
        ));
    };
    new.validate_native()
        .map_err(|e| EditError::InvalidGraphConnection(e.into()))?;
    if old == &new {
        return Ok(None);
    }
    Ok(Some(TimelineCmd::GraphEdit(GraphCmd::SetText {
        graph: graph_id,
        node: node_id,
        old: Box::new(old.clone()),
        new: Box::new(new),
    })))
}

/// Edit one structural clock offset without changing node identity or graph wiring.
pub fn set_time_offset(
    p: &TimelineProject,
    graph_id: GraphId,
    node_id: GraphNodeId,
    new: Tick,
) -> Result<Option<TimelineCmd>, EditError> {
    let g = graph(p, graph_id)?;
    let node = g.nodes.get(&node_id).ok_or(EditError::NoGraph(graph_id))?;
    let super::graph::GraphOp::TimeOffset { offset: old } = node.op else {
        return Err(EditError::InvalidGraphConnection(
            "target node is not TimeOffset".into(),
        ));
    };
    if old == new {
        return Ok(None);
    }
    Ok(Some(TimelineCmd::GraphEdit(GraphCmd::SetTimeOffset {
        graph: graph_id,
        node: node_id,
        old,
        new,
    })))
}

/// Replace shape geometry without losing node identity, animation, edges or layout.
pub fn set_mask_shape(
    p: &TimelineProject,
    graph_id: GraphId,
    node_id: GraphNodeId,
    new: super::graph::MaskShapeKind,
) -> Result<Option<TimelineCmd>, EditError> {
    let g = graph(p, graph_id)?;
    let node = g.nodes.get(&node_id).ok_or(EditError::NoGraph(graph_id))?;
    let super::graph::GraphOp::MaskShape { shape: old } = &node.op else {
        return Err(EditError::InvalidGraphConnection(
            "target node is not MaskShape".into(),
        ));
    };
    new.validate_native()
        .map_err(|e| EditError::InvalidGraphConnection(e.into()))?;
    if old == &new {
        return Ok(None);
    }
    Ok(Some(TimelineCmd::GraphEdit(GraphCmd::SetMaskShape {
        graph: graph_id,
        node: node_id,
        old: Box::new(old.clone()),
        new: Box::new(new),
    })))
}

pub fn set_node_param(
    p: &TimelineProject,
    graph_id: GraphId,
    node_id: GraphNodeId,
    new: GraphNodeParams,
) -> Result<TimelineCmd, EditError> {
    let g = graph(p, graph_id)?;
    let node = g.nodes.get(&node_id).ok_or(EditError::NoGraph(graph_id))?;
    Ok(TimelineCmd::GraphEdit(GraphCmd::SetNodeParam {
        graph: graph_id,
        node: node_id,
        old: Box::new(node.params.base.clone()),
        new: Box::new(new),
    }))
}

pub fn move_node(
    p: &TimelineProject,
    graph_id: GraphId,
    node_id: GraphNodeId,
    new: NodePos,
) -> Result<TimelineCmd, EditError> {
    let g = graph(p, graph_id)?;
    if !g.nodes.contains_key(&node_id) {
        return Err(EditError::NoGraph(graph_id));
    }
    if !new.x.is_finite() || !new.y.is_finite() {
        return Err(EditError::InvalidGraphConnection(
            "node position must be finite".into(),
        ));
    }
    let old = g.ui.get(&node_id).copied();
    Ok(TimelineCmd::GraphEdit(GraphCmd::MoveNode {
        graph: graph_id,
        node: node_id,
        old,
        new: Some(new),
    }))
}
