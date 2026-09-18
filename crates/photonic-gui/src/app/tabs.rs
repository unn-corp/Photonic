//! Multi-document tab management.
//!
//! The active document's live engine state (`Document` / `CommandHistory` /
//! `CanvasView`) lives in the shared `Arc<Mutex>`s and reaches [`PhotonicApp`] as
//! the `&mut` params of [`PhotonicApp::draw`]. Inactive documents are parked in
//! [`DocTab`]. Switching is a `std::mem::swap` between those `&mut` params and a
//! parked tab, so the renderer / MCP server / Lua REPL keep reading the same
//! shared Arc and automatically follow the active document — no host rewiring.

use super::*;
use std::path::PathBuf;

impl PhotonicApp {
    /// Tab-bar label for a document: its file stem if saved, else the doc name.
    pub(crate) fn tab_title(doc: &Document, file: &Option<PathBuf>) -> String {
        if let Some(p) = file {
            if let Some(stem) = p.file_stem() {
                return stem.to_string_lossy().into_owned();
            }
        }
        let name = doc.name.trim();
        if name.is_empty() {
            "Untitled".to_string()
        } else {
            name.to_string()
        }
    }

    /// Ensure at least one tab exists, representing the current live document.
    /// Called at the top of `draw` once the editor is showing (covers CLI-opened
    /// files and the very first frame). The active tab's engine fields are scratch
    /// placeholders — its real state is the `&mut` params + `self.current_file` /
    /// `self.selected_id`.
    pub(crate) fn ensure_initial_tab(&mut self, doc: &Document, history: &CommandHistory) {
        if !self.tabs.is_empty() {
            return;
        }
        let title = Self::tab_title(doc, &self.current_file);
        // A saved file is "clean" at the current HEAD node.
        let last_saved_node = self.current_file.as_ref().map(|_| history.current_node());
        self.tabs.push(DocTab {
            document: Document::default_artboard(),
            history: CommandHistory::default(),
            view: CanvasView::default(),
            current_file: None,
            selected_id: None,
            title,
            dirty: false,
            recovery_path: None,
            last_saved_node,
            mode: AppMode::default(),
            timeline_view: TimelineView::default(),
            playhead: Tick::default(),
            timeline_selection: Vec::new(),
        });
        self.active_tab = 0;
    }

    /// Install a freshly created/loaded document as a new tab and switch to it.
    /// `file` is its `.photon` path (None for a brand-new untitled doc).
    pub(crate) fn open_in_new_tab(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        view: &mut CanvasView,
        new_doc: Document,
        new_history: CommandHistory,
        file: Option<PathBuf>,
    ) {
        // Make sure the currently-active document has a home before we switch off
        // it (e.g. the first document opened from the welcome screen).
        self.ensure_initial_tab(doc, history);
        let title = Self::tab_title(&new_doc, &file);
        let last_saved_node = file.as_ref().map(|_| new_history.current_node());
        let idx = self.tabs.len();
        self.tabs.push(DocTab {
            document: new_doc,
            history: new_history,
            view: CanvasView::default(),
            current_file: file,
            selected_id: None,
            title,
            dirty: false,
            recovery_path: None,
            last_saved_node,
            mode: AppMode::default(),
            timeline_view: TimelineView::default(),
            playhead: Tick::default(),
            timeline_selection: Vec::new(),
        });
        self.switch_tab(idx, doc, history, view);
        // Fit the newly-activated document to the viewport next canvas pass.
        self.fit_pending = true;
    }

    /// Switch the active document to `target`, swapping live engine state with the
    /// parked tab. No-op if already active or out of range.
    pub(crate) fn switch_tab(
        &mut self,
        target: usize,
        doc: &mut Document,
        history: &mut CommandHistory,
        view: &mut CanvasView,
    ) {
        if target >= self.tabs.len() || target == self.active_tab {
            return;
        }
        let a = self.active_tab;
        // Park the active document into its slot (its engine fields were scratch).
        std::mem::swap(doc, &mut self.tabs[a].document);
        std::mem::swap(history, &mut self.tabs[a].history);
        std::mem::swap(view, &mut self.tabs[a].view);
        self.tabs[a].current_file = self.current_file.take();
        self.tabs[a].selected_id = self.selected_id.take();
        self.tabs[a].mode = self.mode;
        self.tabs[a].timeline_view = self.timeline_view;
        self.tabs[a].playhead = self.playhead;
        self.tabs[a].timeline_selection = std::mem::take(&mut self.timeline_selection);
        // Activate the target: its parked engine state moves into the live params;
        // the scratch placeholder lands in the target's now-active slot.
        std::mem::swap(doc, &mut self.tabs[target].document);
        std::mem::swap(history, &mut self.tabs[target].history);
        std::mem::swap(view, &mut self.tabs[target].view);
        self.current_file = self.tabs[target].current_file.take();
        self.selected_id = self.tabs[target].selected_id.take();
        self.mode = self.tabs[target].mode;
        self.timeline_view = self.tabs[target].timeline_view;
        self.playhead = self.tabs[target].playhead;
        self.timeline_selection = std::mem::take(&mut self.tabs[target].timeline_selection);
        self.active_tab = target;
        // A different document is now live — drop any transient per-doc UI state
        // that referenced the old selection.
        self.selected_layer_ids.clear();
    }

