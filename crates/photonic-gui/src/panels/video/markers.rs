//! `DrawerGroup::Markers` panel (26 K-A2) — the markers workflow surface.
//!
//! The marker *container* shipped long ago (`Marker` with a category, a
//! duration and an anchor; `TimelineProject::marker_categories`; clip-scoped
//! `Clip::markers`) and the *workflow* did not: nothing created a category,
//! nothing wrote a clip marker, nothing could give a marker a duration, and
//! there was no list. This panel is that workflow.
//!
//! What it does:
//! - lists every marker of the active sequence in BOTH scopes (sequence markers
//!   and the clip-scoped markers of clips on that sequence), with search,
//!   category filter, scope filter and sort;
//! - navigates: clicking a row seeks the playhead and costs **zero** undo
//!   units, via [`PanelAction::SeekPlayhead`] (the panel only gets a *copy* of
//!   the playhead through `VideoPanelUi`, so it queues an action rather than
//!   seeking directly — same rule the caption editor follows);
//! - edits: name, note, category, position and **duration** — the last of which
//!   is what makes a *ranged* marker reachable at all. `export_per_marker`
//!   (K-F2) fans out one export job per ranged marker, and before this panel
//!   no user could create one;
//! - manages the category registry: seed the defaults, add, rename, recolour,
//!   re-glyph, and delete with an explicit reassign target.
//!
//! ## Undo discipline
//! Every mutation is one `TimelineCmd` handed up as
//! [`PanelAction::ClipEditDiscrete`] (or `ClipEditBatch` for the category
//! seed), i.e. one user verb = one undo unit. Text fields follow
//! `caption_editor.rs`'s shipped rule rather than the pointer-gated coalescing
//! path: the draft lives in `ui.data` and commits **once** on `lost_focus`,
//! so a typing session is one undo step, not one per keystroke.

use crate::panels::{PanelAction, PropPanelCtx};
use egui::{Color32, RichText, Ui};
use egui_phosphor::regular as ph;
use photonic_core::timeline::{
    ops, Marker, MarkerCategory, MarkerCategoryId, MarkerGlyph, MarkerId, MarkerRef, Sequence,
    SequenceId, Tick, TimelineProject,
};
use photonic_core::Color;

const MUTED: Color32 = Color32::from_rgb(0x7A, 0x7A, 0x9A); // `secondary`
const WARN: Color32 = Color32::from_rgb(0xE0, 0x9B, 0x3C);

// ── Pure list model (unit-tested below; no egui) ────────────────────────────

/// Which scopes the list shows.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum MarkerScopeFilter {
    #[default]
    All,
    SequenceOnly,
    ClipOnly,
}

/// Row ordering.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum MarkerSort {
    /// Timeline position, then name — the editing order.
    #[default]
    Time,
    /// Name (case-insensitive), then position.
    Name,
}

/// The panel's filter state (session-only).
#[derive(Clone, Debug, Default)]
pub(crate) struct MarkerFilter {
    /// Case-insensitive substring over name + note. Empty = no filter.
    pub(crate) search: String,
    /// `None` = every category; `Some(None)` = uncategorized only;
    /// `Some(Some(id))` = that category only.
    pub(crate) category: Option<Option<MarkerCategoryId>>,
    pub(crate) scope: MarkerScopeFilter,
    pub(crate) sort: MarkerSort,
    /// Hide markers whose category id is not in the registry.
    pub(crate) hide_dangling: bool,
}

/// One list row: a marker plus everything the row needs that is *not* on the
/// marker itself (its scope, and its TIMELINE position — a clip marker's `at`
/// is clip-relative, so the list must resolve `clip.start + at` or it would
/// sort clip markers into the wrong place).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MarkerRow {
    pub(crate) scope: MarkerRef,
    pub(crate) marker: Marker,
    /// Sequence-relative position — equal to `marker.at` for a sequence marker.
    pub(crate) timeline_at: Tick,
    /// Owning clip's name, for a clip marker.
    pub(crate) clip_name: Option<String>,
    /// True when `marker.category` names a category the project does not have.
    /// Such a marker renders neutral and is flagged; it is never silently
    /// remapped (35 §1.3).
    pub(crate) dangling_category: bool,
}

impl MarkerRow {
    pub(crate) fn is_clip_scoped(&self) -> bool {
        matches!(self.scope, MarkerRef::Clip { .. })
    }
    pub(crate) fn timeline_end(&self) -> Tick {
        self.timeline_at + self.marker.duration
    }
}

