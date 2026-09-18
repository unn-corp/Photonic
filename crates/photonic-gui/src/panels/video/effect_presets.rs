//! Effect-preset library UI plumbing (26 §10 K-B4).
//!
//! The *data* — [`EffectPresetLibrary`], its validation, its config-file store
//! and the one-undo-unit apply — lives in
//! [`photonic_core::timeline::effect_preset`], which explains the
//! user-state-not-document-state decision in full. This module is the GUI half:
//! a session cache of that library, and the widgets `effects_browser.rs` and
//! `clip_inspector.rs` render from it.
//!
//! # Where the library lives at runtime
//!
//! In egui temp memory, keyed by one fixed [`egui::Id`], loaded lazily on first
//! draw. Not on `PhotonicApp` and not in `PropPanelCtx`, for two reasons:
//!
//! 1. Both consumers are `PropPanelCtx`-based drawers that carry `doc: &Document`
//!    and no `&mut` app state, so a shared mutable library would mean widening
//!    that struct — and `panels/mod.rs` is outside this story's territory. The
//!    same reasoning `clip_inspector.rs`'s `FxScopeTab` already records.
//! 2. It is genuinely session state over a config file, not app state: the file
//!    is the source of truth and every mutation writes through to it
//!    immediately, the way a rail click writes `preferences.json` immediately.
//!
//! # Managing the library is NOT an undo unit; applying a preset IS
//!
//! Saving / deleting / renaming a preset and starring an effect touch a config
//! file only — no `Document`, no `CommandHistory`, so no history entry, exactly
//! as 206 §5 argues for layouts. Applying a preset is a document edit and goes
//! out as one [`PanelAction::ClipEditBatch`], which
//! `app/panel_actions.rs` commits through `execute_discrete(Command::Batch(..))`
//! — one user verb, one undo step, however many effects and however many
//! selected clips.

use egui::{Color32, RichText, Ui};
use photonic_core::timeline::commands::VfxOwner;
use photonic_core::timeline::effect_preset::{
    self, EffectPreset, EffectPresetLibrary, LibraryLoad,
};
use photonic_core::timeline::{ClipId, EffectId, TimelineCmd, TimelineProject};

use crate::panels::PanelAction;

const MUTED: Color32 = Color32::from_rgb(0x7A, 0x7A, 0x9A); // `secondary`
const ACCENT: Color32 = Color32::from_rgb(0x6E, 0x56, 0xCF); // `primary`

fn library_id() -> egui::Id {
    egui::Id::new("effect_preset_library")
}

fn load_report_id() -> egui::Id {
    egui::Id::new("effect_preset_library_report")
}

/// One-line summary of the last load, shown once under the presets section so
/// an unresolvable entry is *diagnosed*, not silently swallowed (39 §2.2 rule 3
/// / 206 §4.2 rule 3). Empty when the load was clean.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LibraryStatus(pub(crate) String);

impl LibraryStatus {
    fn of(load: &LibraryLoad) -> LibraryStatus {
        let mut parts = Vec::new();
        if let Some(bad) = load.quarantined.as_ref() {
            parts.push(format!(
                "Preset library could not be read and was moved to {} — starting from \
                 the built-ins.",
                bad.display()
            ));
        }
        if !load.inert.is_empty() {
            let mut names: Vec<String> = load
                .inert
                .iter()
                .map(|(preset, id)| format!("{preset}: {id}"))
                .collect();
            names.sort();
            names.dedup();
            parts.push(format!(
                "{} preset entr{} name effects this build does not have ({}). They are \
                 kept as-is and stay switched off.",
                names.len(),
                if names.len() == 1 { "y" } else { "ies" },
                names.join(", ")
            ));
        }
        LibraryStatus(parts.join(" "))
    }
}

