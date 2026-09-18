//! Edit-tree navigation for [`CommandHistory`] — the undo-tree machinery that
//! turns the old flat undo/redo stacks into a branching history. See
//! [`HistNode`](super::HistNode) for the per-node shape.

use super::*;
use std::collections::HashSet;

/// Serializable snapshot of the whole edit tree (persisted inside a `.photon`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryTree {
    pub nodes: Vec<HistNode>,
    pub root: u64,
    pub current: u64,
    pub next_id: u64,
}

/// Which editing surface produced a history entry.
///
/// A `.photon` holds one document and therefore **one** edit tree, shared by
/// vector mode and the video timeline. A history browser needs to tell the two
/// apart to badge and filter entries (26 §15 K-G5), and the edge `Command` is
/// not part of [`HistoryGraphNode`], so the classification is done here rather
/// than by a view reaching into the tree's internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryEntryKind {
    /// The synthetic root — the pre-edit state, no edge command.
    #[default]
    Root,
    /// A vector/document edit (nodes, layers, artboards, …).
    Vector,
    /// A video timeline edit ([`Command::Timeline`]).
    Timeline,
    /// A batch whose members span both surfaces.
    Mixed,
}

impl HistoryEntryKind {
    /// Classify one edge command. `Batch` folds its members, so a batch of pure
    /// timeline commands still reads as [`HistoryEntryKind::Timeline`] and only a
    /// genuinely cross-surface batch becomes [`HistoryEntryKind::Mixed`].
    pub fn of(cmd: &Command) -> Self {
        match cmd {
            Command::Timeline(_) => Self::Timeline,
            Command::Batch(cmds) => cmds
                .iter()
                .map(Self::of)
                .fold(Self::Root, |acc, k| acc.merge(k)),
            _ => Self::Vector,
        }
    }

    /// Combine two classifications: `Root` is the identity (an empty batch has no
    /// surface), equal kinds collapse, anything else is `Mixed`.
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Root, k) | (k, Self::Root) => k,
            (a, b) if a == b => a,
            _ => Self::Mixed,
        }
    }

    /// Whether this entry touched the video timeline (`Timeline` or `Mixed`).
    pub fn touches_timeline(self) -> bool {
        matches!(self, Self::Timeline | Self::Mixed)
    }

    /// Short, stable label for a history surface (also the search term a user
    /// can type to filter by surface).
    pub fn label(self) -> &'static str {
        match self {
            Self::Root => "initial",
            Self::Vector => "vector",
            Self::Timeline => "timeline",
            Self::Mixed => "mixed",
        }
    }
}

/// One node of the edit tree flattened for rendering (e.g. the GUI commit graph).
/// Carries just what a view needs — topology, label, and the HEAD/root flags.
#[derive(Debug, Clone)]
pub struct HistoryGraphNode {
    pub id: u64,
    pub parent: Option<u64>,
    pub children: Vec<u64>,
    /// The child redo prefers (used to drive the redo button / primary chain).
    pub primary_child: Option<u64>,
    /// User-given name for this state, if any (a labeled commit == a named branch).
    pub label: Option<String>,
    pub description: String,
    pub is_current: bool,
    pub is_root: bool,
    /// Which editing surface produced this entry (26 §15 K-G5).
    pub kind: HistoryEntryKind,
}

impl CommandHistory {
    // ── Tree construction / reset ────────────────────────────────────────────

    /// Reset the tree to a single empty root (the pre-edit state).
    pub(crate) fn init_empty_tree(&mut self) {
        self.nodes.clear();
        self.nodes.insert(
            0,
            HistNode {
                id: 0,
                parent: None,
                command: None,
                children: vec![],
                primary_child: None,
                label: None,
            },
        );
        self.root = 0;
        self.current = 0;
        self.next_id = 1;
    }

