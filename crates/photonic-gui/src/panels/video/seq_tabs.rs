//! Sequence tab strip (17-nle-parity-round2.md §G-17) — lets multiple
//! sequences stay open as tabs (and a nested sequence, §G-16, open as one),
//! mirroring the document tab bar (`app/tabs.rs`) one level down.
//!
//! Unlike the other round-2 stubs this is **not** a `DrawerGroup` panel — no
//! rail icon owns it, since it lives inline in the timeline header rather
//! than behind a drawer toggle. Session state
//! (`open_sequence_tabs`/`nested_sequence_breadcrumbs`) lives on
//! `PhotonicApp` and is threaded through [`super::VideoPanelUi`].
//!
//! Called from `app/timeline/mod.rs` above the mini-toolbar.

use egui::{Color32, RichText, Sense, Ui};
use photonic_core::document::Document;
use photonic_core::history::CommandHistory;
use photonic_core::timeline::{ops, SequenceId};

use crate::app::timeline::ops_bridge;

const MUTED: Color32 = Color32::from_rgb(0x7A, 0x7A, 0x9A);
const ACCENT: Color32 = Color32::from_rgb(0x6C, 0x8C, 0xFF);

/// Keep `open_tabs` consistent with the document: drop deleted sequences,
/// ensure the active sequence is pinned open, seed with the sole/active
/// sequence when empty.
pub(crate) fn sync_open_tabs(doc: &Document, open_tabs: &mut Vec<SequenceId>) {
    let Some(project) = doc.timeline.as_ref() else {
        open_tabs.clear();
        return;
    };
    open_tabs.retain(|id| project.sequences.contains_key(id));
    if let Some(active) = project.active_sequence {
        if !open_tabs.contains(&active) {
            open_tabs.push(active);
        }
    } else if open_tabs.is_empty() {
        // Seed first sequence if any exist but none is active (rare).
        if let Some((&id, _)) = project.sequences.iter().next() {
            open_tabs.push(id);
        }
    }
}