/// Collect and filter the rows for one sequence. Pure: no egui, no document
/// mutation — this is the part with the interesting behaviour, so it is the
/// part with tests.
pub(crate) fn marker_rows(
    project: &TimelineProject,
    seq_id: SequenceId,
    filter: &MarkerFilter,
) -> Vec<MarkerRow> {
    let Some(seq) = project.sequences.get(&seq_id) else {
        return Vec::new();
    };
    let known =
        |c: Option<MarkerCategoryId>| c.is_none_or(|id| project.marker_category(id).is_some());

    let mut rows: Vec<MarkerRow> = Vec::new();
    if filter.scope != MarkerScopeFilter::ClipOnly {
        for m in &seq.markers {
            rows.push(MarkerRow {
                scope: MarkerRef::Sequence {
                    seq: seq_id,
                    marker: m.id,
                },
                timeline_at: m.at,
                dangling_category: !known(m.category),
                clip_name: None,
                marker: m.clone(),
            });
        }
    }
    if filter.scope != MarkerScopeFilter::SequenceOnly {
        for t in seq.tracks() {
            for c in &t.clips {
                for m in &c.markers {
                    rows.push(MarkerRow {
                        scope: MarkerRef::Clip {
                            clip: c.id,
                            marker: m.id,
                        },
                        // `marker_sequence_tick` is the one place this mapping
                        // lives; re-deriving `clip.start + m.at` here would be
                        // a second copy of it.
                        timeline_at: c.marker_sequence_tick(m),
                        dangling_category: !known(m.category),
                        clip_name: Some(if c.name.is_empty() {
                            "(unnamed clip)".to_string()
                        } else {
                            c.name.clone()
                        }),
                        marker: m.clone(),
                    });
                }
            }
        }
    }

    let needle = filter.search.trim().to_lowercase();
    rows.retain(|r| {
        if filter.hide_dangling && r.dangling_category {
            return false;
        }
        if let Some(want) = filter.category {
            if r.marker.category != want {
                return false;
            }
        }
        if needle.is_empty() {
            return true;
        }
        r.marker.name.to_lowercase().contains(&needle)
            || r.marker.note.to_lowercase().contains(&needle)
    });

    match filter.sort {
        MarkerSort::Time => rows.sort_by(|a, b| {
            a.timeline_at
                .cmp(&b.timeline_at)
                .then_with(|| a.marker.name.cmp(&b.marker.name))
                .then_with(|| a.marker.id.cmp(&b.marker.id))
        }),
        MarkerSort::Name => rows.sort_by(|a, b| {
            a.marker
                .name
                .to_lowercase()
                .cmp(&b.marker.name.to_lowercase())
                .then_with(|| a.timeline_at.cmp(&b.timeline_at))
                .then_with(|| a.marker.id.cmp(&b.marker.id))
        }),
    }
    rows
}

/// The nearest marker at or after `from`, and the nearest at or before it —
/// the two navigation verbs the command palette binds. Ties break toward the
/// earlier marker so repeated "next" always advances.
pub(crate) fn next_marker_at(rows: &[MarkerRow], from: Tick) -> Option<Tick> {
    rows.iter()
        .map(|r| r.timeline_at)
        .filter(|t| *t > from)
        .min()
}

pub(crate) fn prev_marker_at(rows: &[MarkerRow], from: Tick) -> Option<Tick> {
    rows.iter()
        .map(|r| r.timeline_at)
        .filter(|t| *t < from)
        .max()
}

// ── Display helpers ─────────────────────────────────────────────────────────

