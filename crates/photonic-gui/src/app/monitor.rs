//! Video-mode program monitor + transport (video-editor-module
//! `04-ui-mode-timeline.md` §1.2, §1.3, §3). Wired to the real engine: when
//! `self.engine` is `Some`, the monitor presents the latest `EngineFrame`
//! (03 §5, presented into an egui native texture by
//! `app/engine.rs::EngineBridge::present_latest` each host frame) and the
//! transport reconciles GUI intent into `EngineCmd`s
//! (`drive_engine_playback`). Also owns mode entry/exit (lazy project
//! creation, §1.3) and the first-run discoverability hints (§1.2).
//!
//! Engine-less hosts (unit tests, GPU-free machines) keep the original
//! wall-clock placeholder playback so the transport/scrub UX still works.

use super::*;
use crate::app::engine;
use crate::app::timeline::ops_bridge;
use crate::commands;
use photonic_core::timeline::{FrameRate, Sequence, SequenceFormat, TICKS_PER_SECOND};
use photonic_video::{PreviewQuality, PreviewTarget, ProxyMode};

// ── Monitor toolbar polish: playback resolution + image zoom (14 §M-5) ─────
//
// Both selectors are GUI-only state — there is no `PhotonicApp` field for
// either (this story's territory is this file alone), so they live in egui's
// per-`Id` temp storage (`Context::data`/`data_mut`), the same mechanism
// already used for other widget-local state in this crate (e.g.
// `color_popup.rs`, `panels/modify.rs`). Written and read every frame, so it
// persists for the life of the session like a normal field would.

/// Interactive preview quality levels (24-preview-media-load §4).
/// `Draft` is the default — proxy when Auto + long edge ≤ 960. `Full` uses
/// sequence size + original media. `Proxy` forces proxy media when ready.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PlaybackResolution {
    Draft,
    Full,
    Proxy,
}

impl PlaybackResolution {
    const ALL: [PlaybackResolution; 3] = [
        PlaybackResolution::Draft,
        PlaybackResolution::Full,
        PlaybackResolution::Proxy,
    ];

    fn label(self) -> &'static str {
        match self {
            PlaybackResolution::Draft => "Draft",
            PlaybackResolution::Full => "Full",
            PlaybackResolution::Proxy => "Proxy",
        }
    }

    fn to_proxy_mode(self) -> ProxyMode {
        match self {
            PlaybackResolution::Draft => ProxyMode::Auto,
            PlaybackResolution::Full => ProxyMode::ForceOriginal,
            PlaybackResolution::Proxy => ProxyMode::ForceProxy,
        }
    }

    fn to_preview_quality(self) -> PreviewQuality {
        match self {
            PlaybackResolution::Draft | PlaybackResolution::Proxy => PreviewQuality::Draft,
            PlaybackResolution::Full => PreviewQuality::Full,
        }
    }
}

/// GUI-only monitor image zoom. `Fit` (default) letterboxes the frame into
/// the available content area — the pre-existing behavior. `Actual` shows it
/// at native pixels and is draggable (`pan`) when it overflows the content
/// area.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
struct MonitorZoom {
    mode: ImageZoomMode,
    /// Drag offset from center. Only meaningful in `Actual` mode; reset to
    /// zero whenever the mode switches back to `Fit`.
    pan: egui::Vec2,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
enum ImageZoomMode {
    #[default]
    Fit,
    Actual,
}

const MONITOR_ZOOM_STATE_ID: &str = "photonic.video_monitor.zoom_state";
const MONITOR_RESOLUTION_STATE_ID: &str = "photonic.video_monitor.playback_resolution";

impl PhotonicApp {
    fn monitor_zoom_state(&self, ctx: &egui::Context) -> MonitorZoom {
        ctx.data(|d| d.get_temp(egui::Id::new(MONITOR_ZOOM_STATE_ID)))
            .unwrap_or_default()
    }

    fn set_monitor_zoom_state(&self, ctx: &egui::Context, zoom: MonitorZoom) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(MONITOR_ZOOM_STATE_ID), zoom));
    }

    fn monitor_playback_resolution(&self, ctx: &egui::Context) -> PlaybackResolution {
        if let Some(engine) = &self.engine {
            if engine.preview_quality == PreviewQuality::Full {
                return PlaybackResolution::Full;
            }
            return if engine.proxy_mode == ProxyMode::ForceProxy {
                PlaybackResolution::Proxy
            } else {
                PlaybackResolution::Draft
            };
        }
        ctx.data(|d| d.get_temp(egui::Id::new(MONITOR_RESOLUTION_STATE_ID)))
            .unwrap_or(PlaybackResolution::Draft)
    }

    /// Persist the chosen resolution and — when an engine is attached —
    /// apply proxy mode + PreviewQuality (24 §4) via the bridge.
    fn set_monitor_playback_resolution(&mut self, ctx: &egui::Context, res: PlaybackResolution) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(MONITOR_RESOLUTION_STATE_ID), res));
        if let Some(bridge) = self.engine.as_mut() {
            bridge.proxy_mode = res.to_proxy_mode();
            bridge.preview_quality = res.to_preview_quality();
        }
    }
}

fn monitor_readiness_text(doc: &Document, status: &photonic_video::EngineStatus) -> String {
    let assets: Vec<_> = match status.preview_target {
        PreviewTarget::Asset { asset, .. } => vec![asset],
        PreviewTarget::Sequence { sequence } => doc
            .timeline
            .as_ref()
            .and_then(|p| p.sequences.get(&sequence))
            .map(|s| {
                s.tracks()
                    .flat_map(|t| &t.clips)
                    .filter(|c| c.start <= status.playhead && status.playhead < c.end())
                    .filter_map(|c| match c.source {
                        photonic_core::timeline::ClipSource::Asset { asset } => Some(asset),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    let mut ready = 0;
    let mut pending = 0;
    let mut failed = 0;
    for id in assets {
        if let Some(proxy) = doc
            .timeline
            .as_ref()
            .and_then(|p| p.media.assets.get(&id))
            .and_then(|m| m.proxy.as_ref())
        {
            match proxy.status {
                photonic_core::timeline::ProxyStatus::Ready => ready += 1,
                photonic_core::timeline::ProxyStatus::Pending => pending += 1,
                photonic_core::timeline::ProxyStatus::Failed => failed += 1,
            }
        }
    }
    let mut label = format!(
        "{:?} · sources {}/{} · proxies {} ready",
        status.preview_quality, status.readiness.ready, status.readiness.requested, ready
    );
    if pending > 0 {
        label.push_str(&format!(", {pending} building"));
    }
    if failed > 0 || status.readiness.failed_sources > 0 {
        label.push_str(&format!(
            " · {} source/proxy failures",
            failed + status.readiness.failed_sources
        ));
    }
    if status.readiness.pending_source_builds > 0 || status.readiness.pending_rasters > 0 {
        label.push_str(" · preparing media");
    }
    if status.readiness.capacity_limited || status.memory.pressure {
        label.push_str(" · preview capacity reached");
    }
    label
}

// ── Master output meter (NLE-parity Gap G-4) ────────────────────────────────
//
// A slim stereo (L/R) peak+RMS column with a clip LED, reserved on the right
// edge of the monitor's content area between the letterboxed image and the
// panel edge (13 §11.1/§11.4 widget shape). Fed by
// `engine::EngineBridge::master_level` — the real
// `photonic_video::audio::Mixer::output_meter()` tap once that method's
// documented seam closes; until then it honestly settles at the silence
// floor rather than animating a fabricated level (see that method's doc).
//
// The peak/RMS ballistics model (fast peak attack, ~40dB/s release, ~300ms
// RMS one-pole, 1.2s peak-hold dwell, latched clip LED) and the green→amber→
// red fill gradient are the same shapes `panels/video/audio_mixer.rs`'s
// `MeterState`/`StereoMeterState`/`meter_color` use for the mixer drawer's
// own master strip (same DESIGN.md-named filled-meter exception, same hex
// values, so both meters read as one instrument) — duplicated here rather
// than shared because this story's territory is `app/{monitor.rs,engine.rs}`
// only; hoisting both into one widget module is a follow-up.

/// Meter floor (09 §2's `-inf` mute floor).
const MASTER_METER_FLOOR_DB: f32 = -60.0;
/// Meter ceiling (09 §2's `+12`, rounded down to match the mixer's headroom).
const MASTER_METER_CEIL_DB: f32 = 12.0;
/// Clip-LED latch threshold (13 §11.1: above -0.3 dBTP).
const MASTER_METER_CLIP_DB: f32 = -0.3;
const MASTER_METER_BAR_W: f32 = 7.0;
const MASTER_METER_BAR_GAP: f32 = 3.0;
/// Total width reserved for the column (both bars + gap + a little breathing
/// room either side) — kept slim, per Gap G-4.
const MASTER_METER_COL_W: f32 = 24.0;
/// Gap between the letterboxed image and the meter column.
const MASTER_METER_MARGIN: f32 = 8.0;

// Meter three-stop gradient — the DESIGN.md-named exception (video module,
// filled meter bars): success(low) → warning(near-clip) → error(clip). Same
// hex values as `panels/video/audio_mixer.rs`'s gradient.
const MASTER_METER_LOW: egui::Color32 = egui::Color32::from_rgb(100, 200, 122); // #64C87A
const MASTER_METER_MID: egui::Color32 = egui::Color32::from_rgb(251, 191, 36); // #FBBF24
const MASTER_METER_HIGH: egui::Color32 = egui::Color32::from_rgb(248, 113, 113); // #F87171

const MASTER_METER_STATE_ID: &str = "photonic.video_monitor.master_meter_state";

/// egui temp-storage key for the "user is holding a scrub drag" flag, shared
/// between `drive_playback` (which sets it) and `draw_master_meter` (which
/// reads it to decide whether the monitor is still animating). Kept as one
/// const so the two call sites can never drift apart.
const MONITOR_SCRUB_DRAGGING_ID: &str = "monitor_scrub_dragging";

/// How far above the meter's silence floor (dB) a channel must still be for its
/// ballistics to count as "moving". Once every value has settled to within this
/// of the floor the meter is visually static and needs no further repaints.
const MASTER_METER_ANIM_EPS_DB: f32 = 0.05;

/// Fraction 0..1 of a dB value across the meter's floor..ceiling range.
fn master_meter_db_to_frac(db: f32) -> f32 {
    ((db - MASTER_METER_FLOOR_DB) / (MASTER_METER_CEIL_DB - MASTER_METER_FLOOR_DB)).clamp(0.0, 1.0)
}

fn master_meter_lerp(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Fill color at fill `frac` — knee at ~0.72 so the bar reads green through
/// nominal, amber approaching clip, red at the top.
fn master_meter_color(frac: f32) -> egui::Color32 {
    let f = frac.clamp(0.0, 1.0);
    if f < 0.72 {
        master_meter_lerp(MASTER_METER_LOW, MASTER_METER_MID, f / 0.72)
    } else {
        master_meter_lerp(MASTER_METER_MID, MASTER_METER_HIGH, (f - 0.72) / 0.28)
    }
}

/// Linear amplitude → dB, floored so a near-silent channel doesn't produce
/// `-inf`/NaN through `log10` (mirrors `panels/video/audio_mixer.rs::lin_to_db`).
fn master_meter_lin_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-6).log10()
}

fn fmt_master_db(db: f32) -> String {
    if db <= MASTER_METER_FLOOR_DB + 0.05 {
        "-inf".to_string()
    } else {
        format!("{db:+.1} dB")
    }
}

/// Peak/RMS ballistics for one channel of the master meter.
#[derive(Copy, Clone, Debug, PartialEq)]
struct MasterMeterChannel {
    peak_db: f32,
    rms_db: f32,
    hold_db: f32,
    hold_age: f32,
    /// Latched red until reset by a click (13 §11.4).
    clip: bool,
}

impl Default for MasterMeterChannel {
    fn default() -> Self {
        MasterMeterChannel {
            peak_db: MASTER_METER_FLOOR_DB,
            rms_db: MASTER_METER_FLOOR_DB,
            hold_db: MASTER_METER_FLOOR_DB,
            hold_age: 0.0,
            clip: false,
        }
    }
}

impl MasterMeterChannel {
    /// `peak_in_db`/`rms_in_db` are the tap's own per-block peak/RMS (or the
    /// floor, absent a tap); ballistics here only smooth *presentation*
    /// motion (attack/release, hold dwell) on top of them.
    fn update(&mut self, peak_in_db: f32, rms_in_db: f32, dt: f32) {
        let dt = dt.clamp(0.0, 0.1);
        // Peak: instantaneous attack, ~40 dB/s release (fast VU-like ballistics).
        if peak_in_db >= self.peak_db {
            self.peak_db = peak_in_db;
        } else {
            self.peak_db = (self.peak_db - 40.0 * dt).max(peak_in_db);
        }
        // RMS: one-pole toward the tap's RMS, ~300 ms window.
        let a = 1.0 - (-dt / 0.3).exp();
        self.rms_db += (rms_in_db - self.rms_db) * a;
        // Peak-hold marker: instant raise, 1.2s dwell, then fall to the peak.
        if self.peak_db >= self.hold_db {
            self.hold_db = self.peak_db;
            self.hold_age = 0.0;
        } else {
            self.hold_age += dt;
            if self.hold_age > 1.2 {
                self.hold_db = (self.hold_db - 20.0 * dt).max(self.peak_db);
            }
        }
        if peak_in_db > MASTER_METER_CLIP_DB {
            self.clip = true;
        }
    }

    /// True while any of this channel's ballistics are still above the silence
    /// floor — i.e. the peak/RMS is still releasing or the peak-hold marker is
    /// still dwelling/falling, so the bar is visibly moving and wants another
    /// frame. The latched clip LED is deliberately excluded: it's a static
    /// indicator, not an animation, so a cleared-but-latched clip must not by
    /// itself pin the monitor at 60fps.
    fn is_animating(&self) -> bool {
        let floor = MASTER_METER_FLOOR_DB + MASTER_METER_ANIM_EPS_DB;
        self.peak_db > floor || self.rms_db > floor || self.hold_db > floor
    }
}

/// Stereo (L/R) ballistics pair, persisted across frames in egui temp storage
/// (see [`MonitorZoom`]'s doc above for why: no `PhotonicApp` field — this
/// story's territory is `app/{monitor.rs,engine.rs}` only).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct MasterMeterState {
    l: MasterMeterChannel,
    r: MasterMeterChannel,
}

impl MasterMeterState {
    fn update(&mut self, level: Option<engine::MasterLevel>, dt: f32) {
        let (peak_db, rms_db) = match level {
            Some(lv) => (
                [
                    master_meter_lin_to_db(lv.peak[0]),
                    master_meter_lin_to_db(lv.peak[1]),
                ],
                [
                    master_meter_lin_to_db(lv.rms[0]),
                    master_meter_lin_to_db(lv.rms[1]),
                ],
            ),
            // No real tap yet (`EngineBridge::master_level`'s doc) — settle
            // honestly at the silence floor rather than animating a
            // fabricated value.
            None => ([MASTER_METER_FLOOR_DB; 2], [MASTER_METER_FLOOR_DB; 2]),
        };
        self.l.update(peak_db[0], rms_db[0], dt);
        self.r.update(peak_db[1], rms_db[1], dt);
    }

    fn clipped(&self) -> bool {
        self.l.clip || self.r.clip
    }

    /// True while either channel is still settling (see
    /// [`MasterMeterChannel::is_animating`]). Drives the monitor's repaint
    /// throttle: continuous frames while the meter is moving, an idle heartbeat
    /// once it has come to rest at the floor.
    fn is_animating(&self) -> bool {
        self.l.is_animating() || self.r.is_animating()
    }

    /// The louder channel's instantaneous peak, for the numeric hover readout.
    fn peak_db(&self) -> f32 {
        self.l.peak_db.max(self.r.peak_db)
    }
}

/// One channel's peak(dim)+RMS(bright) fill plus a peak-hold tick.
fn draw_master_meter_bar(painter: &egui::Painter, rect: egui::Rect, ch: &MasterMeterChannel) {
    painter.rect_filled(rect, 1.0, egui::Color32::from_gray(26));
    let peak_frac = master_meter_db_to_frac(ch.peak_db);
    let rms_frac = master_meter_db_to_frac(ch.rms_db);
    master_meter_fill(
        painter,
        rect,
        peak_frac,
        master_meter_color(peak_frac).gamma_multiply(0.5),
    );
    master_meter_fill(painter, rect, rms_frac, master_meter_color(rms_frac));
    // Peak-hold marker line.
    let hold_frac = master_meter_db_to_frac(ch.hold_db);
    let y = rect.bottom() - hold_frac * rect.height();
    painter.line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        egui::Stroke::new(1.0, egui::Color32::WHITE),
    );
}

fn master_meter_fill(painter: &egui::Painter, rect: egui::Rect, frac: f32, color: egui::Color32) {
    let h = rect.height() * frac.clamp(0.0, 1.0);
    let filled = egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - h), rect.max);
    painter.rect_filled(filled, 1.0, color);
}

