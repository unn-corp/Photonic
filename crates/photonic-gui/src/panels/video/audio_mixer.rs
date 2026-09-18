//! `RightDrawerGroup::AudioMixer` panel (04 §4.1) — channel strips per audio
//! track + a master strip: vertical dB fader, equal-power pan knob, dual
//! peak/RMS meter with clip LED, mute/solo (solo-safe), a per-strip `AudioFxUnit`
//! rack with kind-specific editors, and expandable automation lanes, plus a
//! prominent stereo (L/R) peak+RMS master-bus output meter with clip LEDs
//! (Gap M-7, [`master_output_meter`]). Normative UI: 09 §8 + 13 §11. Interior
//! owned by 09-audio-mixer.md.
//!
//! ## Wiring seam (reported, not silently faked)
//!
//! This panel is a **right-rail** drawer, dispatched from `app/mod.rs` as
//! `draw_audio_mixer(ui, &mut VideoPanelUi)` (built by `video_panel_ui()`).
//! [`VideoPanelUi`] threads only *session* state — it carries **no**
//! `&mut Document` (the `Sequence.audio_tracks` list + `MasterBus` + every
//! `AudioFxUnit`), **no** `&mut CommandHistory` (to record undoable
//! [`AudioCmd`]s), and **no** engine handle. Changing this function's signature
//! to add them would require editing the `app/mod.rs` dispatch call site, which
//! is out of this story's territory and would break `cargo build --workspace` if
//! left uncommitted. Two further layers are missing beneath the GUI:
//!
//! 1. `EngineSession`/`EngineStatus` (02 §1) do **not** surface `audio::mixer`'s
//!    `StereoMeter` taps to the GUI — `get_audio_meters` (10 §3.12 / 13 §11.6)
//!    does not exist yet, so live fader/master meters cannot be polled. This
//!    also covers Gap M-7 (the master-bus output meter, [`master_output_meter`]):
//!    it is built prominent/stereo/peak+RMS/clip-LED per spec and reads the
//!    best-available value (the panned, ballistics-smoothed synthetic level
//!    already driving every other meter here), but the real tap it wants —
//!    `Mixer::output_meter()`'s `Arc<StereoMeter>`, sampled post
//!    `MasterBusParams.volume_db` — is unreachable for the same reason: no
//!    engine handle reaches this function.
//! 2. `audio::mixer`'s `fx_chain` is *an inert pass-through* (see that module's
//!    doc + `audio/dsp/mod.rs`): the DSP units in `audio/dsp` are not connected
//!    into the mixer graph, so added fx are not audible yet (the audio-dsp-wiring
//!    concern this story was told to consume + report).
//!
//! Until that seam is closed the strips render against a **local preview model**
//! held in egui temp memory, clearly banner-labelled, and meters show a
//! *simulated* idle signal so the widgets are reviewable by the later
//! visual-feedback pass. **Every mutation is nevertheless expressed as a real
//! [`AudioCmd`]** and routed through the single [`commit`] sink — the exact,
//! one-line swap point: when `doc`/`history` are threaded in, [`commit`]'s body
//! becomes `history.execute_discrete(Command::Timeline(TimelineCmd::AudioEdit(
//! cmd)), doc)` and the model is sourced from `sequence.audio_tracks`, with no
//! change to any widget. The one real session field this panel *does* own,
//! [`VideoPanelUi::mixer_expanded_tracks`], is wired live (per-strip automation
//! disclosure).

use std::collections::HashSet;

use egui::{pos2, vec2, Color32, Key, Rect, Response, Rounding, Sense, Stroke, Ui, Vec2};
use egui_phosphor::regular as ph;
use photonic_core::timeline::{
    AudioCmd, AudioFxKind, AudioFxUnit, EffectParams, FxOwner, MasterBus, MasterBusParams,
    PropValue, TrackAudio, TrackAudioParams, TrackId,
};

use super::VideoPanelUi;

// ── Scale constants (09 §2/§8) ──────────────────────────────────────────────

/// Fader/meter floor rendered as `-inf` (09 §2: `volume_db` mute floor).
const FLOOR_DB: f64 = -60.0;
/// Fader/meter ceiling (09 §2: `+12`).
const CEIL_DB: f64 = 12.0;
/// Clip-indicator LED latch threshold (13 §11.1: above -0.3 dBTP).
const CLIP_DB: f32 = -0.3;

const FADER_H: f32 = 116.0;
const METER_W: f32 = 9.0;
const STRIP_W: f32 = 88.0;
/// Master-bus output meter (Gap M-7) bar height — taller than [`FADER_H`]'s
/// per-track meters so the final mix level is prominent at a glance.
const MASTER_METER_H: f32 = FADER_H + 24.0;

// Meter three-stop gradient (13 §11.7 — the one DESIGN.md exception where a
// status gradient is appropriate, since a meter is a continuous status signal):
// success(low) → warning(near-clip) → error(clip).
const METER_LOW: Color32 = Color32::from_rgb(100, 200, 122); // #64C87A success
const METER_MID: Color32 = Color32::from_rgb(251, 191, 36); // #FBBF24 warning
const METER_HIGH: Color32 = Color32::from_rgb(248, 113, 113); // #F87171 error

const ALL_FX_KINDS: [AudioFxKind; 4] = [
    AudioFxKind::Eq,
    AudioFxKind::Compressor,
    AudioFxKind::Gate,
    AudioFxKind::Limiter,
];

// ═══════════════════════════════════════════════════════════════════════════
// Pure logic (unit-tested) — the spec-normative math, no egui.
// ═══════════════════════════════════════════════════════════════════════════

/// Equal-power pan law (09 §4): `pan` -1.0(L)..1.0(R) → `(gain_l, gain_r)`.
/// Center (pan=0) is -3 dB per side (`0.707`), summing to unit *power*.
fn pan_law(pan: f64) -> (f32, f32) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f64::consts::FRAC_PI_4; // 0..π/2
    (angle.cos() as f32, angle.sin() as f32)
}

/// Fraction 0..1 of a dB value across `[floor, ceil]` — linear in dB, matching
/// egui's native `Slider` handle position over the same range, so the fader and
/// its neighbouring meter share one scale.
fn db_to_frac(db: f32, floor: f32, ceil: f32) -> f32 {
    ((db - floor) / (ceil - floor)).clamp(0.0, 1.0)
}

/// Meter fill color at fill `frac` (13 §11.7 gradient). Knee at ~0.72 so the
/// bar reads green through nominal, amber approaching clip, red at the top.
fn meter_color(frac: f32) -> Color32 {
    let f = frac.clamp(0.0, 1.0);
    if f < 0.72 {
        lerp_color(METER_LOW, METER_MID, f / 0.72)
    } else {
        lerp_color(METER_MID, METER_HIGH, (f - 0.72) / 0.28)
    }
}