fn to_col32(c: Color) -> Color32 {
    Color32::from_rgb(
        (c.r.clamp(0.0, 1.0) * 255.0) as u8,
        (c.g.clamp(0.0, 1.0) * 255.0) as u8,
        (c.b.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

/// The phosphor icon standing in for a [`MarkerGlyph`] in the list (the ruler
/// paints the real vector glyph; a list row wants a font glyph).
pub(crate) fn glyph_icon(g: MarkerGlyph) -> &'static str {
    match g {
        MarkerGlyph::Diamond => ph::DIAMOND,
        MarkerGlyph::Circle => ph::CIRCLE,
        MarkerGlyph::Square => ph::SQUARE,
        MarkerGlyph::Triangle => ph::TRIANGLE,
        MarkerGlyph::Flag => ph::FLAG,
        MarkerGlyph::Bar => ph::MINUS,
    }
}

pub(crate) const GLYPH_CHOICES: [MarkerGlyph; 6] = [
    MarkerGlyph::Diamond,
    MarkerGlyph::Circle,
    MarkerGlyph::Square,
    MarkerGlyph::Triangle,
    MarkerGlyph::Flag,
    MarkerGlyph::Bar,
];

fn glyph_label(g: MarkerGlyph) -> &'static str {
    match g {
        MarkerGlyph::Diamond => "Diamond",
        MarkerGlyph::Circle => "Circle",
        MarkerGlyph::Square => "Square",
        MarkerGlyph::Triangle => "Triangle",
        MarkerGlyph::Flag => "Flag",
        MarkerGlyph::Bar => "Bar",
    }
}

/// `HH:MM:SS:FF` at the sequence rate, honouring its start timecode — the same
/// formatting the ruler uses, so a note that quotes a marker's timecode matches
/// what the user reads off the timeline.
fn timecode(seq: &Sequence, t: Tick) -> String {
    photonic_core::timeline::Timecode::format_tick(
        t,
        seq.frame_rate,
        seq.start_timecode,
        seq.frame_rate.is_drop_frame_rate(),
    )
}

// ── The panel ───────────────────────────────────────────────────────────────

fn filter_id() -> egui::Id {
    egui::Id::new("markers_panel_filter")
}
fn text_draft_id(marker: MarkerId, field: &str) -> egui::Id {
    egui::Id::new(("markers_panel_text", marker, field))
}
fn new_category_draft_id() -> egui::Id {
    egui::Id::new("markers_panel_new_category")
}
fn category_delete_target_id() -> egui::Id {
    egui::Id::new("markers_panel_category_delete")
}

/// Left-rail Markers drawer.
pub(crate) fn draw_markers(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let Some(project) = ctx.doc.timeline.as_ref() else {
        ui.label(RichText::new("No video project yet.").color(MUTED));
        return;
    };
    let Some(seq_id) = project.active_sequence else {
        ui.label(RichText::new("No active sequence.").color(MUTED));
        return;
    };
    let Some(seq) = project.sequences.get(&seq_id) else {
        ui.label(RichText::new("No active sequence.").color(MUTED));
        return;
    };
    let playhead = ctx.video.playhead;

    let mut filter: MarkerFilter = ui
        .data(|d| d.get_temp::<StoredFilter>(filter_id()))
        .unwrap_or_default()
        .into();
    // The drawer's shared search box doubles as the marker search, matching
    // every other drawer (`draw_drawer` renders it above this fn).
    if !ctx.prop_search.trim().is_empty() {
        filter.search = ctx.prop_search.clone();
    }

    ui.horizontal_wrapped(|ui| {
        if ui
            .button(format!("{} Add at playhead", ph::MAP_PIN))
            .on_hover_text("Add a point marker at the playhead (M)")
            .clicked()
        {
            if let Ok(cmd) = ops::add_marker(project, seq_id, Marker::new(playhead, "Marker")) {
                ctx.action = Some(PanelAction::ClipEditDiscrete(cmd));
            }
        }
        let wr = seq.work_range;
        let add_range = ui.add_enabled(
            wr.is_some(),
            egui::Button::new(format!("{} Add from work range", ph::BRACKETS_SQUARE)),
        );
        if add_range
            .on_hover_text(
                "Add a RANGED marker spanning the in/out work range — the unit \
                 'Export each ranged marker' fans out over",
            )
            .clicked()
        {
            if let Some((s, e)) = wr {
                let mut m = Marker::new(s, "Range");
                m.duration = e - s;
                if let Ok(cmd) = ops::add_marker(project, seq_id, m) {
                    ctx.action = Some(PanelAction::ClipEditDiscrete(cmd));
                }
            }
        }
    });

    ui.add_space(4.0);
    draw_filter_bar(ui, project, &mut filter);
    ui.data_mut(|d| d.insert_temp(filter_id(), StoredFilter::from(&filter)));

    let rows = marker_rows(project, seq_id, &filter);
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "{} marker{}",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        ))
        .small()
        .color(MUTED),
    );

    if rows.is_empty() {
        ui.label(
            RichText::new(
                "No markers match. Double-click the ruler, or use \"Add at playhead\" above.",
            )
            .small()
            .color(MUTED),
        );
    }

    egui::ScrollArea::vertical()
        .id_salt("markers_panel_rows")
        .max_height(320.0)
        .show(ui, |ui| {
            for row in &rows {
                draw_row(ui, ctx, project, seq, row, playhead);
            }
        });

    ui.add_space(8.0);
    ui.separator();
    draw_category_editor(ui, ctx, project);
}

