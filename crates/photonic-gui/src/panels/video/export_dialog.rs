//! Video export dialog (04 §4.1 / 05 §3) — a floating window (like the vector
//! `ExportDialog`, distinct from it) for picking a preset + reframe options and
//! launching an export. Interior owned by 05-import-export.md. Open state and
//! the last-used preset live on [`super::VideoPanelUi::export_dialog_open`] /
//! [`super::VideoPanelUi::last_export_preset`]; the menu/toolbar entry that sets
//! `export_dialog_open` is wired in `app/mod.rs`.
//!
//! `VideoPanelUi` (the skeleton's original arg) carries no `doc`/engine
//! handle — a real preset/format/range picker needs both, so the call site in
//! `app/mod.rs` (`if self.export_dialog_open { ... }`) is widened to pass
//! `doc`/`history` through and to receive back an `Option<ExportJob>`, which
//! it forwards to `EngineSession::send(EngineCmd::Export(..))` when an engine
//! is attached — the same "skeleton call site under-provisions args, widen it"
//! move already used for the timeline panel (`app/mod.rs`'s
//! `draw_timeline_panel` call, see that panel's own history). `EngineCmd::
//! Export` is a documented P3 stub (`photonic_video::session` — "surfaces on
//! `EngineStatus::last_error`"); sending it is still the real, forward-
//! compatible wiring point (02 §1's actual `ExportJob` contract), not a fake
//! one — this dialog does not reimplement the render loop itself.
//!
//! Preset CRUD/validation/format-checklist/range/pre-flight are all fully
//! real: `photonic_video::export::presets` (app-level, no engine needed) and
//! `Sequence::work_range`/`formats` (document state, read from `doc` and
//! written via the existing `ops_bridge::set_work_range`).

use egui::{Color32, RichText};
use photonic_core::document::Document;
use photonic_core::history::CommandHistory;
use photonic_core::timeline::{FrameRate, SequenceId, Tick, TICKS_PER_SECOND};
use photonic_video::export::presets::{
    self, AudioCodec, AudioEncodeSpec, Container, ExportPreset, FrameRatePolicy, QualityMode,
    ResolutionSpec, VideoCodec, VideoEncodeSpec,
};
use photonic_video::session::ExportProgressSnapshot;
use photonic_video::ExportJob;
use std::time::Duration;

use super::VideoPanelUi;

const MUTED: Color32 = Color32::from_rgb(0x7A, 0x7A, 0x9A); // `secondary`
const ACCENT: Color32 = Color32::from_rgb(0x6E, 0x56, 0xCF); // `primary`
const ERROR: Color32 = Color32::from_rgb(0xF8, 0x71, 0x71); // `error`
const ERROR_BG: Color32 = Color32::from_rgba_premultiplied(0x3A, 0x14, 0x14, 0x60);

fn state_id() -> egui::Id {
    egui::Id::new("video_export_dialog_state")
}

/// Shared egui-store key on which `app/monitor.rs` publishes the live
/// [`EngineStatus::export`](photonic_video::EngineStatus) snapshot each frame
/// for this floating dialog to read. The dialog is a separate window with no
/// engine handle of its own, so the monitor's per-frame status poll bridges
/// the two through egui's per-frame data store (05 §3.8 step 8).
pub(crate) fn export_status_id() -> egui::Id {
    egui::Id::new("video_export_status")
}

/// Shared egui-store key the dialog sets to `true` to request cancellation;
/// `app/monitor.rs` drains it and relays `EngineCmd::CancelExport` to the
/// engine (the dialog cannot reach the `EngineSession` directly).
pub(crate) fn export_cancel_id() -> egui::Id {
    egui::Id::new("video_export_cancel_req")
}

#[derive(Clone)]
struct DialogState {
    preset: ExportPreset,
    /// `Some(name)` while `preset` still matches a known preset exactly under
    /// that name; cleared (→ "Custom (based on X)") the moment a field is
    /// hand-edited (05 §3.8 step 2).
    picker_label: String,
    base_name: Option<String>,
    search: String,
    /// Per-`Sequence.formats` index checklist (05 §3.8 step 4).
    formats_checked: Vec<bool>,
    entire_sequence: bool,
    range_start_s: f64,
    range_end_s: f64,
    /// K-F2: one export job per ranged sequence marker (duration > 0).
    export_per_marker: bool,
    /// K-F4 job options (not part of the preset).
    use_proxies: bool,
    preview_resolution: bool,
    prefer_hardware: bool,
    encoder_speed: Option<String>,
    raw_encoder_args: String,
    burn_in_timecode: bool,
    add_to_bin: bool,
    two_pass: bool,
    inhibit_sleep: bool,
    save_as_name: String,
    job: Option<JobState>,
}

#[derive(Clone)]
enum JobState {
    /// The dialog sent `EngineCmd::Export`; per-frame progress is read live
    /// off the [`export_status_id`] blackboard each redraw. `output` is kept
    /// so the success view can show where the file landed (the snapshot does
    /// not carry the path).
    Submitted { output: std::path::PathBuf },
    /// The user pressed Cancel; `app/monitor.rs` has been asked (via
    /// [`export_cancel_id`]) to relay `EngineCmd::CancelExport`. Distinguishes
    /// a user cancel from a clean finish once the engine flips `done`.
    CancelRequested { output: std::path::PathBuf },
}

impl JobState {
    fn output(&self) -> &std::path::Path {
        match self {
            JobState::Submitted { output } | JobState::CancelRequested { output } => output,
        }
    }
}

/// The phase the export-progress UI is in, derived purely from the live
/// [`ExportProgressSnapshot`] plus whether the user asked to cancel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExportPhase {
    /// `EngineCmd::Export` sent, but the engine has not published a snapshot
    /// yet (or the blackboard was just cleared for a fresh launch).
    Submitted,
    /// A running snapshot (`!done && error.is_none()`).
    InProgress,
    /// `done && error.is_none()` after a clean finish.
    Done,
    /// `done` reached while a cancel was outstanding.
    Cancelled,
    /// `error.is_some()` — the encode failed.
    Failed,
}