/// Solo-safe mute/solo resolution (09 §4). Input: one `(mute, solo)` per strip
/// in track order. Any solo active ⇒ only soloed *and* un-muted strips are
/// audible (mute wins over solo); no solo ⇒ ordinary mute-only gating.
fn resolve_audible(strips: &[(bool, bool)]) -> Vec<bool> {
    let any_solo = strips.iter().any(|&(_, solo)| solo);
    strips
        .iter()
        .map(|&(mute, solo)| if any_solo { solo && !mute } else { !mute })
        .collect()
}

/// Format a fader/gain dB value, folding the floor to `-inf`.
fn fmt_db(db: f64) -> String {
    if db <= FLOOR_DB + 0.05 {
        "-inf".to_string()
    } else {
        format!("{db:+.1}")
    }
}

/// Format a pan value as `C` / `L nn` / `R nn` (0..100).
fn fmt_pan(pan: f64) -> String {
    let p = (pan.clamp(-1.0, 1.0) * 100.0).round() as i32;
    match p {
        0 => "C".to_string(),
        n if n < 0 => format!("L{}", -n),
        n => format!("R{n}"),
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Linear amplitude gain → dB, floored so a near-zero gain (e.g. a hard-panned
/// channel's opposite side) doesn't produce `-inf`/NaN through `log10`. Used
/// to split a mono synthetic level across L/R by the real equal-power pan
/// gain (09 §4) when driving the master output meter's stereo image.
fn lin_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-6).log10()
}

/// Peak/RMS meter ballistics + clip latch (13 §11.1/§11.4). Fed a per-block
/// level in dB each frame; models fast-peak attack, slow release, ~300ms RMS,
/// and a peak-hold marker.
#[derive(Clone)]
struct MeterState {
    peak_db: f32,
    rms_db: f32,
    hold_db: f32,
    hold_age: f32,
    /// Latched red until reset by a click (13 §11.4).
    clip: bool,
}

impl Default for MeterState {
    fn default() -> Self {
        MeterState {
            peak_db: FLOOR_DB as f32,
            rms_db: FLOOR_DB as f32,
            hold_db: FLOOR_DB as f32,
            hold_age: 0.0,
            clip: false,
        }
    }
}

impl MeterState {
    fn update(&mut self, level_db: f32, dt: f32) {
        let dt = dt.clamp(0.0, 0.1);
        // Peak: instantaneous attack, ~40 dB/s release (fast VU-like ballistics).
        if level_db >= self.peak_db {
            self.peak_db = level_db;
        } else {
            self.peak_db = (self.peak_db - 40.0 * dt).max(level_db);
        }
        // RMS: one-pole toward level, ~300 ms window.
        let a = 1.0 - (-dt / 0.3).exp();
        self.rms_db += (level_db - self.rms_db) * a;
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
        if level_db > CLIP_DB {
            self.clip = true;
        }
    }
}

/// Two independent [`MeterState`] ballistics tracks (L/R) for the
/// master-bus output meter (Gap M-7: prominent, stereo, peak+RMS, clip LED).
/// `Mixer::output_meter()` (`photonic_video::audio::mixer`) is the real
/// engine-side tap this mirrors — see [`master_output_meter`]'s doc for why
/// this panel can't poll it yet.
#[derive(Clone, Default)]
struct StereoMeterState {
    l: MeterState,
    r: MeterState,
}

impl StereoMeterState {
    fn update(&mut self, level_l_db: f32, level_r_db: f32, dt: f32) {
        self.l.update(level_l_db, dt);
        self.r.update(level_r_db, dt);
    }

    /// Either channel currently latched red (13 §11.4) — drives the numeric
    /// peak readout's color; each bar's own LED is still the click target.
    fn clipped(&self) -> bool {
        self.l.clip || self.r.clip
    }

    /// The louder channel's instantaneous peak, for the numeric dB readout.
    fn peak_db(&self) -> f32 {
        self.l.peak_db.max(self.r.peak_db)
    }

    /// Coarse L/R average RMS, for the master strip's rolling-loudness
    /// readout (already labelled "approx — export authoritative" there).
    fn rms_avg_db(&self) -> f32 {
        (self.l.rms_db + self.r.rms_db) * 0.5
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Preview model (egui temp memory) — real core types, local until the seam.
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone)]
struct MixerStrip {
    track: TrackId,
    name: String,
    audio: TrackAudio,
    meter: MeterState,
}

/// Which fx editor is expanded, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FxSel {
    Track(usize, usize),
    Master(usize),
}

#[derive(Clone)]
struct PreviewMixer {
    strips: Vec<MixerStrip>,
    master: MasterBus,
    master_meter: StereoMeterState,
    open_fx: Option<FxSel>,
}

impl PreviewMixer {
    /// The canonical AS-2 trio (09 §9): Dialogue, Music (ducked by Dialogue),
    /// SFX + a master bus (seeded with its default Limiter, 09 §6.5).
    fn demo() -> Self {
        let dialogue = MixerStrip {
            track: TrackId::new(),
            name: "Dialogue".to_string(),
            audio: TrackAudio::new(),
            meter: MeterState::default(),
        };
        // Music strip with a ducking compressor sidechained to Dialogue (09 §6.3).
        let mut music_audio = TrackAudio::new();
        music_audio.params.base.volume_db = -3.0;
        let mut duck = AudioFxUnit::new(AudioFxKind::Compressor);
        duck.sidechain = Some(dialogue.track);
        music_audio.fx_chain.push(duck);
        let music = MixerStrip {
            track: TrackId::new(),
            name: "Music".to_string(),
            audio: music_audio,
            meter: MeterState::default(),
        };
        let mut sfx_audio = TrackAudio::new();
        sfx_audio.params.base.pan = 0.35;
        let sfx = MixerStrip {
            track: TrackId::new(),
            name: "SFX".to_string(),
            audio: sfx_audio,
            meter: MeterState::default(),
        };
        PreviewMixer {
            strips: vec![dialogue, music, sfx],
            master: MasterBus::new(),
            master_meter: StereoMeterState::default(),
            open_fx: None,
        }
    }
}

