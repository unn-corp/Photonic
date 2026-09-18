//! Layer & group operations (group, collect-in-layer, release-to-layers,
//! merge-layers) extracted from app::mod. Methods on PhotonicApp.
#![allow(clippy::too_many_arguments)]
use super::*;
use photonic_core::layer::LayerId;

impl PhotonicApp {
    /// Group the currently selected nodes. Requires 2+ nodes in selection.
    pub(crate) fn do_group_selected(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        if doc.selection.count() < 2 {
            return;
        }
        let sel_ids: Vec<_> = doc.selection.ids().copied().collect();
        if let Some((layer_id, mut indexed)) = doc.nodes_layer_and_indices(&sel_ids) {
            indexed.sort_by_key(|(_, idx)| *idx);
            let children: Vec<_> = indexed.iter().map(|(id, _)| *id).collect();
            let insert_index = indexed[0].1;
            let group_kind = SceneNodeKind::Group(GroupNode {
                children: children.clone(),
                clip_children: false,
                clip_node_id: None,
                blend_spine_id: None,
                live_boolean: None,
            });
            let group = SceneNode::new("Group", layer_id, group_kind);
            let group_id = group.id;
            let cmd = Command::GroupNodes {
                group,
                layer_id,
                insert_index,
                children,
            };
            history.execute(cmd, doc);
            self.selected_id = Some(group_id);
            doc.selection = Selection::single(group_id);
            *doc_modified = true;
        }
    }

    /// Add a new empty layer at the top of the stack and make it the active
    /// layer so subsequently drawn objects land in it. One undoable step.
    pub(crate) fn do_add_layer(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        // Pick a name that doesn't collide with an existing layer.
        let mut n = doc.layers.len() + 1;
        let name = loop {
            let candidate = format!("Layer {n}");
            if !doc.layers.values().any(|l| l.name == candidate) {
                break candidate;
            }
            n += 1;
        };

        let new_layer = Layer::new(name);
        let new_layer_id = new_layer.id;
        let old_active = doc.active_layer_id;

        history.execute(
            Command::Batch(vec![
                Command::AddLayer { layer: new_layer },
                Command::SetActiveLayer {
                    old_id: old_active,
                    new_id: Some(new_layer_id),
                },
            ]),
            doc,
        );
        *doc_modified = true;
    }

    pub(crate) fn do_collect_in_new_layer(
        &mut self,
        node_ids: Vec<NodeId>,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        // Fall back to current selection when no explicit ids given
        let raw_ids: Vec<NodeId> = if node_ids.is_empty() {
            doc.selection.ids().copied().collect()
        } else {
            node_ids
        };
        if raw_ids.is_empty() {
            return;
        }

        // Resolve group children to their top-level ancestors (deduplicated)
        let mut resolved: Vec<NodeId> = Vec::new();
        for id in raw_ids {
            if let Some(tid) = doc.top_level_ancestor(id) {
                if !resolved.contains(&tid) {
                    resolved.push(tid);
                }
            }
        }
        if resolved.is_empty() {
            return;
        }

        let new_layer = Layer::new("Collected Layer");
        let new_layer_id = new_layer.id;

        let mut cmds = vec![Command::AddLayer { layer: new_layer }];
        for (i, nid) in resolved.iter().enumerate() {
            if let Some((old_layer_id, old_index)) = doc.node_layer_and_index(nid) {
                cmds.push(Command::MoveNodeToLayer {
                    node_id: *nid,
                    old_layer_id,
                    new_layer_id,
                    old_index,
                    new_index: i,
                });
            }
        }
        history.execute(Command::Batch(cmds), doc);
        *doc_modified = true;
    }

    pub(crate) fn do_release_to_layers(
        &mut self,
        node_ids: Vec<NodeId>,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        let raw_ids: Vec<NodeId> = if node_ids.is_empty() {
            doc.selection.ids().copied().collect()
        } else {
            node_ids
        };
        if raw_ids.is_empty() {
            return;
        }

        // Resolve group children to top-level ancestors (deduplicated).
        let mut resolved: Vec<NodeId> = Vec::new();
        for id in raw_ids {
            if let Some(tid) = doc.top_level_ancestor(id) {
                if !resolved.contains(&tid) {
                    resolved.push(tid);
                }
            }
        }
        if resolved.is_empty() {
            return;
        }

        // One new layer per node.
        let mut cmds: Vec<Command> = Vec::new();
        for (seq, nid) in resolved.iter().enumerate() {
            if let Some((old_layer_id, old_index)) = doc.node_layer_and_index(nid) {
                let new_layer = Layer::new(format!("Layer {}", seq + 1));
                let new_layer_id = new_layer.id;
                cmds.push(Command::AddLayer { layer: new_layer });
                cmds.push(Command::MoveNodeToLayer {
                    node_id: *nid,
                    old_layer_id,
                    new_layer_id,
                    old_index,
                    new_index: 0,
                });
            }
        }
        if !cmds.is_empty() {
            history.execute(Command::Batch(cmds), doc);
            *doc_modified = true;
        }
    }