/// Pure, egui-free view-model for [`draw_progress`], so the mapping is unit
/// testable without an `egui::Ui` (05 §3.8 step 8). `draw_progress` renders
/// exactly this.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExportView {
    pub phase: ExportPhase,
    /// Fractional progress `0.0..=1.0` when it is known; `None` when
    /// indeterminate (preparing / no frame total yet / terminal-by-text).
    pub percent: Option<f32>,
    pub frame: u64,
    pub total: u64,
    pub fps: f32,
    pub eta: Duration,
    /// The failure message on [`ExportPhase::Failed`], else `None`.
    pub error: Option<String>,
    /// Short human phase label ("Rendering", "Export complete", …).
    pub label: String,
    /// Whether a Cancel button should be live (only while genuinely running
    /// and not already requested).
    pub cancel_enabled: bool,
}

/// Map the live export snapshot (and whether cancel was requested) to the
/// [`ExportView`] the progress UI renders. Pure — the honest unit-test seam.
pub(crate) fn export_view_model(
    snapshot: Option<&ExportProgressSnapshot>,
    cancel_requested: bool,
) -> ExportView {
    match snapshot {
        None => ExportView {
            phase: ExportPhase::Submitted,
            percent: None,
            frame: 0,
            total: 0,
            fps: 0.0,
            eta: Duration::ZERO,
            error: None,
            label: "Starting export…".to_string(),
            cancel_enabled: !cancel_requested,
        },
        Some(s) if s.error.is_some() => ExportView {
            phase: ExportPhase::Failed,
            percent: None,
            frame: s.frame,
            total: s.total,
            fps: 0.0,
            eta: Duration::ZERO,
            error: s.error.clone(),
            label: "Export failed".to_string(),
            cancel_enabled: false,
        },
        Some(s) if s.done && cancel_requested => ExportView {
            phase: ExportPhase::Cancelled,
            percent: None,
            frame: s.frame,
            total: s.total,
            fps: 0.0,
            eta: Duration::ZERO,
            error: None,
            label: "Export cancelled".to_string(),
            cancel_enabled: false,
        },
        Some(s) if s.done => ExportView {
            phase: ExportPhase::Done,
            percent: Some(1.0),
            frame: s.frame,
            total: s.total,
            fps: 0.0,
            eta: Duration::ZERO,
            error: None,
            label: "Export complete".to_string(),
            cancel_enabled: false,
        },
        Some(s) => {
            let percent = (s.total > 0).then(|| (s.frame as f32 / s.total as f32).clamp(0.0, 1.0));
            let label = if cancel_requested {
                "Cancelling…"
            } else if s.total == 0 {
                "Preparing…"
            } else {
                "Rendering"
            };
            ExportView {
                phase: ExportPhase::InProgress,
                percent,
                frame: s.frame,
                total: s.total,
                fps: s.fps,
                eta: s.eta,
                error: None,
                label: label.to_string(),
                cancel_enabled: !cancel_requested,
            }
        }
    }
}

/// `mm:ss` ETA, or an em-dash when unknown (zero).
fn fmt_eta(eta: Duration) -> String {
    let secs = eta.as_secs();
    if secs == 0 {
        "—".to_string()
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

impl DialogState {
    fn seeded(doc: &Document, seq_id: Option<SequenceId>, last_preset_name: &str) -> Self {
        let preset = find_preset(last_preset_name)
            .or_else(|| find_preset("Web H.264"))
            .unwrap_or_else(|| presets::built_in_presets().remove(0));
        let (formats_checked, range_start_s, range_end_s) = seq_id
            .and_then(|id| doc.timeline.as_ref()?.sequences.get(&id))
            .map_or((vec![true], 0.0, 0.0), |seq| {
                let checked = (0..seq.formats.len().max(1))
                    .map(|i| i == seq.active_format)
                    .collect();
                let (s, e) = seq.work_range.unwrap_or((Tick::ZERO, seq.content_end()));
                (checked, s.as_seconds_f64(), e.as_seconds_f64())
            });
        DialogState {
            picker_label: preset.name.clone(),
            base_name: Some(preset.name.clone()),
            preset,
            search: String::new(),
            formats_checked,
            entire_sequence: false,
            range_start_s,
            range_end_s,
            export_per_marker: false,
            use_proxies: false,
            preview_resolution: false,
            prefer_hardware: false,
            encoder_speed: None,
            raw_encoder_args: String::new(),
            burn_in_timecode: false,
            add_to_bin: false,
            two_pass: false,
            inhibit_sleep: true,
            save_as_name: String::new(),
            job: None,
        }
    }
}

fn find_preset(name: &str) -> Option<ExportPreset> {
    presets::built_in_presets()
        .into_iter()
        .chain(presets::load_custom_presets().unwrap_or_default())
        .find(|p| p.name == name)
}

fn is_builtin(name: &str) -> bool {
    presets::built_in_presets().iter().any(|p| p.name == name)
}

/// §3.4's alpha-capable allow-list, mirrored here since `photonic_video::
/// export::presets::alpha_combo_allowed` is a private validation helper —
/// this is the same predicate the dialog needs to grey the toggle out
/// (`presets::validate` catches the same rule server-side as a backstop).
fn alpha_allowed(container: Container, codec: Option<VideoCodec>) -> bool {
    matches!(
        (container, codec),
        (Container::WebM, Some(VideoCodec::Vp9))
            | (Container::Mov, Some(VideoCodec::ProResLikeMezzanine))
            | (Container::Apng, Some(VideoCodec::Apng))
            | (Container::ImageSequence, Some(VideoCodec::Png))
    )
}

const CONTAINERS: [Container; 7] = [
    Container::Mp4,
    Container::Mov,
    Container::WebM,
    Container::Mkv,
    Container::Gif,
    Container::ImageSequence,
    Container::Apng,
];
const VIDEO_CODECS: [VideoCodec; 7] = [
    VideoCodec::H264,
    VideoCodec::Av1,
    VideoCodec::Vp9,
    VideoCodec::ProResLikeMezzanine,
    VideoCodec::Gif,
    VideoCodec::Png,
    VideoCodec::Apng,
];
const AUDIO_CODECS: [AudioCodec; 3] = [AudioCodec::Aac, AudioCodec::Opus, AudioCodec::Pcm];
const RATE_CHOICES: [FrameRate; 6] = [
    FrameRate::FPS_23_976,
    FrameRate::FPS_24,
    FrameRate::FPS_25,
    FrameRate::FPS_29_97,
    FrameRate::FPS_30,
    FrameRate::FPS_60,
];

fn container_label(c: Container) -> &'static str {
    match c {
        Container::Mp4 => "MP4",
        Container::Mov => "MOV",
        Container::WebM => "WebM",
        Container::Mkv => "MKV",
        Container::Gif => "GIF",
        Container::ImageSequence => "PNG Sequence",
        Container::Apng => "APNG",
    }
}
fn video_codec_label(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H264 => "H.264",
        VideoCodec::Av1 => "AV1",
        VideoCodec::Vp9 => "VP9",
        VideoCodec::ProResLikeMezzanine => "ProRes 4444",
        VideoCodec::Gif => "GIF (paletted)",
        VideoCodec::Png => "PNG",
        VideoCodec::Apng => "APNG",
    }
}
fn audio_codec_label(c: AudioCodec) -> &'static str {
    match c {
        AudioCodec::Aac => "AAC",
        AudioCodec::Opus => "Opus",
        AudioCodec::Pcm => "PCM",
    }
}
fn rate_label(r: FrameRate) -> String {
    format!("{:.3} fps", r.num as f64 / r.den.max(1) as f64)
}