/// Session filter state, in the `Copy`-able shape `ui.data` wants (a `String`
/// search would not be `Copy`; the drawer's own search box owns that anyway).
#[derive(Copy, Clone, Debug, Default)]
struct StoredFilter {
    category: Option<Option<MarkerCategoryId>>,
    scope: MarkerScopeFilter,
    sort: MarkerSort,
    hide_dangling: bool,
}

impl From<&MarkerFilter> for StoredFilter {
    fn from(f: &MarkerFilter) -> Self {
        StoredFilter {
            category: f.category,
            scope: f.scope,
            sort: f.sort,
            hide_dangling: f.hide_dangling,
        }
    }
}

impl From<StoredFilter> for MarkerFilter {
    fn from(s: StoredFilter) -> Self {
        MarkerFilter {
            search: String::new(),
            category: s.category,
            scope: s.scope,
            sort: s.sort,
            hide_dangling: s.hide_dangling,
        }
    }
}

fn draw_filter_bar(ui: &mut Ui, project: &TimelineProject, filter: &mut MarkerFilter) {
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("markers_scope")
            .selected_text(match filter.scope {
                MarkerScopeFilter::All => "All scopes",
                MarkerScopeFilter::SequenceOnly => "Sequence",
                MarkerScopeFilter::ClipOnly => "Clip",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut filter.scope, MarkerScopeFilter::All, "All scopes");
                ui.selectable_value(
                    &mut filter.scope,
                    MarkerScopeFilter::SequenceOnly,
                    "Sequence",
                );
                ui.selectable_value(&mut filter.scope, MarkerScopeFilter::ClipOnly, "Clip");
            });

        let cat_text = match filter.category {
            None => "All categories".to_string(),
            Some(None) => "Uncategorized".to_string(),
            Some(Some(id)) => project
                .marker_category(id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "(missing)".to_string()),
        };
        egui::ComboBox::from_id_salt("markers_category_filter")
            .selected_text(cat_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut filter.category, None, "All categories");
                ui.selectable_value(&mut filter.category, Some(None), "Uncategorized");
                for c in &project.marker_categories {
                    ui.selectable_value(&mut filter.category, Some(Some(c.id)), &c.name);
                }
            });

        egui::ComboBox::from_id_salt("markers_sort")
            .selected_text(match filter.sort {
                MarkerSort::Time => "By time",
                MarkerSort::Name => "By name",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut filter.sort, MarkerSort::Time, "By time");
                ui.selectable_value(&mut filter.sort, MarkerSort::Name, "By name");
            });

        ui.checkbox(&mut filter.hide_dangling, "Hide unresolved")
            .on_hover_text(
                "Hide markers whose category is missing from this project. They \
                 render neutral and are never remapped for you.",
            );
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    project: &TimelineProject,
    seq: &Sequence,
    row: &MarkerRow,
    playhead: Tick,
) {
    let m = &row.marker;
    let cat = m.category.and_then(|id| project.marker_category(id));
    let swatch = m
        .color
        .or(cat.map(|c| c.color))
        .map(to_col32)
        .unwrap_or(MUTED);
    let at_playhead =
        playhead >= row.timeline_at && playhead < row.timeline_end().max(row.timeline_at + Tick(1));

    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(glyph_icon(cat.map(|c| c.glyph).unwrap_or_default()))
                        .color(swatch),
                );
                // Navigation is free: seeking records nothing in history.
                let label = RichText::new(timecode(seq, row.timeline_at)).monospace();
                let label = if at_playhead { label.strong() } else { label };
                if ui
                    .button(label)
                    .on_hover_text("Go to this marker (no undo step)")
                    .clicked()
                {
                    ctx.action = Some(PanelAction::SeekPlayhead {
                        at: row.timeline_at,
                    });
                }
                if m.is_range() {
                    ui.label(
                        RichText::new(format!("→ {}", timecode(seq, row.timeline_end())))
                            .monospace()
                            .small()
                            .color(MUTED),
                    );
                }
                if row.is_clip_scoped() {
                    let name = row.clip_name.as_deref().unwrap_or("");
                    ui.label(
                        RichText::new(format!("{} {name}", ph::FILM_STRIP))
                            .small()
                            .color(MUTED),
                    )
                    .on_hover_text("A clip marker — clip-relative, and it travels with the clip.");
                }
                if row.dangling_category {
                    ui.label(
                        RichText::new(format!("{} unresolved category", ph::WARNING))
                            .small()
                            .color(WARN),
                    )
                    .on_hover_text(
                        "This marker names a category this project does not have. \
                             It renders neutral and is never silently remapped — pick a \
                             category below to resolve it.",
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new(ph::TRASH))
                        .on_hover_text("Remove marker")
                        .clicked()
                    {
                        let cmd = match row.scope {
                            MarkerRef::Sequence { seq, marker } => {
                                ops::remove_marker(project, seq, marker)
                            }
                            MarkerRef::Clip { clip, marker } => {
                                ops::remove_clip_marker(project, clip, marker)
                            }
                        };
                        if let Ok(cmd) = cmd {
                            ctx.action = Some(PanelAction::ClipEditDiscrete(cmd));
                        }
                    }
                });
            });

            // Name + note: one undo step per editing session (commit on blur).
            let mut edited: Option<Marker> = None;
            ui.horizontal(|ui| {
                if let Some(next) = draft_text(ui, m, "name", &m.name, "Marker name") {
                    let mut n = m.clone();
                    n.name = next;
                    edited = Some(n);
                }
            });
            if let Some(next) = draft_text(ui, m, "note", &m.note, "Note") {
                let mut n = m.clone();
                n.note = next;
                edited = Some(n);
            }

            ui.horizontal_wrapped(|ui| {
                // Category picker — also the way to resolve a dangling one.
                let cat_text = match cat {
                    Some(c) => c.name.clone(),
                    None if row.dangling_category => "(missing)".to_string(),
                    None => "Uncategorized".to_string(),
                };
                egui::ComboBox::from_id_salt(("markers_row_cat", m.id))
                    .selected_text(cat_text)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(m.category.is_none(), "Uncategorized")
                            .clicked()
                        {
                            let mut n = m.clone();
                            n.category = None;
                            edited = Some(n);
                        }
                        for c in &project.marker_categories {
                            if ui
                                .selectable_label(m.category == Some(c.id), &c.name)
                                .clicked()
                            {
                                let mut n = m.clone();
                                n.category = Some(c.id);
                                edited = Some(n);
                            }
                        }
                    });

                // Duration: a marker with duration > 0 is RANGED, and ranged is
                // the unit K-F2's per-marker export iterates.
                let fps = (seq.frame_rate.num as f64 / seq.frame_rate.den.max(1) as f64).max(1.0);
                let mut secs =
                    m.duration.0 as f64 / photonic_core::timeline::TICKS_PER_SECOND as f64;
                let resp = ui.add(
                    egui::DragValue::new(&mut secs)
                        .speed(0.05)
                        .range(0.0..=f64::MAX)
                        .suffix(" s")
                        .max_decimals(3),
                );
                resp.on_hover_text(
                    "Marker length. 0 = a point marker; above 0 makes it a RANGED \
                     marker, which \"Export each ranged marker\" exports as its own file.",
                );
                let want =
                    Tick((secs * photonic_core::timeline::TICKS_PER_SECOND as f64).round() as i64);
                if want != m.duration {
                    let mut n = m.clone();
                    n.duration = want.max(Tick::ZERO);
                    edited = Some(n);
                }
                let _ = fps;

                if ui
                    .button(RichText::new(format!("{} Set to playhead", ph::CROSSHAIR)).small())
                    .on_hover_text("Move this marker to the playhead")
                    .clicked()
                {
                    let mut n = m.clone();
                    // A clip marker's `at` is clip-relative, so map back.
                    n.at = match row.scope {
                        MarkerRef::Sequence { .. } => playhead,
                        MarkerRef::Clip { .. } => playhead - (row.timeline_at - m.at),
                    };
                    if n.at >= Tick::ZERO {
                        edited = Some(n);
                    }
                }
            });

            if let Some(next) = edited {
                let cmd = match row.scope {
                    MarkerRef::Sequence { seq, .. } => ops::set_marker(project, seq, next),
                    MarkerRef::Clip { clip, .. } => ops::set_clip_marker(project, clip, next),
                };
                if let Ok(cmd) = cmd {
                    ctx.action = Some(PanelAction::ClipEditDiscrete(cmd));
                }
            }
        });
}