    pub(crate) fn do_merge_layers(
        &mut self,
        layer_ids: Vec<photonic_core::layer::LayerId>,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        if layer_ids.len() < 2 {
            return;
        }
        // Validate
        for lid in &layer_ids {
            if !doc.layers.contains_key(lid) {
                return;
            }
        }

        // Target = first of the selected layers in document order (bottom-most).
        let target_id = match doc.layer_order.iter().find(|id| layer_ids.contains(id)) {
            Some(&id) => id,
            None => return,
        };

        let source_ids: Vec<_> = layer_ids
            .iter()
            .filter(|&&id| id != target_id)
            .copied()
            .collect();

        let mut cmds: Vec<Command> = Vec::new();

        // Process sources in document order.
        let ordered_sources: Vec<_> = doc
            .layer_order
            .iter()
            .filter(|id| source_ids.contains(id))
            .copied()
            .collect();

        let mut new_index_offset = doc.layers[&target_id].node_ids.len();

        for src_id in &ordered_sources {
            let src_layer = doc.layers[src_id].clone();
            for node_id in src_layer.node_ids.clone() {
                if let Some((old_layer_id, old_index)) = doc.node_layer_and_index(&node_id) {
                    cmds.push(Command::MoveNodeToLayer {
                        node_id,
                        old_layer_id,
                        new_layer_id: target_id,
                        old_index,
                        new_index: new_index_offset,
                    });
                    new_index_offset += 1;
                }
            }
            cmds.push(Command::RemoveLayerFull { layer: src_layer });
        }

        if !cmds.is_empty() {
            history.execute(Command::Batch(cmds), doc);
            *doc_modified = true;
        }
    }

    /// The layer new content should target: the active layer, else the topmost
    /// layer in the stack.
    fn target_layer_id(doc: &Document) -> Option<LayerId> {
        doc.active_layer_id
            .or_else(|| doc.layer_order.last().copied())
    }

    /// Delete a layer and everything it contains. Refuses to remove the last
    /// remaining layer. If the deleted layer was active, activation moves to a
    /// surviving layer. One undoable step.
    pub(crate) fn do_delete_layer(
        &mut self,
        layer_id: LayerId,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        if !doc.layers.contains_key(&layer_id) || doc.layer_order.len() < 2 {
            return;
        }
        let mut cmds = vec![Command::RemoveLayer { layer_id }];
        // Re-home the active pointer to a survivor before the layer is gone.
        if doc.active_layer_id == Some(layer_id) {
            let survivor = doc.layer_order.iter().copied().find(|id| *id != layer_id);
            cmds.push(Command::SetActiveLayer {
                old_id: doc.active_layer_id,
                new_id: survivor,
            });
        }
        history.execute(Command::Batch(cmds), doc);
        // Drop any dangling node selection that pointed into the removed layer.
        if self
            .selected_id
            .is_some_and(|id| !doc.nodes.contains_key(&id))
        {
            self.selected_id = None;
            doc.selection.clear();
        }
        *doc_modified = true;
    }

    /// Toggle a layer's visibility or lock flag (only the provided field is
    /// changed). Recorded via `UpdateLayer` so it round-trips through undo.
    pub(crate) fn do_set_layer_flag(
        &mut self,
        layer_id: LayerId,
        visible: Option<bool>,
        locked: Option<bool>,
        opacity: Option<f32>,
        blend_mode: Option<photonic_core::layer::BlendMode>,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        let Some(layer) = doc.layers.get(&layer_id) else {
            return;
        };
        let new_visible = visible.unwrap_or(layer.visible);
        let new_locked = locked.unwrap_or(layer.locked);
        let new_opacity = opacity.map(|o| o.clamp(0.0, 1.0)).unwrap_or(layer.opacity);
        let new_blend_mode = blend_mode.unwrap_or(layer.blend_mode);
        if new_visible == layer.visible
            && new_locked == layer.locked
            && new_opacity == layer.opacity
            && new_blend_mode == layer.blend_mode
        {
            return;
        }
        history.execute(
            Command::UpdateLayer {
                layer_id,
                old_name: layer.name.clone(),
                new_name: layer.name.clone(),
                old_visible: layer.visible,
                new_visible,
                old_locked: layer.locked,
                new_locked,
                old_color: layer.color,
                new_color: layer.color,
                old_is_template: layer.is_template,
                new_is_template: layer.is_template,
                old_opacity: layer.opacity,
                new_opacity,
                old_blend_mode: layer.blend_mode,
                new_blend_mode,
            },
            doc,
        );
        *doc_modified = true;
    }

