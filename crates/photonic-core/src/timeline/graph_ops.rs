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

fn graph(p: &TimelineProject, id: GraphId) -> Result<&super::graph::NodeGraph, EditError> {
    p.graphs.get(&id).ok_or(EditError::NoGraph(id))
}

pub fn add_node(graph_id: GraphId, node: GraphNode, pos: NodePos) -> TimelineCmd {
    TimelineCmd::GraphEdit(GraphCmd::AddNode {
        graph: graph_id,
        node: Box::new(node),
        pos,
        edges: Vec::new(),
    })
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
    }))
}

/// Add an edge, refusing any that would create a cycle (01 §8).
pub fn add_edge(
    p: &TimelineProject,
    graph_id: GraphId,
    from: (GraphNodeId, OutPort),
    to: (GraphNodeId, InPort),
) -> Result<TimelineCmd, EditError> {
    let g = graph(p, graph_id)?;
    if !g.nodes.contains_key(&from.0) || !g.nodes.contains_key(&to.0) {
        return Err(EditError::NoGraph(graph_id));
    }
    if g.would_create_cycle(from.0, to.0) {
        return Err(EditError::WouldCreateCycle);
    }
    Ok(TimelineCmd::GraphEdit(GraphCmd::AddEdge {
        graph: graph_id,
        edge: GraphEdge { from, to },
    }))
}

pub fn remove_edge(graph_id: GraphId, edge: GraphEdge) -> TimelineCmd {
    TimelineCmd::GraphEdit(GraphCmd::RemoveEdge {
        graph: graph_id,
        edge,
    })
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
    let old =
        g.ui.get(&node_id)
            .copied()
            .unwrap_or(NodePos { x: 0.0, y: 0.0 });
    Ok(TimelineCmd::GraphEdit(GraphCmd::MoveNode {
        graph: graph_id,
        node: node_id,
        old,
        new,
    }))
}