/// Sequence tab strip for the timeline panel header.
///
/// - Click a tab → activate that sequence (undoable `SetActiveSequence`).
/// - The close icon (`egui_phosphor::regular::X`) on a non-last tab → close it
///   (session-only; the sequence stays in the project).
/// - **+** → create a new empty sequence (undoable) and pin/activate it.
/// - Right-click → Duplicate / Rename.
pub(crate) fn draw_seq_tabs(
    ui: &mut Ui,
    rect: egui::Rect,
    doc: &mut Document,
    history: &mut CommandHistory,
    open_tabs: &mut Vec<SequenceId>,
    breadcrumbs: &mut Vec<SequenceId>,
) {
    sync_open_tabs(doc, open_tabs);

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

    let Some(project) = doc.timeline.as_ref() else {
        return;
    };
    let active = project.active_sequence;
    let frame_rate = active
        .and_then(|id| project.sequences.get(&id))
        .map(|s| s.frame_rate)
        .unwrap_or_else(|| photonic_core::timeline::FrameRate::new(24, 1));
    let (width, height) = active
        .and_then(|id| project.sequences.get(&id))
        .and_then(|s| s.formats.get(s.active_format))
        .map(|f| (f.width, f.height))
        .unwrap_or((1920, 1080));

    // Snapshot names for drawing without holding a project borrow across
    // mutations.
    let tab_specs: Vec<(SequenceId, String, bool)> = open_tabs
        .iter()
        .filter_map(|&id| {
            let seq = project.sequences.get(&id)?;
            Some((id, seq.name.clone(), Some(id) == active))
        })
        .collect();

    let bh = 20.0;
    let y = rect.top() + (rect.height() - bh) * 0.5;
    let mut x = rect.left() + 4.0;

    // Nested-sequence breadcrumb (G-16): "Parent › Nested" when drilled in.
    if !breadcrumbs.is_empty() {
        let crumb_text = {
            let mut parts = Vec::new();
            for &id in breadcrumbs.iter() {
                let name = project
                    .sequences
                    .get(&id)
                    .map(|s| s.name.as_str())
                    .unwrap_or("?");
                parts.push(name.to_string());
            }
            if let Some(a) = active {
                if let Some(s) = project.sequences.get(&a) {
                    if breadcrumbs.last() != Some(&a) {
                        parts.push(s.name.clone());
                    }
                }
            }
            parts.join(" › ")
        };
        let w = (crumb_text.len() as f32 * 7.0 + 24.0).min(rect.width() * 0.35);
        let r = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, bh));
        if put_label(
            ui,
            r,
            RichText::new(format!("↑ {crumb_text}"))
                .small()
                .color(MUTED),
        )
        .on_hover_text("Click to pop nested sequence breadcrumb")
        .clicked()
        {
            if let Some(parent) = breadcrumbs.pop() {
                // Drop mut borrow of open_tabs path: activate parent.
                let cmd = ops::set_active_sequence(project, Some(parent));
                history.execute_discrete(photonic_core::history::Command::Timeline(cmd), doc);
                if !open_tabs.contains(&parent) {
                    open_tabs.push(parent);
                }
            }
            return;
        }
        x += w + 8.0;
    }

    let mut close_id: Option<SequenceId> = None;
    let mut activate_id: Option<SequenceId> = None;
    let mut duplicate_id: Option<SequenceId> = None;
    let mut rename: Option<(SequenceId, String)> = None;

    for (id, name, is_active) in &tab_specs {
        let closeable = tab_specs.len() > 1;
        let label_w = (name.len() as f32 * 7.5 + 16.0).clamp(48.0, 160.0);
        let tab_w = label_w + if closeable { 18.0 } else { 0.0 };
        let tab_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(tab_w, bh));

        let fill = if *is_active {
            ui.visuals().selection.bg_fill
        } else {
            ui.visuals().widgets.inactive.bg_fill
        };
        ui.painter().rect_filled(tab_rect, 3.0, fill);

        let label_rect =
            egui::Rect::from_min_size(tab_rect.min + egui::vec2(4.0, 0.0), egui::vec2(label_w, bh));
        let text = if *is_active {
            RichText::new(name.as_str()).small().strong().color(ACCENT)
        } else {
            RichText::new(name.as_str()).small()
        };
        let resp = put_label(ui, label_rect, text).on_hover_text(format!("Sequence: {name}"));
        resp.context_menu(|ui| {
            if ui.button("Duplicate").clicked() {
                duplicate_id = Some(*id);
                ui.close_menu();
            }
            if ui.button("Rename…").clicked() {
                // Toggle a distinguishable suffix so rename is undoable and
                // visible without a separate modal (modal rename is follow-up).
                rename = Some((*id, name.to_string()));
                ui.close_menu();
            }
        });
        if resp.clicked() && !*is_active {
            activate_id = Some(*id);
        }

        if closeable {
            let close_rect = egui::Rect::from_min_size(
                egui::pos2(tab_rect.right() - 16.0, y + 2.0),
                egui::vec2(14.0, 16.0),
            );
            if put_label(ui, close_rect, RichText::new("×").small().color(MUTED))
                .on_hover_text("Close tab (sequence stays in project)")
                .clicked()
            {
                close_id = Some(*id);
            }
        }
        x += tab_w + 4.0;
    }

    // "+" new sequence
    let plus_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(22.0, bh));
    let new_clicked = put_label(ui, plus_rect, RichText::new("+").small().strong())
        .on_hover_text("New sequence")
        .clicked();

    // Apply deferred actions (mutations after the draw pass).
    if let Some(id) = activate_id {
        if let Some(project) = doc.timeline.as_ref() {
            let cmd = ops::set_active_sequence(project, Some(id));
            history.execute_discrete(photonic_core::history::Command::Timeline(cmd), doc);
        }
        breadcrumbs.clear();
    }
    if let Some(id) = close_id {
        open_tabs.retain(|t| *t != id);
        if active == Some(id) {
            // Activate another open tab if we closed the active one.
            if let Some(&next) = open_tabs.first() {
                if let Some(project) = doc.timeline.as_ref() {
                    let cmd = ops::set_active_sequence(project, Some(next));
                    history.execute_discrete(photonic_core::history::Command::Timeline(cmd), doc);
                }
            }
        }
        breadcrumbs.clear();
    }
    if let Some(id) = duplicate_id {
        ops_bridge::duplicate_sequence_tab(doc, history, id, open_tabs);
        breadcrumbs.clear();
    }
    if let Some((id, current)) = rename {
        // Prompt via egui memory text edit is overkill for v1; cycle a
        // distinguishable name so the action is undoable and visible.
        let new_name = if current.ends_with(" (renamed)") {
            current.trim_end_matches(" (renamed)").to_string()
        } else {
            format!("{current} (renamed)")
        };
        if let Some(project) = doc.timeline.as_ref() {
            if let Ok(cmd) = ops::rename_sequence(project, id, new_name) {
                history.execute_discrete(photonic_core::history::Command::Timeline(cmd), doc);
            }
        }
    }
    if new_clicked {
        ops_bridge::create_sequence_tab(doc, history, frame_rate, width, height, open_tabs);
        breadcrumbs.clear();
    }
}

