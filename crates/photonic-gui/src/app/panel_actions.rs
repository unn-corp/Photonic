use super::*;

impl PhotonicApp {
    pub(crate) fn process_panel_actions(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        view: &mut CanvasView,
        renderer: &mut PhotonicRenderer,
        history: &mut CommandHistory,
        mut doc_modified: bool,
    ) -> bool {
        // ── Drain panel actions (z-order, boolean ops) ───────────────────────
        // Use take() so `self` is not borrowed during the loop, allowing calls
        // to &self/&mut self methods (build_shape_with_tool, do_group_selected).
        'actions: for action in std::mem::take(&mut self.pending_panel_actions) {
            match action {
                PanelAction::SelectNode { node_id } => {
                    if doc.nodes.contains_key(&node_id) {
                        self.selected_id = Some(node_id);
                        doc.selection = Selection::single(node_id);
                        doc_modified = true;
                    }
                }
                PanelAction::CenterViewOn { canvas_x, canvas_y } => {
                    // Recenter the canvas on the clicked Navigator point, keeping
                    // the current zoom. Mirrors `fit_artboard_to_rect`'s pan math.
                    if let Some(canvas_rect) = self.last_canvas_rect {
                        view.pan_x = canvas_rect.center().x as f64 - canvas_x * view.zoom;
                        view.pan_y = canvas_rect.center().y as f64 - canvas_y * view.zoom;
                        // Snap the smooth-pan velocity so inertia doesn't fight the jump.
                        self.smooth.pan_vel_x = 0.0;
                        self.smooth.pan_vel_y = 0.0;
                        ctx.request_repaint();
                    }
                }
                // ── Media pool (video mode, 05 §2) ───────────────────────────
                PanelAction::MediaImportDialog { bin } => {
                    if self.import_media_files(doc, history, bin) {
                        doc_modified = true;
                    }
                }
                PanelAction::MediaCreateBin { name, parent } => {
                    use photonic_core::timeline::ops;
                    timeline::ops_bridge::ensure_project_and_sequence(
                        doc,
                        history,
                        photonic_core::timeline::FrameRate::FPS_30,
                    );
                    history.execute_discrete(Command::Timeline(ops::create_bin(name, parent)), doc);
                    doc_modified = true;
                }
                PanelAction::MediaRemoveBin { bin } => {
                    use photonic_core::timeline::ops;
                    if let Some(p) = doc.timeline.as_ref() {
                        if let Ok(cmd) = ops::remove_bin(p, bin) {
                            history.execute_discrete(Command::Timeline(cmd), doc);
                            doc_modified = true;
                        }
                    }
                    if self.media_pool_ui.current_bin == Some(bin) {
                        self.media_pool_ui.current_bin = None;
                    }
                }
                PanelAction::MediaRemoveAsset { asset } => {
                    use photonic_core::timeline::ops;
                    if let Some(p) = doc.timeline.as_ref() {
                        if let Ok(cmd) = ops::remove_asset(p, asset) {
                            history.execute_discrete(Command::Timeline(cmd), doc);
                            doc_modified = true;
                        }
                    }
                    if self.media_pool_ui.selected == Some(asset) {
                        self.media_pool_ui.selected = None;
                    }
                }
                PanelAction::MediaRemoveUnused => {
                    use photonic_core::timeline::ops;
                    if let Some(p) = doc.timeline.as_ref() {
                        let cmds = ops::remove_unused_assets(p);
                        if !cmds.is_empty() {
                            let batch = cmds.into_iter().map(Command::Timeline).collect();
                            history.execute_discrete(Command::Batch(batch), doc);
                            doc_modified = true;
                            self.media_pool_ui.selected = None;
                        }
                    }
                }
                PanelAction::MediaSetRating { asset, rating } => {
                    use photonic_core::timeline::ops;
                    if let Some(p) = doc.timeline.as_ref() {
                        if let Ok(cmd) = ops::set_asset_rating(p, asset, rating) {
                            history.execute_discrete(Command::Timeline(cmd), doc);
                            doc_modified = true;
                        }
                    }
                }
                PanelAction::MediaSetTags { asset, tags } => {
                    use photonic_core::timeline::ops;
                    if let Some(p) = doc.timeline.as_ref() {
                        if let Ok(cmds) = ops::set_asset_tags_resolved(p, asset, tags) {
                            if !cmds.is_empty() {
                                let batch = cmds.into_iter().map(Command::Timeline).collect();
                                history.execute_discrete(Command::Batch(batch), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }
                PanelAction::MediaCreateSubclip {
                    asset,
                    in_ticks,
                    out_ticks,
                    name,
                } => {
                    use photonic_core::timeline::Tick;
                    if let Some(id) = crate::app::timeline::ops_bridge::create_subclip(
                        doc,
                        history,
                        asset,
                        (Tick(in_ticks), Tick(out_ticks)),
                        name,
                    ) {
                        self.media_pool_ui.selected = Some(id);
                        doc_modified = true;
                    }
                }
                PanelAction::MediaAssignBin { asset, bin } => {
                    use photonic_core::timeline::ops;
                    if let Some(p) = doc.timeline.as_ref() {
                        if let Ok(cmd) = ops::assign_asset_bin(p, asset, bin) {
                            history.execute_discrete(Command::Timeline(cmd), doc);
                            doc_modified = true;
                        }
                    }
                }
                PanelAction::MediaRelink { asset } => {
                    use photonic_core::timeline::ops;
                    let dialog = rfd::FileDialog::new().set_title("Relink media");
                    let picked = super::run_file_dialog(move || dialog.pick_file());
                    if let Some(new_path) = picked {
                        if let Some(p) = doc.timeline.as_ref() {
                            if let Ok(cmd) = ops::relink_asset(p, asset, new_path) {
                                history.execute_discrete(Command::Timeline(cmd), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }
                PanelAction::MediaSetProxyMode { mode } => {
                    if let Some(bridge) = self.engine.as_mut() {
                        // Reconciled into `EngineCmd::SetProxyMode` next frame.
                        bridge.proxy_mode = mode;
                    }
                }
                PanelAction::MediaGenerateProxies => {
                    let assets = doc
                        .timeline
                        .as_ref()
                        .map(|project| project.media.assets.values().cloned().collect())
                        .unwrap_or_default();
                    self.media_pool_ui
                        .spawn_proxy_generation(assets, self.current_file.clone());
                }
                PanelAction::MediaSetGenerateProxiesOnImport { enabled } => {
                    use photonic_core::timeline::ops;
                    if let Some(project) = doc.timeline.as_ref() {
                        let cmd = ops::set_generate_proxies_on_import(project, enabled);
                        history.execute_discrete(Command::Timeline(cmd), doc);
                        doc_modified = true;
                    }
                }
                PanelAction::MediaAttachProxy { asset } => {
                    // G-15A: file picker → validate (ffprobe) → optional mismatch
                    // override → undoable set_asset_proxy with Attached origin.
                    use photonic_core::timeline::{ops, AssetSource};
                    let Some(project) = doc.timeline.as_ref() else {
                        continue;
                    };
                    let Some(media) = project.media.assets.get(&asset) else {
                        continue;
                    };
                    let AssetSource::File { path: original, .. } = &media.source else {
                        continue;
                    };
                    let original = original.clone();
                    let dialog = rfd::FileDialog::new()
                        .set_title("Attach Proxy")
                        .add_filter("Video", &["mp4", "mov", "mxf", "mkv", "m4v", "avi", "webm"]);
                    let picked = super::run_file_dialog(move || dialog.pick_file());
                    let Some(proxy_path) = picked else {
                        continue;
                    };
                    let tools = match photonic_video::media::ffmpeg_locate::locate() {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::warn!("attach proxy: ffmpeg unavailable: {e}");
                            continue;
                        }
                    };
                    let validation = match photonic_video::media::proxy::validate_attach(
                        &tools,
                        &original,
                        &proxy_path,
                        false,
                    ) {
                        Ok(v) => v,
                        Err(
                            photonic_video::media::proxy::AttachError::DurationMismatch
                            | photonic_video::media::proxy::AttachError::FrameRateMismatch,
                        ) => {
                            // Soft override: re-validate with allow_mismatch when
                            // the user confirms. rfd has no custom modal; use a
                            // second pick-style MessageDialog if available, else
                            // proceed after logging a clear warning.
                            let proceed = rfd::MessageDialog::new()
                                .set_title("Proxy mismatch")
                                .set_description(
                                    "Proxy duration or frame rate does not match the original. Attach anyway?",
                                )
                                .set_buttons(rfd::MessageButtons::YesNo)
                                .show()
                                == rfd::MessageDialogResult::Yes;
                            if !proceed {
                                continue;
                            }
                            match photonic_video::media::proxy::validate_attach(
                                &tools,
                                &original,
                                &proxy_path,
                                true,
                            ) {
                                Ok(v) => v,
                                Err(e) => {
                                    tracing::warn!("attach proxy failed after override: {e}");
                                    continue;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("attach proxy failed: {e}");
                            let _ = rfd::MessageDialog::new()
                                .set_title("Attach Proxy failed")
                                .set_description(e.to_string())
                                .set_buttons(rfd::MessageButtons::Ok)
                                .show();
                            continue;
                        }
                    };
                    if !validation.warnings.is_empty() {
                        tracing::info!("attach proxy warnings: {}", validation.warnings.join("; "));
                    }
                    if let Some(project) = doc.timeline.as_ref() {
                        if let Ok(cmd) =
                            ops::set_asset_proxy(project, asset, Some(validation.proxy))
                        {
                            history.execute_discrete(Command::Timeline(cmd), doc);
                            doc_modified = true;
                        }
                    }
                }
                PanelAction::MediaDetachProxy { asset } => {
                    // Clears the ProxyRef only — never deletes disk files
                    // (Attached user files stay; Generated cleanup is out of
                    // scope for G-15A).
                    use photonic_core::timeline::ops;
                    if let Some(project) = doc.timeline.as_ref() {
                        if let Ok(cmd) = ops::set_asset_proxy(project, asset, None) {
                            history.execute_discrete(Command::Timeline(cmd), doc);
                            doc_modified = true;
                        }
                    }
                }
                PanelAction::MediaInsertAtPlayhead { asset } => {
                    let at = self.playhead;
                    if timeline::ops_bridge::insert_asset_at_first_fit(doc, history, asset, at) {
                        doc_modified = true;
                    }
                }
                // ── Captions (video mode) ─────────────────────────────────────
                PanelAction::CaptionEditBatch(cmds) => {
                    if !cmds.is_empty() {
                        let batch = cmds.into_iter().map(Command::Timeline).collect();
                        history.execute_discrete(Command::Batch(batch), doc);
                        doc_modified = true;
                    }
                }
                // ── Marker navigation (video mode, 26 K-A2) ──────────────────
                // Deliberately does NOT touch `history` or set `doc_modified`:
                // moving the playhead is session state, and a review pass that
                // walks twenty markers must leave the undo stack untouched.
                // Assigning the field is enough — `app/monitor.rs` notices the
                // disagreement with `bridge.agreed_playhead` and issues the seek.
                PanelAction::SeekPlayhead { at } => {
                    self.playhead = at.max(photonic_core::timeline::Tick::ZERO);
                }
                // ── Clip inspector / effects browser (video mode) ────────────
                PanelAction::ClipEditDiscrete(cmd) => {
                    history.execute_discrete(Command::Timeline(cmd), doc);
                    doc_modified = true;
                }
                PanelAction::ClipEditCoalesced(cmd) => {
                    history.execute(Command::Timeline(cmd), doc);
                    doc_modified = true;
                }
                PanelAction::ClipEditBatch(cmds) => {
                    if !cmds.is_empty() {
                        let batch = cmds.into_iter().map(Command::Timeline).collect();
                        history.execute_discrete(Command::Batch(batch), doc);
                        doc_modified = true;
                    }
                }
                PanelAction::ReorderNode { node_id, op } => {
                    if let Some((layer_id, cur_idx)) = doc.node_layer_and_index(&node_id) {
                        let layer_len = doc
                            .layers
                            .get(&layer_id)
                            .map(|l| l.node_ids.len())
                            .unwrap_or(0);
                        if layer_len > 0 {
                            let new_index = match op {
                                ZOrderOp::SendToBack => 0,
                                ZOrderOp::BringToFront => layer_len - 1,
                                ZOrderOp::SendBackward => cur_idx.saturating_sub(1),
                                ZOrderOp::BringForward => (cur_idx + 1).min(layer_len - 1),
                            };
                            if new_index != cur_idx {
                                let cmd = Command::ReorderNode {
                                    layer_id,
                                    node_id,
                                    old_index: cur_idx,
                                    new_index,
                                };
                                history.execute(cmd, doc);
                                doc_modified = true;
                            }
                        }
                    }
                }
                PanelAction::BooleanOp(bool_op) => {
                    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
                    // Divide isn't a fold-into-one op — route the user to it.
                    if matches!(bool_op, BooleanOp::Divide) {
                        self.file_status =
                            Some("Use Pathfinder ▸ Divide to split overlapping shapes".into());
                    } else {
                        // Every selected PATH node, sorted bottom-to-top by the
                        // document's global draw order (works across layers, not
                        // just two same-layer objects like before).
                        let order: Vec<NodeId> =
                            doc.nodes_in_draw_order().iter().map(|n| n.id).collect();
                        let order_of =
                            |id: &NodeId| order.iter().position(|x| x == id).unwrap_or(usize::MAX);
                        let mut path_ids: Vec<NodeId> = doc
                            .selection
                            .ids()
                            .copied()
                            .filter(|id| {
                                matches!(
                                    doc.get_node(id).map(|n| &n.kind),
                                    Some(SceneNodeKind::Path(_))
                                )
                            })
                            .collect();
                        path_ids.sort_by_key(order_of);

                        if path_ids.len() < 2 {
                            self.file_status =
                                Some("Select 2 or more path shapes to combine".into());
                        } else {
                            // Bake each shape's transform into its geometry so the
                            // boolean runs in shared canvas space.
                            let baked: Vec<PathData> = path_ids
                                .iter()
                                .filter_map(|id| {
                                    doc.get_node(id).and_then(|n| match &n.kind {
                                        SceneNodeKind::Path(p) => Some(gui_apply_affine_to_path(
                                            &p.path_data,
                                            n.transform.to_kurbo(),
                                        )),
                                        _ => None,
                                    })
                                })
                                .collect();
                            // Fold the op across all shapes, bottom shape as the
                            // base (matches Minus-Front / additive Union semantics).
                            let mut acc = baked[0].clone();
                            let mut err: Option<String> = None;
                            for p in &baked[1..] {
                                match boolean_op(&acc, p, bool_op) {
                                    Ok(r) => acc = r,
                                    Err(e) => {
                                        err = Some(e.to_string());
                                        break;
                                    }
                                }
                            }

                            if let Some(e) = err {
                                self.file_status = Some(format!("Combine failed: {e}"));
                            } else if acc.to_bez_path().elements().is_empty() {
                                self.file_status = Some(
                                    "Combine produced an empty shape (do they overlap?)".into(),
                                );
                            } else {
                                // Result inherits the bottom shape's layer + look.
                                let base_id = path_ids[0];
                                let base = doc.get_node(&base_id).unwrap();
                                let base_layer = base.layer_id;
                                let (fill, stroke) = match &base.kind {
                                    SceneNodeKind::Path(p) => (p.fill.clone(), p.stroke.clone()),
                                    _ => Default::default(),
                                };
                                let op_name = match bool_op {
                                    BooleanOp::Union => "Union",
                                    BooleanOp::Subtract => "Subtract",
                                    BooleanOp::Intersect => "Intersect",
                                    BooleanOp::Exclude => "Exclude",
                                    BooleanOp::Divide => "Divide",
                                };
                                let mut result_pn = PathNode::new(acc);
                                result_pn.fill = fill;
                                result_pn.stroke = stroke;
                                let result_node = SceneNode::new(
                                    op_name,
                                    base_layer,
                                    SceneNodeKind::Path(result_pn),
                                );
                                let result_id = result_node.id;
                                let mut cmds: Vec<Command> = path_ids
                                    .iter()
                                    .map(|id| Command::RemoveNode { node_id: *id })
                                    .collect();
                                cmds.push(Command::AddNode {
                                    node: result_node,
                                    layer_id: Some(base_layer),
                                });
                                history.execute(Command::Batch(cmds), doc);
                                self.selected_id = Some(result_id);
                                doc.selection = Selection::single(result_id);
                                doc_modified = true;
                                self.file_status =
                                    Some(format!("{op_name}: combined {} shapes", path_ids.len()));
                            }
                        }
                    }
                }
                PanelAction::RestoreCheckpoint(id) => {
                    if let Some(snapshot) = history.restore_checkpoint(id) {
                        *doc = snapshot;
                        self.selected_id = None;
                        doc.selection.clear();
                        doc_modified = true;
                    }
                }
                PanelAction::UpdateNodeFill { node_id, fill } => {
                    // #171: defer recording the solid fill color into the Recent
                    // list until the pointer is released (see post-loop commit),
                    // so dragging inside the picker doesn't stream the whole path.
                    if let photonic_core::style::FillKind::Solid(c) = &fill.kind {
                        self.pending_recent_color = Some(*c);
                    }
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let mut new_node = node.clone();
                        if let SceneNodeKind::Path(pn) = &mut new_node.kind {
                            pn.fill = fill;
                        }
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }
                PanelAction::UpdateNodeStroke { node_id, stroke } => {
                    // #171: defer recording the stroke color into the Recent list
                    // until the pointer is released (see post-loop commit), so
                    // dragging inside the picker doesn't stream the whole path.
                    if stroke.enabled {
                        self.pending_recent_color = Some(stroke.color);
                    }
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let mut new_node = node.clone();
                        if let SceneNodeKind::Path(pn) = &mut new_node.kind {
                            pn.stroke = stroke;
                        }
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::UpdateNodesFill { node_ids, fill } => {
                    // Apply one fill to the whole selection as a single undoable
                    // batch. Covers path and text nodes (the fill-bearing kinds).
                    if let photonic_core::style::FillKind::Solid(c) = &fill.kind {
                        self.pending_recent_color = Some(*c);
                    }
                    let mut cmds: Vec<Command> = Vec::new();
                    for id in &node_ids {
                        if let Some(node) = doc.nodes.get(id) {
                            let mut new_node = node.clone();
                            let changed = match &mut new_node.kind {
                                SceneNodeKind::Path(pn) => {
                                    pn.fill = fill.clone();
                                    true
                                }
                                SceneNodeKind::Text(tn) => {
                                    tn.fill = fill.clone();
                                    true
                                }
                                _ => false,
                            };
                            if changed {
                                cmds.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        doc_modified = true;
                    }
                }
                PanelAction::UpdateNodesStroke { node_ids, stroke } => {
                    if stroke.enabled {
                        self.pending_recent_color = Some(stroke.color);
                    }
                    let mut cmds: Vec<Command> = Vec::new();
                    for id in &node_ids {
                        if let Some(node) = doc.nodes.get(id) {
                            let mut new_node = node.clone();
                            let changed = match &mut new_node.kind {
                                SceneNodeKind::Path(pn) => {
                                    pn.stroke = stroke.clone();
                                    true
                                }
                                SceneNodeKind::Text(tn) => {
                                    tn.stroke = stroke.clone();
                                    true
                                }
                                _ => false,
                            };
                            if changed {
                                cmds.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::UpdateNodeOuterGlow { node_id, glow } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let mut new_node = node.clone();
                        new_node.outer_glow = glow;
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::UpdateNodeInnerGlow { node_id, glow } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let mut new_node = node.clone();
                        new_node.inner_glow = glow;
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::UpdateNodeGaussianGlow { node_id, glow } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let mut new_node = node.clone();
                        new_node.gaussian_glow = glow;
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::SetLocked { node_id, locked } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let mut new_node = node.clone();
                        new_node.locked = locked;
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::SetVisible { node_id, visible } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let mut new_node = node.clone();
                        new_node.visible = visible;
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::SetNodePosition { node_id, x, y } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let mut new_node = node.clone();
                        new_node.transform.matrix[4] = x;
                        new_node.transform.matrix[5] = y;
                        let cmd = Command::UpdateNode {
                            old: node.clone(),
                            new: new_node,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::SetNodeOpacity { node_id, opacity } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let opacity = opacity.clamp(0.0, 1.0);
                        if (node.opacity - opacity).abs() > f32::EPSILON {
                            let mut new_node = node.clone();
                            new_node.opacity = opacity;
                            let cmd = Command::UpdateNode {
                                old: node.clone(),
                                new: new_node,
                            };
                            history.execute(cmd, doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::SetNodeBlendMode {
                    node_id,
                    blend_mode,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if node.blend_mode != blend_mode {
                            let mut new_node = node.clone();
                            new_node.blend_mode = blend_mode;
                            let cmd = Command::UpdateNode {
                                old: node.clone(),
                                new: new_node,
                            };
                            history.execute(cmd, doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::SetNodeEffects { node_id, effects } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if node.effects != effects {
                            let mut new_node = node.clone();
                            new_node.effects = effects;
                            let cmd = Command::UpdateNode {
                                old: node.clone(),
                                new: new_node,
                            };
                            history.execute(cmd, doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::SetNodeSize {
                    node_id,
                    width,
                    height,
                } => {
                    if width > 1e-6 && height > 1e-6 {
                        if let Some(node) = doc.nodes.get(&node_id).cloned() {
                            if let Some(local) = node.local_bounds() {
                                let affine = node.transform.to_kurbo();
                                let corners_x = [local.x0, local.x1, local.x1, local.x0];
                                let corners_y = [local.y0, local.y0, local.y1, local.y1];
                                let (mut ax, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
                                let (mut ay, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
                                for i in 0..4 {
                                    let p = affine * Point::new(corners_x[i], corners_y[i]);
                                    if p.x < ax {
                                        ax = p.x;
                                    }
                                    if p.x > max_x {
                                        max_x = p.x;
                                    }
                                    if p.y < ay {
                                        ay = p.y;
                                    }
                                    if p.y > max_y {
                                        max_y = p.y;
                                    }
                                }
                                let cur_w = max_x - ax;
                                let cur_h = max_y - ay;
                                if cur_w > 1e-9 && cur_h > 1e-9 {
                                    let sx = width / cur_w;
                                    let sy = height / cur_h;
                                    let scale_t = photonic_core::transform::Transform::scale_around(
                                        sx, sy, ax, ay,
                                    );
                                    let mut new_node = node.clone();
                                    new_node.transform = node.transform.then(&scale_t);
                                    history.execute(
                                        Command::UpdateNode {
                                            old: node,
                                            new: new_node,
                                        },
                                        doc,
                                    );
                                    doc_modified = true;
                                }
                            }
                        }
                    }
                }

                PanelAction::RotateNode {
                    node_ids,
                    angle_deg,
                } => {
                    // node_ids[0] is the primary: its current angle defines the delta.
                    if let Some(&primary_id) = node_ids.first() {
                        if let Some(primary) = doc.nodes.get(&primary_id).cloned() {
                            let [a, b, _c, _d, _e, _f] = primary.transform.matrix;
                            let current_rad = b.atan2(a);
                            let delta_rad = angle_deg.to_radians() - current_rad;
                            // Shared pivot: center of the selection's world bounds when
                            // multiple are selected; the node's own center otherwise.
                            let (cx, cy) = if node_ids.len() > 1 {
                                selection_canvas_bounds(doc, &node_ids, renderer)
                                    .map(|(x0, y0, x1, y1)| ((x0 + x1) / 2.0, (y0 + y1) / 2.0))
                                    .unwrap_or((
                                        primary.transform.matrix[4],
                                        primary.transform.matrix[5],
                                    ))
                            } else {
                                match primary.local_bounds() {
                                    Some(local) => {
                                        let c = primary.transform.to_kurbo()
                                            * Point::new(
                                                (local.x0 + local.x1) / 2.0,
                                                (local.y0 + local.y1) / 2.0,
                                            );
                                        (c.x, c.y)
                                    }
                                    None => {
                                        (primary.transform.matrix[4], primary.transform.matrix[5])
                                    }
                                }
                            };
                            let rot_t = photonic_core::transform::Transform::rotate_around(
                                delta_rad, cx, cy,
                            );
                            let mut cmds = Vec::new();
                            for nid in &node_ids {
                                if let Some(node) = doc.nodes.get(nid) {
                                    let mut new_node = node.clone();
                                    // Apply in WORLD space: node transform first, then
                                    // the rotation about the shared pivot.
                                    new_node.transform = rot_t.then(&node.transform);
                                    cmds.push(Command::UpdateNode {
                                        old: node.clone(),
                                        new: new_node,
                                    });
                                }
                            }
                            if !cmds.is_empty() {
                                history.execute(Command::Batch(cmds), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::DuplicateNode { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        let mut copy = node.clone();
                        copy.id = uuid::Uuid::new_v4();
                        copy.name = format!("{} copy", copy.name);
                        copy.transform.matrix[4] += 10.0;
                        copy.transform.matrix[5] += 10.0;
                        let lid = copy.layer_id;
                        let copy_id = copy.id;
                        let cmd = Command::AddNode {
                            node: copy,
                            layer_id: Some(lid),
                        };
                        history.execute(cmd, doc);
                        doc.selection = Selection::single(copy_id);
                        self.selected_id = Some(copy_id);
                        doc_modified = true;
                    }
                }

                PanelAction::DeleteNode { node_id } => {
                    history.execute(Command::RemoveNode { node_id }, doc);
                    if self.selected_id == Some(node_id) {
                        self.selected_id = None;
                        doc.selection.clear();
                    }
                    doc_modified = true;
                }

                PanelAction::OutlineStroke { node_ids } => {
                    // Illustrator "Outline Stroke": turn each path's stroke into a
                    // filled shape. If the path also has a fill, keep the fill on
                    // the original (stroke removed) and add the outline above it;
                    // otherwise the stroke becomes the shape in place.
                    use photonic_core::ops::stroke_outline::{
                        outline_stroke_with_scale as do_outline, transform_uniform_scale,
                    };
                    use photonic_core::style::{Fill, FillKind, Stroke as CoreStroke};
                    let mut cmds: Vec<Command> = Vec::new();
                    let mut new_selection: Vec<NodeId> = Vec::new();
                    for id in &node_ids {
                        let Some(node) = doc.nodes.get(id) else {
                            continue;
                        };
                        let SceneNodeKind::Path(pn) = &node.kind else {
                            continue;
                        };
                        if !pn.stroke.enabled || pn.stroke.width <= 0.0 {
                            continue;
                        }
                        // Photonic strokes are non-scaling: divide the width by the
                        // object's transform scale so the outline matches the drawn
                        // stroke. The outline node keeps this same transform.
                        let obj_scale = transform_uniform_scale(&node.transform.matrix);
                        let Ok(outline) = do_outline(&pn.path_data, &pn.stroke, obj_scale) else {
                            continue;
                        };
                        // Outline fill = stroke color, with stroke opacity folded in.
                        let mut fill_color = pn.stroke.color;
                        fill_color.a *= pn.stroke.opacity;
                        let outline_fill = Fill::solid(fill_color);
                        let has_fill = pn.fill.enabled && !matches!(pn.fill.kind, FillKind::None);

                        if has_fill {
                            let mut original_no_stroke = node.clone();
                            if let SceneNodeKind::Path(p) = &mut original_no_stroke.kind {
                                p.stroke = CoreStroke::none();
                            }
                            let mut outline_node = node.clone();
                            outline_node.id = uuid::Uuid::new_v4();
                            outline_node.name = format!("{} Outline", node.name);
                            if let SceneNodeKind::Path(p) = &mut outline_node.kind {
                                p.path_data = outline;
                                p.fill = outline_fill;
                                p.stroke = CoreStroke::none();
                                p.is_compound = true;
                            }
                            let layer_id = outline_node.layer_id;
                            new_selection.push(outline_node.id);
                            cmds.push(Command::UpdateNode {
                                old: node.clone(),
                                new: original_no_stroke,
                            });
                            cmds.push(Command::AddNode {
                                node: outline_node,
                                layer_id: Some(layer_id),
                            });
                        } else {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Path(p) = &mut new_node.kind {
                                p.path_data = outline;
                                p.fill = outline_fill;
                                p.stroke = CoreStroke::none();
                                p.is_compound = true;
                            }
                            new_selection.push(*id);
                            cmds.push(Command::UpdateNode {
                                old: node.clone(),
                                new: new_node,
                            });
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        doc.selection = Selection::from_ids(new_selection.iter().copied());
                        self.selected_id = new_selection.first().copied();
                        doc_modified = true;
                    }
                }

                PanelAction::StartRasterColorRange { node_id } => {
                    // Arm the eyedropper; the click samples the raster's own
                    // pixels and begins the preview session.
                    self.eyedropper.target = Some(EyedropperTarget::RasterColorRange { node_id });
                    self.eyedropper.skip_click = true;
                }

                PanelAction::SetRasterColorRangeParams {
                    tolerance,
                    contiguous,
                } => {
                    self.raster_mask_tolerance = tolerance;
                    self.raster_mask_contiguous = contiguous;
                    if self.raster_color_range.is_some() {
                        self.refresh_raster_color_range(doc);
                        doc_modified = true;
                    }
                }

                PanelAction::ApplyRasterColorRange => {
                    if self.apply_raster_color_range(doc, history) {
                        doc_modified = true;
                    }
                }

                PanelAction::CancelRasterColorRange => {
                    if self.raster_color_range.is_some() {
                        self.cancel_raster_color_range(doc);
                        doc_modified = true;
                    }
                }

                PanelAction::ClearRasterMask { node_id } => {
                    // Discard any live preview on this node first so the mask
                    // removal starts from the committed state.
                    if self
                        .raster_color_range
                        .as_ref()
                        .is_some_and(|s| s.node_id == node_id)
                    {
                        self.cancel_raster_color_range(doc);
                    }
                    if let Some(node) = doc.get_node(&node_id) {
                        let has_mask =
                            matches!(&node.kind, SceneNodeKind::Raster(r) if r.mask.is_some());
                        if has_mask {
                            let mut updated = node.clone();
                            if let SceneNodeKind::Raster(rn) = &mut updated.kind {
                                rn.mask = None;
                            }
                            history.execute(
                                Command::UpdateNode {
                                    old: node.clone(),
                                    new: updated,
                                },
                                doc,
                            );
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::RasterRemoveBackground { node_id } => {
                    self.start_remove_background(doc, node_id);
                }

                PanelAction::CropRasterToArtboard { node_id } => {
                    // The crop resizes the pixel buffer, which would desync a
                    // live color-range preview's `original` — discard it first.
                    if self
                        .raster_color_range
                        .as_ref()
                        .is_some_and(|s| s.node_id == node_id)
                    {
                        self.cancel_raster_color_range(doc);
                    }
                    if let Some(node) = doc.get_node(&node_id) {
                        let dims = |n: &SceneNode| match &n.kind {
                            SceneNodeKind::Raster(r) => (r.image.width, r.image.height),
                            _ => (0, 0),
                        };
                        let before = (dims(node), node.transform.matrix);
                        // Crop to the artboard the image is actually on (the
                        // one it overlaps most — spatial multi-artboard model),
                        // not the document rect at the origin.
                        let (iw, ih) = dims(node);
                        let [a, _, _, d, tx, ty] = node.transform.matrix;
                        let (ax0, ay0, ax1, ay1) = match doc.artboard_for_rect(
                            tx,
                            ty,
                            tx + a * iw as f64,
                            ty + d * ih as f64,
                        ) {
                            Some(ab) => ab.rect(),
                            None => {
                                self.file_status =
                                    Some("Crop failed: image does not overlap any artboard".into());
                                continue 'actions;
                            }
                        };
                        let mut updated = node.clone();
                        match updated.crop_raster_to_rect(ax0, ay0, ax1, ay1) {
                            Ok(()) => {
                                if (dims(&updated), updated.transform.matrix) != before {
                                    let (w, h) = dims(&updated);
                                    history.execute(
                                        Command::UpdateNode {
                                            old: node.clone(),
                                            new: updated,
                                        },
                                        doc,
                                    );
                                    self.file_status =
                                        Some(format!("Cropped image to artboard ({w}×{h})"));
                                    doc_modified = true;
                                } else {
                                    self.file_status =
                                        Some("Image is already inside the artboard".into());
                                }
                            }
                            Err(e) => self.file_status = Some(format!("Crop failed: {e}")),
                        }
                    }
                }

                PanelAction::AddAnchorPoints { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            let new_path = pn.path_data.subdivide(1);
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                new_pn.path_data = new_path;
                            }
                            let cmd = Command::UpdateNode {
                                old: node.clone(),
                                new: new_node,
                            };
                            history.execute(cmd, doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::JoinPaths { node_ids } => {
                    use photonic_core::ops::join::{close_open_paths, join_two_paths};
                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };

                    if ids.len() == 1 {
                        let nid = ids[0];
                        if let Some(node) = doc.nodes.get(&nid) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                let closed = close_open_paths(&pn.path_data);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = closed;
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
                    } else if ids.len() == 2 {
                        let id_a = ids[0];
                        let id_b = ids[1];
                        if let (Some(node_a), Some(node_b)) =
                            (doc.nodes.get(&id_a).cloned(), doc.nodes.get(&id_b).cloned())
                        {
                            if let (SceneNodeKind::Path(pn_a), SceneNodeKind::Path(pn_b)) =
                                (&node_a.kind, &node_b.kind)
                            {
                                let merged = join_two_paths(&pn_a.path_data, &pn_b.path_data);
                                let mut result = node_a.clone();
                                if let SceneNodeKind::Path(ref mut rp) = result.kind {
                                    rp.path_data = merged;
                                }
                                history.execute(
                                    Command::Batch(vec![
                                        Command::UpdateNode {
                                            old: node_a,
                                            new: result.clone(),
                                        },
                                        Command::RemoveNode { node_id: id_b },
                                    ]),
                                    doc,
                                );
                                doc.selection.clear();
                                doc.selection.add(result.id);
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::PathfinderCrop { node_ids } => {
                    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
                    use photonic_core::transform::Transform;

                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };

                    if ids.len() >= 2 {
                        // Find the frontmost node by z-order.
                        let frontmost_id = ids
                            .iter()
                            .max_by_key(|nid| {
                                doc.node_layer_and_index(nid)
                                    .map(|(lid, pos)| {
                                        let layer_pos = doc
                                            .layer_order
                                            .iter()
                                            .position(|id| id == &lid)
                                            .unwrap_or(0);
                                        (layer_pos, pos)
                                    })
                                    .unwrap_or((0, 0))
                            })
                            .copied();

                        if let Some(front_id) = frontmost_id {
                            if let Some(front_node) = doc.nodes.get(&front_id).cloned() {
                                if let SceneNodeKind::Path(front_pn) = &front_node.kind {
                                    let front_path = gui_apply_affine_to_path(
                                        &front_pn.path_data,
                                        front_node.transform.to_kurbo(),
                                    );
                                    let mut cmds: Vec<Command> = Vec::new();
                                    let mut had_error = false;

                                    for nid in &ids {
                                        if *nid == front_id {
                                            continue;
                                        }
                                        if let Some(node) = doc.nodes.get(nid).cloned() {
                                            if let SceneNodeKind::Path(pn) = &node.kind {
                                                let baked = gui_apply_affine_to_path(
                                                    &pn.path_data,
                                                    node.transform.to_kurbo(),
                                                );
                                                if let Ok(cropped) = boolean_op(
                                                    &baked,
                                                    &front_path,
                                                    BooleanOp::Intersect,
                                                ) {
                                                    let mut new_node = node.clone();
                                                    if let SceneNodeKind::Path(ref mut new_pn) =
                                                        new_node.kind
                                                    {
                                                        new_pn.path_data = cropped;
                                                    }
                                                    new_node.transform = Transform::IDENTITY;
                                                    cmds.push(Command::UpdateNode {
                                                        old: node,
                                                        new: new_node,
                                                    });
                                                } else {
                                                    had_error = true;
                                                }
                                            }
                                        }
                                    }

                                    if !had_error && !cmds.is_empty() {
                                        cmds.push(Command::RemoveNode { node_id: front_id });
                                        history.execute(Command::Batch(cmds), doc);
                                        doc.selection.clear();
                                        doc_modified = true;
                                    }
                                }
                            }
                        }
                    }
                }

                PanelAction::PathfinderMinusBack { node_ids } => {
                    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
                    use photonic_core::transform::Transform;

                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };

                    if ids.len() >= 2 {
                        // Find the frontmost node by z-order.
                        let frontmost_id = ids
                            .iter()
                            .max_by_key(|nid| {
                                doc.node_layer_and_index(nid)
                                    .map(|(lid, pos)| {
                                        let layer_pos = doc
                                            .layer_order
                                            .iter()
                                            .position(|id| id == &lid)
                                            .unwrap_or(0);
                                        (layer_pos, pos)
                                    })
                                    .unwrap_or((0, 0))
                            })
                            .copied();

                        if let Some(front_id) = frontmost_id {
                            if let Some(front_node) = doc.nodes.get(&front_id).cloned() {
                                if let SceneNodeKind::Path(front_pn) = &front_node.kind {
                                    let mut result_path = gui_apply_affine_to_path(
                                        &front_pn.path_data,
                                        front_node.transform.to_kurbo(),
                                    );
                                    let mut cmds: Vec<Command> = Vec::new();
                                    let mut had_error = false;

                                    for nid in &ids {
                                        if *nid == front_id {
                                            continue;
                                        }
                                        if let Some(node) = doc.nodes.get(nid).cloned() {
                                            if let SceneNodeKind::Path(pn) = &node.kind {
                                                let baked = gui_apply_affine_to_path(
                                                    &pn.path_data,
                                                    node.transform.to_kurbo(),
                                                );
                                                match boolean_op(
                                                    &result_path,
                                                    &baked,
                                                    BooleanOp::Subtract,
                                                ) {
                                                    Ok(p) => result_path = p,
                                                    Err(_) => {
                                                        had_error = true;
                                                        break;
                                                    }
                                                }
                                                cmds.push(Command::RemoveNode { node_id: *nid });
                                            }
                                        }
                                    }

                                    if !had_error {
                                        let mut new_front = front_node.clone();
                                        if let SceneNodeKind::Path(ref mut new_pn) = new_front.kind
                                        {
                                            new_pn.path_data = result_path;
                                        }
                                        new_front.transform = Transform::IDENTITY;
                                        let update = Command::UpdateNode {
                                            old: front_node,
                                            new: new_front,
                                        };
                                        // UpdateNode first, then removes, so undo order is correct.
                                        let mut all_cmds = vec![update];
                                        all_cmds.extend(cmds);
                                        history.execute(Command::Batch(all_cmds), doc);
                                        doc.selection.clear();
                                        doc.selection.add(front_id);
                                        doc_modified = true;
                                    }
                                }
                            }
                        }
                    }
                }

                PanelAction::PathfinderMinusFront { node_ids } => {
                    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
                    use photonic_core::transform::Transform;

                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };

                    if ids.len() >= 2 {
                        // Find the frontmost node by z-order.
                        let frontmost_id = ids
                            .iter()
                            .max_by_key(|nid| {
                                doc.node_layer_and_index(nid)
                                    .map(|(lid, pos)| {
                                        let layer_pos = doc
                                            .layer_order
                                            .iter()
                                            .position(|id| id == &lid)
                                            .unwrap_or(0);
                                        (layer_pos, pos)
                                    })
                                    .unwrap_or((0, 0))
                            })
                            .copied();

                        if let Some(front_id) = frontmost_id {
                            if let Some(front_node) = doc.nodes.get(&front_id).cloned() {
                                if let SceneNodeKind::Path(front_pn) = &front_node.kind {
                                    let front_path = gui_apply_affine_to_path(
                                        &front_pn.path_data,
                                        front_node.transform.to_kurbo(),
                                    );
                                    let mut cmds: Vec<Command> = Vec::new();
                                    let mut had_error = false;

                                    for nid in &ids {
                                        if *nid == front_id {
                                            continue;
                                        }
                                        if let Some(node) = doc.nodes.get(nid).cloned() {
                                            if let SceneNodeKind::Path(pn) = &node.kind {
                                                let baked = gui_apply_affine_to_path(
                                                    &pn.path_data,
                                                    node.transform.to_kurbo(),
                                                );
                                                match boolean_op(
                                                    &baked,
                                                    &front_path,
                                                    BooleanOp::Subtract,
                                                ) {
                                                    Ok(result) => {
                                                        let mut new_node = node.clone();
                                                        if let SceneNodeKind::Path(ref mut new_pn) =
                                                            new_node.kind
                                                        {
                                                            new_pn.path_data = result;
                                                        }
                                                        new_node.transform = Transform::IDENTITY;
                                                        cmds.push(Command::UpdateNode {
                                                            old: node,
                                                            new: new_node,
                                                        });
                                                    }
                                                    Err(_) => {
                                                        had_error = true;
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    if !had_error && !cmds.is_empty() {
                                        cmds.push(Command::RemoveNode { node_id: front_id });
                                        history.execute(Command::Batch(cmds), doc);
                                        doc.selection.clear();
                                        doc_modified = true;
                                    }
                                }
                            }
                        }
                    }
                }

                PanelAction::PathfinderTrim { node_ids } => {
                    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
                    use photonic_core::transform::Transform;

                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };

                    if ids.len() >= 2 {
                        // Sort back-to-front by z-order.
                        let mut sorted_ids = ids.clone();
                        sorted_ids.sort_by_key(|nid| {
                            doc.node_layer_and_index(nid)
                                .map(|(lid, pos)| {
                                    let layer_pos = doc
                                        .layer_order
                                        .iter()
                                        .position(|id| id == &lid)
                                        .unwrap_or(0);
                                    (layer_pos, pos)
                                })
                                .unwrap_or((0, 0))
                        });

                        // Bake all paths.
                        let baked: Vec<(NodeId, photonic_core::path::PathData)> = sorted_ids
                            .iter()
                            .filter_map(|nid| {
                                let node = doc.nodes.get(nid)?;
                                if let SceneNodeKind::Path(pn) = &node.kind {
                                    Some((
                                        *nid,
                                        gui_apply_affine_to_path(
                                            &pn.path_data,
                                            node.transform.to_kurbo(),
                                        ),
                                    ))
                                } else {
                                    None
                                }
                            })
                            .collect();

                        if baked.len() >= 2 {
                            let mut cmds: Vec<Command> = Vec::new();
                            let mut had_error = false;

                            for i in 0..baked.len() {
                                let (nid, ref path) = baked[i];
                                let mut trimmed = path.clone();
                                for j in (i + 1)..baked.len() {
                                    match boolean_op(&trimmed, &baked[j].1, BooleanOp::Subtract) {
                                        Ok(p) => trimmed = p,
                                        Err(_) => {
                                            had_error = true;
                                            break;
                                        }
                                    }
                                }
                                if had_error {
                                    break;
                                }
                                if let Some(node) = doc.nodes.get(&nid).cloned() {
                                    let mut new_node = node.clone();
                                    if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                        new_pn.path_data = trimmed;
                                        new_pn.stroke.enabled = false;
                                    }
                                    new_node.transform = Transform::IDENTITY;
                                    cmds.push(Command::UpdateNode {
                                        old: node,
                                        new: new_node,
                                    });
                                }
                            }

                            if !had_error && !cmds.is_empty() {
                                history.execute(Command::Batch(cmds), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::PathfinderOutline { node_ids } => {
                    use photonic_core::style::{Fill, FillKind};

                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };

                    let mut cmds: Vec<Command> = Vec::new();
                    for nid in &ids {
                        if let Some(node) = doc.nodes.get(nid).cloned() {
                            if let SceneNodeKind::Path(ref pn) = node.kind {
                                let stroke_color = match &pn.fill.kind {
                                    FillKind::Solid(c) => *c,
                                    FillKind::Gradient(g) => g
                                        .stops
                                        .first()
                                        .map(|s| s.color)
                                        .unwrap_or(photonic_core::color::Color::BLACK),
                                    FillKind::FluidGradient(fg) => fg
                                        .points
                                        .first()
                                        .map(|p| p.color)
                                        .unwrap_or(photonic_core::color::Color::BLACK),
                                    _ => photonic_core::color::Color::BLACK,
                                };
                                let stroke_width = if pn.stroke.enabled {
                                    pn.stroke.width
                                } else {
                                    1.0
                                };
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.fill = Fill::none();
                                    new_pn.stroke.color = stroke_color;
                                    new_pn.stroke.width = stroke_width;
                                    new_pn.stroke.enabled = true;
                                }
                                cmds.push(Command::UpdateNode {
                                    old: node,
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::PathfinderDivide { node_ids } => {
                    use photonic_core::node::PathNode;
                    use photonic_core::ops::boolean::divide_paths;
                    use photonic_core::ops::transform_ops::apply_affine_to_path;

                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };
                    if ids.len() == 2 {
                        let back_id = ids[0];
                        let front_id = ids[1];
                        if let (Some(back_node), Some(front_node)) = (
                            doc.nodes.get(&back_id).cloned(),
                            doc.nodes.get(&front_id).cloned(),
                        ) {
                            if let (
                                SceneNodeKind::Path(ref back_pn),
                                SceneNodeKind::Path(ref front_pn),
                            ) = (&back_node.kind, &front_node.kind)
                            {
                                let back_baked = apply_affine_to_path(
                                    &back_pn.path_data,
                                    back_node.transform.to_kurbo(),
                                );
                                let front_baked = apply_affine_to_path(
                                    &front_pn.path_data,
                                    front_node.transform.to_kurbo(),
                                );
                                match divide_paths(&back_baked, &front_baked) {
                                    Ok(faces) if !faces.is_empty() => {
                                        let target_layer = back_node.layer_id;
                                        let source_pns: [&PathNode; 2] = [back_pn, front_pn];
                                        let source_nodes: [&SceneNode; 2] =
                                            [&back_node, &front_node];
                                        let mut cmds: Vec<Command> = Vec::new();
                                        cmds.push(Command::RemoveNode { node_id: back_id });
                                        cmds.push(Command::RemoveNode { node_id: front_id });
                                        for (i, (path_data, source_idx)) in
                                            faces.into_iter().enumerate()
                                        {
                                            let src_pn = source_pns[source_idx];
                                            let src_node = source_nodes[source_idx];
                                            let mut new_pn = src_pn.clone();
                                            new_pn.path_data = path_data;
                                            let mut new_node = SceneNode::new(
                                                format!("{} face {}", src_node.name, i + 1),
                                                target_layer,
                                                SceneNodeKind::Path(new_pn),
                                            );
                                            new_node.opacity = src_node.opacity;
                                            new_node.blend_mode = src_node.blend_mode;
                                            new_node.tags = src_node.tags.clone();
                                            cmds.push(Command::AddNode {
                                                node: new_node,
                                                layer_id: Some(target_layer),
                                            });
                                        }
                                        history.execute(Command::Batch(cmds), doc);
                                        doc_modified = true;
                                    }
                                    Ok(_) => {}
                                    Err(error) => {
                                        self.file_status = Some(format!(
                                            "Pathfinder divide failed; document unchanged: {error}"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }

                PanelAction::PathfinderMerge { node_ids } => {
                    use photonic_core::ops::boolean::{boolean_op, BooleanOp};
                    use photonic_core::style::FillKind;
                    use std::collections::HashMap;

                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };

                    if ids.len() >= 2 {
                        // Sort back-to-front.
                        let mut sorted_ids = ids.clone();
                        sorted_ids.sort_by_key(|nid| {
                            doc.node_layer_and_index(nid)
                                .map(|(lid, pos)| {
                                    let lp = doc
                                        .layer_order
                                        .iter()
                                        .position(|id| id == &lid)
                                        .unwrap_or(0);
                                    (lp, pos)
                                })
                                .unwrap_or((0, 0))
                        });

                        let target_layer = doc
                            .nodes
                            .get(&sorted_ids[0])
                            .map(|n| n.layer_id)
                            .unwrap_or_else(|| doc.layer_order[0]);

                        // Collect only path nodes.
                        let baked: Vec<(NodeId, photonic_core::path::PathData)> = sorted_ids
                            .iter()
                            .filter_map(|nid| {
                                let node = doc.nodes.get(nid)?;
                                if let SceneNodeKind::Path(pn) = &node.kind {
                                    Some((
                                        *nid,
                                        gui_apply_affine_to_path(
                                            &pn.path_data,
                                            node.transform.to_kurbo(),
                                        ),
                                    ))
                                } else {
                                    None
                                }
                            })
                            .collect();

                        if baked.len() >= 2 {
                            // Trim each path: subtract all paths above it.
                            let mut trimmed: Vec<(NodeId, photonic_core::path::PathData)> =
                                Vec::new();
                            let mut had_error = false;
                            for i in 0..baked.len() {
                                let (nid, ref path) = baked[i];
                                let mut t = path.clone();
                                for j in (i + 1)..baked.len() {
                                    match boolean_op(&t, &baked[j].1, BooleanOp::Subtract) {
                                        Ok(p) => t = p,
                                        Err(_) => {
                                            had_error = true;
                                            break;
                                        }
                                    }
                                }
                                if had_error {
                                    break;
                                }
                                trimmed.push((nid, t));
                            }

                            if !had_error {
                                // Group trimmed faces by fill color key.
                                let mut groups: Vec<(String, Vec<photonic_core::path::PathData>)> =
                                    Vec::new();
                                let mut key_idx: HashMap<String, usize> = HashMap::new();
                                let mut key_rep: HashMap<String, NodeId> = HashMap::new();
                                for (nid, t_path) in &trimmed {
                                    let k = match doc.nodes.get(nid).map(|n| &n.kind) {
                                        Some(SceneNodeKind::Path(pn)) => match &pn.fill.kind {
                                            FillKind::Solid(c) => format!(
                                                "solid:{:.2},{:.2},{:.2},{:.2}",
                                                c.r, c.g, c.b, c.a
                                            ),
                                            _ => format!("other:{}", nid),
                                        },
                                        _ => format!("other:{}", nid),
                                    };
                                    if let Some(&idx) = key_idx.get(&k) {
                                        groups[idx].1.push(t_path.clone());
                                    } else {
                                        let idx = groups.len();
                                        key_idx.insert(k.clone(), idx);
                                        key_rep.insert(k.clone(), *nid);
                                        groups.push((k, vec![t_path.clone()]));
                                    }
                                }

                                // Union each group and build result nodes.
                                let mut cmds: Vec<Command> = Vec::new();
                                for nid in &sorted_ids {
                                    cmds.push(Command::RemoveNode { node_id: *nid });
                                }
                                let mut union_err = false;
                                for (key, paths) in &groups {
                                    let mut merged = paths[0].clone();
                                    for path in &paths[1..] {
                                        match boolean_op(&merged, path, BooleanOp::Union) {
                                            Ok(p) => merged = p,
                                            Err(_) => {
                                                union_err = true;
                                                break;
                                            }
                                        }
                                    }
                                    if union_err {
                                        break;
                                    }
                                    if let Some(rep_id) = key_rep.get(key) {
                                        if let Some(rep_node) = doc.nodes.get(rep_id).cloned() {
                                            if let SceneNodeKind::Path(ref rep_pn) = rep_node.kind {
                                                let mut new_pn = rep_pn.clone();
                                                new_pn.path_data = merged;
                                                new_pn.stroke.enabled = false;
                                                let label = if paths.len() > 1 {
                                                    format!("{} merged", rep_node.name)
                                                } else {
                                                    rep_node.name.clone()
                                                };
                                                let mut new_node = SceneNode::new(
                                                    label,
                                                    target_layer,
                                                    SceneNodeKind::Path(new_pn),
                                                );
                                                new_node.opacity = rep_node.opacity;
                                                new_node.blend_mode = rep_node.blend_mode;
                                                cmds.push(Command::AddNode {
                                                    node: new_node,
                                                    layer_id: Some(target_layer),
                                                });
                                            }
                                        }
                                    }
                                }

                                if !union_err && cmds.len() > sorted_ids.len() {
                                    history.execute(Command::Batch(cmds), doc);
                                    doc_modified = true;
                                }
                            }
                        }
                    }
                }

                PanelAction::DivideObjectsBelow { node_id } => {
                    use photonic_core::ops::boolean::{boolean_op, divide_paths, BooleanOp};
                    use photonic_core::ops::transform_ops::apply_affine_to_path;

                    if let Some(cutter_node) = doc.nodes.get(&node_id).cloned() {
                        if let SceneNodeKind::Path(ref cutter_pn) = cutter_node.kind {
                            let cutter_baked = apply_affine_to_path(
                                &cutter_pn.path_data,
                                cutter_node.transform.to_kurbo(),
                            );
                            if let Some((cutter_layer_id, cutter_z)) =
                                doc.node_layer_and_index(&node_id)
                            {
                                let below_ids: Vec<NodeId> = doc
                                    .layers
                                    .get(&cutter_layer_id)
                                    .map(|l| l.node_ids[..cutter_z].to_vec())
                                    .unwrap_or_default();

                                let mut cmds: Vec<Command> = Vec::new();
                                for target_id in &below_ids {
                                    if let Some(target_node) = doc.nodes.get(target_id).cloned() {
                                        if let SceneNodeKind::Path(ref target_pn) = target_node.kind
                                        {
                                            let target_baked = apply_affine_to_path(
                                                &target_pn.path_data,
                                                target_node.transform.to_kurbo(),
                                            );
                                            let overlap = match boolean_op(
                                                &target_baked,
                                                &cutter_baked,
                                                BooleanOp::Intersect,
                                            ) {
                                                Ok(overlap) => overlap,
                                                Err(error) => {
                                                    self.file_status = Some(format!(
                                                        "Divide objects failed; document unchanged: {error}"
                                                    ));
                                                    continue 'actions;
                                                }
                                            };
                                            if overlap.is_empty() {
                                                continue;
                                            }
                                            let faces = match divide_paths(
                                                &target_baked,
                                                &cutter_baked,
                                            ) {
                                                Ok(faces) => faces,
                                                Err(error) => {
                                                    self.file_status = Some(format!(
                                                        "Divide objects failed; document unchanged: {error}"
                                                    ));
                                                    continue 'actions;
                                                }
                                            };
                                            cmds.push(Command::RemoveNode {
                                                node_id: *target_id,
                                            });
                                            for (i, (path_data, _)) in faces.into_iter().enumerate()
                                            {
                                                let mut new_pn = target_pn.clone();
                                                new_pn.path_data = path_data;
                                                let mut new_node = SceneNode::new(
                                                    format!("{} face {}", target_node.name, i + 1),
                                                    cutter_layer_id,
                                                    SceneNodeKind::Path(new_pn),
                                                );
                                                new_node.opacity = target_node.opacity;
                                                new_node.blend_mode = target_node.blend_mode;
                                                new_node.tags = target_node.tags.clone();
                                                cmds.push(Command::AddNode {
                                                    node: new_node,
                                                    layer_id: Some(cutter_layer_id),
                                                });
                                            }
                                        }
                                    }
                                }
                                cmds.push(Command::RemoveNode { node_id });
                                if !cmds.is_empty() {
                                    history.execute(Command::Batch(cmds), doc);
                                    doc_modified = true;
                                }
                            }
                        }
                    }
                }

                PanelAction::MakeCompoundPath { node_ids } => {
                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };
                    if ids.len() >= 2 {
                        // Find bottommost node (lowest z-order) as style source.
                        let bottom_id = ids
                            .iter()
                            .min_by_key(|nid| {
                                doc.node_layer_and_index(nid)
                                    .map(|(lid, pos)| {
                                        let layer_pos = doc
                                            .layer_order
                                            .iter()
                                            .position(|id| id == &lid)
                                            .unwrap_or(0);
                                        (layer_pos, pos)
                                    })
                                    .unwrap_or((0, 0))
                            })
                            .copied();

                        if let Some(base_id) = bottom_id {
                            // Delegate to MCP handler by collecting paths.
                            // We need to do it inline here since MCP handler is async and mutexed.
                            // Use the same logic: merge all baked paths into one PathData.
                            let base_node = doc.nodes.get(&base_id).cloned();
                            if let Some(base_node) = base_node {
                                if let SceneNodeKind::Path(ref base_pn) = base_node.kind {
                                    // Build merged path by appending all subpaths.
                                    let mut merged_bez = base_pn.path_data.to_bez_path();
                                    for nid in &ids {
                                        if *nid == base_id {
                                            continue;
                                        }
                                        if let Some(n) = doc.nodes.get(nid) {
                                            if let SceneNodeKind::Path(pn) = &n.kind {
                                                let baked = gui_apply_affine_to_path(
                                                    &pn.path_data,
                                                    n.transform.to_kurbo(),
                                                );
                                                for el in baked.to_bez_path().elements() {
                                                    merged_bez.push(*el);
                                                }
                                            }
                                        }
                                    }
                                    let compound_path =
                                        photonic_core::path::PathData::from_bez_path(&merged_bez);
                                    let (base_layer_id, base_index) =
                                        doc.node_layer_and_index(&base_id).unwrap_or_default();
                                    let mut compound_pn =
                                        photonic_core::node::PathNode::new(compound_path);
                                    compound_pn.fill = base_pn.fill.clone();
                                    compound_pn.stroke = base_pn.stroke.clone();
                                    compound_pn.is_compound = true;
                                    let compound_node = SceneNode::new(
                                        format!("{} (compound)", base_node.name),
                                        base_layer_id,
                                        SceneNodeKind::Path(compound_pn),
                                    );
                                    let compound_id = compound_node.id;
                                    let mut cmds = vec![Command::AddNode {
                                        node: compound_node,
                                        layer_id: Some(base_layer_id),
                                    }];
                                    cmds.push(Command::ReorderNode {
                                        layer_id: base_layer_id,
                                        node_id: compound_id,
                                        old_index: doc.layers[&base_layer_id].node_ids.len(),
                                        new_index: base_index,
                                    });
                                    for nid in &ids {
                                        cmds.push(Command::RemoveNode { node_id: *nid });
                                    }
                                    history.execute(Command::Batch(cmds), doc);
                                    doc.selection.clear();
                                    doc.selection.add(compound_id);
                                    doc_modified = true;
                                }
                            }
                        }
                    }
                }

                PanelAction::ReleaseCompoundPath { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        if let SceneNodeKind::Path(ref pn) = node.kind {
                            // Split compound path into subpaths.
                            let bez = pn.path_data.to_bez_path();
                            let mut subpaths: Vec<kurbo::BezPath> = Vec::new();
                            let mut current = kurbo::BezPath::new();
                            for el in bez.elements() {
                                match el {
                                    kurbo::PathEl::MoveTo(_) => {
                                        if !current.elements().is_empty() {
                                            subpaths.push(current.clone());
                                            current = kurbo::BezPath::new();
                                        }
                                        current.push(*el);
                                    }
                                    _ => current.push(*el),
                                }
                            }
                            if !current.elements().is_empty() {
                                subpaths.push(current);
                            }

                            if subpaths.len() <= 1 {
                                // Nothing to release.
                            } else {
                                let (layer_id, _base_index) =
                                    doc.node_layer_and_index(&node_id).unwrap_or_default();
                                let mut cmds = vec![Command::RemoveNode { node_id }];
                                for (i, sub_bez) in subpaths.iter().enumerate() {
                                    let mut sub_pn = photonic_core::node::PathNode::new(
                                        photonic_core::path::PathData::from_bez_path(sub_bez),
                                    );
                                    sub_pn.fill = pn.fill.clone();
                                    sub_pn.stroke = pn.stroke.clone();
                                    sub_pn.is_compound = false;
                                    let sub_node = SceneNode::new(
                                        format!(
                                            "{} {}",
                                            node.name.trim_end_matches(" (compound)"),
                                            i + 1
                                        ),
                                        layer_id,
                                        SceneNodeKind::Path(sub_pn),
                                    );
                                    cmds.push(Command::AddNode {
                                        node: sub_node,
                                        layer_id: Some(layer_id),
                                    });
                                }
                                history.execute(Command::Batch(cmds), doc);
                                doc.selection.clear();
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::ShearNode {
                    node_ids,
                    shear_x,
                    shear_y,
                } => {
                    if node_ids.len() <= 1 {
                        // Single node: shear about its own local center (unchanged).
                        if let Some(old_node) =
                            node_ids.first().and_then(|id| doc.nodes.get(id).cloned())
                        {
                            let mut new_node = old_node.clone();
                            let (cx, cy) = new_node
                                .local_bounds()
                                .map(|b| (b.x0 + b.width() / 2.0, b.y0 + b.height() / 2.0))
                                .unwrap_or((0.0, 0.0));
                            use photonic_core::ops::transform_ops;
                            transform_ops::shear(&mut new_node, shear_x, shear_y, cx, cy);
                            history.execute(
                                Command::UpdateNode {
                                    old: old_node,
                                    new: new_node,
                                },
                                doc,
                            );
                            doc_modified = true;
                        }
                    } else {
                        // Multi: shear every node about the shared world center.
                        let (cx, cy) = selection_canvas_bounds(doc, &node_ids, renderer)
                            .map(|(x0, y0, x1, y1)| ((x0 + x1) / 2.0, (y0 + y1) / 2.0))
                            .unwrap_or((0.0, 0.0));
                        let m = photonic_core::transform::Transform::shear_around(
                            shear_x, shear_y, cx, cy,
                        );
                        let mut cmds = Vec::new();
                        for nid in &node_ids {
                            if let Some(node) = doc.nodes.get(nid) {
                                let mut new_node = node.clone();
                                // Apply in WORLD space: node transform first, then the
                                // mirror/shear about the shared pivot (correct after moves).
                                new_node.transform = m.then(&node.transform);
                                cmds.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                        if !cmds.is_empty() {
                            history.execute(Command::Batch(cmds), doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::DistributeOnPath {
                    path_node_id,
                    node_ids,
                    align,
                } => {
                    // Resolve from selection if path_node_id is nil.
                    let (guide_id, source_ids) = if path_node_id == uuid::Uuid::nil() {
                        let sel: Vec<NodeId> = doc.selection.node_ids.iter().cloned().collect();
                        if sel.len() < 2 {
                            continue;
                        }
                        // The "guide" is the frontmost path node in the selection.
                        // Find the node with the highest position in the document's node order.
                        // Use the first path node from the selection (selection ordering).
                        let guide = sel
                            .iter()
                            .find(|id| {
                                matches!(
                                    doc.nodes.get(id).map(|n| &n.kind),
                                    Some(SceneNodeKind::Path(_))
                                )
                            })
                            .copied();
                        let guide = match guide {
                            Some(g) => g,
                            None => continue,
                        };
                        let sources: Vec<NodeId> =
                            sel.iter().filter(|&&id| id != guide).copied().collect();
                        (guide, sources)
                    } else {
                        (path_node_id, node_ids)
                    };
                    if source_ids.is_empty() {
                        continue;
                    }

                    let path_data = match doc.nodes.get(&guide_id) {
                        Some(n) => match &n.kind {
                            SceneNodeKind::Path(p) => p.path_data.clone(),
                            _ => continue,
                        },
                        None => continue,
                    };
                    let positions = path_data.sample_positions(source_ids.len());
                    if positions.is_empty() {
                        continue;
                    }

                    let mut commands: Vec<Command> = Vec::new();
                    for (k, (px, py, angle_deg)) in positions.iter().enumerate() {
                        let src_id = source_ids[k % source_ids.len()];
                        if let Some(src) = doc.nodes.get(&src_id).cloned() {
                            let mut new_node = src.clone();
                            new_node.id = uuid::Uuid::new_v4();
                            new_node.name = format!("{} {}", src.name, k + 1);
                            new_node.transform.matrix[4] = px + src.transform.matrix[4];
                            new_node.transform.matrix[5] = py + src.transform.matrix[5];
                            if align {
                                use std::f64::consts::PI;
                                let rad = angle_deg * PI / 180.0;
                                let (cos_r, sin_r) = (rad.cos(), rad.sin());
                                let m = &src.transform.matrix;
                                new_node.transform.matrix[0] = m[0] * cos_r + m[2] * sin_r;
                                new_node.transform.matrix[1] = m[1] * cos_r + m[3] * sin_r;
                                new_node.transform.matrix[2] = -m[0] * sin_r + m[2] * cos_r;
                                new_node.transform.matrix[3] = -m[1] * sin_r + m[3] * cos_r;
                            }
                            let lid = new_node.layer_id;
                            commands.push(Command::AddNode {
                                node: new_node,
                                layer_id: Some(lid),
                            });
                        }
                    }
                    if !commands.is_empty() {
                        history.execute(Command::Batch(commands), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::SnapToPixel { node_ids } => {
                    let mut commands: Vec<Command> = Vec::new();
                    for id in node_ids {
                        if let Some(old_node) = doc.nodes.get(&id).cloned() {
                            let mut new_node = old_node.clone();
                            new_node.transform.matrix[4] = new_node.transform.matrix[4].round();
                            new_node.transform.matrix[5] = new_node.transform.matrix[5].round();
                            let dx =
                                (old_node.transform.matrix[4] - new_node.transform.matrix[4]).abs();
                            let dy =
                                (old_node.transform.matrix[5] - new_node.transform.matrix[5]).abs();
                            if dx > 1e-9 || dy > 1e-9 {
                                commands.push(Command::UpdateNode {
                                    old: old_node,
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !commands.is_empty() {
                        history.execute(Command::Batch(commands), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::SelectSame { node_id, attribute } => {
                    let ref_node = doc.nodes.get(&node_id).cloned();
                    if let Some(ref_node) = ref_node {
                        let tolerance: f32 = 0.01;
                        let mut matched: Vec<NodeId> = Vec::new();
                        for (nid, node) in &doc.nodes {
                            let hits = match attribute {
                                SelectSameAttr::FillColor => {
                                    let rc = gui_solid_fill_color(&ref_node);
                                    let cc = gui_solid_fill_color(node);
                                    match (rc, cc) {
                                        (Some(rc), Some(cc)) => gui_color_dist(rc, cc) <= tolerance,
                                        (None, None) => true,
                                        _ => false,
                                    }
                                }
                                SelectSameAttr::StrokeColor => {
                                    if let (SceneNodeKind::Path(rp), SceneNodeKind::Path(cp)) =
                                        (&ref_node.kind, &node.kind)
                                    {
                                        match (rp.stroke.enabled, cp.stroke.enabled) {
                                            (true, true) => {
                                                gui_color_dist(rp.stroke.color, cp.stroke.color)
                                                    <= tolerance
                                            }
                                            (false, false) => true,
                                            _ => false,
                                        }
                                    } else {
                                        false
                                    }
                                }
                                SelectSameAttr::StrokeWeight => {
                                    if let (SceneNodeKind::Path(rp), SceneNodeKind::Path(cp)) =
                                        (&ref_node.kind, &node.kind)
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
                                SelectSameAttr::BlendMode => ref_node.blend_mode == node.blend_mode,
                                SelectSameAttr::ObjectType => {
                                    std::mem::discriminant(&ref_node.kind)
                                        == std::mem::discriminant(&node.kind)
                                }
                            };
                            if hits {
                                matched.push(*nid);
                            }
                        }
                        doc.selection.clear();
                        for nid in matched {
                            doc.selection.add(nid);
                        }
                        doc_modified = true;
                    }
                }

                PanelAction::ReversePathDirection { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            let reversed = pn.path_data.reverse();
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                new_pn.path_data = reversed;
                            }
                            let cmd = Command::UpdateNode {
                                old: node.clone(),
                                new: new_node,
                            };
                            history.execute(cmd, doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::AverageAnchorPoints { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            let averaged = pn.path_data.average_anchor_points(true, true);
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                new_pn.path_data = averaged;
                            }
                            let cmd = Command::UpdateNode {
                                old: node.clone(),
                                new: new_node,
                            };
                            history.execute(cmd, doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::OpenSimplifyDialog { node_id } => {
                    let node = doc.nodes.get(&node_id);
                    let name = node
                        .map(|n| n.name.clone())
                        .unwrap_or_else(|| node_id.to_string());
                    let orig_points = node
                        .and_then(|n| match &n.kind {
                            SceneNodeKind::Path(pn) => {
                                Some(photonic_core::ops::simplify::count_points(&pn.path_data))
                            }
                            _ => None,
                        })
                        .unwrap_or(0);
                    self.simplify_dialog = Some(SimplifyDialog {
                        node_id,
                        node_name: name,
                        tolerance: 1.0,
                        fit_curves: false,
                        corner_angle_deg: 20.0,
                        refit_existing: false,
                        orig_points,
                        preview: None,
                        cached_tol: f64::NAN,
                        cached_fit: false,
                        cached_angle: f64::NAN,
                        cached_refit: false,
                    });
                }

                PanelAction::OpenMergeVerticesDialog { node_id } => {
                    let node = doc.nodes.get(&node_id);
                    let name = node
                        .map(|n| n.name.clone())
                        .unwrap_or_else(|| node_id.to_string());
                    let orig_points = node
                        .and_then(|n| match &n.kind {
                            SceneNodeKind::Path(pn) => {
                                Some(photonic_core::ops::simplify::count_points(&pn.path_data))
                            }
                            _ => None,
                        })
                        .unwrap_or(0);
                    self.merge_vertices_dialog = Some(MergeVerticesDialog {
                        node_id,
                        node_name: name,
                        threshold: 1.0,
                        orig_points,
                        preview: None,
                        cached_thr: f64::NAN,
                    });
                }

                PanelAction::OpenFindReplaceTextDialog => {
                    self.find_replace_text_dialog = Some(FindReplaceTextDialog {
                        find: String::new(),
                        replace: String::new(),
                        regex: false,
                        case_sensitive: true,
                        selection_only: false,
                    });
                }

                PanelAction::InvertColors { node_ids } => {
                    use photonic_core::style::FillKind;
                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };
                    let mut cmds: Vec<Command> = Vec::new();
                    for id in &ids {
                        if let Some(node) = doc.nodes.get(id) {
                            if let SceneNodeKind::Path(_) = &node.kind {
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut np) = new_node.kind {
                                    match &mut np.fill.kind {
                                        FillKind::Solid(c) => *c = c.invert(),
                                        FillKind::Gradient(g) => {
                                            for stop in &mut g.stops {
                                                stop.color = stop.color.invert();
                                            }
                                        }
                                        FillKind::FluidGradient(fg) => {
                                            for pt in &mut fg.points {
                                                pt.color = pt.color.invert();
                                            }
                                        }
                                        FillKind::MeshGradient(mg) => {
                                            for c in &mut mg.cell_colors {
                                                *c = c.invert();
                                            }
                                        }
                                        FillKind::Pattern(p) => {
                                            p.tile.map_rgb(|[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b]);
                                        }
                                        FillKind::None => {}
                                    }
                                    if np.stroke.enabled {
                                        np.stroke.color = np.stroke.color.invert();
                                    }
                                }
                                cmds.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::ConvertToGrayscale { node_ids } => {
                    use photonic_core::style::FillKind;
                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };
                    let mut cmds: Vec<Command> = Vec::new();
                    for id in &ids {
                        if let Some(node) = doc.nodes.get(id) {
                            if let SceneNodeKind::Path(_) = &node.kind {
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut np) = new_node.kind {
                                    match &mut np.fill.kind {
                                        FillKind::Solid(c) => *c = c.to_grayscale(),
                                        FillKind::Gradient(g) => {
                                            for stop in &mut g.stops {
                                                stop.color = stop.color.to_grayscale();
                                            }
                                        }
                                        FillKind::FluidGradient(fg) => {
                                            for pt in &mut fg.points {
                                                pt.color = pt.color.to_grayscale();
                                            }
                                        }
                                        FillKind::MeshGradient(mg) => {
                                            for c in &mut mg.cell_colors {
                                                *c = c.to_grayscale();
                                            }
                                        }
                                        FillKind::Pattern(p) => {
                                            p.tile.map_rgb(|rgb| {
                                                let l = photonic_core::raster::image::luma(rgb);
                                                [l, l, l]
                                            });
                                        }
                                        FillKind::None => {}
                                    }
                                    if np.stroke.enabled {
                                        np.stroke.color = np.stroke.color.to_grayscale();
                                    }
                                }
                                cmds.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::AdjustColors {
                    node_ids,
                    delta_r,
                    delta_g,
                    delta_b,
                    delta_a,
                } => {
                    use photonic_core::style::FillKind;
                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };
                    let shift = |c: Color| -> Color {
                        Color {
                            r: (c.r + delta_r).clamp(0.0, 1.0),
                            g: (c.g + delta_g).clamp(0.0, 1.0),
                            b: (c.b + delta_b).clamp(0.0, 1.0),
                            a: (c.a + delta_a).clamp(0.0, 1.0),
                        }
                    };
                    let mut cmds: Vec<Command> = Vec::new();
                    for id in &ids {
                        if let Some(node) = doc.nodes.get(id) {
                            if let SceneNodeKind::Path(_) = &node.kind {
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut np) = new_node.kind {
                                    match &mut np.fill.kind {
                                        FillKind::Solid(c) => *c = shift(*c),
                                        FillKind::Gradient(g) => {
                                            for stop in &mut g.stops {
                                                stop.color = shift(stop.color);
                                            }
                                        }
                                        FillKind::FluidGradient(fg) => {
                                            for pt in &mut fg.points {
                                                pt.color = shift(pt.color);
                                            }
                                        }
                                        FillKind::MeshGradient(mg) => {
                                            for c in &mut mg.cell_colors {
                                                *c = shift(*c);
                                            }
                                        }
                                        FillKind::Pattern(p) => {
                                            p.tile.map_pixels(|[r, g, b, a]| {
                                                let c = shift(Color {
                                                    r: r as f32 / 255.0,
                                                    g: g as f32 / 255.0,
                                                    b: b as f32 / 255.0,
                                                    a: a as f32 / 255.0,
                                                });
                                                [
                                                    (c.r * 255.0).round().clamp(0.0, 255.0) as u8,
                                                    (c.g * 255.0).round().clamp(0.0, 255.0) as u8,
                                                    (c.b * 255.0).round().clamp(0.0, 255.0) as u8,
                                                    (c.a * 255.0).round().clamp(0.0, 255.0) as u8,
                                                ]
                                            });
                                        }
                                        FillKind::None => {}
                                    }
                                    if np.stroke.enabled {
                                        np.stroke.color = shift(np.stroke.color);
                                    }
                                }
                                cmds.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::RecolorArtwork { node_ids, palette } => {
                    use photonic_core::style::FillKind;
                    fn color_dist(a: [f32; 4], b: [f32; 4]) -> f32 {
                        let dr = a[0] - b[0];
                        let dg = a[1] - b[1];
                        let db = a[2] - b[2];
                        dr * dr + dg * dg + db * db
                    }
                    fn nearest(c: [f32; 4], pal: &[[f32; 4]]) -> [f32; 4] {
                        *pal.iter()
                            .min_by(|a, b| {
                                color_dist(c, **a)
                                    .partial_cmp(&color_dist(c, **b))
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .unwrap()
                    }
                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };
                    let mut cmds: Vec<Command> = Vec::new();
                    for id in &ids {
                        if let Some(node) = doc.nodes.get(id) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                if pn.fill.enabled {
                                    if let FillKind::Solid(c) = &pn.fill.kind {
                                        let orig = [c.r, c.g, c.b, c.a];
                                        let tgt = nearest(orig, &palette);
                                        if (orig[0] - tgt[0]).abs() > 1e-6
                                            || (orig[1] - tgt[1]).abs() > 1e-6
                                            || (orig[2] - tgt[2]).abs() > 1e-6
                                        {
                                            let mut new_node = node.clone();
                                            if let SceneNodeKind::Path(ref mut np) = new_node.kind {
                                                np.fill.kind = FillKind::Solid(Color {
                                                    r: tgt[0],
                                                    g: tgt[1],
                                                    b: tgt[2],
                                                    a: tgt[3],
                                                });
                                            }
                                            cmds.push(Command::UpdateNode {
                                                old: node.clone(),
                                                new: new_node,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::RecolorPreview { ids, to } => {
                    // Live preview — mutate the captured nodes directly, no history.
                    use photonic_core::style::FillKind;
                    let new_color = Color {
                        r: to[0],
                        g: to[1],
                        b: to[2],
                        a: to[3],
                    };
                    for id in &ids {
                        if let Some(node) = doc.nodes.get_mut(id) {
                            match &mut node.kind {
                                SceneNodeKind::Path(p) => p.fill.kind = FillKind::Solid(new_color),
                                SceneNodeKind::Text(t) => t.fill.kind = FillKind::Solid(new_color),
                                _ => {}
                            }
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::RecolorCommit { ids, from, to } => {
                    // Commit as a single undoable step: old=`from`, new=`to`.
                    use photonic_core::style::FillKind;
                    let from_color = Color {
                        r: from[0],
                        g: from[1],
                        b: from[2],
                        a: from[3],
                    };
                    let to_color = Color {
                        r: to[0],
                        g: to[1],
                        b: to[2],
                        a: to[3],
                    };
                    if (from[0] - to[0]).abs() > 1e-6
                        || (from[1] - to[1]).abs() > 1e-6
                        || (from[2] - to[2]).abs() > 1e-6
                        || (from[3] - to[3]).abs() > 1e-6
                    {
                        let mut cmds: Vec<Command> = Vec::new();
                        for id in &ids {
                            if let Some(node) = doc.nodes.get(id) {
                                // Fabricate old (fill=from) and new (fill=to) from the
                                // current node so undo restores the original color.
                                let mut old_node = node.clone();
                                let mut new_node = node.clone();
                                match &mut old_node.kind {
                                    SceneNodeKind::Path(p) => {
                                        p.fill.kind = FillKind::Solid(from_color)
                                    }
                                    SceneNodeKind::Text(t) => {
                                        t.fill.kind = FillKind::Solid(from_color)
                                    }
                                    _ => {}
                                }
                                match &mut new_node.kind {
                                    SceneNodeKind::Path(p) => {
                                        p.fill.kind = FillKind::Solid(to_color)
                                    }
                                    SceneNodeKind::Text(t) => {
                                        t.fill.kind = FillKind::Solid(to_color)
                                    }
                                    _ => {}
                                }
                                cmds.push(Command::UpdateNode {
                                    old: old_node,
                                    new: new_node,
                                });
                            }
                        }
                        if !cmds.is_empty() {
                            history.execute(Command::Batch(cmds), doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::UngroupNode { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if let SceneNodeKind::Group(g) = &node.kind {
                            let children = g.children.clone();
                            let node_clone = node.clone();
                            if let Some((layer_id, group_index)) =
                                doc.node_layer_and_index(&node_id)
                            {
                                let first_child = children.first().copied();
                                let cmd = Command::UngroupNodes {
                                    group: node_clone,
                                    layer_id,
                                    group_index,
                                    children,
                                };
                                history.execute(cmd, doc);
                                self.selected_id = first_child;
                                if let Some(fc) = first_child {
                                    doc.selection = Selection::single(fc);
                                } else {
                                    doc.selection.clear();
                                }
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::UngroupAllNode { node_id } => {
                    if self.ungroup_all_node(node_id, doc, history) {
                        doc_modified = true;
                    }
                }

                PanelAction::DeleteSelected => {
                    let ids: Vec<_> = doc.selection.ids().copied().collect();
                    if !ids.is_empty() {
                        let cmds = ids
                            .iter()
                            .map(|&id| Command::RemoveNode { node_id: id })
                            .collect();
                        history.execute(Command::Batch(cmds), doc);
                        self.selected_id = None;
                        doc.selection.clear();
                        doc_modified = true;
                    }
                }

                PanelAction::CreateShapeAtPos {
                    shape,
                    canvas_x,
                    canvas_y,
                    fill,
                } => {
                    let half = 50.0_f64;
                    let (sx, sy, ex, ey) = (
                        canvas_x - half,
                        canvas_y - half,
                        canvas_x + half,
                        canvas_y + half,
                    );
                    if shape == ShapeKind::Text {
                        use photonic_core::node::TextNode;
                        let [r, g, b, a] = fill;
                        let mut text_node = TextNode::new("Text");
                        text_node.fill = Fill::solid(Color { r, g, b, a });
                        let num = doc.node_count() + 1;
                        let mut node = SceneNode::new(
                            format!("Text {}", num),
                            Default::default(),
                            SceneNodeKind::Text(text_node),
                        );
                        node.transform =
                            photonic_core::transform::Transform::translate(canvas_x, canvas_y);
                        self.tool_commit_add(node, doc, history, &mut doc_modified);
                    } else {
                        let tool = match shape {
                            ShapeKind::Shape(p) => Tool::from_primitive(p),
                            ShapeKind::Text => unreachable!(),
                        };
                        if let Some(path) = self.build_shape_with_tool(tool, sx, sy, ex, ey) {
                            let stroke_arg = self.prefs.default_stroke_enabled.then_some({
                                (
                                    self.prefs.default_stroke_color,
                                    self.prefs.default_stroke_width,
                                )
                            });
                            let node = make_node(
                                path,
                                fill,
                                stroke_arg,
                                shape.label(),
                                doc.node_count() + 1,
                            );
                            self.tool_commit_add(node, doc, history, &mut doc_modified);
                        }
                    }
                }

                PanelAction::GroupSelected => {
                    self.do_group_selected(doc, history, &mut doc_modified);
                }

                PanelAction::CopyAsSvg { node_ids } => {
                    let ids: Vec<_> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };
                    if !ids.is_empty() {
                        let svg = photonic_core::export::export_nodes_as_svg(doc, &ids);
                        ctx.output_mut(|o| o.copied_text = svg);
                        self.file_status = Some("Copied SVG to clipboard".to_string());
                    }
                }

                PanelAction::DiffWithCheckpoint { checkpoint_id } => {
                    if let Some(snapshot) = history.get_checkpoint_snapshot(checkpoint_id) {
                        let mut highlights = Vec::new();
                        let mut removed_boxes = Vec::new();

                        // Added: in current doc but not in snapshot
                        // Modified: in both but different
                        for (id, node) in &doc.nodes {
                            if !snapshot.nodes.contains_key(id) {
                                highlights.push((*id, DiffCategory::Added));
                            } else if let Some(old) = snapshot.nodes.get(id) {
                                let from_val = serde_json::to_value(old).unwrap_or_default();
                                let to_val = serde_json::to_value(node).unwrap_or_default();
                                if from_val != to_val {
                                    highlights.push((*id, DiffCategory::Modified));
                                }
                            }
                        }

                        // Removed: in snapshot but not in current doc
                        for (id, old_node) in &snapshot.nodes {
                            if !doc.nodes.contains_key(id) {
                                if let Some((cx0, cy0, cx1, cy1)) =
                                    text_aware_canvas_bounds(old_node, renderer)
                                {
                                    removed_boxes.push(egui::Rect::from_min_max(
                                        egui::pos2(cx0 as f32, cy0 as f32),
                                        egui::pos2(cx1 as f32, cy1 as f32),
                                    ));
                                }
                            }
                        }

                        let total = highlights.len() + removed_boxes.len();
                        self.diff.highlights = highlights;
                        self.diff.removed_boxes = removed_boxes;
                        self.diff.overlay_active = true;
                        self.file_status = Some(format!("{} diff change(s) highlighted", total));
                    }
                }

                PanelAction::ClearDiff => {
                    self.diff.highlights.clear();
                    self.diff.removed_boxes.clear();
                    self.diff.overlay_active = false;
                    self.file_status = Some("Diff cleared".to_string());
                }

                PanelAction::StartEyedropper(target) => {
                    self.eyedropper.target = Some(target);
                    self.eyedropper.skip_click = true;
                }

                PanelAction::AddLayer => {
                    self.do_add_layer(doc, history, &mut doc_modified);
                }

                PanelAction::CollectInNewLayer { node_ids } => {
                    self.do_collect_in_new_layer(node_ids, doc, history, &mut doc_modified);
                }

                PanelAction::ReleaseToLayers { node_ids } => {
                    self.do_release_to_layers(node_ids, doc, history, &mut doc_modified);
                }

                PanelAction::MergeLayers { layer_ids } => {
                    self.do_merge_layers(layer_ids, doc, history, &mut doc_modified);
                }

                PanelAction::FlattenArtwork => {
                    let all_ids: Vec<_> = doc.layer_order.clone();
                    if all_ids.len() >= 2 {
                        self.do_merge_layers(all_ids, doc, history, &mut doc_modified);
                    }
                }

                PanelAction::ReorderLayers { new_order } => {
                    // Drag-to-reorder (#169). Only a real change, and only if the
                    // new order is a permutation of the existing stack.
                    let old_order = doc.layer_order.clone();
                    let same_set = new_order.len() == old_order.len()
                        && new_order.iter().all(|id| old_order.contains(id));
                    if same_set && new_order != old_order {
                        history.execute(
                            Command::ReorderLayers {
                                old_order,
                                new_order,
                            },
                            doc,
                        );
                        doc_modified = true;
                    }
                }

                PanelAction::ReparentNode {
                    node_id,
                    new,
                    new_index,
                } => {
                    use photonic_core::document::NodeContainer;
                    // Guard: never drop a node into itself or one of its own
                    // descendants (would create a cycle).
                    let cycle = matches!(new, NodeContainer::Group(gid) if doc.is_ancestor_or_self(node_id, gid));
                    if !cycle {
                        if let Some((old, old_index)) = doc.node_container_and_index(&node_id) {
                            if !(old == new && old_index == new_index) {
                                history.execute(
                                    Command::ReparentNode {
                                        node_id,
                                        old,
                                        old_index,
                                        new,
                                        new_index,
                                    },
                                    doc,
                                );
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::PlaceImageDialog => {
                    // Import (#176): pick an image and embed it, same path as
                    // File → Place Image….
                    if let Some(path) = super::run_file_dialog(|| {
                        rfd::FileDialog::new()
                            .add_filter(
                                "Images",
                                &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tiff"],
                            )
                            .pick_file()
                    }) {
                        self.place_image_file(doc, history, &path);
                        doc_modified = true;
                    }
                }

                PanelAction::OpenEditDuration { seq, track, clip } => {
                    // K-A6: seed the Edit Duration floating form from the live clip.
                    self.edit_duration_dialog =
                        crate::panels::video::duration_dialog::EditDurationDialog::seed(
                            doc, seq, track, clip,
                        );
                }

                PanelAction::FreezeFrame {
                    seq,
                    track,
                    clip,
                    at,
                } => {
                    // K-B14: hold the source frame at `at` for the clip's duration.
                    crate::app::timeline::ops_bridge::freeze_frame(
                        doc, history, seq, track, clip, at,
                    );
                    doc_modified = true;
                }

                PanelAction::ImportMotionMetadata { clip } => {
                    // Parse synchronously, right here: a bad file is then
                    // rejected while the user is still looking at the picker,
                    // rather than surfacing later as a mysterious failed
                    // analysis. 22 §6.6 — report, never guess.
                    let dialog = rfd::FileDialog::new()
                        .add_filter("Gyro metadata", &["gcsv", "json"])
                        .set_title("Select gyro/IMU sidecar");
                    if let Some(path) = super::run_file_dialog(move || dialog.pick_file()) {
                        match photonic_video::media::parse_motion(&path) {
                            Ok(series) => {
                                let spec = photonic_core::timeline::StabilizationSpec::new(
                                    photonic_core::timeline::MotionBinding {
                                        source: photonic_core::timeline::MotionSourceRef::Sidecar {
                                            path: path.clone(),
                                            rel_path: None,
                                            format: series.format,
                                        },
                                        sync: Default::default(),
                                        // Uncalibrated until the user picks a
                                        // profile: rotation-only is the honest
                                        // default, and 22 §6.6 requires it be
                                        // an explicit state rather than a
                                        // silent fallback.
                                        lens: photonic_core::timeline::LensProfileRef::RotationOnly,
                                    },
                                );
                                if crate::app::timeline::ops_bridge::set_clip_stabilization(
                                    doc,
                                    history,
                                    clip,
                                    Some(spec),
                                ) {
                                    doc_modified = true;
                                    self.file_status = Some(format!(
                                        "Bound {} — {} samples{}{}. Run Analyze to stabilize.",
                                        path.file_name()
                                            .map(|n| n.to_string_lossy().into_owned())
                                            .unwrap_or_default(),
                                        series.samples.len(),
                                        series
                                            .sample_rate_hz()
                                            .map(|hz| format!(", {hz:.0} Hz"))
                                            .unwrap_or_default(),
                                        if series.has_accel() {
                                            ", with accelerometer"
                                        } else {
                                            ", gyro only (horizon lock unavailable)"
                                        },
                                    ));
                                }
                            }
                            Err(e) => {
                                self.file_status = Some(format!("Motion metadata rejected: {e}"));
                            }
                        }
                    }
                }

                PanelAction::AnalyzeStabilization { clip } => {
                    // Runs on the engine thread: integrating a long clip's
                    // motion series would stall the UI.
                    match self.engine.as_ref() {
                        Some(engine) => {
                            engine.send_analyze_stabilization(clip);
                            self.file_status = Some("Stabilization analysis running…".into());
                        }
                        // No engine means no preview to stabilize; say so
                        // rather than silently dropping the request.
                        None => {
                            self.file_status = Some(
                                "Stabilization needs the video engine; open a sequence first."
                                    .into(),
                            );
                        }
                    }
                }

                PanelAction::OpenExportDialog => {
                    // Export (#176): open the export dialog seeded from the
                    // Document-tab settings (format, scale, area).
                    let s = self.doc_export;
                    let mut dlg = super::ExportDialog::new(doc);
                    dlg.format = s.format;
                    let scale = s.scale.max(0.05);
                    let (base_w, base_h) = match s.area {
                        super::ExportArea::Document => (doc.width, doc.height),
                        super::ExportArea::ContentBounds => {
                            dlg.crop_to_content = true;
                            (doc.width, doc.height)
                        }
                        super::ExportArea::Artboard => match doc.active_artboard() {
                            Some(a) => {
                                dlg.region_override = Some((a.x, a.y, a.width, a.height));
                                dlg.artboard_target = Some(a.id);
                                (a.width, a.height)
                            }
                            None => {
                                self.file_status = Some(
                                    "No artboard to export — exporting the whole document.".into(),
                                );
                                (doc.width, doc.height)
                            }
                        },
                        super::ExportArea::Selection => {
                            let ids: Vec<_> = doc.selection.node_ids.iter().copied().collect();
                            let mut bbox: Option<(f64, f64, f64, f64)> = None;
                            for id in &ids {
                                if let Some(n) = doc.nodes.get(id) {
                                    if let Some((x0, y0, x1, y1)) =
                                        super::hit_test::node_world_aabb_opt(n)
                                    {
                                        bbox = Some(match bbox {
                                            Some((a, b, c, d)) => {
                                                (a.min(x0), b.min(y0), c.max(x1), d.max(y1))
                                            }
                                            None => (x0, y0, x1, y1),
                                        });
                                    }
                                }
                            }
                            match bbox {
                                Some((x0, y0, x1, y1)) => {
                                    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
                                    dlg.region_override = Some((x0, y0, w, h));
                                    (w, h)
                                }
                                None => {
                                    self.file_status = Some(
                                        "No exportable selection bounds — exporting the whole document."
                                            .into(),
                                    );
                                    (doc.width, doc.height)
                                }
                            }
                        }
                    };
                    dlg.png_width = ((base_w as f32) * scale).round().max(1.0) as u32;
                    dlg.png_height = ((base_h as f32) * scale).round().max(1.0) as u32;
                    self.export_dialog = Some(dlg);
                }

                PanelAction::OpenArtboardExportDialog => {
                    // Batch export: one file per artboard, defaulting to all.
                    let s = self.doc_export;
                    let n = doc.artboards.len().max(1);
                    let mut dlg = super::ExportDialog::new(doc);
                    dlg.format = match s.format {
                        // SVG/ICO have no batch form; fall back to PNG.
                        super::ExportFormat::Svg | super::ExportFormat::Ico => {
                            super::ExportFormat::Png
                        }
                        other => other,
                    };
                    dlg.artboard_export = super::ArtboardExport::Range { start: 1, end: n };
                    dlg.artboard_scale = s.scale.max(0.05);
                    self.export_dialog = Some(dlg);
                }

                PanelAction::SetLayerColor { layer_id, color } => {
                    if let Some(layer) = doc.layers.get(&layer_id) {
                        let cmd = Command::UpdateLayer {
                            layer_id,
                            old_name: layer.name.clone(),
                            new_name: layer.name.clone(),
                            old_visible: layer.visible,
                            new_visible: layer.visible,
                            old_locked: layer.locked,
                            new_locked: layer.locked,
                            old_color: layer.color,
                            new_color: color,
                            old_is_template: layer.is_template,
                            new_is_template: layer.is_template,
                            old_opacity: layer.opacity,
                            new_opacity: layer.opacity,
                            old_blend_mode: layer.blend_mode,
                            new_blend_mode: layer.blend_mode,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::SetLayerTemplate {
                    layer_id,
                    is_template,
                } => {
                    if let Some(layer) = doc.layers.get(&layer_id) {
                        let cmd = Command::UpdateLayer {
                            layer_id,
                            old_name: layer.name.clone(),
                            new_name: layer.name.clone(),
                            old_visible: layer.visible,
                            new_visible: layer.visible,
                            old_locked: layer.locked,
                            // Template layers are implicitly locked.
                            new_locked: if is_template { true } else { layer.locked },
                            old_color: layer.color,
                            new_color: layer.color,
                            old_is_template: layer.is_template,
                            new_is_template: is_template,
                            old_opacity: layer.opacity,
                            new_opacity: layer.opacity,
                            old_blend_mode: layer.blend_mode,
                            new_blend_mode: layer.blend_mode,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::RenameLayer { layer_id, name } => {
                    if let Some(layer) = doc.layers.get(&layer_id) {
                        let cmd = Command::UpdateLayer {
                            layer_id,
                            old_name: layer.name.clone(),
                            new_name: name.clone(),
                            old_visible: layer.visible,
                            new_visible: layer.visible,
                            old_locked: layer.locked,
                            new_locked: layer.locked,
                            old_color: layer.color,
                            new_color: layer.color,
                            old_is_template: layer.is_template,
                            new_is_template: layer.is_template,
                            old_opacity: layer.opacity,
                            new_opacity: layer.opacity,
                            old_blend_mode: layer.blend_mode,
                            new_blend_mode: layer.blend_mode,
                        };
                        history.execute(cmd, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::DeleteLayer { layer_id } => {
                    self.do_delete_layer(layer_id, doc, history, &mut doc_modified);
                }

                PanelAction::SetLayerVisible { layer_id, visible } => {
                    self.do_set_layer_flag(
                        layer_id,
                        Some(visible),
                        None,
                        None,
                        None,
                        doc,
                        history,
                        &mut doc_modified,
                    );
                }

                PanelAction::SetLayerLocked { layer_id, locked } => {
                    self.do_set_layer_flag(
                        layer_id,
                        None,
                        Some(locked),
                        None,
                        None,
                        doc,
                        history,
                        &mut doc_modified,
                    );
                }

                PanelAction::SetLayerOpacity { layer_id, opacity } => {
                    self.do_set_layer_flag(
                        layer_id,
                        None,
                        None,
                        Some(opacity),
                        None,
                        doc,
                        history,
                        &mut doc_modified,
                    );
                }

                PanelAction::SetLayerBlendMode {
                    layer_id,
                    blend_mode,
                } => {
                    self.do_set_layer_flag(
                        layer_id,
                        None,
                        None,
                        None,
                        Some(blend_mode),
                        doc,
                        history,
                        &mut doc_modified,
                    );
                }

                PanelAction::OpenLayerOptions { layer_id } => {
                    if let Some(layer) = doc.layers.get(&layer_id) {
                        self.object_options_dialog =
                            Some(ObjectOptionsDialog::from_layer(layer_id, layer));
                    }
                }

                PanelAction::OpenObjectOptions { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        self.object_options_dialog =
                            Some(ObjectOptionsDialog::from_node(node_id, node));
                    }
                }

                PanelAction::DuplicateLayer { layer_id } => {
                    self.do_duplicate_layer(layer_id, doc, history, &mut doc_modified);
                }

                PanelAction::AddSublayer => {
                    self.do_add_sublayer(doc, history, &mut doc_modified);
                }

                PanelAction::AddLayerMaskSmart => {
                    self.do_add_layer_mask_smart(doc, history, &mut doc_modified);
                }

                PanelAction::AddAdjustmentLayer { kind } => {
                    self.do_add_adjustment_layer(&kind, doc, history, &mut doc_modified);
                }

                PanelAction::OpenColorPopup { node_id, stroke } => {
                    // Anchor the picker at the current pointer (the radial-menu
                    // click site); fall back to a sensible default off-screen.
                    let pos = ctx.pointer_latest_pos().unwrap_or(egui::pos2(240.0, 240.0));
                    self.color_popup = Some(ColorPopupState {
                        node_id,
                        stroke,
                        pos,
                    });
                }

                PanelAction::AlignNodes {
                    operation,
                    key_object_id,
                } => {
                    use photonic_core::transform::Transform;

                    let sel_ids: Vec<NodeId> = doc.selection.ids().copied().collect();
                    if sel_ids.len() >= 2 {
                        let world_bounds = |node: &SceneNode| -> Option<(f64, f64, f64, f64)> {
                            let local = node.local_bounds()?;
                            let corners = [
                                (local.x0, local.y0),
                                (local.x1, local.y0),
                                (local.x1, local.y1),
                                (local.x0, local.y1),
                            ];
                            let pts: Vec<(f64, f64)> = corners
                                .iter()
                                .map(|(x, y)| node.transform.apply(*x, *y))
                                .collect();
                            let min_x = pts.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
                            let min_y = pts.iter().map(|(_, y)| *y).fold(f64::INFINITY, f64::min);
                            let max_x = pts
                                .iter()
                                .map(|(x, _)| *x)
                                .fold(f64::NEG_INFINITY, f64::max);
                            let max_y = pts
                                .iter()
                                .map(|(_, y)| *y)
                                .fold(f64::NEG_INFINITY, f64::max);
                            Some((min_x, min_y, max_x, max_y))
                        };
                        let node_bounds: Vec<(SceneNode, (f64, f64, f64, f64))> = sel_ids
                            .iter()
                            .filter_map(|id| {
                                doc.nodes
                                    .get(id)
                                    .and_then(|n| world_bounds(n).map(|b| (n.clone(), b)))
                            })
                            .collect();
                        if node_bounds.len() >= 2 {
                            let (ref_x0, ref_y0, ref_x1, ref_y1) =
                                if let Some(key_id) = key_object_id {
                                    node_bounds
                                        .iter()
                                        .find(|(n, _)| n.id == key_id)
                                        .map(|(_, b)| *b)
                                        .unwrap_or_else(|| {
                                            let x0 = node_bounds
                                                .iter()
                                                .map(|(_, b)| b.0)
                                                .fold(f64::INFINITY, f64::min);
                                            let y0 = node_bounds
                                                .iter()
                                                .map(|(_, b)| b.1)
                                                .fold(f64::INFINITY, f64::min);
                                            let x1 = node_bounds
                                                .iter()
                                                .map(|(_, b)| b.2)
                                                .fold(f64::NEG_INFINITY, f64::max);
                                            let y1 = node_bounds
                                                .iter()
                                                .map(|(_, b)| b.3)
                                                .fold(f64::NEG_INFINITY, f64::max);
                                            (x0, y0, x1, y1)
                                        })
                                } else {
                                    let x0 = node_bounds
                                        .iter()
                                        .map(|(_, b)| b.0)
                                        .fold(f64::INFINITY, f64::min);
                                    let y0 = node_bounds
                                        .iter()
                                        .map(|(_, b)| b.1)
                                        .fold(f64::INFINITY, f64::min);
                                    let x1 = node_bounds
                                        .iter()
                                        .map(|(_, b)| b.2)
                                        .fold(f64::NEG_INFINITY, f64::max);
                                    let y1 = node_bounds
                                        .iter()
                                        .map(|(_, b)| b.3)
                                        .fold(f64::NEG_INFINITY, f64::max);
                                    (x0, y0, x1, y1)
                                };
                            let ref_cx = (ref_x0 + ref_x1) / 2.0;
                            let ref_cy = (ref_y0 + ref_y1) / 2.0;
                            let mut cmds: Vec<Command> = Vec::new();
                            for (node, bounds) in &node_bounds {
                                // Skip the key object — it is the reference, not moved.
                                if key_object_id.map(|k| k == node.id).unwrap_or(false) {
                                    continue;
                                }
                                let (nx0, ny0, nx1, ny1) = *bounds;
                                let ncx = (nx0 + nx1) / 2.0;
                                let ncy = (ny0 + ny1) / 2.0;
                                let (dx, dy) = match operation.as_str() {
                                    "left" => (ref_x0 - nx0, 0.0),
                                    "center_horizontal" => (ref_cx - ncx, 0.0),
                                    "right" => (ref_x1 - nx1, 0.0),
                                    "top" => (0.0, ref_y0 - ny0),
                                    "center_vertical" => (0.0, ref_cy - ncy),
                                    "bottom" => (0.0, ref_y1 - ny1),
                                    _ => (0.0, 0.0),
                                };
                                if dx.abs() > 1e-9 || dy.abs() > 1e-9 {
                                    let old = node.clone();
                                    let mut new = old.clone();
                                    new.transform =
                                        new.transform.then(&Transform::translate(dx, dy));
                                    cmds.push(Command::UpdateNode { old, new });
                                }
                            }
                            if !cmds.is_empty() {
                                history.execute(Command::Batch(cmds), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }
                PanelAction::AlignToArtboard { operation } => {
                    use photonic_core::transform::Transform;

                    let sel_ids: Vec<NodeId> = doc.selection.ids().copied().collect();
                    if !sel_ids.is_empty() {
                        let ref_x0 = 0.0_f64;
                        let ref_y0 = 0.0_f64;
                        let ref_x1 = doc.width;
                        let ref_y1 = doc.height;
                        let ref_cx = ref_x1 / 2.0;
                        let ref_cy = ref_y1 / 2.0;

                        let world_bounds = |node: &SceneNode| -> Option<(f64, f64, f64, f64)> {
                            let local = node.local_bounds()?;
                            let corners = [
                                (local.x0, local.y0),
                                (local.x1, local.y0),
                                (local.x1, local.y1),
                                (local.x0, local.y1),
                            ];
                            let pts: Vec<(f64, f64)> = corners
                                .iter()
                                .map(|(x, y)| node.transform.apply(*x, *y))
                                .collect();
                            let min_x = pts.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
                            let min_y = pts.iter().map(|(_, y)| *y).fold(f64::INFINITY, f64::min);
                            let max_x = pts
                                .iter()
                                .map(|(x, _)| *x)
                                .fold(f64::NEG_INFINITY, f64::max);
                            let max_y = pts
                                .iter()
                                .map(|(_, y)| *y)
                                .fold(f64::NEG_INFINITY, f64::max);
                            Some((min_x, min_y, max_x, max_y))
                        };

                        let mut cmds: Vec<Command> = Vec::new();
                        for id in &sel_ids {
                            if let Some(node) = doc.nodes.get(id) {
                                if let Some((nx0, ny0, nx1, ny1)) = world_bounds(node) {
                                    let ncx = (nx0 + nx1) / 2.0;
                                    let ncy = (ny0 + ny1) / 2.0;
                                    let (dx, dy) = match operation.as_str() {
                                        "left" => (ref_x0 - nx0, 0.0),
                                        "center_horizontal" => (ref_cx - ncx, 0.0),
                                        "right" => (ref_x1 - nx1, 0.0),
                                        "top" => (0.0, ref_y0 - ny0),
                                        "center_vertical" => (0.0, ref_cy - ncy),
                                        "bottom" => (0.0, ref_y1 - ny1),
                                        _ => (0.0, 0.0),
                                    };
                                    if dx.abs() > 1e-9 || dy.abs() > 1e-9 {
                                        let old = node.clone();
                                        let mut new = old.clone();
                                        new.transform =
                                            new.transform.then(&Transform::translate(dx, dy));
                                        cmds.push(Command::UpdateNode { old, new });
                                    }
                                }
                            }
                        }
                        if !cmds.is_empty() {
                            history.execute(Command::Batch(cmds), doc);
                            doc_modified = true;
                        }
                    }
                }
                PanelAction::ClearGuides => {
                    let old_guides = doc.guides.clone();
                    let new_guides: Vec<_> =
                        old_guides.iter().filter(|g| g.locked).cloned().collect();
                    let removed = old_guides.len() - new_guides.len();
                    if removed > 0 {
                        history.execute(
                            Command::SetGuides {
                                old: old_guides,
                                new: new_guides,
                            },
                            doc,
                        );
                        doc_modified = true;
                    }
                }

                PanelAction::ConvertToSmooth { node_ids } => {
                    convert_anchor_points_gui(true, node_ids, doc, history, &mut doc_modified);
                }

                PanelAction::ConvertToCorner { node_ids } => {
                    convert_anchor_points_gui(false, node_ids, doc, history, &mut doc_modified);
                }

                PanelAction::BlendColors {
                    node_ids,
                    direction,
                } => {
                    use photonic_core::style::FillKind;
                    use photonic_core::Color;

                    // Resolve node list: empty vec means "use current selection".
                    let ids: Vec<NodeId> = if node_ids.is_empty() {
                        doc.selection.ids().copied().collect()
                    } else {
                        node_ids
                    };

                    if ids.len() < 2 {
                        // Not enough nodes — silently ignore.
                    } else {
                        // Collect path nodes, filtering non-path kinds.
                        let mut nodes: Vec<SceneNode> = ids
                            .iter()
                            .filter_map(|id| doc.nodes.get(id))
                            .filter(|n| matches!(n.kind, SceneNodeKind::Path(_)))
                            .cloned()
                            .collect();

                        // Sort by the requested direction.
                        match direction.as_str() {
                            "horizontal" => {
                                nodes.sort_by(|a, b| {
                                    gui_path_center_x(a)
                                        .partial_cmp(&gui_path_center_x(b))
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                });
                            }
                            "vertical" => {
                                nodes.sort_by(|a, b| {
                                    gui_path_center_y(a)
                                        .partial_cmp(&gui_path_center_y(b))
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                });
                            }
                            "depth" => {
                                let mut z_index: std::collections::HashMap<NodeId, usize> =
                                    std::collections::HashMap::new();
                                let mut z = 0usize;
                                for layer_id in &doc.layer_order {
                                    if let Some(layer) = doc.layers.get(layer_id) {
                                        for &nid in &layer.node_ids {
                                            z_index.insert(nid, z);
                                            z += 1;
                                        }
                                    }
                                }
                                nodes.sort_by_key(|n| z_index.get(&n.id).copied().unwrap_or(0));
                            }
                            _ => {} // no sort — use provided order
                        }

                        let n = nodes.len();
                        if n >= 2 {
                            // Extract endpoint solid fill colors.
                            let start_opt = match &nodes[0].kind {
                                SceneNodeKind::Path(p) => match &p.fill.kind {
                                    FillKind::Solid(c) => Some(*c),
                                    _ => None,
                                },
                                _ => None,
                            };
                            let end_opt = match &nodes[n - 1].kind {
                                SceneNodeKind::Path(p) => match &p.fill.kind {
                                    FillKind::Solid(c) => Some(*c),
                                    _ => None,
                                },
                                _ => None,
                            };

                            if let (Some(start_color), Some(end_color)) = (start_opt, end_opt) {
                                let mut cmds: Vec<Command> = Vec::new();
                                for (i, node) in nodes.iter().enumerate() {
                                    if i == 0 || i == n - 1 {
                                        continue;
                                    }
                                    let t = i as f32 / (n - 1) as f32;
                                    let blended = Color {
                                        r: start_color.r + t * (end_color.r - start_color.r),
                                        g: start_color.g + t * (end_color.g - start_color.g),
                                        b: start_color.b + t * (end_color.b - start_color.b),
                                        a: start_color.a + t * (end_color.a - start_color.a),
                                    };
                                    let mut new_node = node.clone();
                                    if let SceneNodeKind::Path(ref mut p) = new_node.kind {
                                        p.fill.kind = FillKind::Solid(blended);
                                    }
                                    cmds.push(Command::UpdateNode {
                                        old: node.clone(),
                                        new: new_node,
                                    });
                                }
                                if !cmds.is_empty() {
                                    history.execute(Command::Batch(cmds), doc);
                                    doc_modified = true;
                                }
                            }
                        }
                    }
                }

                PanelAction::ZigZagPath {
                    node_ids,
                    size,
                    ridges,
                    smooth,
                } => {
                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                let bez = pn.path_data.to_bez_path();
                                let new_bez = gui_zig_zag(&bez, size, ridges, smooth);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                                commands.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !commands.is_empty() {
                        for cmd in commands {
                            history.execute(cmd, doc);
                        }
                        doc_modified = true;
                    }
                }

                PanelAction::PuckerBloat { node_ids, strength } => {
                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                let bez = pn.path_data.to_bez_path();
                                let center = gui_path_centroid(&bez);
                                let new_bez = gui_pucker_bloat(&bez, strength, center);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                                commands.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !commands.is_empty() {
                        for cmd in commands {
                            history.execute(cmd, doc);
                        }
                        doc_modified = true;
                    }
                }

                PanelAction::AddDropShadow { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let mut shadow = node.clone();
                        shadow.id = uuid::Uuid::new_v4();
                        shadow.name = format!("{} Shadow", node.name);
                        shadow.opacity = 0.4;
                        shadow.transform.matrix[4] += 5.0;
                        shadow.transform.matrix[5] += 5.0;
                        match &mut shadow.kind {
                            SceneNodeKind::Path(pn) => {
                                pn.fill = Fill::solid(photonic_core::color::Color::new(
                                    0.0, 0.0, 0.0, 1.0,
                                ));
                                pn.stroke = Stroke::none();
                            }
                            SceneNodeKind::Text(tn) => {
                                tn.fill = Fill::solid(photonic_core::color::Color::new(
                                    0.0, 0.0, 0.0, 1.0,
                                ));
                                tn.stroke = Stroke::none();
                            }
                            SceneNodeKind::Group(_) => {}
                            // raster nodes have no vector fill/stroke
                            SceneNodeKind::Raster(_) => {}
                        }
                        history.execute(
                            Command::AddNode {
                                node: shadow,
                                layer_id: Some(node.layer_id),
                            },
                            doc,
                        );
                        doc_modified = true;
                    }
                }

                PanelAction::SetTextTypography {
                    node_id,
                    line_height,
                    letter_spacing,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if let SceneNodeKind::Text(_tn) = &node.kind {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut new_tn) = new_node.kind {
                                if let Some(lh) = line_height {
                                    new_tn.line_height = lh;
                                }
                                if let Some(ls) = letter_spacing {
                                    new_tn.letter_spacing = ls;
                                }
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

                PanelAction::FlipNodes {
                    node_ids,
                    horizontal,
                } => {
                    if node_ids.len() <= 1 {
                        // Single node: mirror the path geometry in place (unchanged).
                        let mut commands = Vec::new();
                        for nid in &node_ids {
                            if let Some(node) = doc.nodes.get(nid) {
                                if let SceneNodeKind::Path(pn) = &node.kind {
                                    use kurbo::Shape;
                                    let bez = pn.path_data.to_bez_path();
                                    let bbox = bez.bounding_box();
                                    let cx = bbox.x0 + bbox.width() / 2.0;
                                    let cy = bbox.y0 + bbox.height() / 2.0;
                                    let flip = |p: kurbo::Point| -> kurbo::Point {
                                        kurbo::Point::new(
                                            if horizontal { 2.0 * cx - p.x } else { p.x },
                                            if !horizontal { 2.0 * cy - p.y } else { p.y },
                                        )
                                    };
                                    let mut new_bez = BezPath::new();
                                    for el in bez.elements() {
                                        match *el {
                                            PathEl::MoveTo(p) => new_bez.move_to(flip(p)),
                                            PathEl::LineTo(p) => new_bez.line_to(flip(p)),
                                            PathEl::CurveTo(c1, c2, p) => {
                                                new_bez.curve_to(flip(c1), flip(c2), flip(p))
                                            }
                                            PathEl::QuadTo(c, p) => {
                                                new_bez.quad_to(flip(c), flip(p))
                                            }
                                            PathEl::ClosePath => new_bez.close_path(),
                                        }
                                    }
                                    let mut new_node = node.clone();
                                    if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                        new_pn.path_data = PathData::from_bez_path(&new_bez);
                                    }
                                    commands.push(Command::UpdateNode {
                                        old: node.clone(),
                                        new: new_node,
                                    });
                                }
                            }
                        }
                        if !commands.is_empty() {
                            for cmd in commands {
                                history.execute(cmd, doc);
                            }
                            doc_modified = true;
                        }
                    } else {
                        // Multi: mirror the whole selection about its shared center
                        // (any node kind), as one undoable step.
                        let (cx, cy) = selection_canvas_bounds(doc, &node_ids, renderer)
                            .map(|(x0, y0, x1, y1)| ((x0 + x1) / 2.0, (y0 + y1) / 2.0))
                            .unwrap_or((0.0, 0.0));
                        let (sx, sy) = if horizontal { (-1.0, 1.0) } else { (1.0, -1.0) };
                        let m = photonic_core::transform::Transform::scale_around(sx, sy, cx, cy);
                        let mut cmds = Vec::new();
                        for nid in &node_ids {
                            if let Some(node) = doc.nodes.get(nid) {
                                let mut new_node = node.clone();
                                // Apply in WORLD space: node transform first, then the
                                // mirror/shear about the shared pivot (correct after moves).
                                new_node.transform = m.then(&node.transform);
                                cmds.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                        if !cmds.is_empty() {
                            history.execute(Command::Batch(cmds), doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::MirrorCopy { node_ids, axis } => {
                    let flip_h = axis != "vertical";
                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid).cloned() {
                            let layer_id = node.layer_id;
                            let mut cloned = node.clone();
                            cloned.id = uuid::Uuid::new_v4();
                            cloned.name = if cloned.name.is_empty() {
                                "mirror".to_string()
                            } else {
                                format!("{} mirror", cloned.name)
                            };

                            if let SceneNodeKind::Path(ref pn) = node.kind {
                                use kurbo::Shape;
                                let bez = pn.path_data.to_bez_path();
                                let bbox = bez.bounding_box();
                                let cx = bbox.x0 + bbox.width() / 2.0;
                                let cy = bbox.y0 + bbox.height() / 2.0;
                                let flip = |p: kurbo::Point| -> kurbo::Point {
                                    kurbo::Point::new(
                                        if flip_h { 2.0 * cx - p.x } else { p.x },
                                        if !flip_h { 2.0 * cy - p.y } else { p.y },
                                    )
                                };
                                let mut new_bez = BezPath::new();
                                for el in bez.elements() {
                                    match *el {
                                        PathEl::MoveTo(p) => new_bez.move_to(flip(p)),
                                        PathEl::LineTo(p) => new_bez.line_to(flip(p)),
                                        PathEl::CurveTo(c1, c2, p) => {
                                            new_bez.curve_to(flip(c1), flip(c2), flip(p))
                                        }
                                        PathEl::QuadTo(c, p) => new_bez.quad_to(flip(c), flip(p)),
                                        PathEl::ClosePath => new_bez.close_path(),
                                    }
                                }
                                if let SceneNodeKind::Path(ref mut new_pn) = cloned.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                            } else if flip_h {
                                cloned.transform.matrix[0] *= -1.0;
                                cloned.transform.matrix[2] *= -1.0;
                            } else {
                                cloned.transform.matrix[1] *= -1.0;
                                cloned.transform.matrix[3] *= -1.0;
                            }
                            commands.push(Command::AddNode {
                                layer_id: Some(layer_id),
                                node: cloned,
                            });
                        }
                    }
                    if !commands.is_empty() {
                        let batch = if commands.len() == 1 {
                            commands.remove(0)
                        } else {
                            Command::Batch(commands)
                        };
                        history.execute(batch, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::RotateCopies { node_id, count } => {
                    use photonic_core::transform::Transform;
                    if count >= 2 {
                        if let Some(src) = doc.nodes.get(&node_id).cloned() {
                            let layer_id = src.layer_id;
                            let (cx, cy) = if let Some(lb) = src.local_bounds() {
                                let (x0, y0) = src.transform.apply(lb.x0, lb.y0);
                                let (x1, y1) = src.transform.apply(lb.x1, lb.y1);
                                ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
                            } else {
                                src.transform.apply(0.0, 0.0)
                            };
                            let angle_step = std::f64::consts::TAU / count as f64;
                            let orig_tx = src.transform.matrix[4];
                            let orig_ty = src.transform.matrix[5];
                            let mut cmds: Vec<Command> = Vec::new();
                            for i in 1..count {
                                let angle = angle_step * i as f64;
                                let rot = Transform::rotate_around(angle, cx, cy);
                                let mut copy = src.clone();
                                copy.id = uuid::Uuid::new_v4();
                                copy.name = format!("{} copy {}", src.name, i);
                                copy.transform = src.transform.then(&rot);
                                let (rot_tx, rot_ty) = rot.apply(orig_tx, orig_ty);
                                copy.transform.matrix[4] = rot_tx;
                                copy.transform.matrix[5] = rot_ty;
                                cmds.push(Command::AddNode {
                                    node: copy,
                                    layer_id: Some(layer_id),
                                });
                            }
                            if !cmds.is_empty() {
                                history.execute(Command::Batch(cmds), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::CopyAppearance {
                    source_id,
                    target_ids,
                    copy_fill,
                    copy_stroke,
                    copy_opacity,
                } => {
                    if let Some(src) = doc.nodes.get(&source_id).cloned() {
                        let src_fill = if let SceneNodeKind::Path(ref p) = src.kind {
                            Some(p.fill.clone())
                        } else {
                            None
                        };
                        let src_stroke = if let SceneNodeKind::Path(ref p) = src.kind {
                            Some(p.stroke.clone())
                        } else {
                            None
                        };
                        let src_opacity = src.opacity;
                        let mut cmds: Vec<Command> = Vec::new();
                        for tid in target_ids {
                            if let Some(tgt) = doc.nodes.get(&tid).cloned() {
                                let mut new_node = tgt.clone();
                                if copy_opacity {
                                    new_node.opacity = src_opacity;
                                }
                                if let SceneNodeKind::Path(ref mut p) = new_node.kind {
                                    if copy_fill {
                                        if let Some(ref f) = src_fill {
                                            p.fill = f.clone();
                                        }
                                    }
                                    if copy_stroke {
                                        if let Some(ref s) = src_stroke {
                                            p.stroke = s.clone();
                                        }
                                    }
                                }
                                cmds.push(Command::UpdateNode {
                                    old: tgt,
                                    new: new_node,
                                });
                            }
                        }
                        if !cmds.is_empty() {
                            let batch = if cmds.len() == 1 {
                                cmds.remove(0)
                            } else {
                                Command::Batch(cmds)
                            };
                            history.execute(batch, doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::RemoveExportProfile { name } => {
                    doc.export_profiles.retain(|p| p.name != name);
                    doc_modified = true;
                }

                PanelAction::PinObjectGuides { node_ids } => {
                    let tolerance = 0.5_f64;
                    let mut new_guides: Vec<photonic_core::Guide> = Vec::new();

                    let add_h =
                        |pos: f64,
                         new_guides: &mut Vec<photonic_core::Guide>,
                         doc_guides: &[photonic_core::Guide]| {
                            let exists = doc_guides.iter().chain(new_guides.iter()).any(|g| {
                                g.orientation == photonic_core::GuideOrientation::Horizontal
                                    && (g.position - pos).abs() < tolerance
                            });
                            if !exists {
                                new_guides.push(photonic_core::Guide::new(
                                    photonic_core::GuideOrientation::Horizontal,
                                    pos,
                                ));
                            }
                        };
                    let add_v =
                        |pos: f64,
                         new_guides: &mut Vec<photonic_core::Guide>,
                         doc_guides: &[photonic_core::Guide]| {
                            let exists = doc_guides.iter().chain(new_guides.iter()).any(|g| {
                                g.orientation == photonic_core::GuideOrientation::Vertical
                                    && (g.position - pos).abs() < tolerance
                            });
                            if !exists {
                                new_guides.push(photonic_core::Guide::new(
                                    photonic_core::GuideOrientation::Vertical,
                                    pos,
                                ));
                            }
                        };

                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            let tx = node.transform.matrix[4];
                            let ty = node.transform.matrix[5];
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                use kurbo::Shape;
                                let bez = pn.path_data.to_bez_path();
                                let bb = bez.bounding_box();
                                let (x0, y0, x1, y1) =
                                    (bb.x0 + tx, bb.y0 + ty, bb.x1 + tx, bb.y1 + ty);
                                add_h(y0, &mut new_guides, &doc.guides);
                                add_h(y1, &mut new_guides, &doc.guides);
                                add_h((y0 + y1) / 2.0, &mut new_guides, &doc.guides);
                                add_v(x0, &mut new_guides, &doc.guides);
                                add_v(x1, &mut new_guides, &doc.guides);
                                add_v((x0 + x1) / 2.0, &mut new_guides, &doc.guides);
                            }
                        }
                    }
                    if !new_guides.is_empty() {
                        doc.guides.extend(new_guides);
                        doc_modified = true;
                    }
                }

                PanelAction::ReverseNodeOrder { node_ids } => {
                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid).cloned() {
                            if let SceneNodeKind::Group(ref g) = node.kind {
                                if g.children.len() > 1 {
                                    let mut new_node = node.clone();
                                    if let SceneNodeKind::Group(ref mut ng) = new_node.kind {
                                        ng.children.reverse();
                                    }
                                    commands.push(Command::UpdateNode {
                                        old: node,
                                        new: new_node,
                                    });
                                }
                            }
                        }
                    }
                    if !commands.is_empty() {
                        let batch = if commands.len() == 1 {
                            commands.remove(0)
                        } else {
                            Command::Batch(commands)
                        };
                        history.execute(batch, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::ApplyParagraphStyle {
                    node_id,
                    style_name,
                } => {
                    use photonic_core::node::TextAlign;
                    let style = doc
                        .paragraph_styles
                        .iter()
                        .find(|s| s.name == style_name)
                        .cloned();
                    if let (Some(style), Some(node)) = (style, doc.nodes.get(&node_id).cloned()) {
                        if let SceneNodeKind::Text(_) = &node.kind {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut t) = new_node.kind {
                                if let Some(align_str) = &style.align {
                                    t.align = match align_str.as_str() {
                                        "center" => TextAlign::Center,
                                        "right" => TextAlign::Right,
                                        _ => TextAlign::Left,
                                    };
                                }
                                if let Some(lh) = style.line_height {
                                    t.line_height = lh;
                                }
                                if let Some(ls) = style.letter_spacing {
                                    t.letter_spacing = ls;
                                }
                                if let Some(fs) = style.font_size {
                                    t.font_size = fs;
                                }
                                if let Some(ff) = &style.font_family {
                                    t.font_family = ff.clone();
                                }
                            }
                            history.execute(
                                Command::UpdateNode {
                                    old: node,
                                    new: new_node,
                                },
                                doc,
                            );
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::DeleteParagraphStyle { name } => {
                    doc.paragraph_styles.retain(|s| s.name != name);
                    doc_modified = true;
                }

                PanelAction::ApplyCharacterStyle {
                    node_id,
                    style_name,
                } => {
                    use photonic_core::color::Color;
                    use photonic_core::style::Fill;
                    let style = doc
                        .character_styles
                        .iter()
                        .find(|s| s.name == style_name)
                        .cloned();
                    if let (Some(style), Some(node)) = (style, doc.nodes.get(&node_id).cloned()) {
                        if let SceneNodeKind::Text(_) = &node.kind {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut t) = new_node.kind {
                                if let Some(ff) = &style.font_family {
                                    t.font_family = ff.clone();
                                }
                                if let Some(fs) = style.font_size {
                                    t.font_size = fs;
                                }
                                if let Some(fw) = style.font_weight {
                                    t.font_weight = fw;
                                }
                                if let Some(ls) = style.letter_spacing {
                                    t.letter_spacing = ls;
                                }
                                if let Some(lh) = style.line_height {
                                    t.line_height = lh;
                                }
                                if let Some(hex) = &style.fill_hex {
                                    if let Some(color) = Color::from_hex(hex) {
                                        t.fill = Fill::solid(color);
                                    }
                                }
                            }
                            history.execute(
                                Command::UpdateNode {
                                    old: node,
                                    new: new_node,
                                },
                                doc,
                            );
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::DeleteCharacterStyle { name } => {
                    doc.character_styles.retain(|s| s.name != name);
                    doc_modified = true;
                }

                PanelAction::TagNodeForExport {
                    node_id,
                    name,
                    format,
                } => {
                    use photonic_core::AssetExportSpec;
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        let mut new_node = node.clone();
                        new_node.export_spec = Some(AssetExportSpec {
                            name: name.clone(),
                            format: format.clone(),
                            scales: vec![1.0],
                        });
                        history.execute(
                            Command::UpdateNode {
                                old: node,
                                new: new_node,
                            },
                            doc,
                        );
                        doc_modified = true;
                    }
                }

                PanelAction::RemoveExportTag { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        let mut new_node = node.clone();
                        new_node.export_spec = None;
                        history.execute(
                            Command::UpdateNode {
                                old: node,
                                new: new_node,
                            },
                            doc,
                        );
                        doc_modified = true;
                    }
                }

                PanelAction::SelectSimilar { node_ids, match_by } => {
                    use photonic_core::style::FillKind;
                    let tol_f = 5.0_f32 / 255.0_f32;
                    let criteria: Vec<&str> = match_by.split(',').map(|s| s.trim()).collect();

                    // Collect reference attributes.
                    let mut ref_fills: Vec<[f32; 3]> = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            if let SceneNodeKind::Path(p) = &node.kind {
                                if p.fill.enabled {
                                    if let FillKind::Solid(c) = &p.fill.kind {
                                        ref_fills.push([c.r, c.g, c.b]);
                                    }
                                }
                            }
                        }
                    }

                    let color_matches = |a: [f32; 3]| -> bool {
                        ref_fills.iter().any(|rc| {
                            (a[0] - rc[0]).abs() <= tol_f
                                && (a[1] - rc[1]).abs() <= tol_f
                                && (a[2] - rc[2]).abs() <= tol_f
                        })
                    };

                    let matched: Vec<NodeId> = doc
                        .nodes
                        .iter()
                        .filter(|(id, node)| {
                            if node_ids.contains(id) {
                                return false;
                            }
                            for crit in &criteria {
                                let ok = match *crit {
                                    "fill_color" => match &node.kind {
                                        SceneNodeKind::Path(p) if p.fill.enabled => {
                                            if let FillKind::Solid(c) = &p.fill.kind {
                                                color_matches([c.r, c.g, c.b])
                                            } else {
                                                false
                                            }
                                        }
                                        _ => false,
                                    },
                                    "kind" => {
                                        let ref_kind = node_ids
                                            .first()
                                            .and_then(|rid| doc.nodes.get(rid))
                                            .map(|rn| match &rn.kind {
                                                SceneNodeKind::Path(_) => "path",
                                                SceneNodeKind::Text(_) => "text",
                                                SceneNodeKind::Group(_) => "group",
                                                SceneNodeKind::Raster(_) => "raster",
                                            })
                                            .unwrap_or("");
                                        let this_kind = match &node.kind {
                                            SceneNodeKind::Path(_) => "path",
                                            SceneNodeKind::Text(_) => "text",
                                            SceneNodeKind::Group(_) => "group",
                                            SceneNodeKind::Raster(_) => "raster",
                                        };
                                        this_kind == ref_kind
                                    }
                                    _ => true,
                                };
                                if !ok {
                                    return false;
                                }
                            }
                            true
                        })
                        .map(|(id, _)| *id)
                        .collect();

                    doc.selection.node_ids.clear();
                    for nid in node_ids.iter().chain(matched.iter()) {
                        doc.selection.node_ids.insert(*nid);
                    }
                    doc_modified = true;
                }

                PanelAction::CopyDocumentTemplate => {
                    // Build a node-stripped template and copy the JSON to the OS clipboard.
                    let mut template = doc.clone();
                    template.nodes.clear();
                    template.selection = Default::default();
                    for layer in template.layers.values_mut() {
                        layer.node_ids.clear();
                    }
                    if let Ok(json_str) = template.to_json() {
                        ctx.copy_text(json_str);
                    }
                }

                PanelAction::ApplyColorSwatch {
                    node_id,
                    swatch_name,
                } => {
                    if let Some(swatch) = doc.color_swatches.iter().find(|s| s.name == swatch_name)
                    {
                        if let Some(color) =
                            photonic_core::Color::from_hex(&swatch.color_hex.clone())
                        {
                            if let Some(node) = doc.nodes.get(&node_id) {
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut pn) = new_node.kind {
                                    pn.fill = Fill::solid(color);
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

                PanelAction::DeleteColorSwatch { name } => {
                    doc.color_swatches.retain(|s| s.name != name);
                    doc_modified = true;
                }

                PanelAction::LoadSwatchLibrary {
                    library,
                    clear_existing,
                } => {
                    use photonic_core::ColorSwatch;
                    let palette: &[(&str, &str)] = match library.as_str() {
                        "web" => &[
                            ("White", "#ffffff"),
                            ("Silver", "#c0c0c0"),
                            ("Gray", "#808080"),
                            ("Black", "#000000"),
                            ("Red", "#ff0000"),
                            ("Maroon", "#800000"),
                            ("Yellow", "#ffff00"),
                            ("Olive", "#808000"),
                            ("Lime", "#00ff00"),
                            ("Green", "#008000"),
                            ("Aqua", "#00ffff"),
                            ("Teal", "#008080"),
                            ("Blue", "#0000ff"),
                            ("Navy", "#000080"),
                            ("Fuchsia", "#ff00ff"),
                            ("Purple", "#800080"),
                        ],
                        "material" => &[
                            ("Red 500", "#f44336"),
                            ("Pink 500", "#e91e63"),
                            ("Purple 500", "#9c27b0"),
                            ("Deep Purple 500", "#673ab7"),
                            ("Indigo 500", "#3f51b5"),
                            ("Blue 500", "#2196f3"),
                            ("Cyan 500", "#00bcd4"),
                            ("Teal 500", "#009688"),
                            ("Green 500", "#4caf50"),
                            ("Yellow 500", "#ffeb3b"),
                            ("Orange 500", "#ff9800"),
                            ("Deep Orange 500", "#ff5722"),
                            ("Brown 500", "#795548"),
                            ("Grey 500", "#9e9e9e"),
                            ("Blue Grey 500", "#607d8b"),
                            ("White", "#ffffff"),
                        ],
                        "pastels" => &[
                            ("Pastel Pink", "#ffb3ba"),
                            ("Pastel Peach", "#ffdfba"),
                            ("Pastel Yellow", "#ffffba"),
                            ("Pastel Green", "#baffc9"),
                            ("Pastel Blue", "#bae1ff"),
                            ("Pastel Lavender", "#d4baff"),
                            ("Pastel Mint", "#b5ead7"),
                            ("Pastel Lilac", "#c7ceea"),
                            ("Pastel Coral", "#ffd7be"),
                            ("Pastel Sky", "#aec6cf"),
                            ("Pastel Lemon", "#fffacd"),
                            ("Pastel Rose", "#f2c6c2"),
                        ],
                        "earth_tones" => &[
                            ("Terracotta", "#c65d3c"),
                            ("Rust", "#b7410e"),
                            ("Burnt Sienna", "#e97451"),
                            ("Sandy Brown", "#daa06d"),
                            ("Khaki", "#c3a882"),
                            ("Tan", "#d2b48c"),
                            ("Warm Taupe", "#b09080"),
                            ("Driftwood", "#9a7b4f"),
                            ("Saddle Brown", "#8b4513"),
                            ("Dark Chocolate", "#5c3317"),
                            ("Forest Floor", "#4a3728"),
                            ("Moss", "#8a9a5b"),
                        ],
                        "neon" => &[
                            ("Neon Pink", "#ff006e"),
                            ("Neon Orange", "#fb5607"),
                            ("Neon Yellow", "#ffbe0b"),
                            ("Neon Green", "#8338ec"),
                            ("Neon Cyan", "#00f5d4"),
                            ("Neon Blue", "#3a86ff"),
                            ("Electric Lime", "#ccff00"),
                            ("Hot Magenta", "#ff00ff"),
                            ("Laser Lemon", "#ffff66"),
                            ("Neon Red", "#ff073a"),
                            ("Electric Blue", "#00b0ff"),
                            ("UV Purple", "#9400d3"),
                        ],
                        "grayscale" => &[
                            ("White", "#ffffff"),
                            ("Gray 10", "#e6e6e6"),
                            ("Gray 20", "#cccccc"),
                            ("Gray 30", "#b3b3b3"),
                            ("Gray 40", "#999999"),
                            ("Gray 50", "#808080"),
                            ("Gray 60", "#666666"),
                            ("Gray 70", "#4d4d4d"),
                            ("Gray 80", "#333333"),
                            ("Gray 90", "#1a1a1a"),
                            ("Black", "#000000"),
                        ],
                        _ => &[],
                    };
                    if clear_existing {
                        doc.color_swatches.clear();
                    }
                    for (name, hex) in palette {
                        if !doc.color_swatches.iter().any(|s| s.name == *name) {
                            doc.color_swatches.push(ColorSwatch::new(*name, *hex));
                        }
                    }
                    doc_modified = true;
                }

                PanelAction::ImportDesignTokens => {
                    // #207 GUI equivalent: pick a tokens file and register swatches.
                    if self.import_design_tokens_dialog(doc) {
                        doc_modified = true;
                    }
                }

                PanelAction::SaveWidthProfile { stroke_width, name } => {
                    use photonic_core::WidthProfile;
                    // Uniform 2-point profile — same width at both ends
                    let widths = vec![stroke_width, stroke_width];
                    let profile = WidthProfile::new(&name, widths);
                    if let Some(existing) = doc.width_profiles.iter_mut().find(|p| p.name == name) {
                        *existing = profile;
                    } else {
                        doc.width_profiles.push(profile);
                    }
                    self.width_profile_name_input.clear();
                    doc_modified = true;
                }

                PanelAction::ApplyWidthProfile {
                    node_id,
                    profile_name,
                } => {
                    let avg = doc
                        .width_profiles
                        .iter()
                        .find(|p| p.name == profile_name)
                        .map(|p| p.average_width());
                    if let Some(avg_width) = avg {
                        if let Some(node) = doc.nodes.get(&node_id).cloned() {
                            if let SceneNodeKind::Path(_) = &node.kind {
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut pn) = new_node.kind {
                                    pn.stroke.width = avg_width;
                                }
                                history.execute(
                                    Command::UpdateNode {
                                        old: node,
                                        new: new_node,
                                    },
                                    doc,
                                );
                            }
                        }
                    }
                }

                PanelAction::DeleteWidthProfile { name } => {
                    doc.width_profiles.retain(|p| p.name != name);
                    doc_modified = true;
                }

                PanelAction::RenameWidthProfile { old_name, new_name } => {
                    let new_name = new_name.trim().to_string();
                    let exists = doc.width_profiles.iter().any(|p| p.name == old_name);
                    let clashes = doc
                        .width_profiles
                        .iter()
                        .any(|p| p.name == new_name && p.name != old_name);
                    if exists && !new_name.is_empty() && !clashes {
                        let before = doc.width_profiles.clone();
                        let mut after = before.clone();
                        if let Some(p) = after.iter_mut().find(|p| p.name == old_name) {
                            p.name = new_name;
                        }
                        history.execute(
                            Command::SetWidthProfiles {
                                old: before,
                                new: after,
                            },
                            doc,
                        );
                        self.width_profile_name_input.clear();
                        doc_modified = true;
                    }
                }

                PanelAction::SaveGraphicStyle { node_id, name } => {
                    use photonic_core::GraphicStyle;
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let (fill, stroke) = match &node.kind {
                            SceneNodeKind::Path(pn) => (pn.fill.clone(), pn.stroke.clone()),
                            SceneNodeKind::Text(tn) => {
                                use photonic_core::style::Stroke;
                                (tn.fill.clone(), Stroke::none())
                            }
                            SceneNodeKind::Group(_) => {
                                use photonic_core::style::{Fill, Stroke};
                                (Fill::default(), Stroke::none())
                            }
                            // raster nodes have no vector fill/stroke
                            SceneNodeKind::Raster(_) => {
                                use photonic_core::style::{Fill, Stroke};
                                (Fill::default(), Stroke::none())
                            }
                        };
                        let fill_json = serde_json::to_string(&fill).unwrap_or_default();
                        let stroke_json = serde_json::to_string(&stroke).unwrap_or_default();
                        let style = GraphicStyle::new(&name, fill_json, stroke_json, node.opacity);
                        if let Some(existing) =
                            doc.graphic_styles.iter_mut().find(|s| s.name == name)
                        {
                            *existing = style;
                        } else {
                            doc.graphic_styles.push(style);
                        }
                        self.graphic_style_name_input.clear();
                        doc_modified = true;
                    }
                }

                PanelAction::ApplyGraphicStyle {
                    node_id,
                    style_name,
                } => {
                    use photonic_core::style::{Fill, Stroke};
                    let style_data = doc
                        .graphic_styles
                        .iter()
                        .find(|s| s.name == style_name)
                        .cloned();
                    if let Some(style) = style_data {
                        let fill: Fill = serde_json::from_str(&style.fill_json).unwrap_or_default();
                        let stroke: Stroke =
                            serde_json::from_str(&style.stroke_json).unwrap_or_default();
                        if let Some(node) = doc.nodes.get(&node_id).cloned() {
                            let mut new_node = node.clone();
                            new_node.opacity = style.opacity;
                            match &mut new_node.kind {
                                SceneNodeKind::Path(pn) => {
                                    pn.fill = fill;
                                    pn.stroke = stroke;
                                }
                                SceneNodeKind::Text(tn) => {
                                    tn.fill = fill;
                                }
                                SceneNodeKind::Group(_) => {}
                                // raster nodes have no vector fill/stroke
                                SceneNodeKind::Raster(_) => {}
                            }
                            history.execute(
                                Command::UpdateNode {
                                    old: node,
                                    new: new_node,
                                },
                                doc,
                            );
                        }
                    }
                }

                PanelAction::DeleteGraphicStyle { name } => {
                    doc.graphic_styles.retain(|s| s.name != name);
                    doc_modified = true;
                }

                PanelAction::FlattenTransparency => {
                    use photonic_core::style::{Fill, FillKind};
                    let ids: Vec<NodeId> = doc.selection.ids().copied().collect();
                    let target: Vec<NodeId> = if ids.is_empty() {
                        doc.nodes.keys().cloned().collect()
                    } else {
                        ids
                    };

                    fn bake_fill(fill: &Fill, combined: f32) -> Fill {
                        let kind = match &fill.kind {
                            FillKind::Solid(c) => FillKind::Solid(photonic_core::color::Color {
                                r: c.r,
                                g: c.g,
                                b: c.b,
                                a: c.a * combined,
                            }),
                            FillKind::Gradient(g) => {
                                let mut g2 = g.clone();
                                for stop in g2.stops.iter_mut() {
                                    stop.color.a *= combined;
                                }
                                FillKind::Gradient(g2)
                            }
                            other => other.clone(),
                        };
                        Fill {
                            kind,
                            opacity: 1.0,
                            enabled: fill.enabled,
                        }
                    }

                    let mut cmds: Vec<Command> = Vec::new();
                    for nid in target {
                        if let Some(node) = doc.nodes.get(&nid) {
                            let node_opacity = node.opacity;
                            if node_opacity >= 1.0 - f32::EPSILON
                                && match &node.kind {
                                    SceneNodeKind::Path(pn) => pn.fill.opacity >= 1.0 - 1e-6,
                                    SceneNodeKind::Text(tn) => tn.fill.opacity >= 1.0 - 1e-6,
                                    _ => true,
                                }
                            {
                                continue;
                            }
                            let mut new_node = node.clone();
                            new_node.opacity = 1.0;
                            match &mut new_node.kind {
                                SceneNodeKind::Path(pn) => {
                                    let combined = pn.fill.opacity * node_opacity;
                                    pn.fill = bake_fill(&pn.fill, combined);
                                    pn.stroke.color.a *= node_opacity;
                                    pn.stroke.opacity = 1.0;
                                }
                                SceneNodeKind::Text(tn) => {
                                    let combined = tn.fill.opacity * node_opacity;
                                    tn.fill = bake_fill(&tn.fill, combined);
                                }
                                SceneNodeKind::Group(_) => {}
                                // raster nodes have no vector fill to bake
                                SceneNodeKind::Raster(_) => {}
                            }
                            cmds.push(Command::UpdateNode {
                                old: node.clone(),
                                new: new_node,
                            });
                        }
                    }
                    if !cmds.is_empty() {
                        history.execute(Command::Batch(cmds), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::UndoNode { node_id, steps } => {
                    history.revert_node_steps(node_id, steps, doc);
                    doc_modified = true;
                }

                PanelAction::RefreshHistory => {
                    self.history_graph = history.history_graph();
                    self.history_current = history.current_node();
                }

                PanelAction::SetDocumentBleed { bleed_mm, slug_mm } => {
                    doc.bleed_mm = bleed_mm;
                    doc.slug_mm = slug_mm;
                    doc_modified = true;
                }

                PanelAction::SetArtboardMargins {
                    top,
                    right,
                    bottom,
                    left,
                } => {
                    doc.margin_top = top;
                    doc.margin_right = right;
                    doc.margin_bottom = bottom;
                    doc.margin_left = left;
                    doc_modified = true;
                }

                PanelAction::RegisterEventTrigger { event, action_name } => {
                    let already = doc
                        .event_triggers
                        .iter()
                        .any(|t| t.event == event && t.action_name == action_name);
                    let action_exists = doc.action_sets.iter().any(|a| a.name == action_name);
                    if !already && action_exists {
                        doc.event_triggers
                            .push(photonic_core::EventTrigger { event, action_name });
                        doc_modified = true;
                    }
                }

                PanelAction::RemoveEventTrigger { event, action_name } => {
                    if let Some(ref aname) = action_name {
                        doc.event_triggers
                            .retain(|t| !(t.event == event && t.action_name == *aname));
                    } else {
                        doc.event_triggers.retain(|t| t.event != event);
                    }
                    doc_modified = true;
                }

                PanelAction::AddConstructionLine {
                    x,
                    y,
                    angle_degrees,
                } => {
                    use photonic_core::document::{Guide, GuideOrientation};
                    let mut guide = Guide::new(GuideOrientation::Horizontal, 0.0);
                    guide.color = Some([1.0, 0.5, 0.0, 0.85]); // orange
                    guide.angle_degrees = Some(angle_degrees);
                    guide.position_x = x;
                    guide.position_y = y;
                    doc.guides.push(guide);
                    doc_modified = true;
                }

                PanelAction::ApplyGridLayout {
                    group_id,
                    columns,
                    gap_x,
                    gap_y,
                } => {
                    if let Some(group_node) = doc.nodes.get(&group_id) {
                        let child_ids = match &group_node.kind {
                            SceneNodeKind::Group(g) => g.children.clone(),
                            _ => vec![],
                        };
                        if child_ids.len() > 1 {
                            struct CB {
                                id: NodeId,
                                w: f64,
                                h: f64,
                            }
                            let mut children: Vec<CB> = Vec::new();
                            for cid in &child_ids {
                                if let Some(child) = doc.nodes.get(cid) {
                                    let (w, h) = match &child.kind {
                                        SceneNodeKind::Path(pn) => {
                                            if let Some(bb) = pn.path_data.bounding_box() {
                                                (
                                                    bb.width().abs().max(1.0),
                                                    bb.height().abs().max(1.0),
                                                )
                                            } else {
                                                (60.0, 30.0)
                                            }
                                        }
                                        _ => (60.0, 30.0),
                                    };
                                    children.push(CB { id: *cid, w, h });
                                }
                            }
                            let col_width = children.iter().map(|c| c.w).fold(0.0_f64, f64::max);
                            let row_height = children.iter().map(|c| c.h).fold(0.0_f64, f64::max);
                            let mut cmds: Vec<Command> = Vec::new();
                            for (i, child) in children.iter().enumerate() {
                                let col = i % columns;
                                let row = i / columns;
                                let new_tx = col as f64 * (col_width + gap_x);
                                let new_ty = row as f64 * (row_height + gap_y);
                                if let Some(old) = doc.nodes.get(&child.id) {
                                    let mut new_node = old.clone();
                                    new_node.transform.matrix[4] = new_tx;
                                    new_node.transform.matrix[5] = new_ty;
                                    cmds.push(Command::UpdateNode {
                                        old: old.clone(),
                                        new: new_node,
                                    });
                                }
                            }
                            if !cmds.is_empty() {
                                history.execute(Command::Batch(cmds), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::ApplyStackLayout {
                    group_id,
                    align_h,
                    align_v,
                } => {
                    if let Some(group_node) = doc.nodes.get(&group_id) {
                        let child_ids = match &group_node.kind {
                            SceneNodeKind::Group(g) => g.children.clone(),
                            _ => vec![],
                        };
                        if !child_ids.is_empty() {
                            struct CB {
                                id: NodeId,
                                w: f64,
                                h: f64,
                            }
                            let mut children: Vec<CB> = Vec::new();
                            let mut min_x = f64::MAX;
                            let mut min_y = f64::MAX;
                            let mut max_x = f64::MIN;
                            let mut max_y = f64::MIN;
                            for cid in &child_ids {
                                if let Some(child) = doc.nodes.get(cid) {
                                    let (w, h) = match &child.kind {
                                        SceneNodeKind::Path(pn) => {
                                            if let Some(bb) = pn.path_data.bounding_box() {
                                                (
                                                    bb.width().abs().max(1.0),
                                                    bb.height().abs().max(1.0),
                                                )
                                            } else {
                                                (60.0, 30.0)
                                            }
                                        }
                                        _ => (60.0, 30.0),
                                    };
                                    let tx = child.transform.matrix[4];
                                    let ty = child.transform.matrix[5];
                                    min_x = min_x.min(tx);
                                    min_y = min_y.min(ty);
                                    max_x = max_x.max(tx + w);
                                    max_y = max_y.max(ty + h);
                                    children.push(CB { id: *cid, w, h });
                                }
                            }
                            let union_x = min_x;
                            let union_y = min_y;
                            let union_w = (max_x - min_x).max(1.0);
                            let union_h = (max_y - min_y).max(1.0);
                            let mut cmds: Vec<Command> = Vec::new();
                            for child in &children {
                                let new_tx = match align_h.as_str() {
                                    "left" => union_x,
                                    "right" => union_x + union_w - child.w,
                                    _ => union_x + (union_w - child.w) / 2.0,
                                };
                                let new_ty = match align_v.as_str() {
                                    "top" => union_y,
                                    "bottom" => union_y + union_h - child.h,
                                    _ => union_y + (union_h - child.h) / 2.0,
                                };
                                if let Some(old) = doc.nodes.get(&child.id) {
                                    let mut new_node = old.clone();
                                    new_node.transform.matrix[4] = new_tx;
                                    new_node.transform.matrix[5] = new_ty;
                                    cmds.push(Command::UpdateNode {
                                        old: old.clone(),
                                        new: new_node,
                                    });
                                }
                            }
                            if !cmds.is_empty() {
                                history.execute(Command::Batch(cmds), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::ApplyFlexLayout {
                    group_id,
                    direction,
                    gap,
                    align,
                    padding,
                } => {
                    if let Some(group_node) = doc.nodes.get(&group_id) {
                        let child_ids = match &group_node.kind {
                            SceneNodeKind::Group(g) => g.children.clone(),
                            _ => vec![],
                        };
                        if child_ids.len() > 1 {
                            struct ChildBox {
                                id: NodeId,
                                tx: f64,
                                ty: f64,
                                w: f64,
                                h: f64,
                            }
                            let mut children: Vec<ChildBox> = Vec::new();
                            for cid in &child_ids {
                                if let Some(child) = doc.nodes.get(cid) {
                                    let (w, h) = match &child.kind {
                                        SceneNodeKind::Path(pn) => {
                                            if let Some(bb) = pn.path_data.bounding_box() {
                                                (
                                                    bb.width().abs().max(1.0),
                                                    bb.height().abs().max(1.0),
                                                )
                                            } else {
                                                (60.0, 30.0)
                                            }
                                        }
                                        _ => (60.0, 30.0),
                                    };
                                    children.push(ChildBox {
                                        id: *cid,
                                        tx: child.transform.matrix[4],
                                        ty: child.transform.matrix[5],
                                        w,
                                        h,
                                    });
                                }
                            }
                            match direction.as_str() {
                                "column" => children.sort_by(|a, b| {
                                    a.ty.partial_cmp(&b.ty).unwrap_or(std::cmp::Ordering::Equal)
                                }),
                                _ => children.sort_by(|a, b| {
                                    a.tx.partial_cmp(&b.tx).unwrap_or(std::cmp::Ordering::Equal)
                                }),
                            }
                            let cross_max: f64 = match direction.as_str() {
                                "column" => children.iter().map(|c| c.w).fold(0.0_f64, f64::max),
                                _ => children.iter().map(|c| c.h).fold(0.0_f64, f64::max),
                            };
                            let mut cursor = padding;
                            let mut cmds: Vec<Command> = Vec::new();
                            for child in &children {
                                let cross_size = match direction.as_str() {
                                    "column" => child.w,
                                    _ => child.h,
                                };
                                let cross_offset = match align.as_str() {
                                    "start" => padding,
                                    "end" => padding + cross_max - cross_size,
                                    _ => {
                                        padding
                                            + if cross_max > cross_size {
                                                (cross_max - cross_size) / 2.0
                                            } else {
                                                0.0
                                            }
                                    }
                                };
                                let (new_tx, new_ty) = match direction.as_str() {
                                    "column" => (cross_offset, cursor),
                                    _ => (cursor, cross_offset),
                                };
                                let main_size = match direction.as_str() {
                                    "column" => child.h,
                                    _ => child.w,
                                };
                                cursor += main_size + gap;
                                if let Some(old) = doc.nodes.get(&child.id) {
                                    let mut new_node = old.clone();
                                    new_node.transform.matrix[4] = new_tx;
                                    new_node.transform.matrix[5] = new_ty;
                                    cmds.push(Command::UpdateNode {
                                        old: old.clone(),
                                        new: new_node,
                                    });
                                }
                            }
                            if !cmds.is_empty() {
                                history.execute(Command::Batch(cmds), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::DefineSpotColor {
                    name,
                    hex,
                    overprint,
                } => {
                    let hex_norm = if hex.starts_with('#') {
                        hex.clone()
                    } else {
                        format!("#{}", hex)
                    };
                    if let Some(existing) = doc.spot_colors.iter_mut().find(|s| s.name == name) {
                        existing.hex = hex_norm;
                        existing.overprint = overprint;
                    } else {
                        use photonic_core::SpotColor;
                        doc.spot_colors
                            .push(SpotColor::new(name, hex_norm, overprint));
                    }
                    doc_modified = true;
                }

                PanelAction::ApplySpotColor {
                    node_id,
                    color_name,
                } => {
                    let hex = doc
                        .spot_colors
                        .iter()
                        .find(|s| s.name == color_name)
                        .map(|s| s.hex.clone());
                    if let Some(hex) = hex {
                        if let Some(color) = photonic_core::Color::from_hex(&hex) {
                            use photonic_core::style::{Fill, FillKind};
                            let fill = Fill {
                                kind: FillKind::Solid(color),
                                opacity: 1.0,
                                enabled: true,
                            };
                            if let Some(node) = doc.nodes.get(&node_id) {
                                let mut new_node = node.clone();
                                match &mut new_node.kind {
                                    SceneNodeKind::Path(pn) => {
                                        pn.fill = fill;
                                    }
                                    SceneNodeKind::Text(tn) => {
                                        tn.fill = fill;
                                    }
                                    SceneNodeKind::Group(_) => {}
                                    // raster nodes have no vector fill
                                    SceneNodeKind::Raster(_) => {}
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

                PanelAction::DeleteSpotColor { name } => {
                    doc.spot_colors.retain(|s| s.name != name);
                    doc_modified = true;
                }

                PanelAction::SaveGradientSwatch { node_id, name } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        let fill = match &node.kind {
                            SceneNodeKind::Path(pn) => Some(pn.fill.clone()),
                            _ => None,
                        };
                        if let Some(fill) = fill {
                            if let Ok(fill_json) = serde_json::to_string(&fill) {
                                use photonic_core::GradientSwatch;
                                if let Some(existing) =
                                    doc.gradient_swatches.iter_mut().find(|s| s.name == name)
                                {
                                    existing.fill_json = fill_json;
                                } else {
                                    doc.gradient_swatches
                                        .push(GradientSwatch::new(name, fill_json));
                                }
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::ApplyGradientSwatch {
                    node_id,
                    swatch_name,
                } => {
                    let fill_json = doc
                        .gradient_swatches
                        .iter()
                        .find(|s| s.name == swatch_name)
                        .map(|s| s.fill_json.clone());
                    if let Some(fill_json) = fill_json {
                        if let Ok(fill) = serde_json::from_str::<Fill>(&fill_json) {
                            if let Some(node) = doc.nodes.get(&node_id) {
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut pn) = new_node.kind {
                                    pn.fill = fill;
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

                PanelAction::DeleteGradientSwatch { name } => {
                    doc.gradient_swatches.retain(|s| s.name != name);
                    doc_modified = true;
                }

                PanelAction::AnalyzeComposition => {
                    // Run composition analysis inline using doc data
                    use photonic_core::node::SceneNodeKind;
                    use photonic_core::style::FillKind;
                    let mut findings: Vec<String> = Vec::new();

                    let canvas_w = doc.width;
                    let canvas_h = doc.height;
                    let mid_x = canvas_w / 2.0;
                    let mid_y = canvas_h / 2.0;
                    let (mut q_tl, mut q_tr, mut q_bl, mut q_br) = (0usize, 0usize, 0usize, 0usize);

                    struct Info {
                        bx: f64,
                        by: f64,
                        bw: f64,
                        bh: f64,
                        r: f32,
                        g: f32,
                        b: f32,
                        solid: bool,
                    }
                    let mut infos: Vec<Info> = Vec::new();

                    for node in doc.nodes_in_draw_order() {
                        if !node.visible {
                            continue;
                        }
                        let (wx, wy) = node.transform.apply(0.0, 0.0);
                        let (bx, by, bw, bh) = if let Some(lb) = node.local_bounds() {
                            let (x0, y0) = node.transform.apply(lb.x0, lb.y0);
                            let (x1, y1) = node.transform.apply(lb.x1, lb.y1);
                            (
                                x0.min(x1),
                                y0.min(y1),
                                (x1 - x0).abs().max(1.0),
                                (y1 - y0).abs().max(1.0),
                            )
                        } else {
                            (wx, wy, 1.0, 1.0)
                        };
                        let cx = bx + bw / 2.0;
                        let cy = by + bh / 2.0;
                        match (cx < mid_x, cy < mid_y) {
                            (true, true) => q_tl += 1,
                            (false, true) => q_tr += 1,
                            (true, false) => q_bl += 1,
                            (false, false) => q_br += 1,
                        }
                        let (r, g, b, solid) = match &node.kind {
                            SceneNodeKind::Path(pn) => match &pn.fill.kind {
                                FillKind::Solid(c) => (c.r, c.g, c.b, true),
                                _ => (0.5, 0.5, 0.5, false),
                            },
                            SceneNodeKind::Text(tn) => match &tn.fill.kind {
                                FillKind::Solid(c) => (c.r, c.g, c.b, true),
                                _ => (0.0, 0.0, 0.0, true),
                            },
                            SceneNodeKind::Group(_) => (0.5, 0.5, 0.5, false),
                            // raster nodes have no vector fill color
                            SceneNodeKind::Raster(_) => (0.5, 0.5, 0.5, false),
                        };
                        infos.push(Info {
                            bx,
                            by,
                            bw,
                            bh,
                            r,
                            g,
                            b,
                            solid,
                        });
                    }

                    if infos.is_empty() {
                        self.composition_findings =
                            vec!["No visible nodes to analyze.".to_string()];
                    } else {
                        let left = q_tl + q_bl;
                        let right = q_tr + q_br;
                        let top = q_tl + q_tr;
                        let bottom = q_bl + q_br;
                        let h_imb = if left + right > 0 {
                            ((left as f64 - right as f64).abs() / (left + right) as f64 * 100.0)
                                as u32
                        } else {
                            0
                        };
                        let v_imb = if top + bottom > 0 {
                            ((top as f64 - bottom as f64).abs() / (top + bottom) as f64 * 100.0)
                                as u32
                        } else {
                            0
                        };
                        if h_imb > 40 {
                            let side = if left > right { "left" } else { "right" };
                            findings.push(format!(
                                "{} Balance: {}% more objects on the {} ({} left, {} right).",
                                ph::WARNING,
                                h_imb,
                                side,
                                left,
                                right
                            ));
                        }
                        if v_imb > 40 {
                            let side = if top > bottom { "top" } else { "bottom" };
                            findings.push(format!(
                                "{} Balance: {}% more objects near the {} ({} top, {} bottom).",
                                ph::INFO,
                                v_imb,
                                side,
                                top,
                                bottom
                            ));
                        }
                        if h_imb <= 20 && v_imb <= 20 {
                            findings.push(format!(
                                "{} Balance: objects distributed evenly across quadrants.",
                                ph::CHECK
                            ));
                        }
                        let total_area: f64 = infos.iter().map(|n| n.bw * n.bh).sum();
                        let canvas_area = (canvas_w * canvas_h).max(1.0);
                        let density = (total_area / canvas_area * 100.0).min(200.0);
                        if density < 5.0 {
                            findings.push(format!(
                                "{} Density: very sparse ({:.1}% canvas coverage).",
                                ph::INFO,
                                density
                            ));
                        } else if density > 120.0 {
                            findings.push(format!(
                                "{} Density: may be overcrowded ({:.1}% combined coverage).",
                                ph::WARNING,
                                density
                            ));
                        }
                        let mut overlap_count = 0usize;
                        'ov: for i in 0..infos.len() {
                            for j in (i + 1)..infos.len() {
                                let a = &infos[i];
                                let b = &infos[j];
                                if a.bx < b.bx + b.bw
                                    && a.bx + a.bw > b.bx
                                    && a.by < b.by + b.bh
                                    && a.by + a.bh > b.by
                                {
                                    overlap_count += 1;
                                    if overlap_count >= 10 {
                                        break 'ov;
                                    }
                                }
                            }
                        }
                        if overlap_count > 0 {
                            findings.push(format!(
                                "{} Overlaps: {} overlapping object pair(s) detected.",
                                ph::INFO,
                                overlap_count
                            ));
                        }
                        let solid: Vec<_> = infos.iter().filter(|n| n.solid).collect();
                        let unique_colors: std::collections::HashSet<(u8, u8, u8)> = solid
                            .iter()
                            .map(|n| {
                                (
                                    (n.r * 255.0) as u8,
                                    (n.g * 255.0) as u8,
                                    (n.b * 255.0) as u8,
                                )
                            })
                            .collect();
                        if unique_colors.len() > 12 {
                            findings.push(format!("{} Colors: {} unique fill colors — consider reducing for visual cohesion.", ph::INFO, unique_colors.len()));
                        }
                        let off_canvas = infos
                            .iter()
                            .filter(|n| {
                                n.bx + n.bw < 0.0
                                    || n.by + n.bh < 0.0
                                    || n.bx > canvas_w
                                    || n.by > canvas_h
                            })
                            .count();
                        if off_canvas > 0 {
                            findings.push(format!("{} Off-canvas: {} object(s) outside bounds — won't appear in exports.", ph::WARNING, off_canvas));
                        }
                        if findings
                            .iter()
                            .all(|f| f.starts_with(ph::CHECK) || f.starts_with(ph::INFO))
                        {
                            findings.push(format!(
                                "{} {} node(s) analyzed. No critical issues.",
                                ph::CHECK,
                                infos.len()
                            ));
                        }
                        self.composition_findings = findings;
                    }
                }

                PanelAction::DetectRhythms => {
                    use photonic_core::node::SceneNodeKind;
                    let tolerance = 4.0_f64;
                    let min_count = 3usize;

                    struct Metrics {
                        cx: f64,
                        cy: f64,
                        w: f64,
                        rot_deg: f64,
                    }
                    let mut metrics: Vec<Metrics> = Vec::new();
                    for node in doc.nodes_in_draw_order() {
                        if !node.visible {
                            continue;
                        }
                        if matches!(node.kind, SceneNodeKind::Group(_)) {
                            continue;
                        }
                        let (bx, by, bw, bh) = if let Some(lb) = node.local_bounds() {
                            let (x0, y0) = node.transform.apply(lb.x0, lb.y0);
                            let (x1, y1) = node.transform.apply(lb.x1, lb.y1);
                            let nx = x0.min(x1);
                            let ny = y0.min(y1);
                            let nw = (x1 - x0).abs().max(0.001);
                            let nh = (y1 - y0).abs().max(0.001);
                            (nx, ny, nw, nh)
                        } else {
                            let (wx, wy) = node.transform.apply(0.0, 0.0);
                            (wx, wy, 1.0, 1.0)
                        };
                        let rot = {
                            let r = node.transform.matrix[1]
                                .atan2(node.transform.matrix[0])
                                .to_degrees()
                                % 360.0;
                            if r < 0.0 {
                                r + 360.0
                            } else {
                                r
                            }
                        };
                        metrics.push(Metrics {
                            cx: bx + bw / 2.0,
                            cy: by + bh / 2.0,
                            w: bw,
                            rot_deg: rot,
                        });
                    }

                    if metrics.len() < min_count {
                        self.rhythm_findings = vec![format!(
                            "Need ≥{} leaf nodes to detect rhythms ({} found).",
                            min_count,
                            metrics.len()
                        )];
                    } else {
                        let mut findings: Vec<String> = Vec::new();

                        // Horizontal spacing
                        let mut xs: Vec<f64> = metrics.iter().map(|m| m.cx).collect();
                        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let gaps: Vec<f64> = xs.windows(2).map(|w| w[1] - w[0]).collect();
                        if let Some(best) = gaps.iter().filter(|&&g| g >= 1.0).max_by_key(|&&g| {
                            gaps.iter().filter(|&&x| (x - g).abs() < tolerance).count()
                        }) {
                            let cnt = gaps
                                .iter()
                                .filter(|&&g| (g - best).abs() < tolerance)
                                .count();
                            if cnt + 1 >= min_count {
                                findings.push(format!(
                                    "↔ {} objects spaced ~{:.0}px horizontally",
                                    cnt + 1,
                                    best
                                ));
                            }
                        }

                        // Vertical spacing
                        let mut ys: Vec<f64> = metrics.iter().map(|m| m.cy).collect();
                        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let gaps_v: Vec<f64> = ys.windows(2).map(|w| w[1] - w[0]).collect();
                        if let Some(best) = gaps_v.iter().filter(|&&g| g >= 1.0).max_by_key(|&&g| {
                            gaps_v
                                .iter()
                                .filter(|&&x| (x - g).abs() < tolerance)
                                .count()
                        }) {
                            let cnt = gaps_v
                                .iter()
                                .filter(|&&g| (g - best).abs() < tolerance)
                                .count();
                            if cnt + 1 >= min_count {
                                findings.push(format!(
                                    "↕ {} objects spaced ~{:.0}px vertically",
                                    cnt + 1,
                                    best
                                ));
                            }
                        }

                        // Uniform width
                        let mut widths: Vec<f64> = metrics.iter().map(|m| m.w).collect();
                        widths.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        if let Some(best) = widths.iter().filter(|&&w| w >= 1.0).max_by_key(|&&w| {
                            widths
                                .iter()
                                .filter(|&&x| (x - w).abs() < tolerance)
                                .count()
                        }) {
                            let cnt = widths
                                .iter()
                                .filter(|&&w| (w - best).abs() < tolerance)
                                .count();
                            if cnt >= min_count {
                                findings
                                    .push(format!("⇔ {} objects share width ~{:.0}px", cnt, best));
                            }
                        }

                        // Rotation rhythm
                        let mut rots: Vec<f64> = metrics.iter().map(|m| m.rot_deg).collect();
                        rots.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let rot_gaps: Vec<f64> = rots.windows(2).map(|w| w[1] - w[0]).collect();
                        if let Some(best) =
                            rot_gaps.iter().filter(|&&g| g >= 1.0).max_by_key(|&&g| {
                                rot_gaps.iter().filter(|&&x| (x - g).abs() < 3.0).count()
                            })
                        {
                            let cnt = rot_gaps.iter().filter(|&&g| (g - best).abs() < 3.0).count();
                            if cnt + 1 >= min_count && *best >= 5.0 {
                                let n = (360.0 / best).round() as u32;
                                let sym = if (2..=12).contains(&n) {
                                    format!(" ({}× symmetry)", n)
                                } else {
                                    String::new()
                                };
                                findings.push(format!(
                                    "↻ {} objects rotated ~{:.0}°/step{}",
                                    cnt + 1,
                                    best,
                                    sym
                                ));
                            }
                        }

                        if findings.is_empty() {
                            findings
                                .push(format!("No rhythms detected in {} nodes.", metrics.len()));
                        }
                        self.rhythm_findings = findings;
                    }
                }

                PanelAction::PlayAction { name } => {
                    // GUI can't call async MCP handlers; refresh the actions list
                    // Actual playback is available via the MCP play_action tool
                    self.action_names = doc
                        .action_sets
                        .iter()
                        .map(|a| {
                            let cnt = serde_json::from_str::<serde_json::Value>(&a.steps_json)
                                .ok()
                                .and_then(|v| v.as_array().map(|arr| arr.len()))
                                .unwrap_or(0);
                            (a.name.clone(), cnt)
                        })
                        .collect();
                    let _ = name; // Playback requires MCP tool: play_action { "name": "..." }
                }

                PanelAction::DeleteAction { name } => {
                    doc.action_sets.retain(|a| a.name != name);
                    self.action_names = doc
                        .action_sets
                        .iter()
                        .map(|a| {
                            let cnt = serde_json::from_str::<serde_json::Value>(&a.steps_json)
                                .ok()
                                .and_then(|v| v.as_array().map(|arr| arr.len()))
                                .unwrap_or(0);
                            (a.name.clone(), cnt)
                        })
                        .collect();
                    doc_modified = true;
                }

                PanelAction::MeasureDistances { node_ids } => {
                    struct NBox {
                        name: String,
                        x0: f64,
                        y0: f64,
                        x1: f64,
                        y1: f64,
                    }
                    let mut boxes: Vec<NBox> = Vec::new();
                    for &id in &node_ids {
                        if let Some(node) = doc.nodes.get(&id) {
                            let (bx, by, bw, bh) = if let Some(lb) = node.local_bounds() {
                                let (x0, y0) = node.transform.apply(lb.x0, lb.y0);
                                let (x1, y1) = node.transform.apply(lb.x1, lb.y1);
                                let nx = x0.min(x1);
                                let ny = y0.min(y1);
                                let nw = (x1 - x0).abs();
                                let nh = (y1 - y0).abs();
                                (nx, ny, nw, nh)
                            } else {
                                let (wx, wy) = node.transform.apply(0.0, 0.0);
                                (wx, wy, 0.0, 0.0)
                            };
                            boxes.push(NBox {
                                name: if node.name.is_empty() {
                                    id.to_string()
                                } else {
                                    node.name.clone()
                                },
                                x0: bx,
                                y0: by,
                                x1: bx + bw,
                                y1: by + bh,
                            });
                        }
                    }
                    let n = boxes.len();
                    let mut results: Vec<(String, String, f64, f64, f64)> = Vec::new();
                    let pairs: Vec<(usize, usize)> = if n <= 6 {
                        let mut p = Vec::new();
                        for i in 0..n {
                            for j in (i + 1)..n {
                                p.push((i, j));
                            }
                        }
                        p
                    } else {
                        (0..n - 1).map(|i| (i, i + 1)).collect()
                    };
                    for (i, j) in pairs {
                        let a = &boxes[i];
                        let b = &boxes[j];
                        let acx = (a.x0 + a.x1) / 2.0;
                        let acy = (a.y0 + a.y1) / 2.0;
                        let bcx = (b.x0 + b.x1) / 2.0;
                        let bcy = (b.y0 + b.y1) / 2.0;
                        let center_dist = ((bcx - acx).powi(2) + (bcy - acy).powi(2)).sqrt();
                        let h_gap = if a.x1 <= b.x0 {
                            b.x0 - a.x1
                        } else if b.x1 <= a.x0 {
                            b.x1 - a.x0
                        } else {
                            -(a.x1.min(b.x1) - a.x0.max(b.x0))
                        };
                        let v_gap = if a.y1 <= b.y0 {
                            b.y0 - a.y1
                        } else if b.y1 <= a.y0 {
                            b.y1 - a.y0
                        } else {
                            -(a.y1.min(b.y1) - a.y0.max(b.y0))
                        };
                        results.push((
                            a.name.clone(),
                            b.name.clone(),
                            (h_gap * 10.0).round() / 10.0,
                            (v_gap * 10.0).round() / 10.0,
                            (center_dist * 10.0).round() / 10.0,
                        ));
                    }
                    self.distance_results = results;
                }

                PanelAction::DefineGrammarRule {
                    name,
                    rule_type,
                    params_json,
                } => {
                    use photonic_core::GrammarRule;
                    // Validate params as JSON
                    if serde_json::from_str::<serde_json::Value>(&params_json).is_ok() {
                        let rule = GrammarRule::new(&name, &rule_type, &params_json);
                        if let Some(idx) = doc.grammar_rules.iter().position(|r| r.name == name) {
                            doc.grammar_rules[idx] = rule;
                        } else {
                            doc.grammar_rules.push(rule);
                        }
                        self.grammar_rules = doc
                            .grammar_rules
                            .iter()
                            .map(|r| (r.name.clone(), r.rule_type.clone()))
                            .collect();
                        doc_modified = true;
                    }
                }

                PanelAction::DeleteGrammarRule { name } => {
                    doc.grammar_rules.retain(|r| r.name != name);
                    self.grammar_rules = doc
                        .grammar_rules
                        .iter()
                        .map(|r| (r.name.clone(), r.rule_type.clone()))
                        .collect();
                    doc_modified = true;
                }

                PanelAction::CheckGrammar => {
                    use photonic_core::node::SceneNodeKind;
                    use photonic_core::style::FillKind;
                    // Gather document metrics
                    let mut unique_colors: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    let mut min_text_size: f64 = f64::MAX;
                    let mut total_nodes = 0usize;
                    for node in doc.nodes_in_draw_order() {
                        if !node.visible {
                            continue;
                        }
                        total_nodes += 1;
                        match &node.kind {
                            SceneNodeKind::Path(pn) => {
                                if let FillKind::Solid(c) = &pn.fill.kind {
                                    unique_colors
                                        .insert(format!("{:.3},{:.3},{:.3}", c.r, c.g, c.b));
                                }
                            }
                            SceneNodeKind::Text(tn) => {
                                if let FillKind::Solid(c) = &tn.fill.kind {
                                    unique_colors
                                        .insert(format!("{:.3},{:.3},{:.3}", c.r, c.g, c.b));
                                }
                                if tn.font_size < min_text_size {
                                    min_text_size = tn.font_size;
                                }
                            }
                            SceneNodeKind::Group(_) => {}
                            // raster nodes contribute no vector fill colors or text size
                            SceneNodeKind::Raster(_) => {}
                        }
                    }
                    let layer_names: Vec<String> = doc
                        .layer_order
                        .iter()
                        .filter_map(|id| doc.layers.get(id))
                        .map(|l| l.name.clone())
                        .collect();

                    let mut results: Vec<(String, bool, String)> = Vec::new();
                    for rule in &doc.grammar_rules {
                        let params: serde_json::Value = serde_json::from_str(&rule.params_json)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        let (passed, msg) = match rule.rule_type.as_str() {
                            "palette_includes" => {
                                let hex = params["color_hex"].as_str().unwrap_or("").to_lowercase();
                                let hex_trim = hex.trim_start_matches('#');
                                let found = if hex_trim.len() == 6 {
                                    if let (Ok(r), Ok(g), Ok(b)) = (
                                        u8::from_str_radix(&hex_trim[0..2], 16),
                                        u8::from_str_radix(&hex_trim[2..4], 16),
                                        u8::from_str_radix(&hex_trim[4..6], 16),
                                    ) {
                                        let (tr, tg, tb) =
                                            (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
                                        unique_colors.iter().any(|c| {
                                            let p: Vec<f32> = c
                                                .split(',')
                                                .filter_map(|x| x.parse().ok())
                                                .collect();
                                            p.len() == 3
                                                && (p[0] - tr).abs() < 0.02
                                                && (p[1] - tg).abs() < 0.02
                                                && (p[2] - tb).abs() < 0.02
                                        })
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                };
                                if found {
                                    (true, format!("{} present", hex))
                                } else {
                                    (false, format!("{} not found in any fill", hex))
                                }
                            }
                            "max_colors" => {
                                let limit = params["count"].as_u64().unwrap_or(10) as usize;
                                if unique_colors.len() <= limit {
                                    (true, format!("{} colors (≤{})", unique_colors.len(), limit))
                                } else {
                                    (
                                        false,
                                        format!(
                                            "{} colors exceeds limit {}",
                                            unique_colors.len(),
                                            limit
                                        ),
                                    )
                                }
                            }
                            "min_text_size" => {
                                let min_px = params["px"].as_f64().unwrap_or(12.0);
                                if min_text_size == f64::MAX {
                                    (true, "no text nodes (vacuously satisfied)".to_string())
                                } else if min_text_size >= min_px {
                                    (
                                        true,
                                        format!(
                                            "smallest text {:.0}px (≥{:.0})",
                                            min_text_size, min_px
                                        ),
                                    )
                                } else {
                                    (
                                        false,
                                        format!(
                                            "text as small as {:.0}px (min {:.0})",
                                            min_text_size, min_px
                                        ),
                                    )
                                }
                            }
                            "required_layer" => {
                                let target = params["name"].as_str().unwrap_or("");
                                let prefix = params["prefix"].as_str().unwrap_or("");
                                let found = if !target.is_empty() {
                                    layer_names.iter().any(|n| n == target)
                                } else {
                                    layer_names.iter().any(|n| n.starts_with(prefix))
                                };
                                if found {
                                    (true, "layer present".to_string())
                                } else {
                                    (
                                        false,
                                        format!(
                                            "layer not found (have: {})",
                                            layer_names.join(", ")
                                        ),
                                    )
                                }
                            }
                            "max_node_count" => {
                                let limit = params["count"].as_u64().unwrap_or(500) as usize;
                                if total_nodes <= limit {
                                    (true, format!("{} nodes (≤{})", total_nodes, limit))
                                } else {
                                    (
                                        false,
                                        format!("{} nodes exceeds limit {}", total_nodes, limit),
                                    )
                                }
                            }
                            _ => (false, "unknown rule type".to_string()),
                        };
                        results.push((rule.name.clone(), passed, msg));
                    }
                    self.grammar_check_results = results;
                }

                PanelAction::BranchCreate { name } => {
                    // Name the current history state (label the HEAD commit).
                    history.branch_create(name);
                }

                PanelAction::BranchSwitch { name } => {
                    // Non-destructive jump to the named commit.
                    if history.branch_switch(&name, doc) {
                        self.selected_id = None;
                        doc.selection.clear();
                        doc_modified = true;
                    }
                }

                PanelAction::BranchDelete { name } => {
                    history.branch_delete(&name);
                }

                PanelAction::LabelHistoryNode { id, name } => {
                    // Set or clear a specific commit's name (right-click naming).
                    history.set_node_label(id, name);
                }

                PanelAction::BindTextVariable {
                    node_id,
                    variable_name,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.variable_binding = Some(variable_name);
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

                PanelAction::UnbindTextVariable { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.variable_binding = None;
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

                PanelAction::ApplyVariables => {
                    let var_map: std::collections::HashMap<String, String> = doc
                        .variables
                        .iter()
                        .map(|v| (v.name.clone(), v.value.clone()))
                        .collect();
                    let mut commands = Vec::new();
                    for node in doc.nodes.values() {
                        if let SceneNodeKind::Text(ref tn) = node.kind {
                            if let Some(ref binding) = tn.variable_binding {
                                if let Some(value) = var_map.get(binding.as_str()) {
                                    if tn.content != *value {
                                        let mut new_node = node.clone();
                                        if let SceneNodeKind::Text(ref mut new_tn) = new_node.kind {
                                            new_tn.content = value.clone();
                                        }
                                        commands.push(Command::UpdateNode {
                                            old: node.clone(),
                                            new: new_node,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    if !commands.is_empty() {
                        history.execute(Command::Batch(commands), doc);
                        doc_modified = true;
                    }
                }

                PanelAction::DeleteVariable { name } => {
                    doc.variables.retain(|v| v.name != name);
                    doc_modified = true;
                }

                PanelAction::DefineSymbol { node_id, name } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        use photonic_core::Symbol;
                        let sym = Symbol::new(name, node.id);
                        doc.symbols.push(sym);
                        doc_modified = true;
                    }
                }

                PanelAction::PlaceSymbol { symbol_name } => {
                    use photonic_core::transform::Transform;
                    if let Some(sym) = doc.symbols.iter().find(|s| s.name == symbol_name).cloned() {
                        if let Some(master) = doc.nodes.get(&sym.master_node_id).cloned() {
                            let layer_id =
                                doc.layers.values().next().map(|l| l.id).unwrap_or_default();
                            let mut instance = master.clone();
                            instance.id = uuid::Uuid::new_v4();
                            instance.name = format!("{} (instance)", sym.name);
                            instance.layer_id = layer_id;
                            instance.transform = Transform::translate(20.0, 20.0);
                            instance.symbol_ref = Some(sym.id);
                            history.execute(
                                Command::AddNode {
                                    node: instance,
                                    layer_id: Some(layer_id),
                                },
                                doc,
                            );
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::BreakLinkToSymbol { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        let mut new_node = node.clone();
                        new_node.symbol_ref = None;
                        history.execute(
                            Command::UpdateNode {
                                old: node,
                                new: new_node,
                            },
                            doc,
                        );
                        doc_modified = true;
                    }
                }

                PanelAction::DeleteSymbol { name } => {
                    doc.symbols.retain(|s| s.name != name);
                    doc_modified = true;
                }

                PanelAction::SetSymbolOverride {
                    node_id,
                    fill_hex,
                    stroke_hex,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if node.symbol_ref.is_some() {
                            let mut new_node = node.clone();
                            if let Some(hex) = fill_hex {
                                new_node.symbol_fill_override = Some(hex);
                            }
                            if let Some(hex) = stroke_hex {
                                new_node.symbol_stroke_override = Some(hex);
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

                PanelAction::ClearSymbolOverrides { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if node.symbol_ref.is_some() {
                            let mut new_node = node.clone();
                            new_node.symbol_fill_override = None;
                            new_node.symbol_stroke_override = None;
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

                PanelAction::SpraySymbolInstances {
                    symbol_name,
                    count,
                    x,
                    y,
                    spread,
                } => {
                    use photonic_core::transform::Transform;
                    let count = count.max(1).min(200);
                    let spread = if spread <= 0.0 { 100.0 } else { spread };

                    if let Some(symbol) =
                        doc.symbols.iter().find(|s| s.name == symbol_name).cloned()
                    {
                        if let Some(master) = doc.nodes.get(&symbol.master_node_id).cloned() {
                            let Some(layer_id) = doc
                                .active_layer_id
                                .or_else(|| doc.layer_order.first().copied())
                            else {
                                continue 'actions;
                            };
                            const GOLDEN_ANGLE: f64 =
                                std::f64::consts::TAU * (1.0 - 1.0 / 1.618_033_988_749_895);
                            for i in 0..count {
                                let r = spread * ((i as f64 + 0.5) / count as f64).sqrt();
                                let theta = i as f64 * GOLDEN_ANGLE;
                                let ix = x + r * theta.cos();
                                let iy = y + r * theta.sin();
                                let mut instance = master.clone();
                                instance.id = uuid::Uuid::new_v4();
                                instance.name = format!("{} (instance {})", symbol.name, i + 1);
                                instance.layer_id = layer_id;
                                instance.transform = Transform::translate(ix, iy);
                                instance.symbol_ref = Some(symbol.id);
                                history.execute(
                                    Command::AddNode {
                                        node: instance,
                                        layer_id: Some(layer_id),
                                    },
                                    doc,
                                );
                            }
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::LoadSymbolLibrary { library_name } => {
                    use photonic_core::node::{PathNode, SceneNodeKind};
                    use photonic_core::path::PathData;
                    use photonic_core::style::Stroke;
                    use photonic_core::transform::Transform;
                    use photonic_core::Symbol;

                    let entries: Vec<(&str, &str)> = match library_name.as_str() {
                        "arrows" => vec![
                            ("arrow-right",    "M10,45 L70,45 L70,30 L90,50 L70,70 L70,55 L10,55 Z"),
                            ("arrow-left",     "M90,45 L30,45 L30,30 L10,50 L30,70 L30,55 L90,55 Z"),
                            ("arrow-up",       "M45,90 L45,30 L30,30 L50,10 L70,30 L55,30 L55,90 Z"),
                            ("arrow-down",     "M45,10 L45,70 L30,70 L50,90 L70,70 L55,70 L55,10 Z"),
                            ("double-arrow-h", "M10,50 L25,35 L25,43 L75,43 L75,35 L90,50 L75,65 L75,57 L25,57 L25,65 Z"),
                            ("arrow-ne",       "M20,80 L70,30 L45,30 L45,20 L80,20 L80,55 L70,55 L70,30"),
                        ],
                        "shapes" => vec![
                            ("diamond",   "M50,5 L95,50 L50,95 L5,50 Z"),
                            ("hexagon",   "M50,5 L91,27 L91,73 L50,95 L9,73 L9,27 Z"),
                            ("pentagon",  "M50,5 L95,34 L79,88 L21,88 L5,34 Z"),
                            ("star-5pt",  "M50,5 L61,35 L95,35 L68,57 L79,91 L50,70 L21,91 L32,57 L5,35 L39,35 Z"),
                            ("cross",     "M35,5 L65,5 L65,35 L95,35 L95,65 L65,65 L65,95 L35,95 L35,65 L5,65 L5,35 L35,35 Z"),
                            ("checkmark", "M10,50 L35,75 L90,20"),
                        ],
                        "ui" => vec![
                            ("checkbox-empty",   "M10,10 L90,10 L90,90 L10,90 Z M15,15 L85,15 L85,85 L15,85 Z"),
                            ("checkbox-checked", "M10,10 L90,10 L90,90 L10,90 Z M20,50 L40,70 L80,25"),
                            ("radio-empty",      "M50,5 A45,45 0 1 1 49.9,5 Z M50,15 A35,35 0 1 1 49.9,15 Z"),
                            ("close-x",          "M15,15 L85,85 M85,15 L15,85"),
                            ("menu-lines",        "M10,25 L90,25 M10,50 L90,50 M10,75 L90,75"),
                            ("plus-icon",         "M50,10 L50,90 M10,50 L90,50"),
                        ],
                        _ => vec![],
                    };

                    if entries.is_empty() {
                        continue 'actions;
                    }

                    let layer_id = doc
                        .active_layer_id
                        .or_else(|| doc.layer_order.first().copied())
                        .unwrap_or(uuid::Uuid::nil());

                    for (i, (name, path_d)) in entries.iter().enumerate() {
                        let sym_name = format!("{}/{}", library_name, name);
                        if doc.symbols.iter().any(|s| s.name == sym_name) {
                            continue;
                        }
                        let Ok(path_data) = PathData::from_svg(path_d) else {
                            continue;
                        };
                        let mut path_node = PathNode::new(path_data);
                        path_node.stroke = Stroke::none();
                        let mut master = photonic_core::node::SceneNode::new(
                            sym_name.clone(),
                            layer_id,
                            SceneNodeKind::Path(path_node),
                        );
                        master.transform =
                            Transform::translate(-9999.0 + i as f64 * 150.0, -9999.0);
                        master.visible = false;
                        let master_id = master.id;
                        history.execute(
                            Command::AddNode {
                                node: master,
                                layer_id: Some(layer_id),
                            },
                            doc,
                        );
                        doc.symbols.push(Symbol::new(&sym_name, master_id));
                    }
                    doc_modified = true;
                }

                PanelAction::SaveWorkspace { name, search_query } => {
                    // `doc.workspaces` is persisted, so this goes through the
                    // history like any other document edit rather than mutating
                    // the vec in place (SPEC: every document mutation, without
                    // exception, is undoable).
                    let old = doc.workspaces.clone();
                    let mut new = old.clone();
                    if let Some(ws) = new.iter_mut().find(|w| w.name == name) {
                        ws.search_query = search_query;
                    } else {
                        new.push(photonic_core::Workspace { name, search_query });
                    }
                    if new != old {
                        history.execute(Command::SetWorkspaces { old, new }, doc);
                        doc_modified = true;
                    }
                    self.workspace_name_input.clear();
                }

                PanelAction::LoadWorkspace { name } => {
                    if let Some(ws) = doc.workspaces.iter().find(|w| w.name == name) {
                        self.prop_search = ws.search_query.clone();
                    }
                }

                PanelAction::DeleteWorkspace { name } => {
                    let old = doc.workspaces.clone();
                    let new: Vec<_> = old.iter().filter(|w| w.name != name).cloned().collect();
                    if new.len() != old.len() {
                        history.execute(Command::SetWorkspaces { old, new }, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::SetTextArea {
                    text_node_id,
                    area_path_id,
                } => {
                    if let Some(node) = doc.nodes.get(&text_node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.area_path_id = Some(area_path_id);
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

                PanelAction::ClearTextArea { text_node_id } => {
                    if let Some(node) = doc.nodes.get(&text_node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.area_path_id = None;
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

                PanelAction::SetParagraphOptions {
                    node_id,
                    spacing_before,
                    spacing_after,
                    indent,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.paragraph_spacing_before = spacing_before;
                                tn.paragraph_spacing_after = spacing_after;
                                tn.text_indent = indent;
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

                PanelAction::SetTabStops { node_id, stops } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.tab_stops = stops;
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

                PanelAction::ClearTabStops { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.tab_stops.clear();
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

                PanelAction::SetTextDecoration {
                    node_id,
                    decoration,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.text_decoration = decoration;
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

                PanelAction::SetOpenTypeFeatures { node_id, features } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.opentype_features = features;
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

                PanelAction::SetCharacterMetrics {
                    node_id,
                    baseline_shift,
                    script_position,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.baseline_shift = baseline_shift;
                                tn.script_position = script_position;
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

                PanelAction::LinkTextFrames { from_id, to_id } => {
                    if from_id != to_id {
                        let from_node = doc.nodes.get(&from_id).cloned();
                        let to_node = doc.nodes.get(&to_id).cloned();
                        if let (Some(fn_), Some(tn_)) = (from_node, to_node) {
                            if matches!(fn_.kind, SceneNodeKind::Text(_))
                                && matches!(tn_.kind, SceneNodeKind::Text(_))
                            {
                                let mut new_from = fn_.clone();
                                let mut new_to = tn_.clone();
                                if let SceneNodeKind::Text(ref mut t) = new_from.kind {
                                    t.next_frame = Some(to_id);
                                }
                                if let SceneNodeKind::Text(ref mut t) = new_to.kind {
                                    t.prev_frame = Some(from_id);
                                }
                                history.execute(
                                    Command::Batch(vec![
                                        Command::UpdateNode {
                                            old: fn_,
                                            new: new_from,
                                        },
                                        Command::UpdateNode {
                                            old: tn_,
                                            new: new_to,
                                        },
                                    ]),
                                    doc,
                                );
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::UnlinkTextFrames { node_id } => {
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        if let SceneNodeKind::Text(ref tn) = node.kind {
                            let prev_id = tn.prev_frame;
                            let next_id = tn.next_frame;
                            let mut cmds: Vec<Command> = Vec::new();
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut t) = new_node.kind {
                                t.prev_frame = None;
                                t.next_frame = None;
                            }
                            cmds.push(Command::UpdateNode {
                                old: node,
                                new: new_node,
                            });
                            if let Some(pid) = prev_id {
                                if let Some(prev) = doc.nodes.get(&pid).cloned() {
                                    let mut np = prev.clone();
                                    if let SceneNodeKind::Text(ref mut t) = np.kind {
                                        t.next_frame = None;
                                    }
                                    cmds.push(Command::UpdateNode { old: prev, new: np });
                                }
                            }
                            if let Some(nid) = next_id {
                                if let Some(next) = doc.nodes.get(&nid).cloned() {
                                    let mut nn = next.clone();
                                    if let SceneNodeKind::Text(ref mut t) = nn.kind {
                                        t.prev_frame = None;
                                    }
                                    cmds.push(Command::UpdateNode { old: next, new: nn });
                                }
                            }
                            history.execute(Command::Batch(cmds), doc);
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::SetTextDirection { node_id, vertical } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.vertical = vertical;
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

                PanelAction::SetFontStyle { node_id, style } => {
                    use photonic_core::node::FontStyle;
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let fs = match style.to_lowercase().as_str() {
                                "italic" => FontStyle::Italic,
                                "oblique" => FontStyle::Oblique,
                                _ => FontStyle::Normal,
                            };
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.font_style = fs;
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

                PanelAction::SetFontWeight { node_id, weight } => {
                    if let Some(node) = doc.nodes.get(&node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.font_weight = weight.clamp(100, 900);
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

                PanelAction::SetTextPath {
                    text_node_id,
                    path_node_id,
                    offset,
                } => {
                    if let Some(node) = doc.nodes.get(&text_node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.path_spine_id = Some(path_node_id);
                                tn.path_offset = offset;
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

                PanelAction::ClearTextPath { text_node_id } => {
                    if let Some(node) = doc.nodes.get(&text_node_id) {
                        if matches!(node.kind, SceneNodeKind::Text(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Text(ref mut tn) = new_node.kind {
                                tn.path_spine_id = None;
                                tn.path_offset = 0.0;
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

                PanelAction::MakeClippingMask { group_id } => {
                    if let Some(node) = doc.nodes.get(&group_id) {
                        if let SceneNodeKind::Group(ref g) = node.kind {
                            if g.children.len() >= 2 {
                                let clip_id = *g.children.last().unwrap();
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Group(ref mut gn) = new_node.kind {
                                    gn.clip_node_id = Some(clip_id);
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

                PanelAction::ReleaseClippingMask { group_id } => {
                    if let Some(node) = doc.nodes.get(&group_id) {
                        if let SceneNodeKind::Group(_) = node.kind {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Group(ref mut gn) = new_node.kind {
                                gn.clip_node_id = None;
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

                PanelAction::RoundCorners { node_ids, radius } => {
                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                let bez = pn.path_data.to_bez_path();
                                let new_bez = gui_round_corners(&bez, radius);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                                commands.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !commands.is_empty() {
                        for cmd in commands {
                            history.execute(cmd, doc);
                        }
                        doc_modified = true;
                    }
                }

                PanelAction::SetAnchorPosition {
                    node_id,
                    index,
                    x,
                    y,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            let new_bez =
                                bez_set_anchor_position(&pn.path_data.to_bez_path(), index, x, y);
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Path(ref mut np) = new_node.kind {
                                np.path_data = PathData::from_bez_path(&new_bez);
                            }
                            history.execute(
                                Command::UpdateNode {
                                    old: node,
                                    new: new_node,
                                },
                                doc,
                            );
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::RoundSelectedCorners {
                    node_id,
                    indices,
                    radius,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            let bez = pn.path_data.to_bez_path();
                            // Only fillet true straight corners — rounding a
                            // curve-adjacent anchor would flatten the curve.
                            let straight = straight_corners(&bez);
                            let sel: std::collections::HashSet<usize> = indices
                                .iter()
                                .copied()
                                .filter(|i| straight.contains_key(i))
                                .collect();
                            let new_bez = round_selected_corners(&bez, &sel, radius);
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Path(ref mut np) = new_node.kind {
                                np.path_data = PathData::from_bez_path(&new_bez);
                            }
                            history.execute(
                                Command::UpdateNode {
                                    old: node,
                                    new: new_node,
                                },
                                doc,
                            );
                            // Rounding restructures element indices; drop the stale selection.
                            self.point_selected.clear();
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::ConvertAnchorType {
                    node_id,
                    indices,
                    smooth,
                } => {
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            let sel: std::collections::HashSet<usize> =
                                indices.iter().copied().collect();
                            let old_bez = pn.path_data.to_bez_path();
                            let new_bez = bez_convert_anchors(&old_bez, &sel, smooth);
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Path(ref mut np) = new_node.kind {
                                np.path_data = PathData::from_bez_path(&new_bez);
                            }
                            history.execute(
                                Command::UpdateNode {
                                    old: node,
                                    new: new_node,
                                },
                                doc,
                            );
                            // Converting anchors reunifies subpaths and can
                            // materialize seams, restructuring element indices; drop
                            // the stale selection. The total element count is not a
                            // sound proxy for "indices unchanged" — in compound paths
                            // a reunify shrink and a seam grow can cancel while later
                            // subpath anchors still shift — so clear unconditionally,
                            // matching the sibling RoundSelectedCorners / DeleteAnchors
                            // handlers.
                            self.point_selected.clear();
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::DeleteAnchors { node_id, indices } => {
                    if let Some(node) = doc.nodes.get(&node_id).cloned() {
                        if let SceneNodeKind::Path(pn) = &node.kind {
                            let new_bez =
                                bez_remove_elements(&pn.path_data.to_bez_path(), &indices);
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Path(ref mut np) = new_node.kind {
                                np.path_data = PathData::from_bez_path(&new_bez);
                            }
                            history.execute(
                                Command::UpdateNode {
                                    old: node,
                                    new: new_node,
                                },
                                doc,
                            );
                            self.point_selected.clear();
                            doc_modified = true;
                        }
                    }
                }

                PanelAction::WarpEnvelope {
                    node_ids,
                    warp_type,
                    bend,
                } => {
                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                let bez = pn.path_data.to_bez_path();
                                let new_bez = gui_warp_envelope(&bez, &warp_type, bend);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                                commands.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !commands.is_empty() {
                        for cmd in commands {
                            history.execute(cmd, doc);
                        }
                        doc_modified = true;
                    }
                }

                PanelAction::CrystallizePath {
                    node_ids,
                    size,
                    count,
                } => {
                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                let bez = pn.path_data.to_bez_path();
                                let new_bez = gui_crystallize(&bez, size, count);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                                commands.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !commands.is_empty() {
                        for cmd in commands {
                            history.execute(cmd, doc);
                        }
                        doc_modified = true;
                    }
                }

                PanelAction::ScallopPath {
                    node_ids,
                    depth,
                    count,
                } => {
                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                let bez = pn.path_data.to_bez_path();
                                let new_bez = gui_scallop(&bez, depth, count);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                                commands.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !commands.is_empty() {
                        for cmd in commands {
                            history.execute(cmd, doc);
                        }
                        doc_modified = true;
                    }
                }

                PanelAction::BlendObjects {
                    node_id_a,
                    node_id_b,
                    steps,
                } => {
                    gui_blend_objects(node_id_a, node_id_b, steps, doc, history, &mut doc_modified);
                }

                PanelAction::BlendObjectsSmoothColor {
                    node_id_a,
                    node_id_b,
                } => {
                    gui_blend_objects_smooth_color(
                        node_id_a,
                        node_id_b,
                        doc,
                        history,
                        &mut doc_modified,
                    );
                }

                PanelAction::BlendObjectsSpacing {
                    node_id_a,
                    node_id_b,
                    spacing,
                } => {
                    gui_blend_objects_spacing(
                        node_id_a,
                        node_id_b,
                        spacing,
                        doc,
                        history,
                        &mut doc_modified,
                    );
                }

                PanelAction::TwirlPath {
                    node_ids,
                    angle_deg,
                } => {
                    let angle_rad = angle_deg.to_radians();
                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                let bez = pn.path_data.to_bez_path();
                                let center = gui_path_centroid(&bez);
                                let new_bez = gui_twirl(&bez, angle_rad, center);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                                commands.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !commands.is_empty() {
                        for cmd in commands {
                            history.execute(cmd, doc);
                        }
                        doc_modified = true;
                    }
                }

                PanelAction::RoughenPath {
                    node_ids,
                    size,
                    detail,
                    seed,
                } => {
                    let mut commands = Vec::new();
                    let mut idx = 0u64;
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid) {
                            if let SceneNodeKind::Path(pn) = &node.kind {
                                let mut bez = pn.path_data.to_bez_path();
                                for _ in 0..detail {
                                    bez = gui_subdivide_bez(&bez);
                                }
                                let new_bez = gui_roughen(&bez, size, seed + idx);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                                commands.push(Command::UpdateNode {
                                    old: node.clone(),
                                    new: new_node,
                                });
                                idx += 1;
                            }
                        }
                    }
                    if !commands.is_empty() {
                        for cmd in commands {
                            history.execute(cmd, doc);
                        }
                        doc_modified = true;
                    }
                }

                PanelAction::SelectByKind { kind, additive } => {
                    if !additive {
                        doc.selection.clear();
                        self.selected_id = None;
                    }
                    let ids_to_select: Vec<NodeId> = doc
                        .nodes
                        .iter()
                        .filter_map(|(nid, node)| {
                            let matches = match kind.as_str() {
                                "path" => matches!(node.kind, SceneNodeKind::Path(_)),
                                "text" => matches!(node.kind, SceneNodeKind::Text(_)),
                                "group" => matches!(node.kind, SceneNodeKind::Group(_)),
                                "same_layer" => doc
                                    .active_layer_id
                                    .map(|lid| node.layer_id == lid)
                                    .unwrap_or(false),
                                _ => false,
                            };
                            if matches {
                                Some(*nid)
                            } else {
                                None
                            }
                        })
                        .collect();
                    for nid in ids_to_select {
                        doc.selection.add(nid);
                        if self.selected_id.is_none() {
                            self.selected_id = Some(nid);
                        }
                    }
                    doc_modified = true;
                }

                PanelAction::CreateRadarChart => {
                    let cx = doc.width / 2.0;
                    let cy = doc.height / 2.0;
                    gui_create_radar_chart_demo(cx, cy, doc, history, &mut doc_modified);
                }

                PanelAction::CreateStackedBarChart => {
                    let x = doc.width / 2.0 - 150.0;
                    let y = doc.height / 2.0 + 100.0;
                    gui_create_stacked_bar_chart_demo(x, y, doc, history, &mut doc_modified);
                }

                PanelAction::CreateParametricShape { shape_type } => {
                    let cx = doc.width / 2.0;
                    let cy = doc.height / 2.0;
                    gui_create_parametric_shape_demo(
                        &shape_type,
                        cx,
                        cy,
                        doc,
                        history,
                        &mut doc_modified,
                    );
                }

                PanelAction::OffsetPath { node_ids, distance } => {
                    use kurbo::Join;
                    use photonic_core::ops::offset::offset_path as do_offset;

                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid).cloned() {
                            if let SceneNodeKind::Path(ref pn) = node.kind {
                                if let Ok(offset_data) =
                                    do_offset(&pn.path_data, distance, Join::Miter)
                                {
                                    let layer_id = node.layer_id;
                                    let mut new_pn = pn.clone();
                                    new_pn.path_data = offset_data;
                                    let label = if distance >= 0.0 {
                                        format!("{} +{:.0}px", node.name, distance)
                                    } else {
                                        format!("{} {:.0}px", node.name, distance)
                                    };
                                    let new_node = SceneNode::new(
                                        &label,
                                        layer_id,
                                        SceneNodeKind::Path(new_pn),
                                    );
                                    commands.push(Command::AddNode {
                                        node: new_node,
                                        layer_id: Some(layer_id),
                                    });
                                }
                            }
                        }
                    }
                    if !commands.is_empty() {
                        let batch = if commands.len() == 1 {
                            commands.remove(0)
                        } else {
                            Command::Batch(commands)
                        };
                        history.execute(batch, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::CreateTruchetTiling { style } => {
                    let margin = 20.0_f64;
                    let size = (doc.width.min(doc.height) - 2.0 * margin).max(40.0);
                    let x = (doc.width - size) / 2.0;
                    let y = (doc.height - size) / 2.0;
                    gui_create_truchet_tiling_demo(
                        &style,
                        x,
                        y,
                        size,
                        doc,
                        history,
                        &mut doc_modified,
                    );
                }

                PanelAction::DistributeNoOverlap { node_ids } => {
                    let padding = 4.0_f64;
                    let max_iter = 100_usize;
                    let n = node_ids.len().min(100);
                    if n < 2 {
                        // nothing to do
                    } else {
                        let mut offsets: Vec<(f64, f64)> = vec![(0.0, 0.0); n];

                        let world_bboxes: Vec<(f64, f64, f64, f64)> = node_ids[..n]
                            .iter()
                            .map(|id| -> (f64, f64, f64, f64) {
                                if let Some(node) = doc.nodes.get(id) {
                                    let tx = node.transform.matrix[4];
                                    let ty = node.transform.matrix[5];
                                    if let SceneNodeKind::Path(pn) = &node.kind {
                                        let bb = pn
                                            .path_data
                                            .bounding_box()
                                            .unwrap_or(kurbo::Rect::ZERO);
                                        return (bb.x0 + tx, bb.y0 + ty, bb.x1 + tx, bb.y1 + ty);
                                    }
                                    return (tx, ty, tx, ty);
                                }
                                (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64)
                            })
                            .collect();

                        for _ in 0..max_iter {
                            let mut any = false;
                            for i in 0..n {
                                for j in (i + 1)..n {
                                    let half_pad = padding / 2.0;
                                    let (ax0, ay0, ax1, ay1) = (
                                        world_bboxes[i].0 + offsets[i].0 - half_pad,
                                        world_bboxes[i].1 + offsets[i].1 - half_pad,
                                        world_bboxes[i].2 + offsets[i].0 + half_pad,
                                        world_bboxes[i].3 + offsets[i].1 + half_pad,
                                    );
                                    let (bx0, by0, bx1, by1) = (
                                        world_bboxes[j].0 + offsets[j].0 - half_pad,
                                        world_bboxes[j].1 + offsets[j].1 - half_pad,
                                        world_bboxes[j].2 + offsets[j].0 + half_pad,
                                        world_bboxes[j].3 + offsets[j].1 + half_pad,
                                    );
                                    let ox: f64 = (ax1.min(bx1) - ax0.max(bx0)).max(0.0);
                                    let oy: f64 = (ay1.min(by1) - ay0.max(by0)).max(0.0);
                                    if ox > 0.0 && oy > 0.0 {
                                        any = true;
                                        let (px, py) = if ox < oy {
                                            let dir = if (ax0 + ax1) / 2.0 <= (bx0 + bx1) / 2.0 {
                                                -1.0
                                            } else {
                                                1.0
                                            };
                                            (dir * ox / 2.0, 0.0)
                                        } else {
                                            let dir = if (ay0 + ay1) / 2.0 <= (by0 + by1) / 2.0 {
                                                -1.0
                                            } else {
                                                1.0
                                            };
                                            (0.0, dir * oy / 2.0)
                                        };
                                        offsets[i].0 += px;
                                        offsets[i].1 += py;
                                        offsets[j].0 -= px;
                                        offsets[j].1 -= py;
                                    }
                                }
                            }
                            if !any {
                                break;
                            }
                        }

                        let mut commands = Vec::new();
                        for (i, nid) in node_ids[..n].iter().enumerate() {
                            let (dx, dy): (f64, f64) = offsets[i];
                            if dx.abs() > 0.01 || dy.abs() > 0.01 {
                                if let Some(node) = doc.nodes.get(nid).cloned() {
                                    let mut new_node = node.clone();
                                    new_node.transform.matrix[4] += dx;
                                    new_node.transform.matrix[5] += dy;
                                    commands.push(Command::UpdateNode {
                                        old: node,
                                        new: new_node,
                                    });
                                }
                            }
                        }
                        if !commands.is_empty() {
                            let batch = if commands.len() == 1 {
                                commands.remove(0)
                            } else {
                                Command::Batch(commands)
                            };
                            history.execute(batch, doc);
                            doc_modified = true;
                        }
                    } // end else n >= 2
                }

                PanelAction::NoiseDeform {
                    node_ids,
                    amplitude,
                    style,
                } => {
                    let frequency = 0.05_f64;
                    let seed = 0.0_f64;
                    let axis: &str = &style;
                    let deform_x = axis == "both" || axis == "x";
                    let deform_y = axis == "both" || axis == "y";

                    let displace = |pt: kurbo::Point| -> kurbo::Point {
                        let dx = if deform_x {
                            amplitude * (pt.y * frequency + seed).sin()
                                + (amplitude * 0.5) * (pt.y * frequency * 2.1 + seed * 1.3).sin()
                        } else {
                            0.0
                        };
                        let dy = if deform_y {
                            amplitude
                                * (pt.x * frequency + seed + std::f64::consts::FRAC_PI_2).sin()
                                + (amplitude * 0.5) * (pt.x * frequency * 2.1 + seed * 1.7).sin()
                        } else {
                            0.0
                        };
                        kurbo::Point::new(pt.x + dx, pt.y + dy)
                    };

                    let mut commands = Vec::new();
                    for nid in &node_ids {
                        if let Some(node) = doc.nodes.get(nid).cloned() {
                            if let SceneNodeKind::Path(ref pn) = node.kind {
                                let bez = pn.path_data.to_bez_path();
                                let new_els: Vec<kurbo::PathEl> = bez
                                    .iter()
                                    .map(|el| match el {
                                        kurbo::PathEl::MoveTo(p) => {
                                            kurbo::PathEl::MoveTo(displace(p))
                                        }
                                        kurbo::PathEl::LineTo(p) => {
                                            kurbo::PathEl::LineTo(displace(p))
                                        }
                                        kurbo::PathEl::QuadTo(p1, p2) => {
                                            kurbo::PathEl::QuadTo(displace(p1), displace(p2))
                                        }
                                        kurbo::PathEl::CurveTo(p1, p2, p3) => {
                                            kurbo::PathEl::CurveTo(
                                                displace(p1),
                                                displace(p2),
                                                displace(p3),
                                            )
                                        }
                                        kurbo::PathEl::ClosePath => kurbo::PathEl::ClosePath,
                                    })
                                    .collect();
                                let new_bez = kurbo::BezPath::from_vec(new_els);
                                let mut new_node = node.clone();
                                if let SceneNodeKind::Path(ref mut new_pn) = new_node.kind {
                                    new_pn.path_data = PathData::from_bez_path(&new_bez);
                                }
                                commands.push(Command::UpdateNode {
                                    old: node,
                                    new: new_node,
                                });
                            }
                        }
                    }
                    if !commands.is_empty() {
                        let batch = if commands.len() == 1 {
                            commands.remove(0)
                        } else {
                            Command::Batch(commands)
                        };
                        history.execute(batch, doc);
                        doc_modified = true;
                    }
                }

                PanelAction::SetBlendSpine { group_id, path_id } => {
                    if let Some(node) = doc.nodes.get(&group_id) {
                        if matches!(node.kind, SceneNodeKind::Group(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Group(ref mut gn) = new_node.kind {
                                gn.blend_spine_id = Some(path_id);
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

                PanelAction::ClearBlendSpine { group_id } => {
                    if let Some(node) = doc.nodes.get(&group_id) {
                        if matches!(node.kind, SceneNodeKind::Group(_)) {
                            let mut new_node = node.clone();
                            if let SceneNodeKind::Group(ref mut gn) = new_node.kind {
                                gn.blend_spine_id = None;
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

                PanelAction::ExpandBlend { group_id } => {
                    if let Some(node) = doc.nodes.get(&group_id) {
                        if let SceneNodeKind::Group(ref gn) = node.kind {
                            let children = gn.children.clone();
                            let child_count = children.len();
                            if let Some((layer_id, group_index)) =
                                doc.node_layer_and_index(&group_id)
                            {
                                let cmd = Command::UngroupNodes {
                                    group: node.clone(),
                                    layer_id,
                                    group_index,
                                    children,
                                };
                                history.execute(cmd, doc);
                                doc_modified = true;
                                let _ = child_count; // suppress unused warning
                            }
                        }
                    }
                }

                PanelAction::FitToMargins => {
                    let safe_x = doc.margin_left;
                    let safe_y = doc.margin_top;
                    let safe_w = doc.width - doc.margin_left - doc.margin_right;
                    let safe_h = doc.height - doc.margin_top - doc.margin_bottom;

                    if safe_w > 0.0 && safe_h > 0.0 {
                        // Collect target node IDs (selected or all)
                        let target_ids: Vec<_> = if doc.selection.count() > 0 {
                            doc.selection.node_ids.iter().copied().collect()
                        } else {
                            doc.nodes.keys().copied().collect()
                        };

                        // Compute union bbox
                        let mut ux0 = f64::MAX;
                        let mut uy0 = f64::MAX;
                        let mut ux1 = f64::MIN;
                        let mut uy1 = f64::MIN;
                        let mut valid: Vec<photonic_core::node::NodeId> = Vec::new();
                        for nid in &target_ids {
                            if let Some(node) = doc.nodes.get(nid) {
                                if let Some(lb) = node.local_bounds() {
                                    let (x0, y0) = node.transform.apply(lb.x0, lb.y0);
                                    let (x1, y1) = node.transform.apply(lb.x1, lb.y1);
                                    ux0 = ux0.min(x0.min(x1));
                                    uy0 = uy0.min(y0.min(y1));
                                    ux1 = ux1.max(x0.max(x1));
                                    uy1 = uy1.max(y0.max(y1));
                                    valid.push(*nid);
                                }
                            }
                        }

                        if !valid.is_empty() && ux0 < ux1 && uy0 < uy1 {
                            let cw = ux1 - ux0;
                            let ch = uy1 - uy0;
                            let scale = (safe_w / cw).min(safe_h / ch);
                            let cx = (ux0 + ux1) / 2.0;
                            let cy = (uy0 + uy1) / 2.0;
                            let tcx = safe_x + safe_w / 2.0;
                            let tcy = safe_y + safe_h / 2.0;
                            let mut cmds: Vec<Command> = Vec::new();
                            for nid in &valid {
                                if let Some(node) = doc.nodes.get(nid) {
                                    let tx = node.transform.matrix[4];
                                    let ty = node.transform.matrix[5];
                                    let mut nn = node.clone();
                                    nn.transform.matrix[4] = tcx + (tx - cx) * scale;
                                    nn.transform.matrix[5] = tcy + (ty - cy) * scale;
                                    nn.transform.matrix[0] *= scale;
                                    nn.transform.matrix[3] *= scale;
                                    cmds.push(Command::UpdateNode {
                                        old: node.clone(),
                                        new: nn,
                                    });
                                }
                            }
                            if !cmds.is_empty() {
                                history.execute(Command::Batch(cmds), doc);
                                doc_modified = true;
                            }
                        }
                    }
                }

                PanelAction::AddDimension {
                    from_id,
                    to_id,
                    axis,
                } => {
                    use photonic_core::DimensionAnnotation;
                    let from_center = doc.nodes.get(&from_id).map(|n| {
                        if let Some(lb) = n.local_bounds() {
                            let (x0, y0) = n.transform.apply(lb.x0, lb.y0);
                            let (x1, y1) = n.transform.apply(lb.x1, lb.y1);
                            ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
                        } else {
                            n.transform.apply(0.0, 0.0)
                        }
                    });
                    let to_center = doc.nodes.get(&to_id).map(|n| {
                        if let Some(lb) = n.local_bounds() {
                            let (x0, y0) = n.transform.apply(lb.x0, lb.y0);
                            let (x1, y1) = n.transform.apply(lb.x1, lb.y1);
                            ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
                        } else {
                            n.transform.apply(0.0, 0.0)
                        }
                    });
                    if let (Some((fx, fy)), Some((tx, ty))) = (from_center, to_center) {
                        let dim =
                            DimensionAnnotation::new(from_id, to_id, axis, 20.0, fx, fy, tx, ty);
                        doc.dimensions.push(dim);
                        doc_modified = true;
                    }
                }

                PanelAction::RemoveDimension { id } => {
                    doc.dimensions.retain(|d| d.id != id);
                    doc_modified = true;
                }

                PanelAction::JumpToHistory { index } => {
                    let current = history.undo_depth();
                    let max_index = current + history.redo_depth();
                    let target = index.min(max_index);
                    if target < current {
                        for _ in 0..(current - target) {
                            if !history.undo(doc) {
                                break;
                            }
                        }
                        self.selected_id = doc.selection.ids().next().copied();
                        self.invalidate_point_edit(doc);
                        doc_modified = true;
                    } else if target > current {
                        for _ in 0..(target - current) {
                            if !history.redo(doc) {
                                break;
                            }
                        }
                        self.selected_id = doc.selection.ids().next().copied();
                        self.invalidate_point_edit(doc);
                        doc_modified = true;
                    }
                }

                PanelAction::JumpToHistoryNode { id } => {
                    // Branch-aware jump: navigate the edit tree to the clicked
                    // commit (may cross branches via the lowest common ancestor).
                    if history.current_node() != id && history.jump_to_node(id, doc) {
                        self.selected_id = doc.selection.ids().next().copied();
                        self.invalidate_point_edit(doc);
                        doc_modified = true;
                    }
                }

                PanelAction::ReverseBlendSpine { group_id } => {
                    let spine_id = doc.nodes.get(&group_id).and_then(|n| {
                        if let SceneNodeKind::Group(ref gn) = n.kind {
                            gn.blend_spine_id
                        } else {
                            None
                        }
                    });
                    if let Some(sid) = spine_id {
                        if let Some(spine) = doc.nodes.get(&sid) {
                            if matches!(spine.kind, SceneNodeKind::Path(_)) {
                                let mut new_spine = spine.clone();
                                if let SceneNodeKind::Path(ref mut pn) = new_spine.kind {
                                    pn.path_data = pn.path_data.reverse();
                                }
                                history.execute(
                                    Command::UpdateNode {
                                        old: spine.clone(),
                                        new: new_spine,
                                    },
                                    doc,
                                );
                                doc_modified = true;
                            }
                        }
                    }
                }
            }
        }
        doc_modified
    }

    /// OS media picker → media pool L0 stubs → optional auto-place on timeline
    /// (proposal 213). Shared by Media panel and the empty-timeline Import CTA.
    /// Returns true when at least one asset was added.
    pub(crate) fn import_media_files(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        bin: Option<photonic_core::timeline::BinId>,
    ) -> bool {
        // Off the render thread — see `run_file_dialog_multi`. Called inline
        // here, the portal never replies and the app hangs hard enough to be
        // force-quit mid-import.
        let dialog = rfd::FileDialog::new().set_title("Import media").add_filter(
            "Media",
            &[
                "mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "mxf", "mp3", "wav", "aac",
                "flac", "ogg", "m4a", "opus", "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff",
                "tif", "exr", "svg", "photon", "cube",
            ],
        );
        let files = super::run_file_dialog_multi(move || dialog.pick_files());
        let Some(paths) = files else {
            return false;
        };
        let project_path = self.current_file.clone();
        let stubs = self.media_pool_ui.spawn_import(paths, bin, project_path);
        if stubs.is_empty() {
            return false;
        }
        use photonic_core::timeline::ops;
        timeline::ops_bridge::ensure_project_and_sequence(
            doc,
            history,
            photonic_core::timeline::FrameRate::FPS_30,
        );
        let auto_place = self.prefs.auto_place_import_on_timeline;
        let at = self.playhead;
        let mut placed_any = false;
        for asset in stubs {
            let id = asset.id;
            history.execute_discrete(Command::Timeline(ops::add_asset(asset)), doc);
            if auto_place {
                placed_any |= timeline::ops_bridge::insert_asset_at_first_fit(doc, history, id, at);
            }
        }
        if placed_any && !self.prefs.video_coach_dismissed && self.prefs.video_coach_step == 0 {
            // Advance coach: Import done → Split.
            self.prefs.video_coach_step = 1;
            self.prefs.save();
        }
        true
    }
}