impl PhotonicApp {
    fn master_meter_state(&self, ctx: &egui::Context) -> MasterMeterState {
        ctx.data(|d| d.get_temp(egui::Id::new(MASTER_METER_STATE_ID)))
            .unwrap_or_default()
    }

    fn set_master_meter_state(&self, ctx: &egui::Context, state: MasterMeterState) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(MASTER_METER_STATE_ID), state));
    }

    /// Draws the slim stereo peak+RMS column `draw_video_monitor` reserves
    /// beside the letterboxed image (Gap G-4), fed by
    /// `EngineBridge::master_level` (real tap once that seam closes — see
    /// its doc). Advances ballistics every frame (even on engine-less hosts,
    /// at the floor) so the column's layout never jumps when an engine
    /// attaches.
    fn draw_master_meter(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, rect: egui::Rect) {
        let level = self
            .engine
            .as_ref()
            .and_then(engine::EngineBridge::master_level);
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let mut state = self.master_meter_state(ctx);
        state.update(level, dt);
        self.set_master_meter_state(ctx, state);
        if self.engine.is_some() {
            // P1-5: only drive continuous 60fps repaints while something is
            // actually moving — the engine playing back, the meter ballistics
            // still settling toward the floor, or the user holding a scrub
            // drag. When none of those hold (a paused, attached engine with the
            // meter parked at silence), fall back to a slow heartbeat so the
            // monitor drops out of the hot loop and idles instead of pinning
            // the whole app at 60fps merely because an engine is attached.
            // Playback smoothness and meter animation are unchanged WHILE
            // playing (both keep hitting the `request_repaint()` branch).
            let scrubbing = ctx.data(|d| {
                d.get_temp::<bool>(egui::Id::new(MONITOR_SCRUB_DRAGGING_ID))
                    .unwrap_or(false)
            });
            if self.monitor_playing || state.is_animating() || scrubbing {
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }

        let bars_w = MASTER_METER_BAR_W * 2.0 + MASTER_METER_BAR_GAP;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(2.0);
                // Clip LED (top): click to clear a latched clip (13 §11.4).
                let (led_rect, led_resp) =
                    ui.allocate_exact_size(egui::vec2(bars_w, 6.0), egui::Sense::click());
                if led_resp.clicked() {
                    state.l.clip = false;
                    state.r.clip = false;
                    self.set_master_meter_state(ctx, state);
                }
                let led_color = if state.clipped() {
                    MASTER_METER_HIGH
                } else {
                    egui::Color32::from_gray(50)
                };
                ui.painter().rect_filled(led_rect, 2.0, led_color);

                ui.add_space(4.0);
                let bars_h = (rect.height() - 28.0).max(20.0);
                let (bars_rect, _) =
                    ui.allocate_exact_size(egui::vec2(bars_w, bars_h), egui::Sense::hover());
                let l_rect = egui::Rect::from_min_size(
                    bars_rect.min,
                    egui::vec2(MASTER_METER_BAR_W, bars_h),
                );
                let r_rect = egui::Rect::from_min_max(
                    egui::pos2(bars_rect.max.x - MASTER_METER_BAR_W, bars_rect.min.y),
                    bars_rect.max,
                );
                let painter = ui.painter();
                draw_master_meter_bar(painter, l_rect, &state.l);
                draw_master_meter_bar(painter, r_rect, &state.r);

                let tap_note = if level.is_some() {
                    String::new()
                } else {
                    " — engine tap pending".to_string()
                };
                led_resp.on_hover_text(format!(
                    "Master output peak {}{tap_note} — click a clip LED to clear its latch",
                    fmt_master_db(state.peak_db())
                ));
            });
        });
    }
}

// ── Mode entry/exit (04 §1.2, §1.3) ─────────────────────────────────────────

impl PhotonicApp {
    /// Toggle between Vector/Video for the active tab (04 §1.2). Lazily
    /// creates `doc.timeline` (§1.3) on the way into Video so every entry
    /// path — this toggle, the command palette, the Welcome new-video-project
    /// action, and auto-enter-on-open — keeps `self.mode == Video` and
    /// `doc.timeline.is_some()` in lockstep, per the invariant §1.1's central-
    /// panel branch `debug_assert!`s.
    pub(crate) fn enter_or_exit_video_mode(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        match self.mode {
            AppMode::Video => {
                // Exit-to-Vector always pauses first, unconditionally (04 §7
                // "mode-switch race with in-flight engine playback") — a real
                // `EngineCmd::Pause` so a hidden monitor never keeps decoding.
                self.monitor_playing = false;
                if let Some(bridge) = self.engine.as_mut() {
                    bridge.set_playing(false);
                }
                self.mode = AppMode::Vector;
            }
            AppMode::Vector => {
                self.ensure_timeline_project(doc, history);
                self.mode = AppMode::Video;
                self.maybe_show_first_entry_hints();
            }
        }
        // A drawer open in the old mode is meaningless in the new one (04 §4).
        self.open_drawer = None;
    }

    /// First-video-mode-action project creation (04 §1.3), defaulting to a
    /// 1920x1080/30fps sequence. Used by the toolbar toggle, the command
    /// palette, and auto-enter-on-open. The Welcome "New Video Project" flow
    /// calls [`Self::ensure_timeline_project_with`] directly so the user's
    /// chosen format/frame rate is honored instead of the default.
    pub(crate) fn ensure_timeline_project(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        self.ensure_timeline_project_with(doc, history, 1920, 1080, FrameRate::FPS_30);
    }

    /// Create `doc.timeline` (with one default sequence) if it doesn't exist
    /// yet, undoably via `TimelineCmd::CreateProject` + `AddSequence` (01
    /// §10). No-op if a timeline already exists — callers never need to
    /// check first.
    pub(crate) fn ensure_timeline_project_with(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        width: u32,
        height: u32,
        frame_rate: FrameRate,
    ) {
        if doc.timeline.is_some() {
            return;
        }
        use photonic_core::timeline::ops;
        history.execute(Command::Timeline(ops::create_project()), doc);
        let seq = Sequence::new("Sequence 1", frame_rate, width, height);
        history.execute(Command::Timeline(ops::add_sequence(seq)), doc);
    }

    /// First-entry hints (04 §1.2): the one-time shortcut overlay auto-opens
    /// the very first time a session enters video mode; `?` re-opens it any
    /// time after regardless of this having already fired once.
    /// Proposal 213: coach marks start on first entry unless already dismissed.
    fn maybe_show_first_entry_hints(&mut self) {
        if !self.prefs.video_shortcuts_intro_shown {
            self.show_video_shortcut_sheet = true;
            self.prefs.video_shortcuts_intro_shown = true;
            self.prefs.save();
        }
    }

