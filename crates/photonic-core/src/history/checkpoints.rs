use super::*;

impl CommandHistory {
    // ── Persistence (save/restore the full history with the document) ─────────

    /// Capture the persistent history (undo/redo/checkpoints/branches) for
    /// serialization into a `.photon` file. Clones; does not mutate self.
    pub fn snapshot_state(&self) -> HistorySnapshot {
        // Also emit the legacy flat views (the current undo path and primary redo
        // chain) so older builds can still open the file; new builds prefer `tree`.
        let mut undo_stack: Vec<Command> = self
            .ancestor_edges(self.current)
            .into_iter()
            .filter_map(|id| self.nodes.get(&id).and_then(|n| n.command.clone()))
            .collect();
        undo_stack.reverse(); // oldest → newest
        let mut redo_stack: Vec<Command> = self
            .primary_chain(self.current, usize::MAX)
            .into_iter()
            .filter_map(|id| self.nodes.get(&id).and_then(|n| n.command.clone()))
            .collect();
        redo_stack.reverse(); // last element == the next redo, matching the old stack
        HistorySnapshot {
            undo_stack,
            redo_stack,
            checkpoints: self.checkpoints.clone(),
            branches: self.branches.clone(),
            tree: Some(self.export_tree()),
        }
    }

    /// Replace the persistent history with a restored snapshot (on file open),
    /// then re-enforce the current limits. Configured limits, debounce timers,
    /// and the revision counter are preserved. Bumps `revision` so revision-
    /// keyed caches refresh.
    pub fn restore_state(&mut self, s: HistorySnapshot) {
        // Prefer the full edit tree; fall back to reconstructing a linear tree
        // from the legacy flat stacks (files written before branching history).
        if let Some(tree) = s.tree {
            self.load_tree(tree);
        } else {
            self.rebuild_linear_tree(s.undo_stack, s.redo_stack);
        }
        self.checkpoints = s.checkpoints;
        self.branches = s.branches;
        self.warned_at_limit = false;
        self.pending_warning = None;
        self.revision = self.revision.wrapping_add(1);
        // A whole-document swap — no per-node affected-nodes set describes it,
        // so clear rather than push an entry. Forces `changes_since` to report
        // `overflowed = true` for any query spanning this point (03 §2.1).
        self.revision_ring.clear();
        self.enforce_steps();
        self.enforce_size();
    }

    /// Clear all persistent history (undo/redo/checkpoints/branches) while
    /// keeping the configured limits. Used when opening a document that carries
    /// no embedded history, or on New, so a previous project's history can't
    /// bleed into the freshly loaded one. Bumps `revision`.
    pub fn reset(&mut self) {
        self.init_empty_tree();
        self.checkpoints.clear();
        self.branches.clear();
        self.warned_at_limit = false;
        self.pending_warning = None;
        self.revision = self.revision.wrapping_add(1);
        self.revision_ring.clear();
    }

    // ── Checkpoints (git-style commits) ──────────────────────────────────

    /// Save a named snapshot of the document. Returns the new checkpoint ID.
    /// Keeps at most 50 checkpoints; oldest are dropped when the limit is reached.
    pub fn create_checkpoint(&mut self, name: String, doc: &Document) -> Uuid {
        let id = Uuid::new_v4();
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.checkpoints.push(Checkpoint {
            id,
            name,
            created_at,
            snapshot: doc.clone(),
        });
        const MAX_CHECKPOINTS: usize = 50;
        if self.checkpoints.len() > MAX_CHECKPOINTS {
            self.checkpoints.remove(0);
        }
        id
    }

    /// Return summary info for all checkpoints, oldest first.
    pub fn list_checkpoints(&self) -> Vec<CheckpointInfo> {
        self.checkpoints
            .iter()
            .map(|c| CheckpointInfo {
                id: c.id,
                name: c.name.clone(),
                created_at: c.created_at,
            })
            .collect()
    }

    /// Restore the document to a saved checkpoint. Clears undo/redo stacks.
    /// Returns the snapshot to replace the live document, or `None` if not found.
    pub fn restore_checkpoint(&mut self, id: Uuid) -> Option<Document> {
        let snapshot = self
            .checkpoints
            .iter()
            .find(|c| c.id == id)?
            .snapshot
            .clone();
        // The checkpoint snapshot becomes the new baseline — start history fresh.
        self.init_empty_tree();
        Some(snapshot)
    }

    /// Return a clone of the document snapshot at `id` without touching
    /// undo/redo stacks. Use this for read-only operations like diffing.
    pub fn get_checkpoint_snapshot(&self, id: Uuid) -> Option<Document> {
        self.checkpoints
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.snapshot.clone())
    }
}