    /// Close the tab at `idx`. Assumes any unsaved-changes prompt has already been
    /// resolved by the caller. If it's the last tab, returns to the welcome screen.
    pub(crate) fn close_tab(
        &mut self,
        idx: usize,
        doc: &mut Document,
        history: &mut CommandHistory,
        view: &mut CanvasView,
    ) {
        if idx >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            // Last document closed → clear everything and show the welcome screen.
            if let Some(rp) = self.tabs[0].recovery_path.take() {
                let _ = std::fs::remove_file(rp);
            }
            self.tabs.clear();
            self.active_tab = 0;
            self.current_file = None;
            self.selected_id = None;
            *doc = Document::default_artboard();
            history.reset();
            self.show_welcome = true;
            return;
        }
        if idx == self.active_tab {
            // Switch to a neighbour first so the live params hold a document we
            // keep; the closing document's real state parks into `tabs[idx]`.
            let neighbour = if idx + 1 < self.tabs.len() {
                idx + 1
            } else {
                idx - 1
            };
            self.switch_tab(neighbour, doc, history, view);
        }
        if let Some(rp) = self.tabs[idx].recovery_path.take() {
            let _ = std::fs::remove_file(rp);
        }
        self.tabs.remove(idx);
        if self.active_tab > idx {
            self.active_tab -= 1;
        }
    }

    /// Refresh the active tab's title/dirty each frame. Called near the end of
    /// `draw` once `doc_modified` for the frame is known.
    pub(crate) fn sync_active_tab_meta(&mut self, doc: &Document, doc_modified: bool) {
        let title = Self::tab_title(doc, &self.current_file);
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            if doc_modified {
                tab.dirty = true;
            }
            tab.title = title;
        }
    }

    /// Record that the active document was just written to disk: clear its dirty
    /// flag, mark the current history node as the on-disk state (drives the "Last
    /// Save" marker in the branch viewer), and drop any recovery file. Call after
    /// every successful manual save / Save-As of the active document.
    pub(crate) fn mark_active_tab_saved(&mut self, history: &CommandHistory) {
        let node = history.current_node();
        if let Some(t) = self.tabs.get_mut(self.active_tab) {
            t.dirty = false;
            t.last_saved_node = Some(node);
            if let Some(rp) = t.recovery_path.take() {
                let _ = std::fs::remove_file(rp);
            }
        }
    }

    /// Whether any open document (active or parked) has unsaved changes.
    pub(crate) fn any_unsaved(&self) -> bool {
        self.tabs.iter().any(|t| t.dirty)
    }

    /// Public accessor for the host: does the app hold unsaved work? Used by the
    /// window-close handler in `main.rs` to decide whether to prompt before quit.
    pub fn has_unsaved_changes(&self) -> bool {
        self.any_unsaved()
    }

    /// The document tab bar — one chip per open document plus a "new" button.
    /// Interactions set `pending_tab_switch` / `pending_tab_close`, applied at the
    /// top of the next frame where the live engine params are available.
    pub(crate) fn draw_tab_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("doc_tabs")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(6.0, 3.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::horizontal()
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let count = self.tabs.len();
                            let multi = count > 1;
                            for i in 0..count {
                                let active = i == self.active_tab;
                                let dirty = self.tabs[i].dirty;
                                let title = self.tabs[i].title.clone();
                                // Chip: dirty dot + title (click switches / middle
                                // click closes) followed by a close button.
                                let dot = if dirty { "● " } else { "" };
                                let resp = ui
                                    .selectable_label(active, format!("{dot}{title}"))
                                    .on_hover_text(if dirty {
                                        "Unsaved changes"
                                    } else {
                                        "Switch to document"
                                    });
                                if resp.clicked() {
                                    self.pending_tab_switch = Some(i);
                                }
                                if resp.middle_clicked() {
                                    self.pending_tab_close = Some(i);
                                }
                                // Close button — always available (closing the last
                                // tab returns to the welcome screen).
                                if ui
                                    .add(egui::Button::new(ph::X).small().frame(false))
                                    .on_hover_text("Close document")
                                    .clicked()
                                {
                                    self.pending_tab_close = Some(i);
                                }
                                if multi {
                                    ui.separator();
                                }
                            }
                            // New-document button → reuse the File ▸ New modal.
                            if ui
                                .add(egui::Button::new(ph::PLUS).small().frame(false))
                                .on_hover_text("New document")
                                .clicked()
                            {
                                self.new_document_modal =
                                    Some(crate::welcome::NewDocumentForm::new());
                            }
                        });
                    });
            });
    }
}