    /// One-shot check for a CLI/double-click-opened document that already has
    /// a timeline (04 §1.2 "Auto-enter on open") — a project with a timeline
    /// always opens into video mode. Called once, right after
    /// `ensure_initial_tab` on the very first `draw` frame; the
    /// `WelcomeAction::OpenFile` arms handle the equivalent case for files
    /// opened from the welcome screen (`app/mod.rs`, same §1.2 rule).
    pub(crate) fn check_initial_auto_enter(&mut self, doc: &Document) {
        if self.initial_mode_checked {
            return;
        }
        self.initial_mode_checked = true;
        if doc.timeline.is_some() {
            self.mode = AppMode::Video;
        }
    }
}

// ── Sequence/format lookup helpers ──────────────────────────────────────────

/// The active sequence, if a timeline project exists and has one selected.
fn active_sequence(doc: &Document) -> Option<&Sequence> {
    let project = doc.timeline.as_ref()?;
    let id = project.active_sequence?;
    project.sequences.get(&id)
}

/// True if the active sequence has any clip on any track (video or audio).
/// Drives the monitor's empty-state invitation.
fn sequence_has_clips(doc: &Document) -> bool {
    doc.timeline
        .as_ref()
        .and_then(|p| p.active_sequence.and_then(|id| p.sequences.get(&id)))
        .map(|seq| {
            seq.video_tracks
                .iter()
                .chain(seq.audio_tracks.iter())
                .any(|t| !t.clips.is_empty())
        })
        .unwrap_or(false)
}

/// The active sequence's active format, or a 1920x1080 16:9 default when no
/// sequence exists yet (04 §3 "default 16:9 1920x1080 when absent").
fn active_format(doc: &Document) -> SequenceFormat {
    active_sequence(doc)
        .map(|s| s.format().clone())
        .unwrap_or_else(|| SequenceFormat::new("16:9", 1920, 1080))
}

/// The active sequence's frame rate, defaulting to 30fps.
fn active_frame_rate(doc: &Document) -> FrameRate {
    active_sequence(doc)
        .map(|s| s.frame_rate)
        .unwrap_or(FrameRate::FPS_30)
}

/// End tick of the active sequence's content (0 for an empty/absent one).
fn sequence_end_tick(doc: &Document) -> Tick {
    active_sequence(doc)
        .map(|s| s.content_end())
        .unwrap_or(Tick::ZERO)
}

/// Format a tick as SMPTE timecode (K-A12 / 04 §3.2).
/// Uses drop-frame (`;`) automatically for 29.97 / 59.94 and honours the
/// sequence `start_timecode` offset when a document sequence is available.
fn format_timecode(fr: FrameRate, t: Tick) -> String {
    format_timecode_with_start(fr, t, Tick::ZERO)
}

fn format_timecode_with_start(fr: FrameRate, t: Tick, start: Tick) -> String {
    use photonic_core::timeline::Timecode;
    // Prefer drop-frame labelling on NTSC rates (broadcast default).
    Timecode::format_tick(t, fr, start, true)
}

// ── Transport (04 §3.2, §5.1) ───────────────────────────────────────────────
//
// Transport methods mutate GUI *intent* (`monitor_playing`, reverse/speed,
// `self.playhead`); `drive_engine_playback` reconciles that intent into the
// minimal `EngineCmd` stream each frame (02 §1: Play/Pause/Seek/Step/SetLoop)
// and follows `EngineStatus.playhead` while the engine is playing. With no
// engine attached (tests, GPU-less hosts) the original wall-clock placeholder
// (`advance_monitor_playback`) keeps the UX testable.

impl PhotonicApp {
    pub(crate) fn video_play_pause(&mut self) {
        self.finish_precision_trim();
        // Intent only — `drive_engine_playback` sends Play/Pause on the diff.
        self.monitor_playing = !self.monitor_playing;
        if self.monitor_playing {
            self.monitor_play_reverse = false;
            self.monitor_play_speed = 1.0;
        }
    }

    /// J: play reverse; repeated presses ramp speed (reference-NLE convention).
    /// The engine has no reverse primitive (speed-maps story, P8), so with an
    /// engine attached this becomes a coalesced-`Seek` shuttle while the
    /// engine stays paused — see `drive_engine_playback`.
    pub(crate) fn video_play_reverse(&mut self) {
        self.finish_precision_trim();
        if self.monitor_playing && self.monitor_play_reverse {
            self.monitor_play_speed = (self.monitor_play_speed * 2.0).min(8.0);
        } else {
            self.monitor_playing = true;
            self.monitor_play_reverse = true;
            self.monitor_play_speed = 1.0;
        }
    }

    /// K: pause.
    pub(crate) fn video_pause(&mut self) {
        self.monitor_playing = false;
        if let Some(bridge) = self.engine.as_ref() {
            if bridge
                .status()
                .source_audition
                .as_ref()
                .is_some_and(|s| s.playing)
                && !bridge
                    .session()
                    .send(photonic_video::EngineCmd::StopSourceAudition)
            {
                self.file_status = Some("Engine is busy; stop source audition again.".into());
            }
        }
    }

    /// L: play forward; repeated presses ramp speed (1× uses the engine's
    /// audio-mastered `Play`; >1× falls back to the `Seek` shuttle).
    pub(crate) fn video_play_forward(&mut self) {
        self.finish_precision_trim();
        if self.monitor_playing && !self.monitor_play_reverse {
            self.monitor_play_speed = (self.monitor_play_speed * 2.0).min(8.0);
        } else {
            self.monitor_playing = true;
            self.monitor_play_reverse = false;
            self.monitor_play_speed = 1.0;
        }
    }

    pub(crate) fn video_step_back(&mut self, doc: &Document) {
        self.monitor_playing = false;
        let tpf = self.transport_frame_rate(doc).ticks_per_frame().0.max(1);
        if self.io_targets_source_marks() {
            let next = Tick((self.source_marks.source_time.0 - tpf).max(0));
            self.source_marks.source_time = next;
            if let Some(media) = self
                .source_marks
                .armed_asset
                .and_then(|asset| doc.timeline.as_ref()?.media.assets.get(&asset))
            {
                self.source_marks.clamp_to_asset(media);
            }
            let next = self.source_marks.source_time;
            if let (Some(asset), Some(bridge)) =
                (self.source_marks.armed_asset, self.engine.as_mut())
            {
                bridge.seek_source(asset, next);
            }
            return;
        }
        self.playhead = Tick((self.playhead.0 - tpf).max(0));
        if let Some(bridge) = self.engine.as_mut() {
            // Exact-frame step on the engine (02 §4: Step always pauses); the
            // local move above is the optimistic echo of the same arithmetic.
            if bridge.step(-1) {
                bridge.note_agreed(self.playhead);
            }
        }
    }

    pub(crate) fn video_step_forward(&mut self, doc: &Document) {
        self.monitor_playing = false;
        let tpf = self.transport_frame_rate(doc).ticks_per_frame().0.max(1);
        if self.io_targets_source_marks() {
            let mut next = self.source_marks.source_time.0 + tpf;
            if let Some(end) = self.armed_asset_duration(doc) {
                if end.0 > 0 {
                    next = next.min(end.0);
                }
            }
            let next = Tick(next);
            self.source_marks.source_time = next;
            if let Some(media) = self
                .source_marks
                .armed_asset
                .and_then(|asset| doc.timeline.as_ref()?.media.assets.get(&asset))
            {
                self.source_marks.clamp_to_asset(media);
            }
            let next = self.source_marks.source_time;
            if let (Some(asset), Some(bridge)) =
                (self.source_marks.armed_asset, self.engine.as_mut())
            {
                bridge.seek_source(asset, next);
            }
            return;
        }
        let mut next = self.playhead.0 + tpf;
        let end = sequence_end_tick(doc).0;
        if end > 0 {
            next = next.min(end);
        }
        self.playhead = Tick(next);
        if let Some(bridge) = self.engine.as_mut() {
            if bridge.step(1) {
                bridge.note_agreed(self.playhead);
            }
        }
    }

    fn transport_frame_rate(&self, doc: &Document) -> FrameRate {
        if self.io_targets_source_marks() {
            if let Some(rate) = self.source_marks.armed_asset.and_then(|id| {
                doc.timeline
                    .as_ref()?
                    .media
                    .assets
                    .get(&id)?
                    .probe
                    .as_ref()?
                    .video
                    .as_ref()
                    .map(|video| video.frame_rate)
            }) {
                if rate.num > 0 && rate.den > 0 {
                    return rate;
                }
            }
        }
        active_frame_rate(doc)
    }

    fn armed_asset_duration(&self, doc: &Document) -> Option<Tick> {
        let id = self.source_marks.armed_asset?;
        doc.timeline
            .as_ref()?
            .media
            .assets
            .get(&id)?
            .probe
            .as_ref()
            .map(|p| p.duration)
    }

    pub(crate) fn video_playhead_home(&mut self) {
        // Intent only — the reconciler's scrub detector turns the moved
        // playhead into an `EngineCmd::Seek`.
        self.monitor_playing = false;
        if self.io_targets_source_marks() {
            self.source_marks.source_time = Tick::ZERO;
            if let (Some(asset), Some(bridge)) =
                (self.source_marks.armed_asset, self.engine.as_mut())
            {
                bridge.seek_source(asset, Tick::ZERO);
            }
            return;
        }
        self.playhead = Tick::ZERO;
    }

    pub(crate) fn video_playhead_end(&mut self, doc: &Document) {
        self.monitor_playing = false;
        if self.io_targets_source_marks() {
            let end = self.armed_asset_duration(doc).unwrap_or(Tick::ZERO);
            self.source_marks.source_time = end;
            if let (Some(asset), Some(bridge)) =
                (self.source_marks.armed_asset, self.engine.as_mut())
            {
                bridge.seek_source(asset, end);
            }
            return;
        }
        self.playhead = sequence_end_tick(doc);
    }

    /// I: set In point. When the single monitor is peaking a source (or an
    /// asset is armed for 3-point edit), sets **source** marks (G-10 / 24 §3.3,
    /// session-only). Otherwise sets sequence **work range** (document, undoable).
    pub(crate) fn video_set_in(&mut self, doc: &mut Document, history: &mut CommandHistory) {
        if self.io_targets_source_marks() {
            let t = self.source_marks.source_time;
            self.source_marks.set_in(t);
            self.sync_pending_from_source_marks(doc);
            return;
        }
        self.set_work_range_bound(doc, history, true);
    }

    pub(crate) fn video_set_out(&mut self, doc: &mut Document, history: &mut CommandHistory) {
        if self.io_targets_source_marks() {
            let t = self.source_marks.source_time;
            self.source_marks.set_out(t);
            self.sync_pending_from_source_marks(doc);
            return;
        }
        self.set_work_range_bound(doc, history, false);
    }

    /// Clear source In/Out marks (G-10). No-op for work range.
    pub(crate) fn video_clear_source_marks(&mut self) {
        self.source_marks.clear_marks();
        // Keep pending_source only if it came from timeline selection, not marks.
        if self.pending_source.as_ref().and_then(|p| match &p.source {
            photonic_core::timeline::ClipSource::Asset { asset } => Some(*asset),
            _ => None,
        }) == self.source_marks.armed_asset
        {
            // Marks cleared — drop mark-derived pending; re-arm on next edit.
            self.pending_source = None;
        }
    }

    fn io_targets_source_marks(&self) -> bool {
        let preview_is_asset = self
            .engine
            .as_ref()
            .map(|b| b.preview_is_asset())
            .unwrap_or(false);
        crate::app::source_marks::io_targets_source_marks(
            preview_is_asset,
            self.monitor_playing,
            self.source_marks.armed_asset,
        )
    }

    /// Derive `pending_source` from G-10 marks when possible.
    fn sync_pending_from_source_marks(&mut self, doc: &Document) {
        let Some(id) = self.source_marks.armed_asset else {
            return;
        };
        let Some(asset) = doc
            .timeline
            .as_ref()
            .and_then(|p| p.media.assets.get(&id))
            .cloned()
        else {
            return;
        };
        let default_dur = asset
            .probe
            .as_ref()
            .map(|p| p.duration)
            .filter(|d| d.0 > 0)
            .unwrap_or(Tick(5_000_000)); // 5s fallback for unprobed media
        if let Some(ps) = self.source_marks.pending_source(&asset, default_dur) {
            self.pending_source = Some(ps);
        }
    }