/// Floating video export dialog, shown while
/// [`VideoPanelUi::export_dialog_open`] is set. Its own window close button
/// clears that flag. Returns one or more validated [`ExportJob`]s the moment
/// the user commits "Export" (single → engine; multi → [`RenderQueue`]).
pub(crate) fn draw_export_dialog(
    ctx: &egui::Context,
    vid: &mut VideoPanelUi,
    doc: &mut Document,
    history: &mut CommandHistory,
) -> Vec<ExportJob> {
    let Some(project) = doc.timeline.as_ref() else {
        *vid.export_dialog_open = false;
        return Vec::new();
    };
    let seq_id = project.active_sequence;
    let id = state_id();
    let mut state: DialogState = ctx
        .data(|d| d.get_temp::<DialogState>(id))
        .unwrap_or_else(|| DialogState::seeded(doc, seq_id, vid.last_export_preset));

    let mut launch: Vec<ExportJob> = Vec::new();
    let mut open = *vid.export_dialog_open;
    egui::Window::new("Export")
        .id(egui::Id::new("video_export_dialog_window"))
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .open(&mut open)
        .show(ctx, |ui| {
            let Some(seq_id) = seq_id else {
                ui.label(RichText::new("No active sequence.").color(MUTED));
                return;
            };
            if state.job.is_some() {
                draw_progress(ui, &mut state);
                return;
            }
            ui.columns(2, |cols| {
                draw_preset_picker(&mut cols[0], &mut state);
                draw_fields(&mut cols[1], &mut state);
            });
            ui.separator();
            draw_format_checklist(ui, doc, seq_id, &mut state);
            draw_range(ui, doc, history, seq_id, &mut state);
            ui.checkbox(
                &mut state.export_per_marker,
                "Export each ranged marker as a separate file (K-F2)",
            )
            .on_hover_text(
                "When checked, every marker with duration > 0 becomes its own \
                 export job (per selected format). Uses the shared render queue.",
            );
            draw_job_options(ui, &mut state);
            draw_estimate(ui, doc, seq_id, &state);
            let offline = preflight_offline_assets(doc, seq_id);
            if !offline.is_empty() {
                draw_preflight_banner(ui, &offline);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let can_export = offline.is_empty()
                    && presets::validate(&state.preset).is_ok()
                    && state.formats_checked.iter().any(|&c| c);
                if ui
                    .add_enabled(can_export, egui::Button::new("Export"))
                    .clicked()
                {
                    let jobs = build_export_jobs(doc, seq_id, &state);
                    if let Some(first) = jobs.first() {
                        *vid.last_export_preset = state.preset.name.clone();
                        state.job = Some(JobState::Submitted {
                            output: first.output.clone(),
                        });
                        // Clear any prior export's terminal snapshot so the
                        // progress view doesn't flash a stale "complete" before
                        // the engine publishes this job's first snapshot.
                        ctx.data_mut(|d| {
                            d.insert_temp(
                                export_status_id(),
                                Option::<ExportProgressSnapshot>::None,
                            )
                        });
                        launch = jobs;
                    }
                }
                if !can_export {
                    ui.label(
                        RichText::new("Fix the issues above to enable Export.")
                            .color(MUTED)
                            .small(),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.save_as_name)
                            .hint_text("Preset name…")
                            .desired_width(120.0),
                    );
                    let can_save = !state.save_as_name.trim().is_empty()
                        && !is_builtin(&state.save_as_name)
                        && presets::validate(&state.preset).is_ok();
                    if ui
                        .add_enabled(can_save, egui::Button::new("Save as preset…"))
                        .clicked()
                    {
                        let mut to_save = state.preset.clone();
                        to_save.name = state.save_as_name.trim().to_string();
                        if presets::validate(&to_save).is_ok() {
                            let mut customs = presets::load_custom_presets().unwrap_or_default();
                            customs.retain(|p| p.name != to_save.name);
                            customs.push(to_save.clone());
                            let _ = presets::save_custom_presets(&customs);
                            state.picker_label = to_save.name.clone();
                            state.base_name = Some(to_save.name.clone());
                            state.preset = to_save;
                            state.save_as_name.clear();
                        }
                    }
                });
            });
        });
    if !open {
        *vid.export_dialog_open = false;
    }
    ctx.data_mut(|d| d.insert_temp(id, state));
    launch
}