    /// Rebuild a purely-linear tree from a legacy flat `undo`/`redo` snapshot
    /// (files written before the edit tree existed). `undo` is oldest→newest;
    /// `redo` is in redo-stack order (last element = the next redo).
    pub(crate) fn rebuild_linear_tree(&mut self, undo: Vec<Command>, redo: Vec<Command>) {
        self.init_empty_tree();
        for cmd in undo {
            let id = self.next_id;
            self.next_id += 1;
            let parent = self.current;
            self.nodes.insert(
                id,
                HistNode {
                    id,
                    parent: Some(parent),
                    command: Some(cmd),
                    children: vec![],
                    primary_child: None,
                    label: None,
                },
            );
            if let Some(p) = self.nodes.get_mut(&parent) {
                p.children.push(id);
                p.primary_child = Some(id);
            }
            self.current = id;
        }
        // Redo entries are the *future* primary chain hanging off `current`.
        // `redo.rev()` puts them nearest-first (the next redo first).
        let mut tip = self.current;
        for cmd in redo.into_iter().rev() {
            let id = self.next_id;
            self.next_id += 1;
            self.nodes.insert(
                id,
                HistNode {
                    id,
                    parent: Some(tip),
                    command: Some(cmd),
                    children: vec![],
                    primary_child: None,
                    label: None,
                },
            );
            if let Some(p) = self.nodes.get_mut(&tip) {
                p.children.push(id);
                p.primary_child = Some(id);
            }
            tip = id;
        }
    }

    /// Load a full edit tree (the source of truth when a `.photon` carries one).
    pub(crate) fn load_tree(&mut self, t: HistoryTree) {
        self.nodes = t.nodes.into_iter().map(|n| (n.id, n)).collect();
        self.root = t.root;
        self.current = t.current;
        self.next_id = t.next_id;
        // Guard against a corrupt snapshot missing its root/current.
        if !self.nodes.contains_key(&self.root) || !self.nodes.contains_key(&self.current) {
            self.init_empty_tree();
        }
    }

    /// Serialize the current tree.
    pub(crate) fn export_tree(&self) -> HistoryTree {
        HistoryTree {
            nodes: self.nodes.values().cloned().collect(),
            root: self.root,
            current: self.current,
            next_id: self.next_id,
        }
    }

    // ── Path / topology helpers ──────────────────────────────────────────────

    /// Number of edges from the root to `current` (== the linear undo depth).
    pub(crate) fn depth_of(&self, mut id: u64) -> usize {
        let mut n = 0;
        while let Some(node) = self.nodes.get(&id) {
            match node.parent {
                Some(p) => {
                    n += 1;
                    id = p;
                }
                None => break,
            }
        }
        n
    }

    /// Ids of the edges from `id` up to (not including) the root, newest-first.
    pub(crate) fn ancestor_edges(&self, mut id: u64) -> Vec<u64> {
        let mut out = vec![];
        while let Some(node) = self.nodes.get(&id) {
            if let Some(parent) = node.parent {
                out.push(id);
                id = parent;
            } else {
                break;
            }
        }
        out
    }

    /// `id` plus all of its ancestors (up to and including the root).
    pub(crate) fn ancestor_set(&self, mut id: u64) -> HashSet<u64> {
        let mut s = HashSet::new();
        loop {
            s.insert(id);
            match self.nodes.get(&id).and_then(|n| n.parent) {
                Some(p) => id = p,
                None => break,
            }
        }
        s
    }

    /// The primary redo chain descending from `start` (nearest-first), capped.
    pub(crate) fn primary_chain(&self, start: u64, limit: usize) -> Vec<u64> {
        let mut out = vec![];
        let mut id = start;
        while out.len() < limit {
            let child = match self.nodes.get(&id) {
                Some(n) => n.primary_child.or_else(|| n.children.last().copied()),
                None => break,
            };
            match child {
                Some(c) => {
                    out.push(c);
                    id = c;
                }
                None => break,
            }
        }
        out
    }