    fn set_work_range_bound(
        &mut self,
        doc: &mut Document,
        history: &mut CommandHistory,
        is_in: bool,
    ) {
        let playhead = self.playhead;
        let Some(project) = doc.timeline.as_ref() else {
            return;
        };
        let Some(seq_id) = project.active_sequence else {
            return;
        };
        let Some(seq) = project.sequences.get(&seq_id) else {
            return;
        };
        let (mut in_t, mut out_t) = seq.work_range.unwrap_or((Tick::ZERO, seq.content_end()));
        if is_in {
            in_t = playhead;
            if out_t < in_t {
                out_t = in_t;
            }
        } else {
            out_t = playhead;
            if in_t > out_t {
                in_t = out_t;
            }
        }
        ops_bridge::set_work_range(doc, history, seq_id, Some((in_t, out_t)));
    }

    /// Per-frame playback driver: reconcile GUI intent into `EngineCmd`s and
    /// follow `EngineStatus` when an engine is attached; otherwise fall back
    /// to the wall-clock placeholder. Called once per frame from
    /// [`Self::draw_video_monitor`].
    fn drive_playback(&mut self, ctx: &egui::Context, doc: &Document) {
        if let Some((document, command)) = crate::panels::video::source_monitor::take_command(ctx) {
            if document == doc.id {
                self.source_panel_command(doc, command);
            }
        }
        if let Some(time) = self.source_monitor_scrub.take() {
            if let (Some(asset), Some(bridge)) =
                (self.source_marks.armed_asset, self.engine.as_mut())
            {
                bridge.seek_source(asset, time);
            }
        }
        if self.engine.is_none() {
            self.advance_monitor_playback(ctx, doc);
            return;
        }

        if self.precision_trim.is_some() {
            return;
        }

        // Desired-state inputs computed before borrowing the bridge.
        let active_seq = doc.timeline.as_ref().and_then(|p| p.active_sequence);
        let end = sequence_end_tick(doc);
        let loop_range = if self.monitor_loop_enabled && end.0 > 0 {
            Some(
                active_sequence(doc)
                    .and_then(|s| s.work_range)
                    .unwrap_or((Tick::ZERO, end)),
            )
        } else {
            None
        };
        let shuttle =
            self.monitor_playing && (self.monitor_play_reverse || self.monitor_play_speed != 1.0);
        let dt = ctx.input(|i| i.unstable_dt as f64).min(0.25);
        // Take before borrowing the bridge (distinct fields, but keep explicit).
        let want_peek = self.media_pool_ui.want_peek.take();

        let bridge = self.engine.as_mut().expect("checked above");
        bridge.set_active_sequence(active_seq);
        bridge.set_preview_cache_dir(
            self.current_file
                .as_deref()
                .map(photonic_video::media::cache_dir_for_project)
                .unwrap_or_else(|| {
                    std::env::temp_dir()
                        .join("photonic-preview-cache")
                        .join(doc.id.to_string())
                }),
        );
        bridge.apply_proxy_mode();
        bridge.apply_preview_quality();
        bridge.apply_preview_target();
        bridge.set_loop(loop_range);

        // Media-pool click → source peek on the single monitor (24 §3) + arm G-10.
        if let Some(asset) = want_peek.filter(|_| !self.monitor_playing) {
            let at = if self.source_marks.armed_asset == Some(asset) {
                self.source_marks.source_time
            } else {
                Tick::ZERO
            };
            self.source_marks.arm(asset, at);
            if let Some(media) = doc
                .timeline
                .as_ref()
                .and_then(|p| p.media.assets.get(&asset))
            {
                self.source_marks.clamp_to_asset(media);
            }
            bridge.peek_asset(asset, self.source_marks.source_time);
        }

        let source_status = bridge.status();
        if let Some(source) = &source_status.source_audition {
            if source.playing && !self.monitor_playing {
                self.source_marks.arm(source.asset, source.playhead);
                bridge.follow_source_audition(source.asset, source.playhead);
                ctx.request_repaint_after(std::time::Duration::from_millis(8));
            }
        }

        if self.monitor_playing {
            if let Some(sequence) = active_seq {
                bridge.peek_sequence(sequence);
            }
        }

        // User scrub (ruler drag, scrubber, Home/End, marker jump): the playhead
        // moved without the bridge agreeing to it. While the pointer is held
        // down (an active drag), issue a cheap keyframe-preview ScrubSeek so
        // dragging stays live; otherwise a normal Seek. On drag-release, force a
        // settling Seek so the exact frame lands (the last ScrubSeek already
        // recorded agreement, so this else-if is what guarantees the settle).
        // Sequence scrub also returns the monitor to program view (24 §3.2).
        let scrub_id = egui::Id::new(MONITOR_SCRUB_DRAGGING_ID);
        let was_dragging = ctx.data(|d| d.get_temp::<bool>(scrub_id).unwrap_or(false));
        let pointer_down = ctx.input(|i| i.pointer.primary_down());
        if bridge.agreed_playhead != Some(self.playhead) {
            if let Some(seq) = active_seq {
                bridge.peek_sequence(seq);
            }
            if pointer_down {
                ctx.data_mut(|d| d.insert_temp(scrub_id, true));
                bridge.scrub_seek(self.playhead);
            } else {
                ctx.data_mut(|d| d.insert_temp(scrub_id, false));
                bridge.seek(self.playhead);
            }
        } else if was_dragging && !pointer_down {
            ctx.data_mut(|d| d.insert_temp(scrub_id, false));
            bridge.seek(self.playhead);
        }

        if shuttle {
            // Reverse / ramped playback: no engine primitive yet (P8 speed
            // maps), so scrub with coalesced Seeks while the engine is paused.
            bridge.set_playing(false);
            ctx.request_repaint();
            let dir = if self.monitor_play_reverse { -1.0 } else { 1.0 };
            let delta = (dt * TICKS_PER_SECOND as f64 * self.monitor_play_speed * dir) as i64;
            let mut next = self.playhead.0 + delta;
            if self.monitor_loop_enabled && end.0 > 0 {
                next = next.rem_euclid(end.0.max(1));
            } else {
                next = next.max(0);
                if end.0 > 0 && next >= end.0 {
                    next = end.0;
                    self.monitor_playing = false;
                }
            }
            self.playhead = Tick(next);
            bridge.seek(self.playhead);
        } else {
            bridge.set_playing(self.monitor_playing);
            if self.monitor_playing {
                // Cap GUI repaint to ~120 Hz while playing — matches engine
                // poll (4 ms) without oversubscribing the main thread on Windows
                // (composition) or Linux (X11/Wayland present).
                ctx.request_repaint_after(std::time::Duration::from_millis(8));
                let status = bridge.status();
                if status.playing {
                    // The engine's clock is authoritative at 1× (02 §4).
                    self.playhead = status.playhead;
                    bridge.note_agreed(self.playhead);
                }
            } else {
                // Paused: a step / seek / scrub-release-settle just issued an
                // engine command; the target frame is decoded asynchronously on
                // the engine thread and only appears via `present_latest` on a
                // repaint. The idle path throttles to the ~250ms meter heartbeat,
                // which would leave precise stepping/scrubbing frames unshown for
                // up to a quarter second. Keep repainting until the engine has
                // actually presented the frame for the current playhead, then go
                // idle. Self-terminating: the engine always publishes a frame
                // (real or transparent) stamped at the requested tick.
                let want = match bridge.preview_target {
                    PreviewTarget::Asset { source_time, .. } => source_time,
                    _ => active_frame_rate(doc).snap(self.playhead),
                };
                let shown = bridge.presented_frame.map(|(t, _)| t);
                if shown != Some(want) {
                    ctx.request_repaint();
                }
            }
        }
    }

    /// Wall-clock placeholder playback for engine-less hosts (pre-P3
    /// behavior, kept for tests and GPU-free machines).
    fn advance_monitor_playback(&mut self, ctx: &egui::Context, doc: &Document) {
        if !self.monitor_playing {
            return;
        }
        ctx.request_repaint();
        let dt = ctx.input(|i| i.unstable_dt as f64).min(0.25);
        let dir = if self.monitor_play_reverse { -1.0 } else { 1.0 };
        let delta = (dt * TICKS_PER_SECOND as f64 * self.monitor_play_speed * dir) as i64;
        let mut next = self.playhead.0 + delta;
        let end = sequence_end_tick(doc).0;
        if self.monitor_loop_enabled && end > 0 {
            next = next.rem_euclid(end.max(1));
        } else {
            next = next.max(0);
            if end > 0 && next >= end {
                next = end;
                self.monitor_playing = false;
            }
        }
        self.playhead = Tick(next);
    }
}

// ── Central-panel content (04 §1.1 point 3, §3) ─────────────────────────────