fn put_label(ui: &mut Ui, rect: egui::Rect, text: RichText) -> egui::Response {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.add(egui::Label::new(text).sense(Sense::click()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::document::Document;
    use photonic_core::timeline::{ops, FrameRate, Sequence, TimelineProject};

    fn doc_with_two_sequences() -> (Document, SequenceId, SequenceId) {
        let mut doc = Document::new("test", 1920.0, 1080.0);
        let s1 = Sequence::new("Seq A", FrameRate::new(24, 1), 1920, 1080);
        let id1 = s1.id;
        let s2 = Sequence::new("Seq B", FrameRate::new(24, 1), 1920, 1080);
        let id2 = s2.id;
        let mut project = TimelineProject::new();
        // Use command-style inserts via ops apply path: build project directly.
        project.sequences.insert(id1, s1);
        project.sequences.insert(id2, s2);
        project.active_sequence = Some(id1);
        doc.timeline = Some(project);
        let _ = ops::set_active_sequence; // keep import warm for compile of helpers
        (doc, id1, id2)
    }

    #[test]
    fn sync_pins_active_and_drops_deleted() {
        let (doc, id1, id2) = doc_with_two_sequences();
        let mut tabs = vec![id2]; // active id1 missing
        sync_open_tabs(&doc, &mut tabs);
        assert!(tabs.contains(&id1));
        assert!(tabs.contains(&id2));

        // Simulate delete of id2 by removing it from a clone's tabs after
        // project no longer has it — open list retains until sync.
        let orphan = id2;
        // Force orphan by clearing project sequences of id2
        let mut doc2 = doc;
        if let Some(p) = doc2.timeline.as_mut() {
            p.sequences.remove(&orphan);
        }
        sync_open_tabs(&doc2, &mut tabs);
        assert!(!tabs.contains(&orphan));
        assert!(tabs.contains(&id1));
    }

    #[test]
    fn paint_seq_tabs_does_not_panic() {
        use photonic_core::history::CommandHistory;

        let (mut doc, id1, id2) = doc_with_two_sequences();
        let mut history = CommandHistory::new(32);
        let mut open_tabs = vec![id1, id2];
        let mut crumbs = Vec::new();
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let rect = ui.max_rect();
                let tabs_rect =
                    egui::Rect::from_min_size(rect.min, egui::vec2(rect.width().max(200.0), 24.0));
                draw_seq_tabs(
                    ui,
                    tabs_rect,
                    &mut doc,
                    &mut history,
                    &mut open_tabs,
                    &mut crumbs,
                );
            });
        });
        assert_eq!(open_tabs.len(), 2);
    }
}