    /// The child of `root` that lies on the path toward `target`, if any.
    fn child_of_toward(&self, root: u64, mut target: u64) -> Option<u64> {
        loop {
            let n = self.nodes.get(&target)?;
            match n.parent {
                Some(p) if p == root => return Some(target),
                Some(p) => target = p,
                None => return None,
            }
        }
    }

    /// Collect `id` and its whole subtree into `set`.
    fn collect_subtree(&self, id: u64, set: &mut HashSet<u64>) {
        if !set.insert(id) {
            return;
        }
        if let Some(n) = self.nodes.get(&id) {
            let kids = n.children.clone();
            for c in kids {
                self.collect_subtree(c, set);
            }
        }
    }

    // ── Trimming primitives (used by enforce_steps / enforce_size) ────────────

    /// Advance the root one step toward `current`, dropping the old root and any
    /// sibling branches that hung off it. Returns false if root == current.
    pub(crate) fn reroot_once(&mut self) -> bool {
        if self.root == self.current {
            return false;
        }
        let next = match self.child_of_toward(self.root, self.current) {
            Some(n) => n,
            None => return false,
        };
        let mut keep = HashSet::new();
        self.collect_subtree(next, &mut keep);
        self.nodes.retain(|id, _| keep.contains(id));
        if let Some(n) = self.nodes.get_mut(&next) {
            n.parent = None;
            n.command = None;
        }
        self.root = next;
        true
    }

    /// Remove the oldest (lowest-id) leaf that is not on the current path and not
    /// the root. Returns false if there is no such leaf.
    pub(crate) fn prune_oldest_offpath_leaf(&mut self) -> bool {
        let path = self.ancestor_set(self.current);
        let victim = self
            .nodes
            .values()
            .filter(|n| n.children.is_empty() && n.id != self.root && !path.contains(&n.id))
            .map(|n| n.id)
            .min();
        let Some(v) = victim else {
            return false;
        };
        if let Some(parent) = self.nodes.get(&v).and_then(|n| n.parent) {
            if let Some(pn) = self.nodes.get_mut(&parent) {
                pn.children.retain(|c| *c != v);
                if pn.primary_child == Some(v) {
                    pn.primary_child = pn.children.last().copied();
                }
            }
        }
        self.nodes.remove(&v);
        true
    }

    // ── Public: graph + arbitrary-node navigation ─────────────────────────────

    /// The whole edit tree flattened for rendering, newest node first.
    pub fn history_graph(&self) -> Vec<HistoryGraphNode> {
        let mut out: Vec<HistoryGraphNode> = self
            .nodes
            .values()
            .map(|n| HistoryGraphNode {
                id: n.id,
                parent: n.parent,
                children: n.children.clone(),
                primary_child: n.primary_child.or_else(|| n.children.last().copied()),
                label: n.label.clone(),
                description: n
                    .command
                    .as_ref()
                    .map(|c| c.description())
                    .unwrap_or_else(|| "Initial state".to_string()),
                is_current: n.id == self.current,
                is_root: n.id == self.root,
                kind: n
                    .command
                    .as_ref()
                    .map(HistoryEntryKind::of)
                    .unwrap_or(HistoryEntryKind::Root),
            })
            .collect();
        out.sort_by_key(|entry| std::cmp::Reverse(entry.id));
        out
    }

    /// The HEAD node id (what the document currently reflects).
    pub fn current_node(&self) -> u64 {
        self.current
    }

    /// The edge command that produced the current node — the equivalent of the
    /// old `undo_stack.last()`. (Test-only inspection helper.)
    #[cfg(test)]
    pub(crate) fn current_command(&self) -> Option<&Command> {
        self.nodes
            .get(&self.current)
            .and_then(|n| n.command.as_ref())
    }