impl PhotonicApp {
    /// The video-mode program monitor + transport bar, drawn in place of the
    /// vector canvas content when `self.mode == AppMode::Video` (04 §1.1
    /// point 3). With an engine attached this paints the latest presented
    /// `EngineFrame` (03 §5) cropped to the sequence format's logical size;
    /// engine-less hosts keep the dark letterboxed placeholder.
    pub(crate) fn draw_video_monitor(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        rect: egui::Rect,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        self.drive_playback(ctx, doc);
        self.handle_video_keyboard(ctx, doc, history);

        let format = active_format(doc);
        let painter = ui.painter_at(rect);

        // Reserve a top strip for the aspect/frame bar (CAP-012), a thin scrub
        // strip and a transport bar (04 §3.2, QW-5) at the bottom. The video
        // image sits between the format bar and the scrubber.
        const FORMAT_H: f32 = 30.0;
        const SCRUB_H: f32 = 16.0;
        const TRANSPORT_H: f32 = 40.0;
        let format_rect = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(rect.max.x, (rect.min.y + FORMAT_H).min(rect.max.y)),
        );
        let transport_top = (rect.max.y - TRANSPORT_H).max(format_rect.max.y);
        let scrub_top = (transport_top - SCRUB_H).max(format_rect.max.y);
        let content_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, format_rect.max.y),
            egui::pos2(rect.max.x, scrub_top),
        );
        let scrub_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, scrub_top),
            egui::pos2(rect.max.x, transport_top),
        );
        let transport_rect =
            egui::Rect::from_min_max(egui::pos2(rect.min.x, transport_top), rect.max);

        // Master output meter (Gap G-4): reserve a slim stereo column on the
        // right edge of the content area, between the letterboxed image and
        // the panel edge — reserved unconditionally (present even on
        // engine-less hosts, honestly parked at the silence floor) so the
        // layout never jumps when an engine attaches.
        let meter_rect = egui::Rect::from_min_max(
            egui::pos2(
                (content_rect.max.x - MASTER_METER_COL_W).max(content_rect.min.x),
                content_rect.min.y,
            ),
            content_rect.max,
        );
        let image_area = egui::Rect::from_min_max(
            content_rect.min,
            egui::pos2(
                (meter_rect.min.x - MASTER_METER_MARGIN).max(content_rect.min.x),
                content_rect.max.y,
            ),
        );

        // Background + letterbox/pillarbox bars (04 §3.3).
        // Use window_fill (`surface-base`) so residual bars match the floating
        // card gutters instead of reading as a separate pure-black slab.
        painter.rect_filled(rect, 0.0, ui.visuals().window_fill);
        let target_aspect = format.width.max(1) as f32 / format.height.max(1) as f32;
        let avail_w = image_area.width().max(1.0);
        let avail_h = image_area.height().max(1.0);
        let avail_aspect = avail_w / avail_h;

        // Image zoom (14 §M-5 polish): `Fit` letterboxes into the available
        // area exactly as before; `Actual` shows the frame at native pixels,
        // draggable when it overflows the content area.
        let mut zoom = self.monitor_zoom_state(ctx);
        let (video_size, fit_pillarbox) = match zoom.mode {
            ImageZoomMode::Fit => {
                if avail_aspect > target_aspect {
                    // Image is narrower than the area → side bars (pillarbox).
                    (egui::vec2(avail_h * target_aspect, avail_h), true)
                } else {
                    // Image is shorter than the area → top/bottom bars (letterbox).
                    (egui::vec2(avail_w, avail_w / target_aspect), false)
                }
            }
            ImageZoomMode::Actual => (egui::vec2(format.width as f32, format.height as f32), false),
        };
        if zoom.mode == ImageZoomMode::Fit {
            zoom.pan = egui::Vec2::ZERO;
        } else {
            // Pan-to-drag, gated to `Actual` so it never competes with the
            // reframe handles' own drag sense over the default Fit view.
            // Scoped to `image_area` (not `content_rect`) so it doesn't
            // shadow the meter column's own clip-LED click target.
            let pan_resp = ui.interact(
                image_area,
                ui.id().with("monitor_pan_drag"),
                egui::Sense::drag(),
            );
            if pan_resp.dragged() {
                zoom.pan += pan_resp.drag_delta();
            }
            // Clamp so a drag can't push the frame fully out of view — a
            // margin stays reachable even once it's mostly offscreen.
            let max_pan_x = ((video_size.x - avail_w).max(0.0)) / 2.0 + 40.0;
            let max_pan_y = ((video_size.y - avail_h).max(0.0)) / 2.0 + 40.0;
            zoom.pan.x = zoom.pan.x.clamp(-max_pan_x, max_pan_x);
            zoom.pan.y = zoom.pan.y.clamp(-max_pan_y, max_pan_y);
            if pan_resp.hovered() || pan_resp.dragged() {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grab);
            }
        }
        self.set_monitor_zoom_state(ctx, zoom);
        // Fit + pillarbox: pin the frame to the LEFT of `image_area` so the
        // leftover bar sits on the right (next to the meter / right rail)
        // instead of splitting into a dead black box between the left drawer
        // and the preview — that gap is what reads as a UI defect when flipping
        // aspect-ratio tabs (Social 16:9 / 9:16 / 1:1 …). Letterbox (top/bottom
        // bars) and Actual mode stay centered.
        let video_rect = if fit_pillarbox {
            let top = image_area.center().y - video_size.y * 0.5;
            egui::Rect::from_min_size(egui::pos2(image_area.left(), top) + zoom.pan, video_size)
        } else {
            egui::Rect::from_center_size(image_area.center() + zoom.pan, video_size)
        };
        // Clipped to `image_area` (not `content_rect`) so a zoomed-in
        // (Actual) frame can't paint over the format/scrub/transport bars
        // above and below it, nor over the master-meter column beside it.
        let content_painter = ui.painter_at(image_area);
        content_painter.rect_filled(video_rect, 0.0, egui::Color32::from_rgb(24, 24, 28));

        // ── EngineFrame presentation (03 §5) ────────────────────────────────
        // `EngineBridge::present_latest` (run by the host each frame, before
        // egui) has already presented the newest frame into a registered
        // native texture; paint it cropped to the format's logical size (the
        // engine texture is pool-bucket padded — facade note).
        let mut drew_frame = false;
        let mut split_drag: Option<f32> = None;
        if let Some(bridge) = &self.engine {
            if let (Some(tex), Some((_, fseq))) = (&bridge.monitor_tex, bridge.presented_frame) {
                let active = doc.timeline.as_ref().and_then(|p| p.active_sequence);
                if active == Some(fseq) {
                    let logical_size = bridge
                        .presented_logical_size
                        .unwrap_or((format.width, format.height));
                    let uv = engine::padded_uv(logical_size, tex.physical);
                    // K-B5: vertical wipe — left clean (no clip looks), right graded.
                    if bridge.compare_effects {
                        if let Some(clean) = &bridge.compare_tex {
                            let split = bridge.compare_split.clamp(0.05, 0.95);
                            let mid = video_rect.left() + video_rect.width() * split;
                            let left = egui::Rect::from_min_max(
                                video_rect.min,
                                egui::pos2(mid, video_rect.bottom()),
                            );
                            let right = egui::Rect::from_min_max(
                                egui::pos2(mid, video_rect.top()),
                                video_rect.max,
                            );
                            let uv_left = egui::Rect::from_min_max(
                                uv.min,
                                egui::pos2(uv.min.x + uv.width() * split, uv.max.y),
                            );
                            let uv_right = egui::Rect::from_min_max(
                                egui::pos2(uv.min.x + uv.width() * split, uv.min.y),
                                uv.max,
                            );
                            content_painter.image(clean.id, left, uv_left, egui::Color32::WHITE);
                            content_painter.image(tex.id, right, uv_right, egui::Color32::WHITE);
                            content_painter.line_segment(
                                [
                                    egui::pos2(mid, video_rect.top()),
                                    egui::pos2(mid, video_rect.bottom()),
                                ],
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(240, 240, 250)),
                            );
                            let handle = egui::Rect::from_center_size(
                                egui::pos2(mid, video_rect.center().y),
                                egui::vec2(10.0, 48.0),
                            );
                            let resp = ui.interact(
                                handle,
                                ui.id().with("k_b5_compare_split"),
                                egui::Sense::drag(),
                            );
                            if resp.dragged() {
                                if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                                    split_drag = Some(
                                        ((p.x - video_rect.left()) / video_rect.width())
                                            .clamp(0.05, 0.95),
                                    );
                                }
                            }
                            content_painter.rect_filled(
                                handle,
                                3.0,
                                egui::Color32::from_rgba_unmultiplied(240, 240, 250, 180),
                            );
                        } else {
                            content_painter.image(tex.id, video_rect, uv, egui::Color32::WHITE);
                        }
                    } else {
                        content_painter.image(tex.id, video_rect, uv, egui::Color32::WHITE);
                    }
                    drew_frame = true;
                }
            }
        }
        if let Some(s) = split_drag {
            if let Some(eng) = self.engine.as_mut() {
                eng.compare_split = s;
            }
        }

        if self.monitor_safe_area {
            draw_safe_area_guides(&content_painter, video_rect);
        }
        if self.monitor_comp_grid > 0 {
            draw_composition_grid(&content_painter, video_rect, self.monitor_comp_grid);
        }

        // Reframe transform handles (04 §3.3, 05 §4.2, CAP-012) — real, undoable
        // edits via `ops::set_clip_prop`. Opt-in: only drawn when the Transform
        // tool is toggled on, so the handles don't sit over the picture whenever
        // a clip merely happens to be selected.
        if self.monitor_transform_tool {
            super::reframe::draw_reframe_handles(
                ui,
                video_rect,
                doc,
                history,
                &self.timeline_selection,
            );
        }

        if self.engine.is_none() {
            painter.text(
                video_rect.center(),
                egui::Align2::CENTER_CENTER,
                "No preview — video engine unavailable on this host",
                egui::FontId::proportional(15.0),
                egui::Color32::from_gray(130),
            );
        }

        // ── Buffering spinner + engine error surface (04 §3.3) ──────────────
        if let Some(bridge) = &self.engine {
            let status = bridge.status();

            // Bridge the floating video export dialog to live engine state: it
            // is a separate window with no engine handle, so publish the latest
            // `EngineStatus::export` snapshot onto egui's per-frame store for it
            // to read, and relay a Cancel it requested back to the engine.
            use crate::panels::video::export_dialog::{export_cancel_id, export_status_id};
            ctx.data_mut(|d| d.insert_temp(export_status_id(), status.export.clone()));
            let cancel_id = export_cancel_id();
            let cancel_requested = ctx.data_mut(|d| {
                let v = d.get_temp::<bool>(cancel_id).unwrap_or(false);
                if v {
                    d.insert_temp(cancel_id, false);
                }
                v
            });
            if cancel_requested {
                bridge
                    .session()
                    .send(photonic_video::EngineCmd::CancelExport);
            }

            let tpf = active_frame_rate(doc).ticks_per_frame().0.max(1);
            let presented_time = bridge.presented_frame.map(|(t, _)| t);
            let buffering = !drew_frame && status.playing
                || engine::is_buffering(status.playing, status.playhead, presented_time, tpf, 4);
            if buffering {
                let spinner_rect =
                    egui::Rect::from_center_size(video_rect.center(), egui::vec2(28.0, 28.0));
                ui.put(spinner_rect, egui::Spinner::new().size(26.0));
            } else if !drew_frame && !sequence_has_clips(doc) {
                // Proposal 213: CapCut-class empty monitor — primary Import CTA.
                painter.text(
                    video_rect.center() + egui::vec2(0.0, -18.0),
                    egui::Align2::CENTER_CENTER,
                    format!("{}  Import media to begin", ph::FILM_STRIP),
                    egui::FontId::proportional(16.0),
                    egui::Color32::from_gray(150),
                );
                painter.text(
                    video_rect.center() + egui::vec2(0.0, 6.0),
                    egui::Align2::CENTER_CENTER,
                    "Drop files here · Media panel · or timeline Import button",
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_gray(110),
                );
                let btn = egui::Rect::from_center_size(
                    video_rect.center() + egui::vec2(0.0, 40.0),
                    egui::vec2(148.0, 28.0),
                );
                if ui
                    .put(
                        btn,
                        egui::Button::new(format!("{} Import…", ph::UPLOAD_SIMPLE)),
                    )
                    .clicked()
                {
                    self.open_drawer = Some(crate::panels::DrawerGroup::MediaPool);
                    self.prefs.open_drawer = Some(crate::panels::DrawerGroup::MediaPool);
                    self.import_media_files(doc, history, None);
                }
            }
            let readiness = monitor_readiness_text(doc, &status);
            painter.text(
                video_rect.left_top() + egui::vec2(6., 6.),
                egui::Align2::LEFT_TOP,
                readiness,
                egui::FontId::proportional(11.),
                egui::Color32::from_gray(200),
            );
            if let Some(error) = &status.source_audition_error {
                painter.text(
                    video_rect.left_top() + egui::vec2(6., 22.),
                    egui::Align2::LEFT_TOP,
                    error,
                    egui::FontId::proportional(12.),
                    ui.visuals().error_fg_color,
                );
            }
            if let Some(err) = &status.last_error {
                // 36: Diagnostic → badge mapping (code + severity colour).
                let badge = crate::panels::video::diagnostics::diag_badge(err);
                painter.text(
                    video_rect.left_bottom() + egui::vec2(6.0, -6.0),
                    egui::Align2::LEFT_BOTTOM,
                    format!("{} · {}", badge.severity_label, badge.text),
                    egui::FontId::proportional(12.0),
                    badge.color,
                );
            }
        }

        self.draw_master_meter(ui, ctx, meter_rect);
        self.draw_monitor_scrubber(ui, scrub_rect, doc);
        self.draw_transport_bar(ui, transport_rect, doc, history);
        self.draw_precision_trim(ui.ctx(), doc, history);
        self.draw_format_bar(ui, format_rect, doc, history);
        self.draw_video_shortcut_sheet(ctx);
        self.draw_video_coach_marks(ctx, doc);
    }

    /// Aspect/frame bar above the monitor (CAP-012): one-click preset chips to
    /// switch the sequence between 16:9 / 9:16 / 1:1 / 4:5 / 4:3 / 21:9, the
    /// active one highlighted. Clicking a preset activates it (or adds+activates
    /// it if the sequence doesn't have it yet), undoably — so reframing the
    /// whole edit for a different platform is a single, discoverable click.
    /// Also hosts the "Fit clips" auto-reframe button (14 §9/CAP-012,
    /// `app/reframe.rs::fit_clips_to_active_format`) — the CapCut "Auto
    /// reframe" affordance that center-fills the selected clip(s) (or every
    /// clip if none are selected) for whichever format is active above.
    fn draw_format_bar(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        let Some(seq_id) = doc.timeline.as_ref().and_then(|p| p.active_sequence) else {
            return;
        };
        let (cur_w, cur_h) = {
            let f = active_format(doc);
            (f.width, f.height)
        };
        // Snapshot before entering the `FnOnce` ui closure below, which
        // already needs to reborrow `doc`/`history` mutably.
        let selection = self.timeline_selection.clone();
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(format!("{} Frame", ph::CROP)).weak());
                ui.separator();
                let mut clicked: Option<(&str, u32, u32)> = None;
                for &(name, w, h) in super::timeline::ops_bridge::ASPECT_PRESETS {
                    let active = w == cur_w && h == cur_h;
                    // Proposal 213: social-friendly labels for common deliverables.
                    let label = match name {
                        "9:16" => "Social 9:16",
                        "16:9" => "Social 16:9",
                        "1:1" => "1:1",
                        other => other,
                    };
                    if ui
                        .selectable_label(active, label)
                        .on_hover_text(format!("Switch sequence to {name} ({w}×{h})"))
                        .clicked()
                    {
                        clicked = Some((name, w, h));
                    }
                }
                if let Some((name, w, h)) = clicked {
                    super::timeline::ops_bridge::switch_to_aspect(history, doc, seq_id, name, w, h);
                }

                ui.separator();
                if ui
                    .button(format!("{} Fit clips", ph::FRAME_CORNERS))
                    .on_hover_text(
                        "Auto-frame the selected clip(s) — or every clip if none are \
                         selected — to fill the current aspect (center-fill, one undo step)",
                    )
                    .clicked()
                {
                    super::reframe::fit_clips_to_active_format(doc, history, &selection);
                }

                // Playback resolution + image zoom (14 §M-5 polish):
                // right-aligned so they read as a distinct group from the
                // aspect/reframe controls above. Added in reverse (right_to_
                // left) order so Resolution reads before Zoom, left to right.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Right gutter, mirroring the `add_space(4.0)` on the left:
                    // without it the zoom combo's right border lands exactly on
                    // the panel's clip edge and gets shaved off.
                    ui.add_space(4.0);
                    self.draw_zoom_selector(ui);
                    ui.separator();
                    self.draw_resolution_selector(ui);
                });
            });
        });
    }

    /// Playback-resolution selector (14 §M-5): Full / 1/2 / 1/4 preview
    /// scale. See [`PlaybackResolution`] for the engine-side mapping.
    fn draw_resolution_selector(&mut self, ui: &mut egui::Ui) {
        let mut res = self.monitor_playback_resolution(ui.ctx());
        let prev = res;
        egui::ComboBox::from_id_salt("monitor_playback_resolution")
            .selected_text(res.label())
            .width(48.0)
            .show_ui(ui, |ui| {
                for opt in PlaybackResolution::ALL {
                    ui.selectable_value(&mut res, opt, opt.label());
                }
            })
            .response
            .on_hover_text(
                "Preview quality — Draft (default) caps resolution + uses proxies \
                 when ready; Full is originals at sequence size; Proxy forces proxy media",
            );
        ui.label(egui::RichText::new(ph::GAUGE).weak())
            .on_hover_text("Playback resolution");
        if res != prev {
            self.set_monitor_playback_resolution(ui.ctx(), res);
        }
    }

    /// Image-zoom selector (14 §M-5): `Fit` (default, letterboxed) or
    /// `Actual` (100%, native pixels, draggable) for the monitor picture.
    fn draw_zoom_selector(&mut self, ui: &mut egui::Ui) {
        let mut zoom = self.monitor_zoom_state(ui.ctx());
        let prev_mode = zoom.mode;
        egui::ComboBox::from_id_salt("monitor_image_zoom")
            .selected_text(match zoom.mode {
                ImageZoomMode::Fit => "Fit",
                ImageZoomMode::Actual => "100%",
            })
            .width(48.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut zoom.mode, ImageZoomMode::Fit, "Fit");
                ui.selectable_value(&mut zoom.mode, ImageZoomMode::Actual, "100%");
            })
            .response
            .on_hover_text("Image zoom — Fit letterboxes, 100% shows native pixels (drag to pan)");
        ui.label(egui::RichText::new(ph::MAGNIFYING_GLASS).weak())
            .on_hover_text("Image zoom");
        if zoom.mode != prev_mode {
            if zoom.mode == ImageZoomMode::Fit {
                zoom.pan = egui::Vec2::ZERO;
            }
            self.set_monitor_zoom_state(ui.ctx(), zoom);
        }
    }

    /// Play/pause/step buttons, timecode readout, loop + safe-area toggles,
    /// and in/out buttons (04 §3.2).
    fn draw_transport_bar(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.horizontal_centered(|ui| {
                // The transport plate starts flush against the panel's clip
                // edge, so without a gutter the first button's left border is
                // shaved off. Same gutter the format bar above already uses.
                ui.add_space(4.0);
                if ui
                    .button(ph::SKIP_BACK)
                    .on_hover_text("Playhead to Start (Home)")
                    .clicked()
                {
                    self.video_playhead_home();
                }
                if ui
                    .button(ph::CARET_LEFT)
                    .on_hover_text("Step Back One Frame (←)")
                    .clicked()
                {
                    self.video_step_back(doc);
                }
                let play_icon = if self.monitor_playing {
                    ph::PAUSE
                } else {
                    ph::PLAY
                };
                if ui
                    .button(play_icon)
                    .on_hover_text("Play / Pause (Space)")
                    .clicked()
                {
                    self.video_play_pause();
                }
                if ui
                    .button(ph::CARET_RIGHT)
                    .on_hover_text("Step Forward One Frame (→)")
                    .clicked()
                {
                    self.video_step_forward(doc);
                }
                if ui
                    .button(ph::SKIP_FORWARD)
                    .on_hover_text("Playhead to End (End)")
                    .clicked()
                {
                    self.video_playhead_end(doc);
                }

                ui.separator();
                if ui
                    .selectable_label(self.monitor_loop_enabled, ph::REPEAT)
                    .on_hover_text("Loop playback")
                    .clicked()
                {
                    self.monitor_loop_enabled = !self.monitor_loop_enabled;
                }

                ui.menu_button("Preview",|ui|{
                    if let Some(engine)=self.engine.as_ref() {
                        let status=engine.session().preview_status();
                        let mut profile=status.profile;
                        use photonic_video::preview::PreviewCodec;
                        ui.label("Preview format · Full resolution");
                        for (codec,label) in [(PreviewCodec::IntraH264,"H.264 · opaque"),(PreviewCodec::IntraProResLike,"ProRes · alpha"),(PreviewCodec::Lossless,"VP9 lossless mode · alpha")] {
                            ui.selectable_value(&mut profile.codec,codec,label);
                        }
                        ui.add(egui::DragValue::new(&mut profile.quality).range(0..=51).prefix("Quality "));
                        if profile.codec==PreviewCodec::Lossless {ui.small("Uses an 8-bit YUV conversion with alpha.");}
                        if profile!=status.profile && !engine.session().send(photonic_video::EngineCmd::SetPreviewProfile(profile)) {
                            self.file_status=Some("Preview format request rejected: engine busy.".into());
                        }
                        if let Some(error)=&status.error {ui.colored_label(ui.visuals().error_fg_color,error);}
                        ui.separator();
                    }
                    for (label,id) in [("Add zone from In/Out","video.add_preview_zone"),("Remove zone in In/Out","video.remove_preview_zone"),("Remove all zones","video.remove_all_preview_zones"),("Render preview","video.render_preview"),("Stop preview render","video.stop_preview_render")] {
                        if ui.button(label).clicked(){self.video_preview_action(doc,history,id);ui.close_menu();}
                    }
                });
                ui.separator();
                // Single-monitor mode badge (24 §3.3): SOURCE vs SEQUENCE.
                let badge = self
                    .engine
                    .as_ref()
                    .map(|b| match &b.status().preview_target {
                        PreviewTarget::Asset { .. } => "SOURCE",
                        PreviewTarget::Sequence { .. } => "SEQUENCE",
                    })
                    .unwrap_or("SEQUENCE");
                ui.label(
                    egui::RichText::new(badge)
                        .small()
                        .strong()
                        .color(egui::Color32::from_rgb(0x7a, 0x9e, 0xb8)),
                )
                .on_hover_text(
                    "Single monitor — SEQUENCE is the timeline; SOURCE is a media-pool peek",
                );

                if self.io_targets_source_marks() {
                    let auditioning = self.engine.as_ref().is_some_and(|engine|engine.status().source_audition.as_ref().is_some_and(|a|a.playing));
                    if ui.button(if auditioning {"Stop source"} else {"Audition source"}).clicked() {
                        if auditioning {self.source_panel_command(doc,crate::panels::video::source_monitor::SourceCommand::Stop);}
                        else {self.video_audition_source(doc);}
                    }
                }
                ui.separator();
                // Prominent current / total timecode readout (pro-NLE feel).
                // When SOURCE peaking: source clock (G-10); else sequence playhead.
                let fr = active_frame_rate(doc);
                let start_tc = active_sequence(doc)
                    .map(|s| s.start_timecode)
                    .unwrap_or(Tick::ZERO);
                let source_io = self.io_targets_source_marks();
                let (now, end) = if source_io {
                    (
                        self.source_marks.source_time,
                        self.armed_asset_duration(doc)
                            .unwrap_or(self.source_marks.source_time),
                    )
                } else {
                    (self.playhead, sequence_end_tick(doc))
                };
                // Sequence labels include start_timecode; source peek is absolute.
                let tc_start = if source_io { Tick::ZERO } else { start_tc };
                ui.label(
                    egui::RichText::new(format_timecode_with_start(fr, now, tc_start))
                        .monospace()
                        .size(16.0)
                        .strong()
                        .color(egui::Color32::from_rgb(0x9d, 0x8c, 0xf5)),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "/ {}",
                        format_timecode_with_start(fr, end, tc_start)
                    ))
                    .monospace()
                    .weak(),
                );

                ui.separator();
                let i_tip = if source_io {
                    "Set Source In (I) — marks for 3-point Insert/Overwrite"
                } else {
                    "Set Work In (I) — sequence work range"
                };
                let o_tip = if source_io {
                    "Set Source Out (O) — marks for 3-point Insert/Overwrite"
                } else {
                    "Set Work Out (O) — sequence work range"
                };
                if ui.button("I").on_hover_text(i_tip).clicked() {
                    self.video_set_in(doc, history);
                }
                if ui.button("O").on_hover_text(o_tip).clicked() {
                    self.video_set_out(doc, history);
                }
                if source_io {
                    if ui
                        .button(ph::X)
                        .on_hover_text("Clear source marks")
                        .clicked()
                    {
                        self.video_clear_source_marks();
                    }
                    // Show active source range on the transport.
                    if self.source_marks.mark_in.is_some() || self.source_marks.mark_out.is_some() {
                        let fr = active_frame_rate(doc);
                        let a = self
                            .source_marks
                            .mark_in
                            .map(|t| format_timecode(fr, t))
                            .unwrap_or_else(|| "—".into());
                        let b = self
                            .source_marks
                            .mark_out
                            .map(|t| format_timecode(fr, t))
                            .unwrap_or_else(|| "—".into());
                        ui.label(
                            egui::RichText::new(format!("Src {a}–{b}"))
                                .monospace()
                                .small()
                                .weak(),
                        );
                    }
                }

                ui.separator();
                if ui
                    .button(ph::BOOKMARK_SIMPLE)
                    .on_hover_text("Add Marker at Playhead (M)")
                    .clicked()
                {
                    self.timeline_add_marker_at_playhead(doc, history);
                }

                ui.separator();
                if ui
                    .selectable_label(self.monitor_safe_area, ph::CROP)
                    .on_hover_text("Safe-area guides")
                    .clicked()
                {
                    self.monitor_safe_area = !self.monitor_safe_area;
                }
                // K-B17: alpha as luminance — judge a key against something
                // other than black. Toggles the present channel on the engine
                // bridge (view state, not an undo unit).
                {
                    let alpha_on = self
                        .engine
                        .as_ref()
                        .map(|e| e.alpha_view())
                        .unwrap_or(false);
                    if ui
                        .selectable_label(alpha_on, "α")
                        .on_hover_text("Alpha view — show the alpha channel as luminance (K-B17)")
                        .clicked()
                    {
                        if let Some(eng) = self.engine.as_mut() {
                            eng.toggle_alpha_view();
                        }
                    }
                }
                // K-B5: vertical wipe — clean (no clip looks) | graded.
                {
                    let cmp_on = self
                        .engine
                        .as_ref()
                        .map(|e| e.compare_effects)
                        .unwrap_or(false);
                    if ui
                        .selectable_label(cmp_on, "A|B")
                        .on_hover_text(
                            "Compare effects — left clean (no clip effects/grade), right graded; drag the divider (K-B5)",
                        )
                        .clicked()
                    {
                        if let Some(eng) = self.engine.as_mut() {
                            eng.toggle_compare_effects();
                        }
                    }
                }
                {
                    let grid_label = match self.monitor_comp_grid {
                        1 => "⅓",
                        2 => "φ",
                        _ => "▦",
                    };
                    let tip = match self.monitor_comp_grid {
                        1 => "Composition grid: rule of thirds (click to cycle)",
                        2 => "Composition grid: golden ratio (click to cycle)",
                        _ => "Composition grid: off (click for thirds)",
                    };
                    if ui
                        .selectable_label(self.monitor_comp_grid > 0, grid_label)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        self.monitor_comp_grid = (self.monitor_comp_grid + 1) % 3;
                    }
                }
                if ui
                    .selectable_label(self.monitor_transform_tool, ph::ARROWS_OUT_CARDINAL)
                    .on_hover_text("Transform tool — reposition/scale/rotate the selected clip")
                    .clicked()
                {
                    self.monitor_transform_tool = !self.monitor_transform_tool;
                }
            });
        });
    }

    /// Program-monitor scrub bar (NLE parity QW-5): a thin draggable position
    /// strip under the picture, spanning the whole sequence. Click/drag seeks
    /// the single shared `self.playhead` (frame-snapped, like the timeline
    /// ruler) — writing here updates the same playhead the ruler reads, so
    /// there is never a second playhead.
    fn draw_monitor_scrubber(&mut self, ui: &mut egui::Ui, rect: egui::Rect, doc: &Document) {
        let end = sequence_end_tick(doc);
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();

        // A centered groove with a small horizontal margin so the playhead
        // handle never clips at the extremes.
        const MARGIN_X: f32 = 6.0;
        let track_left = rect.left() + MARGIN_X;
        let track_width = (rect.width() - 2.0 * MARGIN_X).max(1.0);
        let cy = rect.center().y;
        let groove = egui::Rect::from_min_max(
            egui::pos2(track_left, cy - 2.0),
            egui::pos2(track_left + track_width, cy + 2.0),
        );
        painter.rect_filled(groove, 2.0, visuals.faint_bg_color);

        // No content yet → an inert groove (nothing to scrub).
        if end.0 <= 0 {
            return;
        }

        let accent = visuals.selection.stroke.color;
        let head_x = scrub_tick_to_x(self.playhead, track_left, track_width, end);
        let filled = egui::Rect::from_min_max(
            egui::pos2(track_left, cy - 2.0),
            egui::pos2(head_x, cy + 2.0),
        );
        painter.rect_filled(filled, 2.0, accent);
        painter.circle_filled(egui::pos2(head_x, cy), 5.0, accent);

        // Click/drag anywhere on the strip seeks (frame-snapped).
        let resp = ui.interact(
            rect,
            ui.id().with("monitor_scrubber"),
            egui::Sense::click_and_drag(),
        );
        if resp.clicked() || resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let fr = active_frame_rate(doc);
                self.playhead = scrub_x_to_tick(pos.x, track_left, track_width, end, fr);
            }
        }
        if resp.hovered() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
        }
    }

    /// Video-mode keyboard dispatch (04 §5.1) — the sibling block to the
    /// vector-canvas space-pan/arrow-nudge/WASD-pan input at the resolution
    /// rule in §5.2: mutually exclusive by construction because this is only
    /// ever called from [`Self::draw_video_monitor`], which the central-panel
    /// branch (04 §1.1 point 3) only reaches when `self.mode == Video` — the
    /// vector blocks are simply never reached that frame (`app/mod.rs`'s
    /// central-panel `if self.mode == AppMode::Video { ...; return; }`).
    fn handle_video_keyboard(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        if self.precision_trim.is_some() {
            return;
        }
        if !viewport_kb(ctx) {
            return;
        }
        // K-A7: while a keyboard grab is active, arrow / edit-point keys belong
        // to the grab session (handled in `handle_timeline_shortcuts`) — do not
        // step the playhead or jump edit points underneath a move preview.
        let grab_active = self.timeline_grab.is_some();
        const KEYS: &[commands::CommandId] = &[
            "video.play_pause",
            "video.play_reverse",
            "video.pause",
            "video.play_forward",
            "video.step_back",
            "video.step_forward",
            "video.prev_edit_point",
            "video.next_edit_point",
            "video.prev_snap",
            "video.next_snap",
            "video.set_in",
            "video.set_out",
            "video.split_at_playhead",
            "video.delete_clip",
            "video.ripple_delete",
            "video.copy",
            "video.cut",
            "video.paste",
            "video.add_marker",
            "video.add_bookmark",
            "video.insert_edit",
            "video.overwrite_edit",
            "video.lift_edit",
            "video.extract_edit",
            "video.extract_frame",
            "video.extract_frame_to_bin",
            "video.toggle_razor",
            "video.toggle_snap",
            "video.zoom_in",
            "video.zoom_out",
            "video.zoom_fit",
            "video.playhead_home",
            "video.playhead_end",
            "video.grab_item",
        ];
        for &id in KEYS {
            if grab_active
                && matches!(
                    id,
                    "video.step_back"
                        | "video.step_forward"
                        | "video.prev_edit_point"
                        | "video.next_edit_point"
                )
            {
                continue;
            }
            // `video.grab_item` is also listed in `handle_timeline_shortcuts` —
            // only dispatch it here (central monitor path always runs in video
            // mode) so Shift+G never toggles twice in one frame.
            if self.binding_pressed(ctx, id) {
                self.dispatch_command(id, doc, history);
            }
        }
        // Backspace is a second hardwired delete accelerator alongside the
        // remappable `video.delete_clip`/`video.ripple_delete` (Delete /
        // Shift+Delete) — the keymap holds only one binding per command, so the
        // NLE-standard Backspace-to-delete is matched here like the `?` sheet
        // key below. Shift = ripple; other modifiers are ignored so it never
        // collides with a text edit or a chorded shortcut.
        let backspace_delete = ctx.input(|i| {
            i.key_pressed(egui::Key::Backspace)
                && !i.modifiers.ctrl
                && !i.modifiers.command
                && !i.modifiers.mac_cmd
                && !i.modifiers.alt
        });
        if backspace_delete {
            let ripple = ctx.input(|i| i.modifiers.shift);
            let id = if ripple {
                "video.ripple_delete"
            } else {
                "video.delete_clip"
            };
            self.dispatch_command(id, doc, history);
        }
        // `?` opens the shortcut sheet (04 §1.2) — not a rebindable CommandId,
        // matching the existing Escape/palette-open pattern of a few hardcoded
        // global keys elsewhere in this file.
        let opens_sheet = ctx.input(|i| {
            i.key_pressed(egui::Key::Questionmark)
                || (i.modifiers.shift && i.key_pressed(egui::Key::Slash))
        });
        if opens_sheet {
            self.show_video_shortcut_sheet = true;
        }
    }
}

