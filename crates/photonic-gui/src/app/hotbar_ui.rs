use super::*;

impl PhotonicApp {
    // ── Adaptive hotbar (#154 Phase 4) ───────────────────────────────────────

    /// Classify the current selection into a hotbar context bucket. Returns the
    /// bucket, whether a lone selection is a group (enables Ungroup), whether a
    /// lone selection is a path the colour ops can actually recolour (enables
    /// Invert / Grayscale), and the single selected node id (for single-target
    /// actions).
    fn hotbar_bucket(&self, doc: &Document) -> (HotbarBucket, bool, bool, Option<NodeId>) {
        match doc.selection.count() {
            0 => (HotbarBucket::Empty, false, false, None),
            1 => {
                let id = doc.selection.ids().next().copied();
                let kind = id.and_then(|i| doc.get_node(&i)).map(|n| &n.kind);
                let (bucket, is_group) = match kind {
                    Some(SceneNodeKind::Text(_)) => (HotbarBucket::Text, false),
                    Some(SceneNodeKind::Group(_)) => (HotbarBucket::Shape, true),
                    _ => (HotbarBucket::Shape, false),
                };
                // Invert/Grayscale only mutate a Path's fill (non-`None`) and an
                // enabled stroke; gate the buttons on something they can change.
                let is_fillable_path = matches!(kind, Some(SceneNodeKind::Path(p))
                    if !matches!(p.fill.kind, photonic_core::style::FillKind::None)
                        || p.stroke.enabled);
                (bucket, is_group, is_fillable_path, id)
            }
            _ => (HotbarBucket::Multi, false, false, None),
        }
    }

    /// Rebuild the cached hotbar ordering only when its signature changed —
    /// bucket, single-group flag, or mode. This is what keeps Adaptive ordering
    /// calm: scores bumped by clicks do not re-rank until the bucket changes.
    fn refresh_hotbar_cache(
        &mut self,
        bucket: HotbarBucket,
        single_is_group: bool,
        single_is_fillable_path: bool,
    ) {
        let mode = self.prefs.hotbar_mode;
        let stale = match &self.hotbar_cache {
            Some(c) => {
                c.bucket != bucket
                    || c.single_is_group != single_is_group
                    || c.single_is_fillable_path != single_is_fillable_path
                    || c.mode != mode
            }
            None => true,
        };
        if stale {
            let items = hotbar::ordered_items(
                bucket,
                single_is_group,
                single_is_fillable_path,
                mode,
                |id| self.prefs.hotbar_score(bucket, id),
            );
            self.hotbar_cache = Some(HotbarCacheState {
                bucket,
                single_is_group,
                single_is_fillable_path,
                mode,
                items,
            });
        }
    }

    /// Draw the vector hotbar. Rather than a full-width docked bar, it floats
    /// as a centred, content-width rounded pill pinned just below the top
    /// toolbar — detached from the panel stack so it hugs its contents and
    /// overlays the vector canvas. Video mode has its own monitor controls.
    pub(crate) fn draw_hotbar(&mut self, ctx: &egui::Context, doc: &mut Document) {
        if self.mode != AppMode::Vector {
            return;
        }
        let (bucket, single_is_group, single_is_fillable_path, single_id) = self.hotbar_bucket(doc);
        self.refresh_hotbar_cache(bucket, single_is_group, single_is_fillable_path);
        let items = self
            .hotbar_cache
            .as_ref()
            .map(|c| c.items.clone())
            .unwrap_or_default();
        if items.is_empty() {
            return;
        }
        let active_tool = self.active_tool;

        // `available_rect().top()` is the y directly under the top toolbar (this
        // runs after that panel is added, before the side panels), so anchoring
        // there pins the pill to the top of the canvas without covering the
        // toolbar. CENTER_TOP centres it horizontally in the window.
        let top_y = ctx.available_rect().top() + 6.0;
        // Floating pill: fill + border + rounded corners, sized to its contents.
        let frame = egui::Frame::popup(&ctx.style())
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(8.0, 3.0));