/// The single mutation sink. Today it applies `cmd` to the in-panel preview
/// model; when the right-rail dispatch threads `&mut Document` + `&mut
/// CommandHistory` (see module seam note), this body becomes
/// `history.execute_discrete(Command::Timeline(TimelineCmd::AudioEdit(cmd)),
/// doc)` and the model is read back from `sequence.audio_tracks` — no widget
/// changes. Kept a distinct fn so the swap is one edit and so the op→state
/// application is unit-testable against the real [`AudioCmd`] vocabulary.
fn commit(model: &mut PreviewMixer, cmd: AudioCmd) {
    let find = |m: &mut PreviewMixer, t: TrackId| m.strips.iter_mut().position(|s| s.track == t);
    match cmd {
        AudioCmd::SetTrackAudioProp { track, new, .. } => {
            if let Some(i) = find(model, track) {
                model.strips[i].audio.params.base = new;
            }
        }
        AudioCmd::SetTrackMuteSolo { track, new, .. } => {
            if let Some(i) = find(model, track) {
                model.strips[i].audio.mute = new.0;
                model.strips[i].audio.solo = new.1;
            }
        }
        AudioCmd::SetMasterBusProp { new, .. } => model.master.params.base = new,
        AudioCmd::AddAudioFx { owner, index, unit } => {
            if let Some(chain) = fx_chain_mut(model, owner) {
                let index = index.min(chain.len());
                chain.insert(index, unit);
            }
        }
        AudioCmd::RemoveAudioFx { owner, index, .. } => {
            if let Some(chain) = fx_chain_mut(model, owner) {
                if index < chain.len() {
                    chain.remove(index);
                }
            }
        }
        AudioCmd::ReorderAudioFx {
            owner, new_order, ..
        } => {
            if let Some(chain) = fx_chain_mut(model, owner) {
                if new_order.len() == chain.len() && new_order.iter().all(|&i| i < chain.len()) {
                    *chain = new_order.iter().map(|&i| chain[i].clone()).collect();
                }
            }
        }
        // Clip/fade/channel-map/loudness/ducking ops are surfaced by other
        // panels (clip overlays, export dialog) or not yet driven from this
        // strip UI; ignored by the preview sink.
        _ => {}
    }
}

fn fx_chain_mut(model: &mut PreviewMixer, owner: FxOwner) -> Option<&mut Vec<AudioFxUnit>> {
    match owner {
        FxOwner::Track(t) => model
            .strips
            .iter_mut()
            .find(|s| s.track == t)
            .map(|s| &mut s.audio.fx_chain),
        FxOwner::Master => Some(&mut model.master.fx_chain),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Panel entry
// ═══════════════════════════════════════════════════════════════════════════

/// Right-rail Audio Mixer drawer. Called directly from `app/mod.rs`'s
/// right-drawer match (no `PropPanelCtx`).
pub(crate) fn draw_audio_mixer(ui: &mut Ui, vid: &mut VideoPanelUi) {
    let id = ui.id().with("audio_mixer_preview");
    let mut model: PreviewMixer = ui
        .data_mut(|d| d.get_temp::<PreviewMixer>(id))
        .unwrap_or_else(PreviewMixer::demo);

    // Header + honest, tint-only seam banner (13 §11.7 / DESIGN "tint, never
    // fill"): live tracks/meters/fx are pending the doc/history/engine seam.
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Audio Mixer").strong());
    });
    ui.label(
        egui::RichText::new(
            "Preview — strips not yet bound to timeline audio tracks; meters simulated (wiring seam).",
        )
        .small()
        .color(ui.visuals().warn_fg_color),
    );
    ui.add_space(4.0);

    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let t = ui.input(|i| i.time) as f32;

    // Ballistics pass (before draw): non-audible strips fall to the floor, which
    // exercises `resolve_audible` live, not just in tests.
    let flags: Vec<(bool, bool)> = model
        .strips
        .iter()
        .map(|s| (s.audio.mute, s.audio.solo))
        .collect();
    let audible = resolve_audible(&flags);
    let mut master_l = FLOOR_DB as f32;
    let mut master_r = FLOOR_DB as f32;
    for (i, s) in model.strips.iter_mut().enumerate() {
        let lvl = if audible[i] {
            synthetic_level_db(i as f32, t, s.audio.params.base.volume_db)
        } else {
            FLOOR_DB as f32
        };
        s.meter.update(lvl, dt);
        // Split the (still-synthetic) level across L/R by each strip's real
        // equal-power pan gain (09 §4), so the master output meter's stereo
        // image at least tracks live pan/mute/solo control state honestly —
        // only the per-block level number itself is simulated (module doc's
        // wiring-seam note).
        let (gain_l, gain_r) = pan_law(s.audio.params.base.pan);
        master_l = master_l.max(lvl + lin_to_db(gain_l));
        master_r = master_r.max(lvl + lin_to_db(gain_r));
    }
    let headroom = model.master.params.base.volume_db as f32 + 2.0;
    let master_l = (master_l + headroom).min(CEIL_DB as f32);
    let master_r = (master_r + headroom).min(CEIL_DB as f32);
    model.master_meter.update(master_l, master_r, dt);
    // Keep meters animating while the drawer is visible.
    ui.ctx().request_repaint();

    let any_solo = flags.iter().any(|&(_, solo)| solo);
    let mut pending: Vec<AudioCmd> = Vec::new();

    let PreviewMixer {
        strips,
        master,
        master_meter,
        open_fx,
    } = &mut model;
    let expanded: &mut HashSet<TrackId> = vid.mixer_expanded_tracks;

    // Scroll on both axes and claim the host's full area: the strips are a
    // fixed-size rack, so whichever axis the window is short on has to scroll
    // rather than clip. `auto_shrink` off keeps the rack anchored to the
    // window's top-left as it is resized instead of re-centring every frame.
    egui::ScrollArea::both()
        .id_salt("audio_mixer_strips")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for (idx, strip) in strips.iter_mut().enumerate() {
                    channel_strip(ui, idx, strip, any_solo, expanded, &mut pending, open_fx);
                    strip_separator(ui);
                }
                master_strip(ui, master, master_meter, &mut pending, open_fx);
            });
        });

    for cmd in pending {
        commit(&mut model, cmd);
    }
    ui.data_mut(|d| d.insert_temp(id, model));
}

/// A deterministic, clearly-simulated idle level (dB) so the meter ballistics,
/// gradient, and clip LED are visible for review. Replaced by real
/// `get_audio_meters` polling when the engine-meter seam lands.
fn synthetic_level_db(seed: f32, t: f32, fader_db: f64) -> f32 {
    let wobble = (t * 2.3 + seed * 1.7).sin() * 4.0 + (t * 6.1 + seed).sin() * 2.0;
    ((fader_db as f32) - 9.0 + wobble).clamp(FLOOR_DB as f32, 6.0)
}

// ═══════════════════════════════════════════════════════════════════════════
// Channel strip + master strip
// ═══════════════════════════════════════════════════════════════════════════