fn draw_progress(ui: &mut egui::Ui, state: &mut DialogState) {
    // 05 §3.8 step 8: non-modal progress with frame/total/fps/ETA/cancel, read
    // live off the engine's `EngineStatus::export` snapshot that `app/monitor.
    // rs` republishes onto the shared egui store each frame (this floating
    // window has no engine handle of its own). Never a blocking modal.
    let snapshot: Option<ExportProgressSnapshot> = ui
        .ctx()
        .data(|d| d.get_temp::<Option<ExportProgressSnapshot>>(export_status_id()))
        .flatten();
    let cancel_requested = matches!(state.job, Some(JobState::CancelRequested { .. }));
    let view = export_view_model(snapshot.as_ref(), cancel_requested);

    ui.label(RichText::new(view.label.as_str()).strong());

    match view.phase {
        ExportPhase::Submitted | ExportPhase::InProgress => {
            let bar = match view.percent {
                Some(p) => egui::ProgressBar::new(p).show_percentage(),
                None => egui::ProgressBar::new(0.0).animate(true),
            };
            ui.add(bar);
            ui.label(
                RichText::new(format!(
                    "Frame {}/{} · {:.1} fps · ETA {}",
                    view.frame,
                    view.total,
                    view.fps,
                    fmt_eta(view.eta)
                ))
                .color(MUTED)
                .small(),
            );
            // Keep animating while the export runs.
            ui.ctx().request_repaint();
        }
        ExportPhase::Done => {
            ui.add(egui::ProgressBar::new(1.0));
            ui.label(RichText::new("Export complete.").color(ACCENT));
            if let Some(job) = &state.job {
                ui.label(
                    RichText::new(format!("Saved to {}", job.output().display()))
                        .color(MUTED)
                        .small(),
                );
            }
        }
        ExportPhase::Cancelled => {
            ui.label(RichText::new("Export cancelled — no output written.").color(MUTED));
        }
        ExportPhase::Failed => {
            egui::Frame::none()
                .fill(ERROR_BG)
                .inner_margin(egui::Margin::same(6.0))
                .rounding(3.0)
                .show(ui, |ui| {
                    ui.colored_label(
                        ERROR,
                        view.error
                            .as_deref()
                            .unwrap_or("The export did not complete."),
                    );
                });
        }
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(view.cancel_enabled, egui::Button::new("Cancel"))
            .clicked()
        {
            // Preserve the output path across the phase change and ask the
            // monitor to relay `EngineCmd::CancelExport` to the engine.
            if let Some(JobState::Submitted { output }) = &state.job {
                let output = output.clone();
                state.job = Some(JobState::CancelRequested { output });
            }
            ui.ctx()
                .data_mut(|d| d.insert_temp(export_cancel_id(), true));
        }
        let terminal = matches!(
            view.phase,
            ExportPhase::Done | ExportPhase::Cancelled | ExportPhase::Failed
        );
        if terminal && ui.button("Close").clicked() {
            state.job = None;
        }
    });
}

fn draw_preset_picker(ui: &mut egui::Ui, state: &mut DialogState) {
    ui.label(RichText::new("Preset").strong());
    ui.add(
        egui::TextEdit::singleline(&mut state.search)
            .hint_text("Search presets…")
            .desired_width(180.0),
    );
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .show(ui, |ui| {
            let q = state.search.to_lowercase();
            ui.label(RichText::new("BUILT-IN").small().color(MUTED));
            for p in presets::built_in_presets() {
                if !q.is_empty() && !p.name.to_lowercase().contains(&q) {
                    continue;
                }
                let selected = state.base_name.as_deref() == Some(p.name.as_str());
                if ui
                    .selectable_label(selected, format!("\u{1F512} {}", p.name))
                    .clicked()
                {
                    state.picker_label = p.name.clone();
                    state.base_name = Some(p.name.clone());
                    state.preset = p;
                }
            }
            let customs = presets::load_custom_presets().unwrap_or_default();
            if !customs.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new("CUSTOM").small().color(MUTED));
                for p in customs {
                    if !q.is_empty() && !p.name.to_lowercase().contains(&q) {
                        continue;
                    }
                    let selected = state.base_name.as_deref() == Some(p.name.as_str());
                    ui.horizontal(|ui| {
                        if ui.selectable_label(selected, &p.name).clicked() {
                            state.picker_label = p.name.clone();
                            state.base_name = Some(p.name.clone());
                            state.preset = p.clone();
                        }
                        if ui
                            .small_button("\u{2715}")
                            .on_hover_text("Delete")
                            .clicked()
                        {
                            let mut remaining = presets::load_custom_presets().unwrap_or_default();
                            remaining.retain(|c| c.name != p.name);
                            let _ = presets::save_custom_presets(&remaining);
                        }
                    });
                }
            }
        });
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "Selected: {}",
            if state.base_name.as_deref() == Some(state.preset.name.as_str()) {
                state.picker_label.clone()
            } else {
                format!(
                    "Custom (based on {})",
                    state.base_name.as_deref().unwrap_or("—")
                )
            }
        ))
        .small(),
    );
}

/// Marks the picker "Custom (based on X)" the moment any field diverges from
/// the selected preset (05 §3.8 step 2) — called after every field edit.
fn note_customized(state: &mut DialogState) {
    if state.base_name.as_deref() != Some(state.preset.name.as_str()) {
        // Already showing "Custom (based on X)" — nothing to do, `base_name`
        // is the anchor and stays put until a different preset is picked.
    }
}