/// Map a tick in `[0, end]` to an x within `[left, left + width]` for the
/// monitor scrub bar (QW-5). Clamps to the track; `end <= 0` pins to the left.
fn scrub_tick_to_x(t: Tick, left: f32, width: f32, end: Tick) -> f32 {
    if end.0 <= 0 {
        return left;
    }
    let frac = (t.0 as f32 / end.0 as f32).clamp(0.0, 1.0);
    left + frac * width
}

/// Map a pointer x to a frame-snapped tick in `[0, end]` for the monitor scrub
/// bar (QW-5). Inverse of [`scrub_tick_to_x`], then snapped to the frame grid.
fn scrub_x_to_tick(x: f32, left: f32, width: f32, end: Tick, fr: FrameRate) -> Tick {
    if width <= 0.0 || end.0 <= 0 {
        return Tick::ZERO;
    }
    let frac = ((x - left) / width).clamp(0.0, 1.0);
    let raw = Tick((frac as f64 * end.0 as f64).round() as i64);
    fr.snap(raw).clamp(Tick::ZERO, end)
}

/// K-E3 composition grids: rule of thirds (`mode=1`) or golden ratio (`mode=2`).
fn draw_composition_grid(painter: &egui::Painter, video_rect: egui::Rect, mode: u8) {
    let stroke = egui::Stroke::new(
        1.0,
        egui::Color32::from_rgba_unmultiplied(220, 220, 220, 90),
    );
    let (fx, fy) = match mode {
        2 => (0.382_f32, 0.618_f32), // golden section
        _ => (1.0 / 3.0, 2.0 / 3.0), // thirds
    };
    let xs = [
        video_rect.left() + video_rect.width() * fx,
        video_rect.left() + video_rect.width() * fy,
    ];
    let ys = [
        video_rect.top() + video_rect.height() * fx,
        video_rect.top() + video_rect.height() * fy,
    ];
    for x in xs {
        painter.line_segment(
            [
                egui::pos2(x, video_rect.top()),
                egui::pos2(x, video_rect.bottom()),
            ],
            stroke,
        );
    }
    for y in ys {
        painter.line_segment(
            [
                egui::pos2(video_rect.left(), y),
                egui::pos2(video_rect.right(), y),
            ],
            stroke,
        );
    }
}