#[allow(clippy::too_many_arguments)]
fn channel_strip(
    ui: &mut Ui,
    idx: usize,
    strip: &mut MixerStrip,
    any_solo: bool,
    expanded: &mut HashSet<TrackId>,
    pending: &mut Vec<AudioCmd>,
    open_fx: &mut Option<FxSel>,
) {
    ui.allocate_ui_with_layout(
        vec2(STRIP_W, ui.available_height()),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            // Header: solo-active warning wash on the header only (13 §11.7),
            // name, and the automation-disclosure chevron (wired to the real
            // `mixer_expanded_tracks` session field).
            let is_expanded = expanded.contains(&strip.track);
            header_row(ui, &strip.name, strip.audio.solo, is_expanded, |exp| {
                if exp {
                    expanded.remove(&strip.track);
                } else {
                    expanded.insert(strip.track);
                }
            });

            // Meter + fader.
            let mut db = strip.audio.params.base.volume_db;
            let mut vol_changed = false;
            ui.horizontal(|ui| {
                draw_meter(ui, &mut strip.meter);
                vol_changed |= fader(ui, &mut db).changed();
            });
            vol_changed |= ui
                .add(
                    egui::DragValue::new(&mut db)
                        .range(FLOOR_DB..=CEIL_DB)
                        .speed(0.15)
                        .fixed_decimals(1)
                        .suffix(" dB"),
                )
                .on_hover_text("Fader gain (arrow keys to nudge)")
                .changed();
            if vol_changed {
                let old = strip.audio.params.base;
                let new = TrackAudioParams {
                    volume_db: db.clamp(FLOOR_DB, CEIL_DB),
                    pan: old.pan,
                };
                pending.push(AudioCmd::SetTrackAudioProp {
                    track: strip.track,
                    old,
                    new,
                });
            }

            // Pan.
            let mut pan = strip.audio.params.base.pan;
            let mut pan_changed = false;
            ui.horizontal(|ui| {
                pan_changed |= pan_knob(ui, &mut pan).changed();
                ui.label(egui::RichText::new(fmt_pan(pan)).monospace().small());
            });
            if pan_changed {
                let old = strip.audio.params.base;
                let new = TrackAudioParams {
                    volume_db: old.volume_db,
                    pan: pan.clamp(-1.0, 1.0),
                };
                pending.push(AudioCmd::SetTrackAudioProp {
                    track: strip.track,
                    old,
                    new,
                });
            }

            // Mute / solo (solo-safe — 09 §4).
            let (mute, solo) = (strip.audio.mute, strip.audio.solo);
            if let Some((nm, ns)) = mute_solo_row(ui, mute, solo, any_solo) {
                pending.push(AudioCmd::SetTrackMuteSolo {
                    track: strip.track,
                    old: (mute, solo),
                    new: (nm, ns),
                });
            }

            ui.separator();
            fx_rack(
                ui,
                FxOwner::Track(strip.track),
                &strip.audio.fx_chain,
                idx,
                pending,
                open_fx,
            );

            // Automation lanes (13 §11.3): revealed by the header chevron, driven
            // by the real `mixer_expanded_tracks` set.
            if expanded.contains(&strip.track) {
                automation_lanes(ui);
            }

            // Inline fx editor for a slot opened on this strip.
            if let Some(FxSel::Track(s, slot)) = *open_fx {
                if s == idx && slot < strip.audio.fx_chain.len() {
                    fx_editor(ui, &mut strip.audio.fx_chain[slot], open_fx);
                }
            }
        },
    );
}

fn master_strip(
    ui: &mut Ui,
    master: &mut MasterBus,
    meter: &mut StereoMeterState,
    pending: &mut Vec<AudioCmd>,
    open_fx: &mut Option<FxSel>,
) {
    ui.allocate_ui_with_layout(
        vec2(STRIP_W, ui.available_height()),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            header_row(ui, "Master", false, false, |_| {});

            // Gap M-7: the prominent stereo output meter, standalone above the
            // fader (not squeezed beside it like the per-track strips) so it
            // reads as the mix's final, authoritative level.
            master_output_meter(ui, meter);
            ui.add_space(4.0);

            let mut db = master.params.base.volume_db;
            let mut changed = fader(ui, &mut db).changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut db)
                        .range(FLOOR_DB..=CEIL_DB)
                        .speed(0.15)
                        .fixed_decimals(1)
                        .suffix(" dB"),
                )
                .changed();
            if changed {
                let old = master.params.base;
                let new = MasterBusParams {
                    volume_db: db.clamp(FLOOR_DB, CEIL_DB),
                };
                pending.push(AudioCmd::SetMasterBusProp { old, new });
            }

            // Live integrated-loudness readout (09 §8 / 13 §11.1) — an
            // approximate rolling estimate; the export value is authoritative.
            let lufs = approx_lufs(meter.rms_avg_db());
            ui.label(
                egui::RichText::new(format!("{lufs:.1} LUFS"))
                    .monospace()
                    .small(),
            );
            ui.label(
                egui::RichText::new("approx — export authoritative")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );

            ui.separator();
            fx_rack(
                ui,
                FxOwner::Master,
                &master.fx_chain,
                usize::MAX,
                pending,
                open_fx,
            );
            // Master must keep a limiter (09 §6.5) — warn if removed.
            if !master
                .fx_chain
                .iter()
                .any(|u| u.kind == AudioFxKind::Limiter)
            {
                ui.label(
                    egui::RichText::new("no limiter")
                        .small()
                        .color(ui.visuals().warn_fg_color),
                );
            }

            if let Some(FxSel::Master(slot)) = *open_fx {
                if slot < master.fx_chain.len() {
                    fx_editor(ui, &mut master.fx_chain[slot], open_fx);
                }
            }
        },
    );
}

/// Rolling loudness estimate from the RMS meter (approximate; 09 §6.6's gated
/// R128 measurement is the authoritative export value). ~ -0.691 dB K-weighting
/// offset folded in as a coarse constant for the live readout only.
fn approx_lufs(rms_db: f32) -> f32 {
    (rms_db - 0.691).clamp(FLOOR_DB as f32, 0.0)
}

// ═══════════════════════════════════════════════════════════════════════════
// Widgets
// ═══════════════════════════════════════════════════════════════════════════

/// Strip header: solo-active `warning` wash (header only), title, and (when
/// `on_toggle` is meaningful) an automation-disclosure chevron.
fn header_row(ui: &mut Ui, name: &str, solo: bool, expanded: bool, on_toggle: impl FnOnce(bool)) {
    let full = ui.available_width();
    ui.horizontal(|ui| {
        if solo {
            let bg = ui.visuals().warn_fg_color.gamma_multiply(0.22);
            let r = ui.max_rect();
            ui.painter().rect_filled(
                Rect::from_min_size(r.min, vec2(full, 18.0)),
                Rounding::same(3.0),
                bg,
            );
        }
        let chevron = if expanded {
            ph::CARET_DOWN
        } else {
            ph::CARET_RIGHT
        };
        if ui
            .add(egui::Button::new(chevron).small().frame(false))
            .on_hover_text("Automation lanes")
            .clicked()
        {
            on_toggle(expanded);
        }
        ui.label(egui::RichText::new(name).small().strong());
    });
}