fn draw_fields(ui: &mut egui::Ui, state: &mut DialogState) {
    ui.label(RichText::new("Settings").strong());
    let p = &mut state.preset;
    let mut changed = false;

    egui::Grid::new("export_fields_grid")
        .num_columns(2)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            ui.label("Container");
            egui::ComboBox::new("export_container", "")
                .selected_text(container_label(p.container))
                .show_ui(ui, |ui| {
                    for c in CONTAINERS {
                        if ui
                            .selectable_value(&mut p.container, c, container_label(c))
                            .clicked()
                        {
                            changed = true;
                        }
                    }
                });
            ui.end_row();

            ui.label("Video");
            ui.horizontal(|ui| {
                let mut has_video = p.video.is_some();
                if ui.checkbox(&mut has_video, "").changed() {
                    p.video = if has_video {
                        Some(VideoEncodeSpec {
                            codec: VideoCodec::H264,
                            quality: QualityMode::Crf(20.0),
                        })
                    } else {
                        None
                    };
                    changed = true;
                }
                if let Some(v) = p.video.as_mut() {
                    egui::ComboBox::new("export_vcodec", "")
                        .selected_text(video_codec_label(v.codec))
                        .show_ui(ui, |ui| {
                            for c in VIDEO_CODECS {
                                if ui
                                    .selectable_value(&mut v.codec, c, video_codec_label(c))
                                    .clicked()
                                {
                                    changed = true;
                                }
                            }
                        });
                }
            });
            ui.end_row();

            if let Some(v) = p.video.as_mut() {
                ui.label("Quality");
                ui.horizontal(|ui| {
                    let mut is_crf = matches!(v.quality, QualityMode::Crf(_));
                    let mut is_lossless = matches!(v.quality, QualityMode::Lossless);
                    if ui.selectable_label(is_crf, "CRF").clicked() {
                        v.quality = QualityMode::Crf(20.0);
                        changed = true;
                    }
                    if ui
                        .selectable_label(!is_crf && !is_lossless, "Bitrate")
                        .clicked()
                    {
                        v.quality = QualityMode::Bitrate {
                            target_kbps: 6000,
                            max_kbps: 9000,
                        };
                        changed = true;
                    }
                    if ui.selectable_label(is_lossless, "Lossless").clicked() {
                        v.quality = QualityMode::Lossless;
                        changed = true;
                    }
                    is_crf = matches!(v.quality, QualityMode::Crf(_));
                    is_lossless = matches!(v.quality, QualityMode::Lossless);
                    match &mut v.quality {
                        QualityMode::Crf(f) => {
                            changed |= ui
                                .add(egui::DragValue::new(f).range(0.0..=51.0).speed(0.5))
                                .changed();
                        }
                        QualityMode::Bitrate {
                            target_kbps,
                            max_kbps,
                        } => {
                            changed |= ui
                                .add(
                                    egui::DragValue::new(target_kbps)
                                        .range(100..=100_000)
                                        .suffix(" kbps"),
                                )
                                .changed();
                            changed |= ui
                                .add(
                                    egui::DragValue::new(max_kbps)
                                        .range(100..=200_000)
                                        .suffix(" max"),
                                )
                                .changed();
                        }
                        QualityMode::Lossless => {}
                    }
                    let _ = (is_crf, is_lossless);
                });
                ui.end_row();
            }

            ui.label("Audio");
            ui.horizontal(|ui| {
                let mut has_audio = p.audio.is_some();
                if ui.checkbox(&mut has_audio, "").changed() {
                    p.audio = if has_audio {
                        Some(AudioEncodeSpec {
                            codec: AudioCodec::Aac,
                            bitrate_kbps: Some(128),
                        })
                    } else {
                        None
                    };
                    changed = true;
                }
                if let Some(a) = p.audio.as_mut() {
                    egui::ComboBox::new("export_acodec", "")
                        .selected_text(audio_codec_label(a.codec))
                        .show_ui(ui, |ui| {
                            for c in AUDIO_CODECS {
                                if ui
                                    .selectable_value(&mut a.codec, c, audio_codec_label(c))
                                    .clicked()
                                {
                                    changed = true;
                                }
                            }
                        });
                    if a.codec != AudioCodec::Pcm {
                        let mut kbps = a.bitrate_kbps.unwrap_or(128);
                        if ui
                            .add(
                                egui::DragValue::new(&mut kbps)
                                    .range(32..=512)
                                    .suffix(" kbps"),
                            )
                            .changed()
                        {
                            a.bitrate_kbps = Some(kbps);
                            changed = true;
                        }
                    }
                }
            });
            ui.end_row();

            ui.label("Resolution");
            ui.horizontal(|ui| {
                let mut is_source = matches!(p.resolution, ResolutionSpec::SourceFormat);
                let mut is_explicit = matches!(p.resolution, ResolutionSpec::Explicit { .. });
                if ui.selectable_label(is_source, "Source format").clicked() {
                    p.resolution = ResolutionSpec::SourceFormat;
                    changed = true;
                }
                if ui.selectable_label(is_explicit, "Explicit").clicked() {
                    p.resolution = ResolutionSpec::Explicit { w: 1920, h: 1080 };
                    changed = true;
                }
                if ui
                    .selectable_label(matches!(p.resolution, ResolutionSpec::Scale(_)), "Scale")
                    .clicked()
                {
                    p.resolution = ResolutionSpec::Scale(1.0);
                    changed = true;
                }
                is_source = matches!(p.resolution, ResolutionSpec::SourceFormat);
                is_explicit = matches!(p.resolution, ResolutionSpec::Explicit { .. });
                match &mut p.resolution {
                    ResolutionSpec::Explicit { w, h } => {
                        changed |= ui.add(egui::DragValue::new(w).range(1..=8192)).changed();
                        changed |= ui.add(egui::DragValue::new(h).range(1..=8192)).changed();
                    }
                    ResolutionSpec::Scale(s) => {
                        changed |= ui
                            .add(egui::DragValue::new(s).range(0.05..=4.0).speed(0.01))
                            .changed();
                    }
                    ResolutionSpec::SourceFormat => {}
                }
                let _ = (is_source, is_explicit);
            });
            ui.end_row();

            ui.label("Frame rate");
            ui.horizontal(|ui| {
                let mut is_match = matches!(p.frame_rate, FrameRatePolicy::MatchSequence);
                if ui.selectable_label(is_match, "Match sequence").clicked() {
                    p.frame_rate = FrameRatePolicy::MatchSequence;
                    changed = true;
                }
                if ui.selectable_label(!is_match, "Explicit").clicked() {
                    p.frame_rate = FrameRatePolicy::Explicit(FrameRate::FPS_30);
                    changed = true;
                }
                is_match = matches!(p.frame_rate, FrameRatePolicy::MatchSequence);
                if let FrameRatePolicy::Explicit(r) = &mut p.frame_rate {
                    egui::ComboBox::new("export_rate", "")
                        .selected_text(rate_label(*r))
                        .show_ui(ui, |ui| {
                            for choice in RATE_CHOICES {
                                if ui.selectable_value(r, choice, rate_label(choice)).clicked() {
                                    changed = true;
                                }
                            }
                        });
                }
                let _ = is_match;
            });
            ui.end_row();

            ui.label("Alpha");
            let allowed = alpha_allowed(p.container, p.video.as_ref().map(|v| v.codec));
            ui.add_enabled_ui(allowed, |ui| {
                let mut a = p.alpha && allowed;
                if ui.checkbox(&mut a, "").changed() {
                    p.alpha = a;
                    changed = true;
                }
            })
            .response
            .on_disabled_hover_text(
                "Alpha needs WebM+VP9, MOV+ProRes 4444, APNG, or a PNG sequence (05 §3.4)",
            );
            if !allowed {
                p.alpha = false;
            }
            ui.end_row();

            if matches!(p.container, Container::Mp4 | Container::Mov) {
                ui.label("Fast start");
                if ui.checkbox(&mut p.faststart, "").changed() {
                    changed = true;
                }
                ui.end_row();
            } else {
                p.faststart = false;
            }

            ui.label("Loudness target");
            egui::ComboBox::new("export_loudness", "")
                .selected_text(match p.loudness_target {
                    None => "Off",
                    Some(t) if (t.integrated_lufs + 14.0).abs() < 0.01 => "-14 LUFS (streaming)",
                    Some(t) if (t.integrated_lufs + 23.0).abs() < 0.01 => "-23 LUFS (broadcast)",
                    Some(_) => "Custom",
                })
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(p.loudness_target.is_none(), "Off")
                        .clicked()
                    {
                        p.loudness_target = None;
                        changed = true;
                    }
                    if ui.button("-14 LUFS (streaming)").clicked() {
                        p.loudness_target = Some(photonic_video::export::presets::LoudnessTarget {
                            integrated_lufs: -14.0,
                            true_peak_dbtp: -1.0,
                        });
                        changed = true;
                    }
                    if ui.button("-23 LUFS (broadcast)").clicked() {
                        p.loudness_target = Some(photonic_video::export::presets::LoudnessTarget {
                            integrated_lufs: -23.0,
                            true_peak_dbtp: -2.0,
                        });
                        changed = true;
                    }
                });
            ui.end_row();
        });

    if let Err(e) = presets::validate(p) {
        ui.label(RichText::new(format!("{e}")).color(ERROR).small());
    }
    if changed {
        note_customized(state);
    }
}