/// A single-line text field whose draft lives in `ui.data` and commits ONCE on
/// blur, returning the new value only when it actually changed. `Escape`
/// abandons the draft. This is `caption_editor.rs`'s shipped rule: the
/// coalescing path is pointer-gated (`app/mod.rs` opens it only while the
/// pointer is down) so typing would otherwise emit one undo entry per frame.
fn draft_text(ui: &mut Ui, m: &Marker, field: &str, current: &str, hint: &str) -> Option<String> {
    let id = text_draft_id(m.id, field);
    let mut buf = ui
        .data(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| current.to_string());
    let resp = ui.add(
        egui::TextEdit::singleline(&mut buf)
            .hint_text(hint)
            .desired_width(f32::INFINITY),
    );
    if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|d| d.remove::<String>(id));
        return None;
    }
    if resp.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(id));
        return (buf != current).then_some(buf);
    }
    if resp.has_focus() || resp.changed() {
        ui.data_mut(|d| d.insert_temp(id, buf));
    }
    None
}

fn draw_category_editor(ui: &mut Ui, ctx: &mut PropPanelCtx, project: &TimelineProject) {
    ui.label(RichText::new("CATEGORIES").small().color(MUTED));

    if project.marker_categories.is_empty() {
        ui.label(
            RichText::new("This project has no marker categories yet.")
                .small()
                .color(MUTED),
        );
        if ui
            .button(format!("{} Add the default set", ph::SPARKLE))
            .on_hover_text("Marker / Cut / Note / Todo / Chapter — one undo step")
            .clicked()
        {
            let cmds = ops::seed_marker_categories(project);
            if !cmds.is_empty() {
                ctx.action = Some(PanelAction::ClipEditBatch(cmds));
            }
        }
    }

    let pending_delete = ui.data(|d| d.get_temp::<MarkerCategoryId>(category_delete_target_id()));

    for c in &project.marker_categories {
        ui.horizontal(|ui| {
            let mut rgb = [c.color.r, c.color.g, c.color.b];
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                let mut n = c.clone();
                n.color = Color::rgb(rgb[0], rgb[1], rgb[2]);
                if let Ok(cmd) = ops::set_marker_category(project, n) {
                    ctx.action = Some(PanelAction::ClipEditDiscrete(cmd));
                }
            }
            let id = egui::Id::new(("markers_cat_name", c.id));
            let mut buf = ui
                .data(|d| d.get_temp::<String>(id))
                .unwrap_or_else(|| c.name.clone());
            let resp = ui.add(
                egui::TextEdit::singleline(&mut buf)
                    .desired_width(120.0)
                    .hint_text("Category name"),
            );
            if resp.lost_focus() {
                ui.data_mut(|d| d.remove::<String>(id));
                if buf != c.name && !buf.trim().is_empty() {
                    let mut n = c.clone();
                    n.name = buf.trim().to_string();
                    if let Ok(cmd) = ops::set_marker_category(project, n) {
                        ctx.action = Some(PanelAction::ClipEditDiscrete(cmd));
                    }
                }
            } else if resp.has_focus() || resp.changed() {
                ui.data_mut(|d| d.insert_temp(id, buf));
            }

            egui::ComboBox::from_id_salt(("markers_cat_glyph", c.id))
                .selected_text(format!("{} {}", glyph_icon(c.glyph), glyph_label(c.glyph)))
                .width(110.0)
                .show_ui(ui, |ui| {
                    for g in GLYPH_CHOICES {
                        if ui
                            .selectable_label(
                                c.glyph == g,
                                format!("{} {}", glyph_icon(g), glyph_label(g)),
                            )
                            .clicked()
                            && c.glyph != g
                        {
                            let mut n = c.clone();
                            n.glyph = g;
                            if let Ok(cmd) = ops::set_marker_category(project, n) {
                                ctx.action = Some(PanelAction::ClipEditDiscrete(cmd));
                            }
                        }
                    }
                });

            let used = project.markers_in_category(c.id).len();
            if used > 0 {
                ui.label(RichText::new(format!("{used}")).small().color(MUTED))
                    .on_hover_text(format!("{used} marker(s) use this category"));
            }
            if ui
                .button(RichText::new(ph::TRASH))
                .on_hover_text("Delete this category")
                .clicked()
            {
                ui.data_mut(|d| d.insert_temp(category_delete_target_id(), c.id));
            }
        });

        // Delete confirmation, inline: deleting a category is the one place
        // the "never silently remapped" rule needs a decision from the user,
        // so we ask for it rather than picking one for them.
        if pending_delete == Some(c.id) {
            let used = project.markers_in_category(c.id).len();
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "Delete \"{}\"? {} marker(s) reference it.",
                        c.name, used
                    ))
                    .small(),
                );
                ui.horizontal_wrapped(|ui| {
                    let mut commit = |ui: &mut Ui, target: Option<MarkerCategoryId>| {
                        if let Ok(cmd) = ops::remove_marker_category(project, c.id, target) {
                            ctx.action = Some(PanelAction::ClipEditDiscrete(cmd));
                        }
                        ui.data_mut(|d| d.remove::<MarkerCategoryId>(category_delete_target_id()));
                    };
                    if ui
                        .button("Delete, clear category")
                        .on_hover_text("Those markers become uncategorized (undoable)")
                        .clicked()
                    {
                        commit(ui, None);
                    }
                    for other in &project.marker_categories {
                        if other.id == c.id {
                            continue;
                        }
                        if ui.button(format!("Move to \"{}\"", other.name)).clicked() {
                            commit(ui, Some(other.id));
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        ui.data_mut(|d| d.remove::<MarkerCategoryId>(category_delete_target_id()));
                    }
                });
            });
        }
    }

    ui.horizontal(|ui| {
        let id = new_category_draft_id();
        let mut buf = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_default();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut buf)
                .desired_width(140.0)
                .hint_text("New category name"),
        );
        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let clicked = ui
            .button(format!("{} Add", ph::PLUS))
            .on_hover_text("Create a marker category")
            .clicked();
        if (submit || clicked) && !buf.trim().is_empty() {
            let cat = MarkerCategory::new(buf.trim(), Color::rgb(0.55, 0.55, 0.75));
            if let Ok(cmd) = ops::add_marker_category(project, cat) {
                ctx.action = Some(PanelAction::ClipEditDiscrete(cmd));
            }
            ui.data_mut(|d| d.remove::<String>(id));
        } else {
            ui.data_mut(|d| d.insert_temp(id, buf));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{Clip, ClipSource, FrameRate, Track, TrackKind};

    /// A project with one sequence, one clip, one sequence marker and one clip
    /// marker — enough to prove scope, position mapping and filtering.
    fn fixture() -> (TimelineProject, SequenceId, MarkerId, MarkerId) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("S", FrameRate::FPS_30, 1920, 1080);
        let mut track = Track::new(TrackKind::Video, "V1");
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: Color::rgb(1.0, 0.0, 0.0),
            },
            Tick(1000),
            Tick(1000),
        );
        clip.name = "shot 1".into();
        let mut cm = Marker::clip_scoped(Tick(250), "beat");
        cm.note = "drum hit".into();
        let cm_id = cm.id;
        clip.markers.push(cm);
        track.clips.push(clip);
        seq.video_tracks.push(track);
        let mut sm = Marker::new(Tick(500), "intro");
        sm.duration = Tick(300);
        let sm_id = sm.id;
        seq.markers.push(sm);
        let seq_id = seq.id;
        project.insert_sequence(seq);
        (project, seq_id, sm_id, cm_id)
    }

    #[test]
    fn rows_cover_both_scopes_and_map_clip_markers_onto_the_timeline() {
        let (p, seq_id, sm, cm) = fixture();
        let rows = marker_rows(&p, seq_id, &MarkerFilter::default());
        assert_eq!(rows.len(), 2, "{rows:?}");
        // Sorted by TIMELINE position: the sequence marker at 500 comes before
        // the clip marker, which lives at clip.start(1000) + at(250) = 1250.
        assert_eq!(rows[0].marker.id, sm);
        assert_eq!(rows[0].timeline_at, Tick(500));
        assert_eq!(rows[1].marker.id, cm);
        assert_eq!(
            rows[1].timeline_at,
            Tick(1250),
            "a clip marker's `at` is clip-relative and must be rebased for the list"
        );
        assert_eq!(rows[1].marker.at, Tick(250), "the model value is untouched");
        assert_eq!(rows[1].clip_name.as_deref(), Some("shot 1"));
        assert!(rows[1].is_clip_scoped() && !rows[0].is_clip_scoped());
        // The ranged sequence marker reports its end.
        assert_eq!(rows[0].timeline_end(), Tick(800));
    }

    #[test]
    fn scope_filter_selects_one_side_only() {
        let (p, seq_id, sm, cm) = fixture();
        let only_seq = marker_rows(
            &p,
            seq_id,
            &MarkerFilter {
                scope: MarkerScopeFilter::SequenceOnly,
                ..Default::default()
            },
        );
        assert_eq!(only_seq.len(), 1);
        assert_eq!(only_seq[0].marker.id, sm);
        let only_clip = marker_rows(
            &p,
            seq_id,
            &MarkerFilter {
                scope: MarkerScopeFilter::ClipOnly,
                ..Default::default()
            },
        );
        assert_eq!(only_clip.len(), 1);
        assert_eq!(only_clip[0].marker.id, cm);
    }

    #[test]
    fn search_matches_name_and_note_case_insensitively() {
        let (p, seq_id, sm, cm) = fixture();
        let by_name = marker_rows(
            &p,
            seq_id,
            &MarkerFilter {
                search: "INTRO".into(),
                ..Default::default()
            },
        );
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].marker.id, sm);
        // The note is searched too — otherwise a review note is unfindable.
        let by_note = marker_rows(
            &p,
            seq_id,
            &MarkerFilter {
                search: "drum".into(),
                ..Default::default()
            },
        );
        assert_eq!(by_note.len(), 1);
        assert_eq!(by_note[0].marker.id, cm);
        let none = marker_rows(
            &p,
            seq_id,
            &MarkerFilter {
                search: "nothing here".into(),
                ..Default::default()
            },
        );
        assert!(none.is_empty(), "a non-matching search must filter it out");
    }

    #[test]
    fn category_filter_and_dangling_flag() {
        let (mut p, seq_id, sm, _) = fixture();
        let cat = MarkerCategory::new("Cut", Color::rgb(1.0, 0.0, 0.0));
        let cat_id = cat.id;
        p.marker_categories.push(cat);
        let ghost = MarkerCategoryId::new();
        {
            let s = p.sequences.get_mut(&seq_id).unwrap();
            s.markers[0].category = Some(cat_id);
            s.video_tracks[0].clips[0].markers[0].category = Some(ghost);
        }

        let by_cat = marker_rows(
            &p,
            seq_id,
            &MarkerFilter {
                category: Some(Some(cat_id)),
                ..Default::default()
            },
        );
        assert_eq!(by_cat.len(), 1);
        assert_eq!(by_cat[0].marker.id, sm);
        assert!(!by_cat[0].dangling_category);

        // The marker naming a category the project lacks is FLAGGED, not
        // dropped and not remapped (35 §1.3).
        let all = marker_rows(&p, seq_id, &MarkerFilter::default());
        let ghost_row = all
            .iter()
            .find(|r| r.marker.category == Some(ghost))
            .unwrap();
        assert!(ghost_row.dangling_category);
        assert_eq!(
            ghost_row.marker.category,
            Some(ghost),
            "a dangling reference must survive untouched"
        );

        let hidden = marker_rows(
            &p,
            seq_id,
            &MarkerFilter {
                hide_dangling: true,
                ..Default::default()
            },
        );
        assert_eq!(
            hidden.len(),
            1,
            "hide_dangling drops exactly the flagged row"
        );

        // "Uncategorized" is its own filter, distinct from "all".
        let uncat = marker_rows(
            &p,
            seq_id,
            &MarkerFilter {
                category: Some(None),
                ..Default::default()
            },
        );
        assert!(uncat.is_empty(), "both markers carry a category here");
    }

    #[test]
    fn name_sort_differs_from_time_sort() {
        let (p, seq_id, sm, cm) = fixture();
        let by_time = marker_rows(&p, seq_id, &MarkerFilter::default());
        let by_name = marker_rows(
            &p,
            seq_id,
            &MarkerFilter {
                sort: MarkerSort::Name,
                ..Default::default()
            },
        );
        // "beat" < "intro", but the clip marker is LATER on the timeline — so
        // the two orders genuinely disagree and the sort control does work.
        assert_eq!(
            (by_time[0].marker.id, by_time[1].marker.id),
            (sm, cm),
            "time order"
        );
        assert_eq!(
            (by_name[0].marker.id, by_name[1].marker.id),
            (cm, sm),
            "name order"
        );
    }

    #[test]
    fn next_and_prev_marker_skip_the_current_position() {
        let (p, seq_id, _, _) = fixture();
        let rows = marker_rows(&p, seq_id, &MarkerFilter::default());
        assert_eq!(next_marker_at(&rows, Tick(0)), Some(Tick(500)));
        // Landing exactly on a marker must still advance, or "next" would stick.
        assert_eq!(next_marker_at(&rows, Tick(500)), Some(Tick(1250)));
        assert_eq!(next_marker_at(&rows, Tick(1250)), None);
        assert_eq!(prev_marker_at(&rows, Tick(1250)), Some(Tick(500)));
        assert_eq!(prev_marker_at(&rows, Tick(500)), None);
    }

    #[test]
    fn unknown_sequence_yields_no_rows() {
        let (p, _, _, _) = fixture();
        assert!(marker_rows(&p, SequenceId::new(), &MarkerFilter::default()).is_empty());
    }
}