/// Vertical dB fader — egui's native vertical `Slider` (13 §11.6: "no custom
/// widget needed here"), keyboard-nudgeable once focused (§11.8).
fn fader(ui: &mut Ui, db: &mut f64) -> Response {
    let prev = ui.spacing().slider_width;
    ui.spacing_mut().slider_width = FADER_H;
    let resp = ui.add(
        egui::Slider::new(db, FLOOR_DB..=CEIL_DB)
            .vertical()
            .show_value(false)
            .custom_formatter(|n, _| fmt_db(n)),
    );
    ui.spacing_mut().slider_width = prev;
    resp
}

/// Equal-power pan knob — custom-painted 1D rotary (13 §11.1), pointer-drag
/// primary with arrow-key nudge when focused (Shift = ×10) and a `DragValue`
/// numeric fallback handled by the caller (§11.8 keyboard-path requirement).
fn pan_knob(ui: &mut Ui, pan: &mut f64) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click_and_drag());
    if resp.dragged() {
        *pan = (*pan + resp.drag_delta().x as f64 * 0.012).clamp(-1.0, 1.0);
        resp.mark_changed();
    }
    if resp.double_clicked() {
        *pan = 0.0;
        resp.mark_changed();
    }
    if resp.has_focus() {
        let (left, right, big) = ui.input(|i| {
            (
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.modifiers.shift,
            )
        });
        let step = if big { 0.1 } else { 0.01 };
        if left {
            *pan = (*pan - step).clamp(-1.0, 1.0);
            resp.mark_changed();
        }
        if right {
            *pan = (*pan + step).clamp(-1.0, 1.0);
            resp.mark_changed();
        }
    }

    let vis = ui.visuals();
    let painter = ui.painter();
    let center = rect.center();
    let radius = rect.width() * 0.5 - 2.0;
    painter.circle_filled(center, radius, vis.extreme_bg_color);
    let ring = if resp.has_focus() {
        vis.selection.stroke.color
    } else {
        vis.widgets.inactive.bg_stroke.color
    };
    painter.circle_stroke(center, radius, Stroke::new(1.0, ring));
    // Indicator: up at center, swinging ±~137° across the pan range.
    let ang = -std::f32::consts::FRAC_PI_2 + (*pan as f32) * 2.4;
    let dir = Vec2::angled(ang);
    painter.line_segment(
        [center, center + dir * radius],
        Stroke::new(2.0, vis.text_color()),
    );
    let (gl, gr) = pan_law(*pan);
    resp.on_hover_text(format!(
        "Pan {} — equal-power L {gl:.2} / R {gr:.2} (drag; arrows nudge, Shift ×10; double-click = center)",
        fmt_pan(*pan),
    ))
}

/// Dual peak/RMS meter with a clickable clip-indicator LED stacked above the
/// bar (13 §11.1/§11.4). The LED latches red above -0.3 dBTP; click resets it.
fn draw_meter(ui: &mut Ui, meter: &mut MeterState) {
    draw_meter_sized(ui, meter, FADER_H);
}

/// [`draw_meter`] at an explicit bar height — factored out so the master-bus
/// output meter (Gap M-7) can render a taller bar than the per-track strips
/// for prominence, without duplicating the LED/fill/peak-hold painting.
fn draw_meter_sized(ui: &mut Ui, meter: &mut MeterState, height: f32) {
    ui.vertical(|ui| {
        // Clip LED (top): click to clear a latched clip.
        let (led, led_resp) = ui.allocate_exact_size(vec2(METER_W, 6.0), Sense::click());
        if led_resp.clicked() {
            meter.clip = false;
        }
        let (rect, _) = ui.allocate_exact_size(vec2(METER_W, height), Sense::hover());

        let vis = ui.visuals();
        let led_color = if meter.clip {
            METER_HIGH
        } else {
            vis.widgets.inactive.bg_fill
        };
        let track_bg = vis.extreme_bg_color;
        let hold_col = vis.text_color();
        let painter = ui.painter();
        painter.rect_filled(led, Rounding::same(1.0), led_color);
        painter.rect_filled(rect, Rounding::same(1.0), track_bg);

        let peak_frac = db_to_frac(meter.peak_db, FLOOR_DB as f32, CEIL_DB as f32);
        let rms_frac = db_to_frac(meter.rms_db, FLOOR_DB as f32, CEIL_DB as f32);
        fill_bar(
            painter,
            rect,
            peak_frac,
            meter_color(peak_frac).gamma_multiply(0.5),
        );
        fill_bar(painter, rect, rms_frac, meter_color(rms_frac));

        // Peak-hold marker line.
        let hold_frac = db_to_frac(meter.hold_db, FLOOR_DB as f32, CEIL_DB as f32);
        let y = rect.bottom() - hold_frac * rect.height();
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, hold_col),
        );
    });
}

/// Prominent master-bus output meter (Gap M-7, 13 §11.1/§11.4): stereo (L/R)
/// peak+RMS bars — each with its own clickable clip LED — under an "OUTPUT"
/// label and above a numeric peak-dB readout, deliberately taller/wider
/// (`MASTER_METER_H`, two bars) than the single per-track strip meters so the
/// final mix level reads at a glance without hunting.
///
/// ## Reading this against the real engine
///
/// The engine-side equivalent is `audio::mixer::Mixer::output_meter()`
/// (`photonic-video`): a lock-free `Arc<StereoMeter>` sampled post
/// `MasterBusParams.volume_db` — the exact "Output" tap this widget wants.
/// It is not reachable here: `draw_audio_mixer(ui, &mut VideoPanelUi)` is
/// called with no engine handle (see this module's top doc, "Wiring seam"),
/// and `EngineSession`/`EngineStatus` don't surface any `StereoMeter` to the
/// GUI yet regardless (`get_audio_meters` doesn't exist, 10 §3.12 / 13
/// §11.6). So this renders the best-available value: the same honestly
/// simulated-and-labelled ballistics pass already driving every other meter
/// in this file (`draw_audio_mixer`'s per-frame update, panned per-strip by
/// real pan-law gain — see its call site). Swapping to
/// `mixer.output_meter().peak()`/`.rms()` once that seam closes is a
/// same-shape `[f32; 2]`-per-frame feed into [`StereoMeterState::update`];
/// no widget change.
fn master_output_meter(ui: &mut Ui, meter: &mut StereoMeterState) {
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new("OUTPUT")
                .small()
                .strong()
                .color(ui.visuals().weak_text_color()),
        );
        ui.horizontal(|ui| {
            draw_meter_sized(ui, &mut meter.l, MASTER_METER_H);
            ui.add_space(3.0);
            draw_meter_sized(ui, &mut meter.r, MASTER_METER_H);
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("L").small().weak());
            ui.add_space(METER_W - 2.0);
            ui.label(egui::RichText::new("R").small().weak());
        });
        let peak = egui::RichText::new(fmt_db(meter.peak_db() as f64))
            .monospace()
            .small()
            .strong();
        let peak = if meter.clipped() {
            peak.color(METER_HIGH)
        } else {
            peak
        };
        ui.label(peak).on_hover_text(
            "Master output peak (louder channel) — click a clip LED to clear its latch",
        );
    });
}

