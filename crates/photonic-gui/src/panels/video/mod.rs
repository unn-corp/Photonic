//! Video-mode panel skeletons (video-editor-module `04-ui-mode-timeline.md`
//! §4.1 panel map). This module is the choke-point split of the former
//! `panels/video_stubs.rs`: one file per panel, each with a stub `draw` fn and
//! a "filled by <phase> panel story" marker so the six panel builders work
//! disjointly without touching `panels/mod.rs`'s `draw_drawer` dispatch, the
//! `PhotonicApp` struct, or each other.
//!
//! Panel → owning spec:
//! - [`clip_inspector`]  — 04-ui-mode-timeline.md (shell) / 01 §6.2 (widgets)
//! - [`effects_browser`] — 08-fusion-node-flows.md
//! - [`caption_editor`]  — 06-captions-ai.md
//! - [`color_page`]      — 07-color-grading.md (right-drawer controls + scopes)
//! - [`node_editor`]     — 08-fusion-node-flows.md (left palette + central canvas)
//! - [`audio_mixer`]     — 09-audio-mixer.md
//! - [`export_dialog`]   — 05-import-export.md
//! - [`duration_dialog`] — 26 K-A6 Edit Duration (position/in/out/duration + ripple)
//! - [`markers`]        — 26 K-A2 marker system depth (list/navigate/edit,
//!   ranged markers, clip markers, and the `MarkerCategory` registry)
//! - [`titles`]          — 05-import-export.md §4b / 17 G-12 (minimal: starter
//!   presets + `ClipSource::Text` insert/edit; not the full VectorDoc template
//!   system — see [`titles`]'s module doc for the scope cut)
//!
//! 17-nle-parity-round2.md choke-point additions (stub — filled by each
//! named story; the real surface for `source_monitor`/`multicam` is mostly
//! `app/monitor.rs`, out of this crate-relative module's territory, so these
//! two stay deliberately thin):
//! - [`source_monitor`]  — 17 G-10 (dual-monitor + true source in/out marks)
//! - [`multicam`]        — 17 G-20 (multicam angle grid + live cutting)
//! - [`transcript`]      — 20 §9 G-18 (derived transcript and synchronized edits)
//! - [`seq_tabs`]        — 17 G-17 (sequence tab strip). Not a `DrawerGroup`
//!   panel — no rail icon owns it; the timeline-panel story embeds it in the
//!   timeline panel header (`app/timeline/mod.rs`, out of this territory).

use std::collections::HashSet;

use photonic_core::timeline::{
    ClipId, CueId, GradeOpId, GraphId, GraphNodeId, SequenceId, Tick, TrackId,
};

pub(crate) mod audio_mixer;
pub(crate) mod caption_editor;
pub(crate) mod clip_inspector;
pub(crate) mod color_page;
pub(crate) mod diagnostics;
pub(crate) mod duration_dialog;
pub(crate) mod effect_presets;
pub(crate) mod effects_browser;
pub(crate) mod export_dialog;
pub(crate) mod keyframe_editor;
pub(crate) mod markers;
pub(crate) mod multicam;
pub(crate) mod node_editor;
pub(crate) mod param_expr;
pub(crate) mod render_queue_panel;
pub(crate) mod seq_tabs;
pub(crate) mod source_monitor;
pub(crate) mod titles;
pub(crate) mod transcript;

/// Which sub-section of the right-drawer Color Controls group is active
/// (07 §6). Owned by the color page story; defined here so the shared
/// [`VideoPanelUi`] / `PhotonicApp` field type is stable while that story is
/// unwritten.
#[allow(dead_code)] // variants beyond the default are constructed by the color-page story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub(crate) enum ColorPageTab {
    /// Lift/gamma/gain wheels (07 §3.5).
    #[default]
    Wheels,
    /// Master + per-channel curve editor (07 §3.6).
    Curves,
    /// HSL qualifier secondary (07 §3.7).
    Qualifier,
    /// 3D LUT browser / intensity (07 §3.8).
    Lut,
}

/// Which scope the floating scopes panel renders (07 §6, `scopes.rs`). Owned by
/// the color page story; defined here for field-type stability (see
/// [`ColorPageTab`]).
#[allow(dead_code)] // variants beyond the default are constructed by the color-page story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub(crate) enum ScopeKind {
    /// Per-column intensity waveform (07 §6 waveform).
    #[default]
    Waveform,
    /// RGB parade (three side-by-side waveforms).
    Parade,
    /// Cb/Cr vectorscope with skin-tone line (07 §6 vectorscope).
    Vectorscope,
    /// Luma/RGB histogram (07 §6 histogram).
    Histogram,
    /// K-E1: audio spectrum (dB vs frequency) during playback.
    AudioSpectrum,
}