/// Run `f` against the session library, loading it from
/// `<config>/effect_presets.json` on first use. Returns `f`'s value.
///
/// `f` gets `&mut`; if it reports `true` ("I changed something") the library is
/// written straight back to disk. A write failure is logged and left in the
/// session copy rather than thrown at the user mid-interaction — the file is
/// re-written on the next successful mutation.
pub(crate) fn with_library<R>(ui: &Ui, f: impl FnOnce(&mut EffectPresetLibrary) -> (R, bool)) -> R {
    let mut lib = ui.data_mut(|d| d.get_temp::<EffectPresetLibrary>(library_id()));
    if lib.is_none() {
        let load = effect_preset::load_library().unwrap_or_else(|e| {
            tracing::warn!("effect preset library: {e}");
            LibraryLoad::default()
        });
        let status = LibraryStatus::of(&load);
        ui.data_mut(|d| d.insert_temp(load_report_id(), status));
        lib = Some(load.library);
    }
    let mut lib = lib.unwrap_or_default();
    let (out, dirty) = f(&mut lib);
    if dirty {
        if let Err(e) = effect_preset::save_library(&lib) {
            tracing::warn!("effect preset library save failed: {e}");
        }
    }
    ui.data_mut(|d| d.insert_temp(library_id(), lib));
    out
}

/// The one-shot load diagnostic, if the last load had one.
pub(crate) fn library_status(ui: &Ui) -> LibraryStatus {
    ui.data(|d| d.get_temp::<LibraryStatus>(load_report_id()))
        .unwrap_or_default()
}

// ── Favourites ───────────────────────────────────────────────────────────────

/// The user's starred effect ids, in their own order. Read **once per browser
/// draw** and passed down: `with_library` clones the whole library, and calling
/// it per row would clone it once for every catalogue entry, every frame.
pub(crate) fn favourite_ids(ui: &Ui) -> Vec<String> {
    with_library(ui, |lib| (lib.favourites.clone(), false))
}

/// A star toggle for one catalogue effect. Returns `true` when it was clicked
/// (the library has already been mutated and written through).
///
/// `starred` comes from one [`favourite_ids`] read for the whole panel, so the
/// non-click path touches no library state at all.
///
/// The glyph is a phosphor icon, never a bare Unicode star — `no_tofu_glyphs`
/// exists because exactly that kind of label rendered as a box. Only the
/// `Regular` phosphor variant is loaded (`photonic-app/src/main.rs`), so the
/// on/off states differ by **colour**, using two palette tokens that already
/// exist, rather than by a `STAR_FILL` glyph this build has no font for.
pub(crate) fn favourite_toggle(ui: &mut Ui, id: &EffectId, starred: bool) -> bool {
    let colour = if starred { ACCENT } else { MUTED };
    let tip = if starred {
        "Remove from favourites"
    } else {
        "Add to favourites"
    };
    let resp = ui
        .add(
            egui::Button::new(
                RichText::new(egui_phosphor::regular::STAR)
                    .small()
                    .color(colour),
            )
            .frame(false),
        )
        .on_hover_text(tip);
    if resp.clicked() {
        let owned = id.as_str().to_string();
        with_library(ui, |lib| {
            lib.toggle_favourite(&owned);
            ((), true)
        });
        return true;
    }
    false
}

/// The subset of `favourites` this build can actually offer, in the user's own
/// order. An id with no manifest stays in the file (39 §2.2) and is simply not
/// returned here — it is not dropped, and it reappears on a build that has the
/// effect.
pub(crate) fn resolvable_favourites(favourites: &[String]) -> Vec<EffectId> {
    favourites
        .iter()
        .map(|f| EffectId::new(f.clone()))
        .filter(|id| photonic_core::timeline::manifest(id.clone()).is_some())
        .collect()
}

// ── Applying a preset ────────────────────────────────────────────────────────