/// Action-safe (90%) / title-safe (80%) guide rectangles (04 §3.3).
fn draw_safe_area_guides(painter: &egui::Painter, video_rect: egui::Rect) {
    let action_safe = video_rect.shrink2(video_rect.size() * 0.05);
    let title_safe = video_rect.shrink2(video_rect.size() * 0.10);
    painter.rect_stroke(
        action_safe,
        0.0,
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 110),
        ),
    );
    painter.rect_stroke(
        title_safe,
        0.0,
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 200, 80, 130),
        ),
    );
}

// ── First-run hints (04 §1.2) + social coach (proposal 213 / 43 §2.6) ───────

impl PhotonicApp {
    /// Three-step coach: Import → Split → Export (proposal 213 §4).
    /// Dismissible; reset from Preferences → Behavior.
    pub(crate) fn draw_video_coach_marks(&mut self, ctx: &egui::Context, doc: &Document) {
        if self.prefs.video_coach_dismissed
            || !self.coach_allowed_this_session
            || self.mode != AppMode::Video
        {
            return;
        }
        // Burn the persisted "first run" bit the moment the card is about to be
        // drawn, not when it is dismissed. Quitting mid-guide used to leave the
        // bit unset, so the guide reappeared on every launch; recording it here
        // makes "first time" mean the first launch that actually showed it.
        if !self.prefs.video_coach_shown_once {
            self.prefs.video_coach_shown_once = true;
            self.prefs.save();
        }
        // Reduced motion: still show the card (static), just no tween elsewhere.
        let mut step = self.prefs.video_coach_step.min(2);
        let (title, body, next_label) = match step {
            0 => (
                "1 · Import",
                "Import a clip (timeline Import, Media panel, or drop files). \
                 Auto-place is on by default so it lands on the timeline.",
                "Next",
            ),
            1 => (
                "2 · Split & trim",
                "Select a clip, press S to split at the playhead, or use Del / Shift+Del \
                 to delete or ripple-delete. Drag edges to trim.",
                "Next",
            ),
            _ => (
                "3 · Export",
                "Click Export on the timeline toolbar. Pick Social 9:16 or Social 16:9 \
                 for a one-click social deliverable.",
                "Done",
            ),
        };
        let has_clips = sequence_has_clips(doc);
        let advanced = crate::preferences::coach_auto_advance_on_clips(step, has_clips);
        if advanced != step {
            step = advanced;
            self.prefs.video_coach_step = step;
            self.prefs.save();
        }

        egui::Area::new(egui::Id::new("video_coach_marks"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::same(12.0))
                    .show(ui, |ui| {
                        ui.set_max_width(280.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Social cut").strong().small());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!("{}/3", step + 1))
                                            .weak()
                                            .small(),
                                    );
                                },
                            );
                        });
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(title).strong());
                        ui.label(body);
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            // Skip and Next are peers, so they get one shared
                            // size — the old `small_button` made Skip visibly
                            // shorter than its neighbour.
                            let btn = egui::vec2(72.0, ui.spacing().interact_size.y);
                            if ui.add_sized(btn, egui::Button::new(next_label)).clicked() {
                                let (new_step, dismissed) =
                                    crate::preferences::coach_advance_button(step);
                                if dismissed {
                                    self.prefs.video_coach_dismissed = true;
                                } else {
                                    self.prefs.video_coach_step = new_step;
                                }
                                self.prefs.save();
                            }
                            if ui.add_sized(btn, egui::Button::new("Skip")).clicked()
                                && crate::preferences::coach_skip_dismisses()
                            {
                                self.prefs.video_coach_dismissed = true;
                                self.prefs.save();
                            }
                        });
                    });
            });
    }

    /// One-time toolbar callout pointing at the Video toggle, anchored below
    /// its rect. Persisted-dismissed via `prefs.video_hint_dismissed`.
    pub(crate) fn draw_video_hint_callout(&mut self, ctx: &egui::Context, anchor: egui::Rect) {
        if self.prefs.video_hint_dismissed || self.mode == AppMode::Video {
            return;
        }
        egui::Area::new(egui::Id::new("video_mode_hint_callout"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor.left_bottom() + egui::vec2(0.0, 6.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(240.0);
                    ui.label("New: edit video timelines — click here or Ctrl+Shift+V");
                    if ui.small_button("Got it").clicked() {
                        self.prefs.video_hint_dismissed = true;
                        self.prefs.save();
                    }
                });
            });
    }

    /// One-time shortcut-sheet overlay (04 §1.2), auto-opened on first video-
    /// mode entry and re-openable via `?` thereafter.
    pub(crate) fn draw_video_shortcut_sheet(&mut self, ctx: &egui::Context) {
        if !self.show_video_shortcut_sheet {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_video_shortcut_sheet = false;
            return;
        }
        let mut open = true;
        egui::Window::new("Video Mode Shortcuts")
            .id(egui::Id::new("video_shortcut_sheet"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(300.0);
                egui::Grid::new("video_shortcut_grid")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        // Kept in sync with the `video.*` default bindings in
                        // `commands.rs` — verify there before editing here.
                        const ROWS: &[(&str, &str)] = &[
                            ("Space", "Play / Pause"),
                            ("J / K / L", "Play reverse / Pause / Play forward"),
                            ("← / →", "Step one frame"),
                            ("Shift+← / Shift+→", "Previous / next edit point"),
                            ("I / O", "Set in / out point"),
                            ("Alt+Space", "Audition source audio"),
                            ("Shift+T", "Precision trim"),
                            ("Alt+← / Alt+→", "Back / enter nested sequence"),
                            ("S", "Split clip at playhead"),
                            ("Del / Backspace", "Delete selected clip (Shift = ripple)"),
                            ("Ctrl+C / X / V", "Copy / cut / paste clip"),
                            ("M", "Add marker at playhead"),
                            ("B", "Add bookmark at playhead"),
                            ("N", "Toggle snapping"),
                            ("C", "Razor (blade) tool"),
                            ("?", "Show this sheet again"),
                        ];
                        for (key, desc) in ROWS {
                            ui.strong(*key);
                            ui.label(*desc);
                            ui.end_row();
                        }
                    });
            });
        if !open {
            self.show_video_shortcut_sheet = false;
        }
    }
}