fn fill_bar(painter: &egui::Painter, rect: Rect, frac: f32, color: Color32) {
    let h = frac.clamp(0.0, 1.0) * rect.height();
    if h <= 0.0 {
        return;
    }
    let r = Rect::from_min_max(pos2(rect.left(), rect.bottom() - h), rect.right_bottom());
    painter.rect_filled(r, Rounding::same(1.0), color);
}

/// Mute/solo paired toggles. Returns the new `(mute, solo)` on a click.
/// Solo uses the `warning` accent (13 §11.1) rather than the `primary`
/// selected-state accent.
fn mute_solo_row(ui: &mut Ui, mute: bool, solo: bool, _any_solo: bool) -> Option<(bool, bool)> {
    let mut out = None;
    ui.horizontal(|ui| {
        if ui
            .add(egui::SelectableLabel::new(mute, "M"))
            .on_hover_text("Mute")
            .clicked()
        {
            out = Some((!mute, solo));
        }
        let solo_txt = if solo {
            egui::RichText::new("S")
                .color(ui.visuals().warn_fg_color)
                .strong()
        } else {
            egui::RichText::new("S")
        };
        if ui
            .add(egui::SelectableLabel::new(solo, solo_txt))
            .on_hover_text("Solo (solo-safe)")
            .clicked()
        {
            out = Some((mute, !solo));
        }
    });
    out
}

/// Ordered `AudioFxUnit` rack (13 §11.1): add / remove / reorder (keyboard
/// up-down fallback per 13 §16), double-click a slot to open its editor.
fn fx_rack(
    ui: &mut Ui,
    owner: FxOwner,
    chain: &[AudioFxUnit],
    strip_idx: usize,
    pending: &mut Vec<AudioCmd>,
    open_fx: &mut Option<FxSel>,
) {
    let n = chain.len();
    for (slot, unit) in chain.iter().enumerate() {
        ui.horizontal(|ui| {
            let is_open = matches!(open_fx, Some(sel) if *sel == fx_sel(owner, strip_idx, slot));
            let label = format!("{} {}", fx_kind_icon(unit.kind), fx_kind_label(unit.kind));
            let txt = if unit.enabled {
                egui::RichText::new(label)
            } else {
                egui::RichText::new(label).weak().strikethrough()
            };
            if ui
                .add(egui::SelectableLabel::new(is_open, txt))
                .on_hover_text("Double-click to edit")
                .double_clicked()
            {
                *open_fx = if is_open {
                    None
                } else {
                    Some(fx_sel(owner, strip_idx, slot))
                };
            }
            // Reorder up/down (keyboard-reachable buttons — 13 §16 fallback).
            if slot > 0 && ui.small_button(ph::CARET_UP).clicked() {
                pending.push(reorder_swap(owner, n, slot, slot - 1));
            }
            if slot + 1 < n && ui.small_button(ph::CARET_DOWN).clicked() {
                pending.push(reorder_swap(owner, n, slot, slot + 1));
            }
            if ui.small_button(ph::X).on_hover_text("Remove").clicked() {
                pending.push(AudioCmd::RemoveAudioFx {
                    owner,
                    index: slot,
                    unit: unit.clone(),
                });
                if matches!(open_fx, Some(sel) if *sel == fx_sel(owner, strip_idx, slot)) {
                    *open_fx = None;
                }
            }
        });
    }
    ui.menu_button(format!("{} Add fx", ph::PLUS), |ui| {
        for kind in ALL_FX_KINDS {
            if ui.button(fx_kind_label(kind)).clicked() {
                pending.push(AudioCmd::AddAudioFx {
                    owner,
                    index: n,
                    unit: AudioFxUnit::new(kind),
                });
                ui.close_menu();
            }
        }
    });
}

fn fx_sel(owner: FxOwner, strip_idx: usize, slot: usize) -> FxSel {
    match owner {
        FxOwner::Master => FxSel::Master(slot),
        FxOwner::Track(_) => FxSel::Track(strip_idx, slot),
    }
}

fn reorder_swap(owner: FxOwner, n: usize, a: usize, b: usize) -> AudioCmd {
    let old_order: Vec<usize> = (0..n).collect();
    let mut new_order = old_order.clone();
    new_order.swap(a, b);
    AudioCmd::ReorderAudioFx {
        owner,
        old_order,
        new_order,
    }
}

/// Compact kind-specific fx editor (13 §11.2). EQ = per-band freq/gain/Q rows;
/// Compressor/Gate/Limiter = their scoped param sliders (09 §6). Draggable
/// EQ-curve / GR-meter widgets are a later visual-pass follow-up; these
/// param rows are fully keyboard-operable now.
fn fx_editor(ui: &mut Ui, unit: &mut AudioFxUnit, open_fx: &mut Option<FxSel>) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(fx_kind_label(unit.kind))
                    .small()
                    .strong(),
            );
            ui.checkbox(&mut unit.enabled, "on");
            if ui.small_button(ph::X).clicked() {
                *open_fx = None;
            }
        });
        let p = &mut unit.params.base;
        match unit.kind {
            AudioFxKind::Eq => {
                eq_band(ui, p, "low_shelf", "Low shelf", false);
                eq_band(ui, p, "band1", "Band 1", true);
                eq_band(ui, p, "band2", "Band 2", true);
                eq_band(ui, p, "band3", "Band 3", true);
                eq_band(ui, p, "high_shelf", "High shelf", false);
            }
            AudioFxKind::Compressor => {
                param_row(
                    ui,
                    p,
                    "params.threshold_db",
                    "Threshold",
                    -60.0..=0.0,
                    0.2,
                    " dB",
                );
                param_row(ui, p, "params.ratio", "Ratio", 1.0..=20.0, 0.05, ":1");
                param_row(ui, p, "params.attack_ms", "Attack", 0.1..=200.0, 0.5, " ms");
                param_row(
                    ui,
                    p,
                    "params.release_ms",
                    "Release",
                    5.0..=2000.0,
                    1.0,
                    " ms",
                );
                param_row(ui, p, "params.makeup_db", "Makeup", 0.0..=24.0, 0.1, " dB");
                if let Some(sc) = unit.sidechain {
                    ui.label(
                        egui::RichText::new(format!("sidechain: {}", short_id(sc)))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
            }
            AudioFxKind::Gate => {
                param_row(
                    ui,
                    p,
                    "params.threshold_db",
                    "Threshold",
                    -80.0..=0.0,
                    0.2,
                    " dB",
                );
                param_row(ui, p, "params.attack_ms", "Attack", 0.1..=100.0, 0.2, " ms");
                param_row(ui, p, "params.hold_ms", "Hold", 0.0..=500.0, 1.0, " ms");
                param_row(
                    ui,
                    p,
                    "params.release_ms",
                    "Release",
                    5.0..=2000.0,
                    1.0,
                    " ms",
                );
                param_row(ui, p, "params.range_db", "Range", 0.0..=90.0, 0.2, " dB");
            }
            AudioFxKind::Limiter => {
                param_row(
                    ui,
                    p,
                    "params.ceiling_db",
                    "Ceiling",
                    -12.0..=0.0,
                    0.1,
                    " dBTP",
                );
                param_row(
                    ui,
                    p,
                    "params.release_ms",
                    "Release",
                    1.0..=1000.0,
                    1.0,
                    " ms",
                );
            }
            // Forward-compat (39 §2.2): an fx kind this build does not
            // understand is non-editable but retained verbatim. `AudioFxKind`
            // is `#[non_exhaustive]`, so this wildcard also covers any future
            // kind a newer build adds.
            _ => {
                ui.label(
                    egui::RichText::new(
                        "This effect was made by a newer Photonic build and can't be \
                         edited here. It is preserved untouched.",
                    )
                    .small()
                    .color(ui.visuals().warn_fg_color),
                );
            }
        }
    });
}