fn draw_format_checklist(
    ui: &mut egui::Ui,
    doc: &Document,
    seq_id: SequenceId,
    state: &mut DialogState,
) {
    let Some(seq) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq_id)) else {
        return;
    };
    if seq.formats.len() <= 1 {
        return;
    }
    if state.formats_checked.len() != seq.formats.len() {
        state.formats_checked = (0..seq.formats.len())
            .map(|i| i == seq.active_format)
            .collect();
    }
    ui.label(RichText::new("Export formats").strong());
    ui.horizontal_wrapped(|ui| {
        for (i, fmt) in seq.formats.iter().enumerate() {
            ui.checkbox(
                &mut state.formats_checked[i],
                format!("{} ({}×{})", fmt.name, fmt.width, fmt.height),
            );
        }
    });
}

fn draw_range(
    ui: &mut egui::Ui,
    doc: &mut Document,
    history: &mut CommandHistory,
    seq_id: SequenceId,
    state: &mut DialogState,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Range").strong());
        if ui
            .selectable_label(!state.entire_sequence, "Work range")
            .clicked()
        {
            state.entire_sequence = false;
        }
        if ui
            .selectable_label(state.entire_sequence, "Entire sequence")
            .clicked()
        {
            state.entire_sequence = true;
            if let Some(seq) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq_id)) {
                state.range_start_s = 0.0;
                state.range_end_s = seq.content_end().as_seconds_f64();
            }
        }
        if !state.entire_sequence {
            let s0 = ui.add(
                egui::DragValue::new(&mut state.range_start_s)
                    .speed(0.1)
                    .suffix("s"),
            );
            let s1 = ui.add(
                egui::DragValue::new(&mut state.range_end_s)
                    .speed(0.1)
                    .suffix("s"),
            );
            if s0.changed() || s1.changed() {
                if state.range_end_s < state.range_start_s {
                    state.range_end_s = state.range_start_s;
                }
                let range = Some((
                    Tick((state.range_start_s * TICKS_PER_SECOND as f64) as i64),
                    Tick((state.range_end_s * TICKS_PER_SECOND as f64) as i64),
                ));
                crate::app::timeline::ops_bridge::set_work_range(doc, history, seq_id, range);
            }
        }
    });
}

/// Approximate size/time (05 §3.8 step 6 — "expectation-setting... not a hard
/// promise"). Deliberately a rough heuristic, not 02 §8's perf-budget table.
fn draw_estimate(ui: &mut egui::Ui, doc: &Document, seq_id: SequenceId, state: &DialogState) {
    let Some(seq) = doc.timeline.as_ref().and_then(|p| p.sequences.get(&seq_id)) else {
        return;
    };
    let duration_s = if state.entire_sequence {
        seq.content_end().as_seconds_f64()
    } else {
        (state.range_end_s - state.range_start_s).max(0.0)
    };
    let video_kbps = match state.preset.video.as_ref().map(|v| &v.quality) {
        Some(QualityMode::Bitrate { target_kbps, .. }) => *target_kbps as f64,
        Some(QualityMode::Crf(crf)) => (12_000.0 - *crf as f64 * 180.0).max(500.0),
        Some(QualityMode::Lossless) => 40_000.0,
        None => 0.0,
    };
    let audio_kbps = state
        .preset
        .audio
        .as_ref()
        .and_then(|a| a.bitrate_kbps)
        .unwrap_or(0) as f64;
    let mb = duration_s * (video_kbps + audio_kbps) / 8.0 / 1024.0;
    ui.label(
        RichText::new(format!(
            "~{:.0} MB, ~{:.0}s render (rough estimate, not a promise)",
            mb.max(0.0),
            duration_s.max(0.0)
        ))
        .color(MUTED)
        .small(),
    );
}

/// Pre-flight offline-media check (05 §3.8 step 7, §8 risk row): walk every
/// clip's referenced asset in the sequence and flag unreachable ones before
/// the Export button is enabled.
fn preflight_offline_assets(doc: &Document, seq_id: SequenceId) -> Vec<String> {
    let Some(project) = doc.timeline.as_ref() else {
        return Vec::new();
    };
    let Some(seq) = project.sequences.get(&seq_id) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for track in seq.tracks() {
        for clip in &track.clips {
            let Some(asset_id) = clip.source.asset() else {
                continue;
            };
            if let Some(asset) = project.media.assets.get(&asset_id) {
                if crate::panels::media_pool::asset_is_offline(asset) {
                    names.push(clip.name.clone());
                }
            }
        }
    }
    names
}

fn draw_preflight_banner(ui: &mut egui::Ui, offline: &[String]) {
    // The one named "fill a row" exception in DESIGN.md's Do's/Don'ts: a
    // blocking pre-flight failure must not be miss-able (13 §13.5).
    egui::Frame::none()
        .fill(ERROR_BG)
        .inner_margin(egui::Margin::same(6.0))
        .rounding(3.0)
        .show(ui, |ui| {
            ui.colored_label(
                ERROR,
                format!(
                    "Offline media blocks export — {} clip(s): {}",
                    offline.len(),
                    offline.join(", ")
                ),
            );
            ui.label(
                RichText::new("Relink from the Media Pool drawer, then retry.")
                    .color(MUTED)
                    .small(),
            );
        });
}