/// The slice of `PhotonicApp` video-editor session state the video panels read
/// and mutate, threaded to every video panel stub so panel builders never touch
/// the `PhotonicApp` struct or the `draw_*` call sites.
///
/// Left-rail drawers reach it through [`super::PropPanelCtx::video`]; the
/// right-rail drawers, the floating scopes / export panels, and the central
/// node canvas receive `&mut VideoPanelUi` directly (built by
/// `PhotonicApp::video_panel_ui`).
///
/// Every field carries a `[panel]` tag naming its owning story.
#[allow(dead_code)] // fields are the wired API surface consumed as each panel story is filled in.
pub(crate) struct VideoPanelUi<'a> {
    /// Clips selected in the timeline (04 §2.6) — read-only targeting for the
    /// clip inspector / effects / color panels.
    pub(crate) selection: &'a [ClipId],
    /// [color_page] selected grade op in the clip's grade stack (07 §1).
    pub(crate) selected_grade_op: &'a mut Option<GradeOpId>,
    /// [color_page] active Color Controls sub-tab (07 §6).
    pub(crate) color_page_tab: &'a mut ColorPageTab,
    /// [color_page] floating scopes panel visibility (07 §6).
    pub(crate) scopes_panel_open: &'a mut bool,
    /// [color_page] which scope the floating panel shows (07 §6).
    pub(crate) scope_kind: &'a mut ScopeKind,
    /// [node_editor] graph open in the central node canvas, if any (08 §6.1).
    pub(crate) open_graph: &'a mut Option<GraphId>,
    /// [node_editor] selected node in the open graph (08 §6.1 inspector).
    pub(crate) selected_graph_node: &'a mut Option<GraphNodeId>,
    /// [node_editor] whether the central panel shows the node canvas instead of
    /// the program monitor (08 §6.1 central-panel content state).
    pub(crate) node_canvas_active: &'a mut bool,
    /// [audio_mixer] track strips whose EQ/comp/automation are expanded (09).
    pub(crate) mixer_expanded_tracks: &'a mut HashSet<TrackId>,
    /// [clip_inspector] clip whose animated props the keyframe editor targets
    /// (04 §4.1 / 01 §6).
    pub(crate) keyframe_editor_target: &'a mut Option<ClipId>,
    /// [caption_editor] cue currently being edited (06).
    pub(crate) caption_edit_cue: &'a mut Option<CueId>,
    /// [export_dialog] whether the video export dialog is open (05 §3).
    pub(crate) export_dialog_open: &'a mut bool,
    /// [export_dialog] name of the last-used export preset, seed for the dialog
    /// (05 §3). Session-only here; the export story persists it to prefs.
    pub(crate) last_export_preset: &'a mut String,
    /// [titles] Live playhead position (04 §6), read-only here. `PropPanelCtx`
    /// otherwise carries no live playhead (see `keyframe_editor.rs`'s
    /// `no_live_playhead` note) — the Titles panel needs the real value to
    /// insert a starter title at the playhead, so it's threaded through here
    /// rather than faked.
    pub(crate) playhead: Tick,

    // ── 17-nle-parity-round2.md choke-point additions ───────────────────────
    // Session state for the four round-2 stub panels above. Each field names
    // its owning story, same discipline as the block above.
    /// [source_monitor, 17 G-10] Scrub-bar playhead within the armed
    /// source's own media, independent of the program-monitor/timeline
    /// `playhead` above. The armed source and its in/out trim marks already
    /// live on `PhotonicApp::pending_source` (spec 16 §1) — this is the one
    /// new piece a source-monitor UI needs. `None` = nothing scrubbed yet.
    pub(crate) source_marks: &'a mut crate::app::source_marks::SourceMarksSession,
    pub(crate) source_audition: Option<photonic_video::source_audition::SourceAuditionStatus>,
    pub(crate) source_audition_error: Option<String>,
    pub(crate) source_monitor_scrub: &'a mut Option<Tick>,
    /// [multicam, 17 G-20] Angle currently cut to in the open multicam clip
    /// (Premiere's live 1-9 number-key cutting). `None` = no angle chosen.
    pub(crate) multicam_active_angle: &'a mut Option<u8>,
    /// [multicam, 17 G-20] Whether the central panel is showing the
    /// multicam angle grid instead of the program monitor — the multicam
    /// analogue of `node_canvas_active` above.
    pub(crate) multicam_view_open: &'a mut bool,
    /// [transcript, 17 G-18] Whether the text-based transcript editing
    /// panel is open.
    pub(crate) transcript_panel_open: &'a mut bool,
    /// [transcript, 17 G-18] Scroll offset (px) of the transcript panel's
    /// word list, preserved across frames like other scroll-position state.
    pub(crate) transcript_scroll: &'a mut f32,
    /// [seq_tabs, 17 G-17] Sequence ids pinned open as tabs in the timeline
    /// panel's tab strip, in display order. `TimelineProject::active_sequence`
    /// (document state) decides which tab is highlighted; this just tracks
    /// which stay open rather than closing whenever they lose focus.
    pub(crate) open_sequence_tabs: &'a mut Vec<SequenceId>,
    /// [seq_tabs, 17 G-16/G-17] Breadcrumb stack of sequence ids drilled
    /// into via nested-sequence navigation — empty when viewing a top-level
    /// sequence directly; each entry is the sequence the next one down was
    /// opened from (`ClipSource::NestedSequence`, 01 §1).
    pub(crate) nested_sequence_breadcrumbs: &'a mut Vec<SequenceId>,
}