    /// Duplicate a layer and deep-copy all its objects into a new layer (named
    /// "… Copy") inserted directly above the source. One undoable step. Mirrors
    /// the `duplicate_layer` MCP tool.
    pub(crate) fn do_duplicate_layer(
        &mut self,
        layer_id: LayerId,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        let Some(src) = doc.layers.get(&layer_id).cloned() else {
            return;
        };
        let mut new_layer = photonic_core::layer::Layer::new(format!("{} Copy", src.name));
        new_layer.visible = src.visible;
        new_layer.locked = src.locked;
        new_layer.opacity = src.opacity;
        new_layer.blend_mode = src.blend_mode;
        new_layer.color = src.color;
        new_layer.is_template = src.is_template;
        let new_layer_id = new_layer.id;

        let mut commands = vec![Command::AddLayer { layer: new_layer }];
        for &nid in &src.node_ids {
            if let Some(node) = doc.nodes.get(&nid) {
                let mut cloned = node.clone();
                cloned.id = uuid::Uuid::new_v4();
                cloned.name = format!("{} (copy)", node.name);
                cloned.layer_id = new_layer_id;
                commands.push(Command::AddNode {
                    node: cloned,
                    layer_id: Some(new_layer_id),
                });
            }
        }
        history.execute(Command::Batch(commands), doc);
        // Place the copy directly above the source in the stack.
        if let (Some(src_pos), Some(new_pos)) = (
            doc.layer_order.iter().position(|l| *l == layer_id),
            doc.layer_order.iter().position(|l| *l == new_layer_id),
        ) {
            let mut order = doc.layer_order.clone();
            let moved = order.remove(new_pos);
            let insert_at = order.iter().position(|l| *l == layer_id).unwrap_or(src_pos) + 1;
            order.insert(insert_at.min(order.len()), moved);
            if order != doc.layer_order {
                history.execute(
                    Command::ReorderLayers {
                        old_order: doc.layer_order.clone(),
                        new_order: order,
                    },
                    doc,
                );
            }
        }
        self.selected_layer_ids = vec![new_layer_id];
        *doc_modified = true;
    }

    /// Create a new empty group ("sublayer") nesting container at the top of the
    /// active layer and select it. Objects can then be dragged into it in the
    /// Layers tree. One undoable step.
    pub(crate) fn do_add_sublayer(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        let Some(layer_id) = Self::target_layer_id(doc) else {
            return;
        };
        let group_kind = SceneNodeKind::Group(GroupNode {
            children: Vec::new(),
            clip_children: false,
            clip_node_id: None,
            blend_spine_id: None,
            live_boolean: None,
        });
        let group = SceneNode::new("Sublayer", layer_id, group_kind);
        let group_id = group.id;
        history.execute(
            Command::AddNode {
                node: group,
                layer_id: Some(layer_id),
            },
            doc,
        );
        self.selected_id = Some(group_id);
        doc.selection = Selection::single(group_id);
        *doc_modified = true;
    }