/// Commands applying `preset` to every clip in `targets`, as ONE batch.
///
/// A target that no longer resolves is skipped rather than failing the whole
/// apply — a stale GUI selection is not a user error, the same call the shipped
/// `timeline_paste_attributes` makes for K-B15. An entry the build has no
/// manifest for is still applied, inert; see the core module.
pub(crate) fn apply_to_clips(
    project: &TimelineProject,
    preset: &EffectPreset,
    targets: &[ClipId],
) -> Vec<TimelineCmd> {
    let mut cmds = Vec::new();
    for &clip in targets {
        if let Ok(mut c) = effect_preset::apply_commands(project, VfxOwner::Clip(clip), preset) {
            cmds.append(&mut c);
        }
    }
    cmds
}

/// Wrap [`apply_to_clips`] as the panel action that becomes one undo step.
pub(crate) fn apply_action(
    project: &TimelineProject,
    preset: &EffectPreset,
    targets: &[ClipId],
) -> Option<PanelAction> {
    let cmds = apply_to_clips(project, preset, targets);
    (!cmds.is_empty()).then_some(PanelAction::ClipEditBatch(cmds))
}

/// Apply a preset to ONE scope — the clip inspector's route, which follows its
/// Clip / Track / Master / Asset tab rather than the timeline selection.
///
/// Still one undo unit: `ClipEditBatch` is committed as a single
/// `Command::Batch`, and a one-effect preset is a one-member batch rather than
/// a different carrier, so the undo count does not depend on preset size.
pub(crate) fn apply_action_for_owner(
    project: &TimelineProject,
    preset: &EffectPreset,
    owner: VfxOwner,
) -> Option<PanelAction> {
    let cmds = effect_preset::apply_commands(project, owner, preset).ok()?;
    (!cmds.is_empty()).then_some(PanelAction::ClipEditBatch(cmds))
}

/// The clip inspector's preset bar for the scope it is currently editing:
/// apply any preset to this stack, and save this stack as a new preset.
///
/// Applying is one undo unit; saving is a config-file write and is not
/// undoable. Both facts are in the hover text, because a user cannot be
/// expected to infer which of two adjacent buttons lands on `Ctrl+Z`.
pub(crate) fn draw_scope_preset_bar(
    ui: &mut Ui,
    project: &TimelineProject,
    owner: VfxOwner,
    action: &mut Option<PanelAction>,
) {
    let Ok(stack) = photonic_core::timeline::ops::effect_stack(project, owner) else {
        return;
    };
    let stack = stack.to_vec();
    let grade = photonic_core::timeline::ops::scope_grade(project, owner)
        .ok()
        .flatten()
        .cloned();

    egui::CollapsingHeader::new("Presets")
        .id_salt(("fx_presets", owner))
        .default_open(false)
        .show(ui, |ui| {
            let presets: Vec<EffectPreset> = with_library(ui, |lib| (lib.catalogue(), false));
            let status = library_status(ui);
            if !status.0.is_empty() {
                ui.label(muted(status.0.clone()));
            }
            ui.horizontal_wrapped(|ui| {
                for preset in &presets {
                    let resp = ui
                        .add(egui::Button::new(preset.name.clone()).small())
                        .on_hover_text(format!(
                            "{} — add to this stack ({}). One undo step.",
                            preset_summary(preset),
                            owner.scope_noun()
                        ));
                    if resp.clicked() {
                        if let Some(a) = apply_action_for_owner(project, preset, owner) {
                            *action = Some(a);
                        }
                    }
                }
            });
            draw_save_row(ui, &stack, grade.as_ref());
        });
}

// ── "Save this stack as a preset" ────────────────────────────────────────────