    /// Navigate the document to an arbitrary tree node: undo up to the lowest
    /// common ancestor, then redo down to `target`. Reuses each edge command's
    /// `apply`/`inverse`, so it works across branches. Returns whether it landed
    /// on `target`.
    pub fn jump_to_node(&mut self, target: u64, doc: &mut Document) -> bool {
        if self.current == target {
            return true;
        }
        if !self.nodes.contains_key(&target) {
            return false;
        }
        // Lowest common ancestor: walk target upward until it meets the current
        // ancestor set.
        let anc_cur = self.ancestor_set(self.current);
        let mut lca = target;
        while !anc_cur.contains(&lca) {
            match self.nodes.get(&lca).and_then(|n| n.parent) {
                Some(p) => lca = p,
                None => break,
            }
        }
        // Undo (apply inverses) from current up to the LCA.
        let mut guard = self.nodes.len() + 1;
        while self.current != lca && guard > 0 {
            if !self.undo(doc) {
                break;
            }
            guard -= 1;
        }
        // Descend LCA → target, steering redo down the correct child each step.
        let mut down = vec![];
        let mut id = target;
        while id != lca {
            down.push(id);
            match self.nodes.get(&id).and_then(|n| n.parent) {
                Some(p) => id = p,
                None => break,
            }
        }
        down.reverse();
        for step in down {
            if let Some(n) = self.nodes.get_mut(&self.current) {
                n.primary_child = Some(step);
            }
            if !self.redo(doc) {
                break;
            }
        }
        self.current == target
    }
}

#[cfg(test)]
mod tests {
    //! Read-only query surface behind the Undo History panel (26 §15 K-G5).

    use super::*;
    use crate::node::{PathNode, SceneNodeKind};
    use crate::path::PathData;
    use crate::timeline::{AssetId, TimelineCmd};

    fn doc() -> Document {
        Document::new("test", 100.0, 100.0)
    }

    fn add_node(doc: &Document) -> Command {
        let layer_id = doc.active_layer_id.unwrap();
        Command::AddNode {
            node: SceneNode::new(
                "rect",
                layer_id,
                SceneNodeKind::Path(PathNode::new(PathData::rect(0.0, 0.0, 10.0, 10.0))),
            ),
            layer_id: None,
        }
    }

    fn rate_asset() -> Command {
        Command::Timeline(TimelineCmd::SetAssetRating {
            asset: AssetId::nil(),
            old: None,
            new: Some(3),
        })
    }

    #[test]
    fn entry_kind_classifies_each_surface() {
        let d = doc();
        assert_eq!(
            HistoryEntryKind::of(&add_node(&d)),
            HistoryEntryKind::Vector
        );
        assert_eq!(
            HistoryEntryKind::of(&rate_asset()),
            HistoryEntryKind::Timeline
        );
    }

    #[test]
    fn entry_kind_folds_batches_by_membership() {
        let d = doc();
        // A batch of one surface keeps that surface…
        assert_eq!(
            HistoryEntryKind::of(&Command::Batch(vec![rate_asset(), rate_asset()])),
            HistoryEntryKind::Timeline
        );
        assert_eq!(
            HistoryEntryKind::of(&Command::Batch(vec![add_node(&d), add_node(&d)])),
            HistoryEntryKind::Vector
        );
        // …a cross-surface batch is Mixed, in either member order.
        assert_eq!(
            HistoryEntryKind::of(&Command::Batch(vec![add_node(&d), rate_asset()])),
            HistoryEntryKind::Mixed
        );
        assert_eq!(
            HistoryEntryKind::of(&Command::Batch(vec![rate_asset(), add_node(&d)])),
            HistoryEntryKind::Mixed
        );
        // An empty batch has no surface at all.
        assert_eq!(
            HistoryEntryKind::of(&Command::Batch(vec![])),
            HistoryEntryKind::Root
        );
    }