/// K-F4 job-level options (not preset fields) + K-F5 hardware preference.
fn draw_job_options(ui: &mut egui::Ui, state: &mut DialogState) {
    ui.collapsing("Job options (K-F4 / K-F5)", |ui| {
        ui.checkbox(&mut state.use_proxies, "Use proxies when available")
            .on_hover_text("Fast verification render via ProxyMode::ForceProxy.");
        ui.checkbox(
            &mut state.preview_resolution,
            "Render at preview (Draft) resolution",
        )
        .on_hover_text("Caps the long edge at the Draft preview size.");
        ui.checkbox(
            &mut state.prefer_hardware,
            "Prefer hardware encoder (fail-closed)",
        )
        .on_hover_text(
            "Uses a probed HW encoder (NVENC/VAAPI/VideoToolbox/QSV) when \
             available for this codec. Errors if none is present — never \
             silently falls back (K-F5 / 23 §10.3).",
        );
        ui.checkbox(
            &mut state.burn_in_timecode,
            "Burn-in timecode / frame number",
        )
        .on_hover_text("Overlays sequence timecode on the encode path (K-F polish).");
        ui.checkbox(&mut state.add_to_bin, "Add result to media bin when done")
            .on_hover_text("Host imports the finished file into the media pool (K-F polish).");
        state.two_pass = false;
        ui.add_enabled(
            false,
            egui::Checkbox::new(&mut state.two_pass, "Two-pass encode (unavailable)"),
        )
        .on_disabled_hover_text("The streaming export engine does not support two-pass encoding.");
        ui.checkbox(
            &mut state.inhibit_sleep,
            "Inhibit system sleep during render",
        )
        .on_hover_text("Best-effort: keeps the machine awake while the job runs.");
        ui.horizontal(|ui| {
            ui.label("Encoder speed:");
            let mut speed = state.encoder_speed.clone().unwrap_or_default();
            if ui
                .add(
                    egui::TextEdit::singleline(&mut speed)
                        .hint_text("e.g. veryfast")
                        .desired_width(100.0),
                )
                .changed()
            {
                state.encoder_speed = if speed.trim().is_empty() {
                    None
                } else {
                    Some(speed.trim().to_string())
                };
            }
        });
        ui.label(
            RichText::new("Raw encoder args (key=value …)")
                .small()
                .color(MUTED),
        );
        ui.add(
            egui::TextEdit::singleline(&mut state.raw_encoder_args)
                .hint_text("x265-params keyint=60 …")
                .desired_width(f32::INFINITY),
        );
    });
}

/// Build one job per selected format × (range OR each ranged marker) (K-F1/F2).
fn build_export_jobs(doc: &Document, seq_id: SequenceId, state: &DialogState) -> Vec<ExportJob> {
    let Some(project) = doc.timeline.as_ref() else {
        return Vec::new();
    };
    let Some(seq) = project.sequences.get(&seq_id) else {
        return Vec::new();
    };
    let format_indices: Vec<usize> = state
        .formats_checked
        .iter()
        .enumerate()
        .filter_map(|(i, &c)| c.then_some(i))
        .collect();
    let format_indices = if format_indices.is_empty() {
        vec![seq.active_format]
    } else {
        format_indices
    };

    // Ranges: either the dialog range, or one per ranged sequence marker (K-F2).
    let ranges: Vec<(String, Option<(Tick, Tick)>)> = if state.export_per_marker {
        let mut segs: Vec<_> = seq
            .markers
            .iter()
            .filter(|m| m.duration.0 > 0)
            .map(|m| {
                let safe = m
                    .name
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect::<String>();
                (safe, Some((m.at, m.end())))
            })
            .collect();
        if segs.is_empty() {
            // No ranged markers — fall back to the dialog range so Export
            // still does something useful.
            segs.push(("full".into(), dialog_range(state)));
        }
        segs
    } else {
        vec![("export".into(), dialog_range(state))]
    };

    let mut jobs = Vec::new();
    for &format_index in &format_indices {
        let fmt_tag = seq
            .formats
            .get(format_index)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| format!("f{format_index}"));
        for (seg_tag, range) in &ranges {
            let base = format!(
                "{}_{}_{seg_tag}",
                seq.name.replace(' ', "_"),
                fmt_tag.replace(' ', "_")
            );
            jobs.push(ExportJob {
                sequence: seq_id,
                format_index,
                preset: state.preset.clone(),
                output: default_output_path_tagged(&state.preset, &base),
                range: *range,
                options: photonic_video::session::RenderJobOptions {
                    use_proxies: state.use_proxies,
                    preview_resolution: state.preview_resolution,
                    prefer_hardware: state.prefer_hardware,
                    raw_encoder_args: state
                        .raw_encoder_args
                        .split_whitespace()
                        .map(str::to_string)
                        .collect(),
                    encoder_speed: state.encoder_speed.clone(),
                    burn_in_timecode: state.burn_in_timecode,
                    add_to_bin: state.add_to_bin,
                    two_pass: state.two_pass,
                    inhibit_sleep: state.inhibit_sleep,
                },
            });
        }
    }
    jobs
}

fn dialog_range(state: &DialogState) -> Option<(Tick, Tick)> {
    if state.entire_sequence {
        None
    } else {
        Some((
            Tick((state.range_start_s * TICKS_PER_SECOND as f64) as i64),
            Tick((state.range_end_s * TICKS_PER_SECOND as f64) as i64),
        ))
    }
}

fn default_output_path_tagged(preset: &ExportPreset, base: &str) -> std::path::PathBuf {
    let ext = match preset.container {
        Container::Mp4 => "mp4",
        Container::Mov => "mov",
        Container::WebM => "webm",
        Container::Mkv => "mkv",
        Container::Gif => "gif",
        Container::ImageSequence => "%05d.png",
        Container::Apng => "apng.png",
    };
    std::env::temp_dir().join(format!("{base}_{}.{ext}", preset.name.replace(' ', "_")))
}