fn eq_band(ui: &mut Ui, p: &mut EffectParams, band: &str, label: &str, has_q: bool) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small());
        param_val(
            ui,
            p,
            &format!("params.{band}.freq_hz"),
            20.0..=20_000.0,
            5.0,
            " Hz",
        );
        param_val(
            ui,
            p,
            &format!("params.{band}.gain_db"),
            -24.0..=24.0,
            0.1,
            " dB",
        );
        if has_q {
            param_val(ui, p, &format!("params.{band}.q"), 0.1..=10.0, 0.05, " Q");
        }
    });
}

/// Labeled parameter row (label + numeric `DragValue`). Writes back as a
/// `PropValue::Float` on change.
fn param_row(
    ui: &mut Ui,
    p: &mut EffectParams,
    path: &str,
    label: &str,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
    suffix: &str,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small());
        param_val(ui, p, path, range, speed, suffix);
    });
}

fn param_val(
    ui: &mut Ui,
    p: &mut EffectParams,
    path: &str,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
    suffix: &str,
) {
    let mut v = get_f(p, path);
    if ui
        .add(
            egui::DragValue::new(&mut v)
                .range(range)
                .speed(speed)
                .fixed_decimals(1)
                .suffix(suffix),
        )
        .changed()
    {
        p.set(path, PropValue::Float(v));
    }
}

fn get_f(p: &EffectParams, path: &str) -> f64 {
    match p.get(path) {
        Some(PropValue::Float(x)) => *x,
        _ => 0.0,
    }
}

/// Automation-lane placeholder shown when a strip is expanded (13 §11.3): the
/// `volume`/`pan` `PropertyTrack` lanes reuse the shared keyframe UI once the
/// timeline overlay story wires them; this reveals the disclosure state that is
/// tracked live in `mixer_expanded_tracks`.
fn automation_lanes(ui: &mut Ui) {
    ui.add_space(2.0);
    for lane in ["volume", "pan"] {
        let (rect, _) = ui.allocate_exact_size(vec2(STRIP_W - 8.0, 12.0), Sense::hover());
        let vis = ui.visuals();
        ui.painter()
            .rect_filled(rect, Rounding::same(2.0), vis.faint_bg_color);
        ui.painter().text(
            rect.left_center() + vec2(4.0, 0.0),
            egui::Align2::LEFT_CENTER,
            lane,
            egui::FontId::proportional(9.0),
            vis.weak_text_color(),
        );
    }
}

fn strip_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, ui.available_height()), Sense::hover());
    ui.painter().rect_filled(
        rect,
        Rounding::ZERO,
        ui.visuals().widgets.noninteractive.bg_stroke.color,
    );
}

fn fx_kind_label(kind: AudioFxKind) -> &'static str {
    match kind {
        AudioFxKind::Eq => "EQ",
        AudioFxKind::Compressor => "Comp",
        AudioFxKind::Gate => "Gate",
        AudioFxKind::Limiter => "Limiter",
        // Forward-compat (39 §2.2): show the preserved tag as the display name.
        AudioFxKind::Unknown(t) => t.as_str(),
        _ => "Unsupported",
    }
}

fn fx_kind_icon(kind: AudioFxKind) -> &'static str {
    match kind {
        AudioFxKind::Eq => ph::SLIDERS,
        AudioFxKind::Compressor => ph::WAVE_SINE,
        AudioFxKind::Gate => ph::GAUGE,
        AudioFxKind::Limiter => ph::SHIELD,
        // Forward-compat (39 §2.2): a neutral marker for an fx this build lacks.
        _ => ph::QUESTION,
    }
}