    #[test]
    fn touches_timeline_covers_timeline_and_mixed_only() {
        for k in [HistoryEntryKind::Timeline, HistoryEntryKind::Mixed] {
            assert!(
                k.touches_timeline(),
                "{k:?} should count as a timeline edit"
            );
        }
        for k in [HistoryEntryKind::Root, HistoryEntryKind::Vector] {
            assert!(!k.touches_timeline(), "{k:?} must not count as timeline");
        }
    }

    #[test]
    fn graph_nodes_carry_their_kind_and_root_is_root() {
        let mut d = doc();
        let mut h = CommandHistory::new(200);
        h.execute(add_node(&d), &mut d);
        h.execute(rate_asset(), &mut d);

        let graph = h.history_graph();
        // Newest-first: timeline edit, vector edit, root.
        let kinds: Vec<HistoryEntryKind> = graph.iter().map(|n| n.kind).collect();
        assert_eq!(
            kinds,
            vec![
                HistoryEntryKind::Timeline,
                HistoryEntryKind::Vector,
                HistoryEntryKind::Root
            ]
        );
        // The root node is exactly the one classified Root.
        let root_ids: Vec<u64> = graph.iter().filter(|n| n.is_root).map(|n| n.id).collect();
        let root_kinds: Vec<u64> = graph
            .iter()
            .filter(|n| n.kind == HistoryEntryKind::Root)
            .map(|n| n.id)
            .collect();
        assert_eq!(root_ids, root_kinds);
    }

    /// Browsing the history is navigation, not editing: a jump (the panel's only
    /// write) must never append a node, and jumping away and back must land on
    /// the same HEAD. Without this, clicking around the Undo History panel would
    /// grow the very list it is showing.
    #[test]
    fn jumping_records_no_history_and_round_trips() {
        let mut d = doc();
        let mut h = CommandHistory::new(200);
        h.execute(add_node(&d), &mut d);
        h.execute(rate_asset(), &mut d);
        h.execute(add_node(&d), &mut d);

        let before = h.history_graph();
        let head = h.current_node();
        let oldest_edit = before
            .iter()
            .filter(|n| !n.is_root)
            .min_by_key(|n| n.id)
            .expect("an edit exists")
            .id;
        let doc_at_head = serde_json::to_string(&d).expect("serialize doc");

        assert!(h.jump_to_node(oldest_edit, &mut d));
        assert_eq!(h.current_node(), oldest_edit);
        assert_eq!(
            h.history_graph().len(),
            before.len(),
            "a jump must not append a history entry"
        );

        assert!(h.jump_to_node(head, &mut d));
        assert_eq!(h.current_node(), head);
        assert_eq!(
            h.history_graph().len(),
            before.len(),
            "the return jump must not append one either"
        );
        assert_eq!(
            serde_json::to_string(&d).expect("serialize doc"),
            doc_at_head,
            "jump away and back is document identity"
        );
    }

    /// The panel's "N branches" readout counts childless nodes in the flattened
    /// graph, so pin that the graph exposes the topology that makes it correct:
    /// one tip while linear, two after undo-then-edit forks the tree.
    #[test]
    fn graph_exposes_one_tip_per_divergent_path() {
        let tips = |h: &CommandHistory| {
            h.history_graph()
                .iter()
                .filter(|n| n.children.is_empty())
                .count()
        };

        let mut d = doc();
        let mut h = CommandHistory::new(200);
        assert_eq!(tips(&h), 1, "a bare root is its own single tip");

        h.execute(add_node(&d), &mut d);
        h.execute(add_node(&d), &mut d);
        assert_eq!(tips(&h), 1, "linear history: one tip");

        // Undo then edit → fork. The retained branch is a second tip.
        assert!(h.undo(&mut d));
        h.execute(add_node(&d), &mut d);
        assert_eq!(tips(&h), 2, "undo-then-edit must retain the old branch");
        assert!(
            h.history_graph()
                .iter()
                .any(|n| n.is_current && n.children.is_empty()),
            "HEAD is a leaf after the fork"
        );
    }
}