#[cfg(test)]
mod scrubber_tests {
    use super::*;

    #[test]
    fn tick_to_x_maps_endpoints_and_midpoint() {
        // 0 → left, end → right, half → middle.
        assert_eq!(scrub_tick_to_x(Tick(0), 100.0, 200.0, Tick(1000)), 100.0);
        assert_eq!(scrub_tick_to_x(Tick(1000), 100.0, 200.0, Tick(1000)), 300.0);
        assert_eq!(scrub_tick_to_x(Tick(500), 100.0, 200.0, Tick(1000)), 200.0);
    }

    #[test]
    fn tick_to_x_clamps_out_of_range_and_handles_empty() {
        // Past the end clamps to the right edge; empty sequence pins to left.
        assert_eq!(scrub_tick_to_x(Tick(5000), 0.0, 200.0, Tick(1000)), 200.0);
        assert_eq!(scrub_tick_to_x(Tick(500), 40.0, 200.0, Tick(0)), 40.0);
    }

    #[test]
    fn x_to_tick_is_frame_snapped_and_clamped() {
        let fr = FrameRate::FPS_30;
        // One second of content.
        let end = Tick(30 * TICKS_PER_SECOND);
        // Clicking left of the track clamps to 0.
        assert_eq!(scrub_x_to_tick(-50.0, 0.0, 300.0, end, fr), Tick::ZERO);
        // Clicking past the right edge clamps to the (frame-snapped) end.
        let right = scrub_x_to_tick(9999.0, 0.0, 300.0, end, fr);
        assert!(right <= end);
        assert_eq!(right, fr.snap(right), "result must sit on a frame boundary");
        // A mid-track click snaps to a frame boundary and stays in range.
        let mid = scrub_x_to_tick(150.0, 0.0, 300.0, end, fr);
        assert_eq!(mid, fr.snap(mid));
        assert!(mid > Tick::ZERO && mid < end);
    }

    #[test]
    fn x_to_tick_handles_degenerate_track_width() {
        let fr = FrameRate::FPS_30;
        assert_eq!(scrub_x_to_tick(10.0, 0.0, 0.0, Tick(1000), fr), Tick::ZERO);
    }
}

/// Transport intent (04 §3.2 / §5.1) — pure GUI flags the engine reconciler reads.
#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn play_pause_j_k_l_toggle_intent() {
        let mut app = PhotonicApp::default();
        assert!(!app.monitor_playing);

        app.video_play_pause();
        assert!(app.monitor_playing);
        assert!(!app.monitor_play_reverse);
        assert_eq!(app.monitor_play_speed, 1.0);

        app.video_play_reverse();
        assert!(app.monitor_playing);
        assert!(app.monitor_play_reverse);

        app.video_play_forward();
        assert!(app.monitor_playing);
        assert!(!app.monitor_play_reverse);

        app.video_pause();
        assert!(!app.monitor_playing);

        // Repeated L ramps speed while already playing forward.
        app.video_play_forward();
        app.video_play_forward();
        assert_eq!(app.monitor_play_speed, 2.0);
    }

    #[test]
    fn step_pauses_and_moves_playhead_by_one_frame() {
        let mut app = PhotonicApp::default();
        let doc = Document::new("t", 1920.0, 1080.0);
        app.monitor_playing = true;
        app.playhead = Tick::from_seconds(1);
        let before = app.playhead;

        app.video_step_forward(&doc);
        assert!(!app.monitor_playing);
        assert!(app.playhead.0 > before.0);

        let mid = app.playhead;
        app.video_step_back(&doc);
        assert!(!app.monitor_playing);
        assert!(app.playhead.0 < mid.0);
        assert!(app.playhead.0 >= 0);
    }

    #[test]
    fn playhead_home_seeks_to_zero_and_pauses() {
        let mut app = PhotonicApp::default();
        app.playhead = Tick::from_seconds(5);
        app.monitor_playing = true;
        app.video_playhead_home();
        assert_eq!(app.playhead, Tick::ZERO);
        assert!(!app.monitor_playing);
    }
}

#[cfg(test)]
mod master_meter_tests {
    use super::*;

    #[test]
    fn db_to_frac_maps_ends_and_clamps() {
        assert_eq!(master_meter_db_to_frac(MASTER_METER_FLOOR_DB), 0.0);
        assert_eq!(master_meter_db_to_frac(MASTER_METER_CEIL_DB), 1.0);
        assert_eq!(master_meter_db_to_frac(MASTER_METER_FLOOR_DB - 40.0), 0.0);
        assert_eq!(master_meter_db_to_frac(MASTER_METER_CEIL_DB + 40.0), 1.0);
    }

    #[test]
    fn meter_color_walks_the_gradient() {
        assert_eq!(master_meter_color(0.0), MASTER_METER_LOW);
        assert_eq!(master_meter_color(0.72), MASTER_METER_MID);
        assert_eq!(master_meter_color(1.0), MASTER_METER_HIGH);
        // Monotonically loses green as it climbs toward clip.
        assert!(master_meter_color(0.0).g() >= master_meter_color(0.72).g());
        assert!(master_meter_color(0.72).g() >= master_meter_color(1.0).g());
    }

    #[test]
    fn lin_to_db_floors_near_silence_without_producing_inf_or_nan() {
        let db = master_meter_lin_to_db(0.0);
        assert!(db.is_finite());
        assert!(
            db < MASTER_METER_FLOOR_DB,
            "near-zero gain reads well below floor"
        );
        // Unity gain is 0 dB.
        assert!((master_meter_lin_to_db(1.0)).abs() < 1e-3);
    }

    #[test]
    fn channel_peak_attacks_instantly_and_releases_gradually() {
        let mut ch = MasterMeterChannel::default();
        ch.update(-3.0, -3.0, 1.0);
        assert!((ch.peak_db - (-3.0)).abs() < 1e-3, "instant peak attack");
        // A quieter block afterwards releases at ~40 dB/s, not instantly.
        ch.update(MASTER_METER_FLOOR_DB, MASTER_METER_FLOOR_DB, 0.05);
        assert!(ch.peak_db < -3.0 && ch.peak_db > -3.0 - 40.0 * 0.05 - 1e-3);
    }

    #[test]
    fn channel_clip_latches_until_explicitly_cleared() {
        let mut ch = MasterMeterChannel::default();
        assert!(!ch.clip);
        ch.update(0.0, 0.0, 0.02);
        assert!(ch.clip, "level above the clip threshold latches the LED");
        // Dropping back to silence does not self-clear the latch.
        ch.update(MASTER_METER_FLOOR_DB, MASTER_METER_FLOOR_DB, 1.0);
        assert!(ch.clip);
        ch.clip = false;
        assert!(!ch.clip);
    }

    #[test]
    fn state_with_no_tap_settles_at_the_floor() {
        let mut state = MasterMeterState::default();
        for _ in 0..50 {
            state.update(None, 0.05);
        }
        assert!(!state.clipped());
        assert!((state.peak_db() - MASTER_METER_FLOOR_DB).abs() < 1.0);
    }

    #[test]
    fn is_animating_gates_repaints_on_movement_not_idle_or_latched_clip() {
        // A fresh, silent meter is static → no continuous repaint wanted.
        let mut state = MasterMeterState::default();
        assert!(
            !state.is_animating(),
            "a meter parked at the floor must let the monitor idle"
        );

        // A live (clipping) level makes it move → wants frames.
        state.update(
            Some(engine::MasterLevel {
                peak: [1.0, 1.0],
                rms: [0.4, 0.4],
            }),
            0.05,
        );
        assert!(state.is_animating(), "a moving meter must keep repainting");

        // After long silence it releases back to the floor and goes static
        // again — even with the clip LED still latched, which must NOT by
        // itself pin the monitor at 60fps.
        for _ in 0..200 {
            state.update(None, 0.05);
        }
        assert!(state.clipped(), "the loud block latched the clip LED");
        assert!(
            !state.is_animating(),
            "a settled meter idles even while a clip stays latched"
        );
    }

    #[test]
    fn state_tracks_channels_independently_and_reports_the_louder_peak() {
        let mut state = MasterMeterState::default();
        state.update(
            Some(engine::MasterLevel {
                peak: [0.5, 0.1],
                rms: [0.4, 0.05],
            }),
            1.0,
        );
        assert!(state.l.peak_db > state.r.peak_db);
        assert_eq!(state.peak_db(), state.l.peak_db);
    }
}