fn default_output_path(preset: &ExportPreset, seq_name: &str) -> std::path::PathBuf {
    let ext = match preset.container {
        Container::Mp4 => "mp4",
        Container::Mov => "mov",
        Container::WebM => "webm",
        Container::Mkv => "mkv",
        Container::Gif => "gif",
        Container::ImageSequence => "%05d.png",
        Container::Apng => "apng.png",
    };
    std::env::temp_dir().join(format!(
        "{seq_name}_{}.{ext}",
        preset.name.replace(' ', "_")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_allowed_matches_the_documented_allow_list() {
        assert!(alpha_allowed(Container::WebM, Some(VideoCodec::Vp9)));
        assert!(alpha_allowed(
            Container::Mov,
            Some(VideoCodec::ProResLikeMezzanine)
        ));
        assert!(alpha_allowed(Container::Apng, Some(VideoCodec::Apng)));
        assert!(alpha_allowed(
            Container::ImageSequence,
            Some(VideoCodec::Png)
        ));
        assert!(!alpha_allowed(Container::Mp4, Some(VideoCodec::H264)));
        assert!(!alpha_allowed(Container::WebM, None));
    }

    #[test]
    fn build_export_jobs_emits_one_per_checked_format() {
        let mut doc = Document::new("t", 100.0, 100.0);
        let mut project = photonic_core::timeline::TimelineProject::new();
        let mut seq = photonic_core::timeline::Sequence::new("Seq", FrameRate::FPS_30, 1920, 1080);
        // Second format so multi-format has something to expand.
        seq.formats
            .push(photonic_core::timeline::SequenceFormat::new(
                "9:16", 1080, 1920,
            ));
        let seq_id = seq.id;
        project.insert_sequence(seq);
        doc.timeline = Some(project);

        let mut state = DialogState::seeded(&doc, Some(seq_id), "Web H.264");
        state.formats_checked = vec![true, true];
        state.entire_sequence = true;
        state.export_per_marker = false;
        let jobs = build_export_jobs(&doc, seq_id, &state);
        assert_eq!(jobs.len(), 2, "one job per checked format");
        assert_ne!(jobs[0].format_index, jobs[1].format_index);
    }

    #[test]
    fn build_export_jobs_marker_mode_fans_out_ranged_markers() {
        use photonic_core::timeline::Marker;
        let mut doc = Document::new("t", 100.0, 100.0);
        let mut project = photonic_core::timeline::TimelineProject::new();
        let mut seq = photonic_core::timeline::Sequence::new("Seq", FrameRate::FPS_30, 640, 360);
        let mut m1 = Marker::new(Tick(0), "Intro");
        m1.duration = Tick(TICKS_PER_SECOND);
        let mut m2 = Marker::new(Tick(TICKS_PER_SECOND * 2), "Outro");
        m2.duration = Tick(TICKS_PER_SECOND);
        // Point marker (duration 0) must be skipped.
        seq.markers
            .push(Marker::new(Tick(TICKS_PER_SECOND), "Point"));
        seq.markers.push(m1);
        seq.markers.push(m2);
        let seq_id = seq.id;
        project.insert_sequence(seq);
        doc.timeline = Some(project);

        let mut state = DialogState::seeded(&doc, Some(seq_id), "Web H.264");
        state.formats_checked = vec![true];
        state.export_per_marker = true;
        let jobs = build_export_jobs(&doc, seq_id, &state);
        assert_eq!(jobs.len(), 2, "two ranged markers → two jobs");
        assert!(jobs.iter().all(|j| j.range.is_some()));
    }

    #[test]
    fn default_output_path_uses_container_extension() {
        let p = presets::built_in_presets()
            .into_iter()
            .find(|p| p.name == "GIF")
            .unwrap();
        let out = default_output_path(&p, "Sequence 1");
        assert!(out.to_string_lossy().ends_with(".gif"));
    }

    fn snap(frame: u64, total: u64, done: bool, error: Option<&str>) -> ExportProgressSnapshot {
        ExportProgressSnapshot {
            job: 1,
            frame,
            total,
            fps: 24.0,
            eta: Duration::from_secs(75),
            done,
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn view_model_submitted_when_no_snapshot_yet() {
        let v = export_view_model(None, false);
        assert_eq!(v.phase, ExportPhase::Submitted);
        assert_eq!(v.percent, None);
        assert!(v.cancel_enabled);
    }

    #[test]
    fn view_model_in_progress_maps_percent_and_enables_cancel() {
        let s = snap(30, 120, false, None);
        let v = export_view_model(Some(&s), false);
        assert_eq!(v.phase, ExportPhase::InProgress);
        assert_eq!(v.percent, Some(0.25));
        assert_eq!(v.frame, 30);
        assert_eq!(v.total, 120);
        assert_eq!(v.fps, 24.0);
        assert!(v.cancel_enabled);
        assert_eq!(v.label, "Rendering");
    }

    #[test]
    fn view_model_preparing_when_total_unknown() {
        let s = snap(0, 0, false, None);
        let v = export_view_model(Some(&s), false);
        assert_eq!(v.phase, ExportPhase::InProgress);
        assert_eq!(v.percent, None); // indeterminate until a frame total exists
        assert_eq!(v.label, "Preparing…");
    }

    #[test]
    fn view_model_done_is_full_and_disables_cancel() {
        let s = snap(120, 120, true, None);
        let v = export_view_model(Some(&s), false);
        assert_eq!(v.phase, ExportPhase::Done);
        assert_eq!(v.percent, Some(1.0));
        assert!(!v.cancel_enabled);
    }

    #[test]
    fn view_model_error_takes_priority_and_carries_message() {
        // Even a `done` snapshot with an error is a failure, not a success.
        let s = snap(40, 120, true, Some("encoder exited 1"));
        let v = export_view_model(Some(&s), false);
        assert_eq!(v.phase, ExportPhase::Failed);
        assert_eq!(v.error.as_deref(), Some("encoder exited 1"));
        assert!(!v.cancel_enabled);
    }

    #[test]
    fn view_model_cancelled_distinguished_from_clean_done() {
        // A clean `done` (no error) reached while cancel was requested reads as
        // Cancelled, not Done — the one distinction the snapshot can't make on
        // its own (cancel and success both land `done && error.is_none()`).
        let s = snap(40, 120, true, None);
        assert_eq!(export_view_model(Some(&s), false).phase, ExportPhase::Done);
        assert_eq!(
            export_view_model(Some(&s), true).phase,
            ExportPhase::Cancelled
        );
    }

    #[test]
    fn view_model_cancel_pending_relabels_and_locks_button() {
        let s = snap(40, 120, false, None);
        let v = export_view_model(Some(&s), true);
        assert_eq!(v.phase, ExportPhase::InProgress);
        assert_eq!(v.label, "Cancelling…");
        assert!(!v.cancel_enabled); // already requested — no double-send
    }

    #[test]
    fn eta_formats_as_mm_ss_or_dash() {
        assert_eq!(fmt_eta(Duration::from_secs(75)), "1:15");
        assert_eq!(fmt_eta(Duration::from_secs(9)), "0:09");
        assert_eq!(fmt_eta(Duration::ZERO), "—");
    }
}