    /// Add a layer mask using the smartest interpretation of the selection:
    /// a raster alpha mask when a single raster node is selected, otherwise a
    /// clipping mask built from a 2+ selection (topmost object clips the rest).
    pub(crate) fn do_add_layer_mask_smart(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        // ── Raster branch: give a selected raster node an editable alpha mask.
        if let Some(sel) = self.selected_id {
            if let Some(node) = doc.nodes.get(&sel) {
                if let SceneNodeKind::Raster(r) = &node.kind {
                    if r.mask.is_none() {
                        let w = r.image.width.max(1);
                        let h = r.image.height.max(1);
                        let mut new_node = node.clone();
                        if let SceneNodeKind::Raster(rn) = &mut new_node.kind {
                            rn.mask = Some(photonic_core::Mask::full(w, h));
                        }
                        history.execute(
                            Command::UpdateNode {
                                old: node.clone(),
                                new: new_node,
                            },
                            doc,
                        );
                        *doc_modified = true;
                        return;
                    }
                }
            }
        }

        // ── Vector branch: 2+ selected → group them and clip to the top object.
        if doc.selection.count() >= 2 {
            let sel_ids: Vec<NodeId> = doc.selection.ids().copied().collect();
            if let Some((layer_id, mut indexed)) = doc.nodes_layer_and_indices(&sel_ids) {
                indexed.sort_by_key(|(_, idx)| *idx);
                let children: Vec<NodeId> = indexed.iter().map(|(id, _)| *id).collect();
                let insert_index = indexed[0].1;
                let clip_node_id = children.last().copied();
                let group_kind = SceneNodeKind::Group(GroupNode {
                    children: children.clone(),
                    clip_children: false,
                    clip_node_id,
                    blend_spine_id: None,
                    live_boolean: None,
                });
                let group = SceneNode::new("Clip Mask", layer_id, group_kind);
                let group_id = group.id;
                history.execute(
                    Command::GroupNodes {
                        group,
                        layer_id,
                        insert_index,
                        children,
                    },
                    doc,
                );
                self.selected_id = Some(group_id);
                doc.selection = Selection::single(group_id);
                *doc_modified = true;
            }
        }
    }

    /// Create a non-destructive adjustment layer of `kind` (MCP adjustment
    /// vocabulary) at the top of the active layer, seeded with neutral defaults
    /// so it initially changes nothing until tuned in the Inspector.
    pub(crate) fn do_add_adjustment_layer(
        &mut self,
        kind: &str,
        doc: &mut Document,
        history: &mut CommandHistory,
        doc_modified: &mut bool,
    ) {
        let Some(layer_id) = Self::target_layer_id(doc) else {
            return;
        };
        let spec = default_adjustment_spec(kind);
        let name = format!("{} (adjustment)", adjustment_label(kind));
        let node = SceneNode::new(
            name,
            layer_id,
            SceneNodeKind::Raster(photonic_core::RasterNode::adjustment_layer(spec)),
        );
        let node_id = node.id;
        history.execute(
            Command::AddNode {
                node,
                layer_id: Some(layer_id),
            },
            doc,
        );
        self.selected_id = Some(node_id);
        doc.selection = Selection::single(node_id);
        *doc_modified = true;
    }
}

/// The curated adjustment types offered in the Layers-panel slide-up tray, as
/// `(mcp_kind, display_label)` pairs. Order defines the tile grid order.
pub(crate) const ADJUSTMENT_TILES: &[(&str, &str)] = &[
    ("brightness_contrast", "Bright"),
    ("levels", "Levels"),
    ("curves", "Curves"),
    ("exposure", "Exposure"),
    ("hue_saturation", "Hue/Sat"),
    ("vibrance", "Vibrance"),
    ("black_and_white", "B&W"),
    ("invert", "Invert"),
    ("posterize", "Poster"),
    ("threshold", "Thresh"),
];

/// Short display label for an adjustment kind (falls back to the raw name).
pub(crate) fn adjustment_label(kind: &str) -> &str {
    ADJUSTMENT_TILES
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, label)| *label)
        .unwrap_or(kind)
}

/// Build a neutral (identity) [`AdjustmentSpec`] for a curated adjustment kind
/// so a freshly-created adjustment layer is a no-op until the user tunes it.
fn default_adjustment_spec(kind: &str) -> photonic_core::AdjustmentSpec {
    use photonic_core::AdjustmentSpec as A;
    match kind {
        "levels" => A::Levels {
            in_black: 0.0,
            in_white: 1.0,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        },
        "curves" => A::Curves {
            rgb: Vec::new(),
            red: Vec::new(),
            green: Vec::new(),
            blue: Vec::new(),
        },
        "exposure" => A::Exposure { stops: 0.0 },
        "hue_saturation" => A::HueSaturation {
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
        },
        "vibrance" => A::Vibrance { amount: 0.0 },
        "black_and_white" => A::BlackAndWhite {
            weights: [0.299, 0.587, 0.114],
        },
        "invert" => A::Invert,
        "posterize" => A::Posterize { levels: 4 },
        "threshold" => A::Threshold { level: 0.5 },
        // brightness_contrast + any unknown kind → neutral brightness/contrast.
        _ => A::BrightnessContrast {
            brightness: 0.0,
            contrast: 0.0,
        },
    }
}