fn short_id(id: TrackId) -> String {
    id.0.to_string().chars().take(6).collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests — the spec-normative pure logic + the real-AudioCmd apply sink.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn pan_law_matches_spec() {
        // 09 §4: hard-left/right, and -3dB (0.707) each at center.
        let (l, r) = pan_law(-1.0);
        assert!(approx(l, 1.0) && approx(r, 0.0));
        let (l, r) = pan_law(1.0);
        assert!(approx(l, 0.0) && approx(r, 1.0));
        let (l, r) = pan_law(0.0);
        assert!(approx(l, std::f32::consts::FRAC_1_SQRT_2));
        assert!(approx(r, std::f32::consts::FRAC_1_SQRT_2));
        // Equal-power: L²+R² == 1 across the sweep.
        for i in -10..=10 {
            let (l, r) = pan_law(i as f64 / 10.0);
            assert!(approx(l * l + r * r, 1.0));
        }
    }

    #[test]
    fn pan_law_clamps_out_of_range() {
        assert_eq!(pan_law(-5.0), pan_law(-1.0));
        assert_eq!(pan_law(5.0), pan_law(1.0));
    }

    #[test]
    fn db_to_frac_maps_ends_and_clamps() {
        assert!(approx(
            db_to_frac(FLOOR_DB as f32, FLOOR_DB as f32, CEIL_DB as f32),
            0.0
        ));
        assert!(approx(
            db_to_frac(CEIL_DB as f32, FLOOR_DB as f32, CEIL_DB as f32),
            1.0
        ));
        assert!(approx(
            db_to_frac(-999.0, FLOOR_DB as f32, CEIL_DB as f32),
            0.0
        ));
        assert!(approx(
            db_to_frac(999.0, FLOOR_DB as f32, CEIL_DB as f32),
            1.0
        ));
    }

    #[test]
    fn meter_color_walks_the_gradient() {
        assert_eq!(meter_color(0.0), METER_LOW);
        assert_eq!(meter_color(0.72), METER_MID);
        assert_eq!(meter_color(1.0), METER_HIGH);
        // Green channel fades monotonically as the fill approaches clip
        // (success 200 → warning 191 → error 113).
        assert!(meter_color(0.0).g() >= meter_color(0.72).g());
        assert!(meter_color(0.72).g() >= meter_color(1.0).g());
        // A mid-low fill blends strictly between the low and mid anchors.
        assert!(meter_color(0.36).r() > METER_LOW.r() && meter_color(0.36).r() < METER_MID.r());
    }

    #[test]
    fn resolve_audible_solo_safe() {
        // No solo → mute-only (09 §4).
        assert_eq!(
            resolve_audible(&[(false, false), (true, false)]),
            vec![true, false]
        );
        // Any solo → only soloed strips; mute still wins over solo.
        assert_eq!(
            resolve_audible(&[(false, false), (false, true), (true, true)]),
            vec![false, true, false]
        );
        // Multiple solos are additive.
        assert_eq!(
            resolve_audible(&[(false, true), (false, true), (false, false)]),
            vec![true, true, false]
        );
    }

    #[test]
    fn fmt_helpers() {
        assert_eq!(fmt_db(FLOOR_DB), "-inf");
        assert_eq!(fmt_db(0.0), "+0.0");
        assert_eq!(fmt_db(-6.0), "-6.0");
        assert_eq!(fmt_pan(0.0), "C");
        assert_eq!(fmt_pan(-0.5), "L50");
        assert_eq!(fmt_pan(0.32), "R32");
    }

    #[test]
    fn meter_clip_latches_and_peak_attack_is_instant() {
        let mut m = MeterState::default();
        m.update(-3.0, 0.016);
        assert!(approx(m.peak_db, -3.0), "instant peak attack");
        assert!(!m.clip);
        m.update(0.0, 0.016); // above -0.3 dBTP
        assert!(m.clip, "clip latches above -0.3 dBTP");
        // Latch persists even as the level drops.
        m.update(-40.0, 0.5);
        assert!(m.clip);
    }

    #[test]
    fn lin_to_db_matches_known_points() {
        assert!(approx(lin_to_db(1.0), 0.0));
        // Equal-power center pan gain (0.707) is -3.01 dB per channel.
        assert!(approx(lin_to_db(std::f32::consts::FRAC_1_SQRT_2), -3.0103));
        // Near-zero gain floors instead of producing -inf/NaN.
        assert!(lin_to_db(0.0).is_finite());
    }

    #[test]
    fn stereo_meter_state_tracks_channels_independently() {
        let mut m = StereoMeterState::default();
        assert!(!m.clipped());
        m.update(-6.0, -20.0, 0.016);
        assert!(approx(m.l.peak_db, -6.0));
        assert!(approx(m.r.peak_db, -20.0));
        // Louder channel wins the numeric peak readout.
        assert!(approx(m.peak_db(), -6.0));
        assert!(!m.clipped());
        // Only the right channel clips; the left LED stays clear, but the
        // aggregate `clipped()` (driving the numeric readout's color) trips.
        m.update(-6.0, 0.0, 0.016);
        assert!(!m.l.clip);
        assert!(m.r.clip);
        assert!(m.clipped());
    }

    #[test]
    fn commit_applies_real_audio_cmds() {
        let mut m = PreviewMixer::demo();
        let t0 = m.strips[0].track;

        // Fader op.
        let old_params = m.strips[0].audio.params.base;
        commit(
            &mut m,
            AudioCmd::SetTrackAudioProp {
                track: t0,
                old: old_params,
                new: TrackAudioParams {
                    volume_db: -6.0,
                    pan: 0.25,
                },
            },
        );
        assert_eq!(m.strips[0].audio.params.base.volume_db, -6.0);
        assert_eq!(m.strips[0].audio.params.base.pan, 0.25);

        // Mute/solo op.
        commit(
            &mut m,
            AudioCmd::SetTrackMuteSolo {
                track: t0,
                old: (false, false),
                new: (true, true),
            },
        );
        assert!(m.strips[0].audio.mute && m.strips[0].audio.solo);

        // Master prop.
        let old_master = m.master.params.base;
        commit(
            &mut m,
            AudioCmd::SetMasterBusProp {
                old: old_master,
                new: MasterBusParams { volume_db: -2.0 },
            },
        );
        assert_eq!(m.master.params.base.volume_db, -2.0);
    }

    #[test]
    fn commit_add_remove_reorder_fx() {
        let mut m = PreviewMixer::demo();
        let t0 = m.strips[0].track;
        assert_eq!(m.strips[0].audio.fx_chain.len(), 0);

        commit(
            &mut m,
            AudioCmd::AddAudioFx {
                owner: FxOwner::Track(t0),
                index: 0,
                unit: AudioFxUnit::new(AudioFxKind::Eq),
            },
        );
        commit(
            &mut m,
            AudioCmd::AddAudioFx {
                owner: FxOwner::Track(t0),
                index: 1,
                unit: AudioFxUnit::new(AudioFxKind::Compressor),
            },
        );
        assert_eq!(m.strips[0].audio.fx_chain.len(), 2);
        assert_eq!(m.strips[0].audio.fx_chain[0].kind, AudioFxKind::Eq);

        // Reorder swaps slots 0 and 1.
        commit(&mut m, reorder_swap(FxOwner::Track(t0), 2, 0, 1));
        assert_eq!(m.strips[0].audio.fx_chain[0].kind, AudioFxKind::Compressor);
        assert_eq!(m.strips[0].audio.fx_chain[1].kind, AudioFxKind::Eq);

        // Remove slot 0.
        let unit = m.strips[0].audio.fx_chain[0].clone();
        commit(
            &mut m,
            AudioCmd::RemoveAudioFx {
                owner: FxOwner::Track(t0),
                index: 0,
                unit,
            },
        );
        assert_eq!(m.strips[0].audio.fx_chain.len(), 1);
        assert_eq!(m.strips[0].audio.fx_chain[0].kind, AudioFxKind::Eq);
    }

    #[test]
    fn master_seeds_limiter_and_reorder_ignores_bad_permutation() {
        let mut m = PreviewMixer::demo();
        assert!(m
            .master
            .fx_chain
            .iter()
            .any(|u| u.kind == AudioFxKind::Limiter));
        // A wrong-length permutation is a no-op (guards against corruption).
        let before = m.master.fx_chain.len();
        commit(
            &mut m,
            AudioCmd::ReorderAudioFx {
                owner: FxOwner::Master,
                old_order: vec![0],
                new_order: vec![0, 1, 2],
            },
        );
        assert_eq!(m.master.fx_chain.len(), before);
    }

    #[test]
    fn eq_param_roundtrips_through_effect_params() {
        let mut unit = AudioFxUnit::new(AudioFxKind::Eq);
        unit.params
            .base
            .set("params.band1.gain_db", PropValue::Float(3.5));
        assert_eq!(get_f(&unit.params.base, "params.band1.gain_db"), 3.5);
        // Seeded default frequency is present (09 §6.1).
        assert_eq!(get_f(&unit.params.base, "params.band1.freq_hz"), 500.0);
    }
}