        let mut invoked: Option<HotbarItem> = None;
        egui::Area::new(egui::Id::new("hotbar"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top_y))
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    invoked = hotbar::render(ui, &items, active_tool);
                });
            });

        if let Some(item) = invoked {
            self.invoke_hotbar_item(item, bucket, single_id, doc);
        }
    }

    /// Run a hotbar item: bump its per-bucket usage (persisted), then either
    /// apply the tool (reusing the existing tool-apply path) or dispatch the
    /// existing `PanelAction`(s) for the verb against the live selection.
    fn invoke_hotbar_item(
        &mut self,
        item: HotbarItem,
        bucket: HotbarBucket,
        single_id: Option<NodeId>,
        doc: &Document,
    ) {
        // Usage tracking — bump + persist (drives Adaptive ranking next rebuild).
        self.prefs.bump_hotbar_usage(bucket, item.id);
        self.prefs.save();

        match item.effect {
            HotbarEffect::Tool(tool) => {
                // Same tool-apply logic the Tools drawer/rail uses.
                self.pen_points.clear();
                self.pencil_points.clear();
                self.lasso_points.clear();
                self.isolated_group = None;
                self.clear_point_edit();
                self.active_tool = tool;
            }
            HotbarEffect::Action(action) => {
                for pa in Self::hotbar_panel_actions(action, bucket, single_id, doc) {
                    self.pending_panel_actions.push(pa);
                }
            }
        }
    }

    /// Map a hotbar verb to the existing [`PanelAction`](s) for the live
    /// selection. Single-target verbs use `single_id`; multi-selection verbs
    /// either have a dedicated "selected" action or fan out over the selection.
    fn hotbar_panel_actions(
        action: HotbarAction,
        bucket: HotbarBucket,
        single_id: Option<NodeId>,
        doc: &Document,
    ) -> Vec<PanelAction> {
        let sel: Vec<NodeId> = doc.selection.ids().copied().collect();
        let is_multi = bucket == HotbarBucket::Multi;
        match action {
            HotbarAction::Duplicate => {
                if is_multi {
                    sel.iter()
                        .map(|id| PanelAction::DuplicateNode { node_id: *id })
                        .collect()
                } else {
                    single_id
                        .map(|id| vec![PanelAction::DuplicateNode { node_id: id }])
                        .unwrap_or_default()
                }
            }
            HotbarAction::Delete => {
                if is_multi {
                    vec![PanelAction::DeleteSelected]
                } else {
                    single_id
                        .map(|id| vec![PanelAction::DeleteNode { node_id: id }])
                        .unwrap_or_default()
                }
            }
            HotbarAction::Group => vec![PanelAction::GroupSelected],
            HotbarAction::Ungroup => single_id
                .map(|id| vec![PanelAction::UngroupNode { node_id: id }])
                .unwrap_or_default(),
            HotbarAction::UngroupAll => single_id
                .map(|id| vec![PanelAction::UngroupAllNode { node_id: id }])
                .unwrap_or_default(),
            HotbarAction::BringToFront => {
                if is_multi {
                    sel.iter()
                        .map(|id| PanelAction::ReorderNode {
                            node_id: *id,
                            op: ZOrderOp::BringToFront,
                        })
                        .collect()
                } else {
                    single_id
                        .map(|id| {
                            vec![PanelAction::ReorderNode {
                                node_id: id,
                                op: ZOrderOp::BringToFront,
                            }]
                        })
                        .unwrap_or_default()
                }
            }
            HotbarAction::SendToBack => {
                if is_multi {
                    sel.iter()
                        .map(|id| PanelAction::ReorderNode {
                            node_id: *id,
                            op: ZOrderOp::SendToBack,
                        })
                        .collect()
                } else {
                    single_id
                        .map(|id| {
                            vec![PanelAction::ReorderNode {
                                node_id: id,
                                op: ZOrderOp::SendToBack,
                            }]
                        })
                        .unwrap_or_default()
                }
            }
            HotbarAction::BoolUnion => {
                vec![PanelAction::BooleanOp(
                    photonic_core::ops::boolean::BooleanOp::Union,
                )]
            }
            HotbarAction::BoolSubtract => {
                vec![PanelAction::BooleanOp(
                    photonic_core::ops::boolean::BooleanOp::Subtract,
                )]
            }
            HotbarAction::AlignLeft => vec![PanelAction::AlignNodes {
                operation: "left".into(),
                key_object_id: None,
            }],
            HotbarAction::AlignCenterH => vec![PanelAction::AlignNodes {
                operation: "center_horizontal".into(),
                key_object_id: None,
            }],
            // Empty vec = "use current selection" (resolved in the action handler).
            HotbarAction::CopyAsSvg => vec![PanelAction::CopyAsSvg { node_ids: vec![] }],
            HotbarAction::Invert => vec![PanelAction::InvertColors { node_ids: vec![] }],
            HotbarAction::Grayscale => {
                vec![PanelAction::ConvertToGrayscale { node_ids: vec![] }]
            }
        }
    }
}