/// The name to seed the save field with: `"My Look"`, `"My Look 2"`, … — the
/// first one not already taken by a built-in or a user preset.
///
/// The point is that the *default* action never overwrites and never lands on a
/// read-only built-in name. Typing an existing user preset's name deliberately
/// still replaces it in place ([`EffectPresetLibrary::upsert`]); that is an
/// explicit choice, not the path of least resistance.
pub(crate) fn unique_name(lib: &EffectPresetLibrary, wanted: &str) -> String {
    let taken: Vec<String> = lib.catalogue().into_iter().map(|p| p.name).collect();
    if !taken.iter().any(|n| n == wanted) {
        return wanted.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{wanted} {n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// The stem the save field seeds from.
const DEFAULT_PRESET_NAME: &str = "My Look";

/// The inline "Save stack as preset…" row: a name field, a Save button, and
/// the sticky result line under them.
///
/// Saving writes a config file and produces **no** undo entry, deliberately —
/// see the module doc. The result line is parked in egui memory rather than
/// returned, so it survives the frames after the click instead of flashing for
/// one repaint.
pub(crate) fn draw_save_row(
    ui: &mut Ui,
    effects: &[photonic_core::timeline::ClipEffect],
    grade: Option<&photonic_core::timeline::Grade>,
) {
    let buf_id = ui.id().with("fx_preset_save_name");
    let status_id = ui.id().with("fx_preset_save_status");
    let mut name: String = ui.data(|d| d.get_temp(buf_id)).unwrap_or_default();
    if name.is_empty() {
        // Seeded (not merely hinted) so the default action is a valid,
        // non-colliding save: an empty field disables the button, and a hint
        // that looks like a name but is not one is a known trap.
        name = with_library(ui, |lib| (unique_name(lib, DEFAULT_PRESET_NAME), false));
    }
    let empty = effects.is_empty() && grade.is_none();

    ui.horizontal(|ui| {
        ui.add_enabled(
            !empty,
            egui::TextEdit::singleline(&mut name)
                .hint_text("Preset name")
                .desired_width(120.0),
        );
        let can_save = !empty && !name.trim().is_empty();
        let save = ui
            .add_enabled(can_save, egui::Button::new("Save stack as preset"))
            .on_hover_text(if empty {
                "This stack is empty — add an effect first."
            } else {
                "Save these effects (and this grade) to your preset library. \
                 A library edit, not a document edit: it is not undoable."
            });
        if save.clicked() {
            let preset = EffectPreset::new(name.trim(), effects.to_vec(), grade.cloned());
            let label = name.trim().to_string();
            let status = with_library(ui, |lib| match lib.upsert(preset) {
                Ok(()) => (format!("Saved preset \"{label}\"."), true),
                Err(e) => (e.to_string(), false),
            });
            ui.data_mut(|d| d.insert_temp(status_id, status));
            name.clear();
        }
    });

    ui.data_mut(|d| d.insert_temp(buf_id, name));
    let status: String = ui.data(|d| d.get_temp(status_id)).unwrap_or_default();
    if !status.is_empty() {
        ui.label(muted(status));
    }
}

/// Muted one-liner describing a preset's content, for a row's hover text.
pub(crate) fn preset_summary(preset: &EffectPreset) -> String {
    let mut parts = Vec::new();
    match preset.effects.len() {
        0 => {}
        1 => parts.push("1 effect".to_string()),
        n => parts.push(format!("{n} effects")),
    }
    if preset.grade.is_some() {
        parts.push("grade".to_string());
    }
    let inert = preset.inert_ids();
    if !inert.is_empty() {
        parts.push(format!("{} unavailable here", inert.len()));
    }
    if parts.is_empty() {
        "empty".to_string()
    } else {
        parts.join(", ")
    }
}

/// Section heading, matching `effects_browser.rs`'s idiom.
pub(crate) fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).small().color(MUTED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::document::Document;
    use photonic_core::history::{Command, CommandHistory};
    use photonic_core::timeline::{
        Clip, ClipEffect, ClipSource, FrameRate, Grade, Sequence, Tick, TimelineProject, Track,
        TrackKind,
    };

    fn doc_with_clips(n: usize) -> (Document, Vec<ClipId>) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("S", FrameRate::FPS_30, 320, 180);
        let mut track = Track::new(TrackKind::Video, "V1");
        let mut ids = Vec::new();
        for i in 0..n {
            let clip = Clip::new(
                ClipSource::SolidColor {
                    color: photonic_core::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                },
                Tick(i as i64 * 1000),
                Tick(1000),
            );
            ids.push(clip.id);
            track.clips.push(clip);
        }
        seq.video_tracks.push(track);
        project.insert_sequence(seq);
        let mut doc = Document::new("t", 1.0, 1.0);
        doc.timeline = Some(project);
        (doc, ids)
    }

    fn built_in() -> EffectPreset {
        effect_preset::built_in_presets()
            .into_iter()
            .find(|p| p.effects.len() > 1)
            .expect("a multi-effect built-in must exist for this fixture")
    }

    /// The DoD point that matters: applying a preset across a multi-selection
    /// is ONE undo unit, and undo restores the document exactly.
    #[test]
    fn applying_across_a_multi_selection_is_one_undo_unit() {
        let (mut doc, ids) = doc_with_clips(3);
        let preset = built_in();
        let before = doc.clone();
        let mut history = CommandHistory::new(64);

        let action = apply_action(doc.timeline.as_ref().unwrap(), &preset, &ids)
            .expect("three live clips must produce commands");
        let PanelAction::ClipEditBatch(cmds) = action else {
            panic!("apply must be a single batch carrier");
        };
        assert_eq!(
            cmds.len(),
            preset.effects.len() * ids.len(),
            "one AddEffect per (clip, effect)"
        );

        let depth = history.undo_depth();
        history.execute_discrete(
            Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
            &mut doc,
        );
        assert_eq!(
            history.undo_depth(),
            depth + 1,
            "N clips x M effects must still be exactly one undo step"
        );
        for &id in &ids {
            let stack = photonic_core::timeline::ops::effect_stack(
                doc.timeline.as_ref().unwrap(),
                VfxOwner::Clip(id),
            )
            .unwrap();
            assert_eq!(stack.len(), preset.effects.len());
        }

        history.undo(&mut doc);
        assert_eq!(
            doc.timeline, before.timeline,
            "one undo must restore every clip"
        );
    }

    /// The inspector's scope route (Clip / Track / Master / Asset tabs) is the
    /// same one-batch carrier. Track is the interesting one: it has no
    /// `SetClipProp`-style whole-owner snapshot to ride, so a regression that
    /// re-routed preset apply through the clip-only path would fail here.
    #[test]
    fn applying_to_a_track_scope_is_also_one_batch() {
        let (mut doc, _) = doc_with_clips(1);
        let track_id = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .values()
            .next()
            .unwrap()
            .video_tracks[0]
            .id;
        let owner = VfxOwner::Track(track_id);
        let preset = built_in();
        let before = doc.clone();
        let mut history = CommandHistory::new(64);

        let action = apply_action_for_owner(doc.timeline.as_ref().unwrap(), &preset, owner)
            .expect("a live track must accept a preset");
        let PanelAction::ClipEditBatch(cmds) = action else {
            panic!("scope apply must be a single batch carrier");
        };
        assert_eq!(cmds.len(), preset.effects.len());
        let depth = history.undo_depth();
        history.execute_discrete(
            Command::Batch(cmds.into_iter().map(Command::Timeline).collect()),
            &mut doc,
        );
        assert_eq!(history.undo_depth(), depth + 1);
        let stack =
            photonic_core::timeline::ops::effect_stack(doc.timeline.as_ref().unwrap(), owner)
                .unwrap();
        assert_eq!(
            stack.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            preset
                .effects
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>(),
            "the track stack must end up in the preset's own order"
        );
        history.undo(&mut doc);
        assert_eq!(doc.timeline, before.timeline);
    }

    #[test]
    fn a_stale_selection_entry_is_skipped_not_fatal() {
        let (doc, mut ids) = doc_with_clips(2);
        ids.push(photonic_core::timeline::ClipId::new()); // deleted since selection
        let preset = built_in();
        let cmds = apply_to_clips(doc.timeline.as_ref().unwrap(), &preset, &ids);
        assert_eq!(
            cmds.len(),
            preset.effects.len() * 2,
            "the two live clips still get the preset"
        );
    }

    #[test]
    fn an_empty_selection_produces_no_action_and_therefore_no_undo_step() {
        let (doc, _) = doc_with_clips(1);
        assert!(apply_action(doc.timeline.as_ref().unwrap(), &built_in(), &[]).is_none());
    }

    #[test]
    fn unique_name_never_collides_with_a_built_in_or_a_user_preset() {
        let mut lib = EffectPresetLibrary::new();
        let taken = effect_preset::built_in_presets()[0].name.clone();
        assert_eq!(unique_name(&lib, &taken), format!("{taken} 2"));
        assert_eq!(
            unique_name(&lib, DEFAULT_PRESET_NAME),
            DEFAULT_PRESET_NAME,
            "the seeded default must be usable as-is on a fresh library"
        );
        lib.upsert(EffectPreset::new(
            format!("{taken} 2"),
            vec![ClipEffect::new(photonic_core::timeline::EffectKind::Blur)],
            None,
        ))
        .unwrap();
        assert_eq!(unique_name(&lib, &taken), format!("{taken} 3"));
        assert_eq!(unique_name(&lib, "Untouched"), "Untouched");
    }

    /// Favourites are an *ordering over the catalogue*: unknown ids are not
    /// offered, everything else keeps the user's order (not manifest order,
    /// which is alphabetical and would be indistinguishable for a sorted list —
    /// hence the deliberately unsorted fixture).
    #[test]
    fn resolvable_favourites_filters_unknown_ids_and_keeps_the_users_order() {
        let stored = vec![
            "stylize.glow".to_string(),
            "future.effect".to_string(),
            "blur.gaussian".to_string(),
        ];
        let mut alphabetical = stored.clone();
        alphabetical.sort();
        assert_ne!(
            stored, alphabetical,
            "fixture must not already be sorted, or order preservation is unprovable"
        );
        let out: Vec<String> = resolvable_favourites(&stored)
            .into_iter()
            .map(|id| id.as_str().to_string())
            .collect();
        assert_eq!(out, vec!["stylize.glow", "blur.gaussian"]);
    }

    #[test]
    fn preset_summary_names_both_content_and_unavailability() {
        let mut fx = ClipEffect::new(photonic_core::timeline::EffectKind::Blur);
        fx.id = EffectId::new("future.effect".to_string());
        let mut preset = EffectPreset::new("P", vec![fx], Some(Grade::new()));
        effect_preset::finalize(&mut preset);
        let summary = preset_summary(&preset);
        assert!(summary.contains("1 effect"), "{summary}");
        assert!(summary.contains("grade"), "{summary}");
        assert!(summary.contains("unavailable"), "{summary}");

        let clean = &effect_preset::built_in_presets()[0];
        assert!(
            !preset_summary(clean).contains("unavailable"),
            "a resolvable preset must not claim anything is unavailable"
        );
    }

    #[test]
    fn library_status_is_empty_for_a_clean_load_and_names_the_problem_otherwise() {
        assert_eq!(LibraryStatus::of(&LibraryLoad::default()).0, "");
        let load = LibraryLoad {
            inert: vec![("Mine".into(), "future.effect".into())],
            quarantined: Some(std::path::PathBuf::from("/tmp/effect_presets.json.bad")),
            ..Default::default()
        };
        let status = LibraryStatus::of(&load).0;
        assert!(status.contains("effect_presets.json.bad"), "{status}");
        assert!(status.contains("future.effect"), "{status}");
        assert!(status.contains("Mine"), "{status}");
    }
}
