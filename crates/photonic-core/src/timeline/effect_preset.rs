//! Effect presets, custom stacks and favourites (26 §10 K-B4).
//!
//! # The key decision: a preset library is USER state, not DOCUMENT state
//!
//! An effect preset is a **user library**, stored in the app config directory
//! (`<config>/effect_presets.json`), *not* in the `.photon`. Three siblings
//! already reasoned their way to the same placement and this item is
//! deliberately consistent with them:
//!
//! * `docs/proposals/195` (K-C1) put a behaviour flag in `AppPreferences`
//!   rather than the document because "a setting that changes behaviour must
//!   not arrive inside a stranger project file".
//! * `docs/proposals/206` (K-G3) put layout presets in a *separate*
//!   `<config>/layouts.json` and explicitly **not** in `preferences.json`,
//!   because `AppPreferences::load` ends in `unwrap_or_default()` — one bad
//!   byte there silently resets every preference the user has. This module
//!   follows 206 exactly: its own file, its own tolerant loader, and a
//!   quarantine instead of an overwrite when the file will not parse.
//! * `photonic_video::export::presets` is the shipped shape for "built-ins in
//!   Rust + a user store in the config dir", down to the path-parameterized
//!   `_from` / `_to` test hooks. This module mirrors it.
//!
//! Consequences, stated as answers rather than omissions:
//!
//! * **`CURRENT_FORMAT_VERSION` stays at 5** and `Document` does not change.
//!   Nothing here is serialized into a project file, so there is no migration
//!   and no version bump to spend the `COMPAT_WINDOW` on.
//! * **Managing the library is not undoable** — saving, renaming, deleting a
//!   preset and starring an effect mutate a config file, never the document,
//!   so they correctly produce no history entry.
//! * **Applying a preset IS a document edit and is exactly ONE undo unit.**
//!   [`apply_commands`] returns the commands for the caller to wrap in a single
//!   `Command::Batch`, the same contract `ops::paste_clip_attributes` (K-B15)
//!   uses for a multi-clip paste.
//!
//! # Why `photonic-core` and not `photonic-video`
//!
//! 26 §10 K-B4 sketches `photonic-video/src/effects/presets.rs` "mirroring
//! `export/presets.rs`". That sketch predates the relocation of the effect
//! model into this crate: a preset's entire content is `Vec<ClipEffect>` plus
//! `Option<Grade>`, and every rule it must honour — id backfill, param
//! migration ([`effect_manifest::migrate`](super::effect_manifest::migrate)),
//! and inert-and-preservation for an id this build has no manifest for — is
//! implemented here in `photonic-core`. Putting the type in `photonic-video`
//! would add a strictly larger dependency for zero engine content and would
//! make every consumer (GUI, MCP) reach through the engine crate for a plain
//! data record. `export::presets` lives in `photonic-video` because *export*
//! is that crate's domain; effects are this one's.
//!
//! # Unknown effect ids (39 §2.2, spec 30 §2.6)
//!
//! A preset written by another build may name an effect this build has no
//! manifest for. The house rule is **inert-and-preserved: never dropped, never
//! guessed**. [`finalize`] marks such an entry `inert` and leaves its params
//! byte-identical, exactly as `timeline::load::finalize_effect_ids` does for a
//! document — with one deliberate difference recorded at [`finalize`]: it does
//! **not** clear `enabled`, because doing so in a *library* file would burn the
//! user's real choice into the store the first time they opened the library on
//! a build that lacked the effect.
//!
//! # Scope (`Applicability`) is gated in `ops::add_effect_scoped`, not here
//!
//! [`apply_commands`] takes any [`VfxOwner`] and appends via
//! [`ops::add_effect_scoped`](super::ops::add_effect_scoped). That helper
//! refuses owners outside
//! [`Applicability`](super::effect_manifest::Applicability) for known
//! manifests; this module does **not** invent a second gate. The catalogue
//! currently uses `ALL_SCOPES` (K-B1-compatible); `CLIP_ONLY` remains for
//! per-id curation. `apply_commands_inherits_applicability_gate` pins the
//! inheritance.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::clip::ClipEffect;
use super::commands::{TimelineCmd, VfxOwner};
use super::effect_manifest::{manifest, migrate, EffectId};
use super::grade::Grade;
use super::ops::{self, EditError};
use super::sequence::TimelineProject;

/// Schema version of `effect_presets.json`. Advisory, exactly as 206 §4.2 rule
/// 4 specifies for a config file: a *newer* version still loads best-effort,
/// because a user who ran a newer build once must not lose their library
/// permanently. It exists for diagnostics and for a future real migration, not
/// as a gate.
pub const LIBRARY_VERSION: u32 = 1;

// ── Schema ───────────────────────────────────────────────────────────────────

/// One named, saved effect stack.
///
/// This is deliberately the same vocabulary the scoped effect ops move around:
/// an ordered `Vec<ClipEffect>` plus optionally a [`Grade`] — i.e. the
/// look-carrying half of `ops::ClipAttributes` minus the two families that only
/// make sense per clip (`transform` is geometry, `audio` is timing-sensitive
/// and clamped per target).
///
/// A **parameter preset** ("Heavy Blur" for `blur.gaussian`) is not a separate
/// type: it is a preset whose stack is exactly one effect and which carries no
/// grade. [`EffectPreset::parameter_preset_for`] derives that rather than
/// storing a discriminant that could go stale against the content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectPreset {
    pub name: String,
    #[serde(default)]
    pub effects: Vec<ClipEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<Grade>,
}

impl EffectPreset {
    /// A preset from an ordered stack (the "save this stack as…" verb).
    pub fn new(name: impl Into<String>, effects: Vec<ClipEffect>, grade: Option<Grade>) -> Self {
        EffectPreset {
            name: name.into(),
            effects,
            grade,
        }
    }

    /// `Some(id)` when this preset is a single-effect *parameter* preset — the
    /// shape the effects browser files under its own effect rather than in the
    /// flat custom-stack list.
    pub fn parameter_preset_for(&self) -> Option<&EffectId> {
        match (self.effects.as_slice(), self.grade.as_ref()) {
            ([only], None) => Some(&only.id),
            _ => None,
        }
    }

    /// Entries this build has no manifest for, in stack order. Non-empty means
    /// the preset still applies — the unknown entries land inert-and-preserved.
    pub fn inert_ids(&self) -> Vec<String> {
        self.effects
            .iter()
            .filter(|e| e.inert)
            .map(|e| e.id.as_str().to_string())
            .collect()
    }
}

/// The user's preset library: their saved stacks plus their favourited effect
/// ids. Built-ins are **not** stored here (see [`built_in_presets`]) so a
/// shipped preset can never go stale in a user's file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EffectPresetLibrary {
    #[serde(default)]
    pub version: u32,
    /// User-defined presets, in the user's own order.
    #[serde(default)]
    pub presets: Vec<EffectPreset>,
    /// Favourited effect ids, in the user's own order — a plain ordering over
    /// the manifest catalogue the effects browser already renders, not a copy
    /// of it. An id this build has no manifest for stays in the list untouched
    /// (39 §2.2) and is simply not offered; it comes back the moment the user
    /// opens the build that has it.
    #[serde(default)]
    pub favourites: Vec<String>,
}

// ── Validation ───────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EffectPresetError {
    #[error("preset name must not be empty")]
    EmptyName,
    #[error("preset `{0}` has no effects and no grade — nothing to apply")]
    Empty(String),
    #[error("`{0}` is a built-in preset: duplicate it under a new name to edit it")]
    BuiltInName(String),
    #[error("no user preset named `{0}`")]
    NotFound(String),
}

/// Structural validation, run before a preset is persisted. Mirrors
/// `export::presets::validate`'s role: the UI refuses these cases too, but a
/// hand-authored or imported file must not be able to smuggle one past it.
pub fn validate(preset: &EffectPreset) -> Result<(), EffectPresetError> {
    if preset.name.trim().is_empty() {
        return Err(EffectPresetError::EmptyName);
    }
    if preset.effects.is_empty() && preset.grade.is_none() {
        return Err(EffectPresetError::Empty(preset.name.clone()));
    }
    Ok(())
}

// ── Built-in catalog ─────────────────────────────────────────────────────────

/// Seed one manifest-backed effect and override the listed param paths.
///
/// Every path is asserted against the manifest by
/// `built_in_param_paths_are_declared_by_their_manifest`, so a built-in cannot
/// quietly carry a param the effect does not have.
fn seeded(id: &'static str, params: &[(&str, f64)]) -> Option<ClipEffect> {
    let mut fx = ClipEffect::from_manifest(EffectId::new_static(id))?;
    for (path, value) in params {
        fx.params.base.set(
            super::anim::PropPath::new(*path),
            super::anim::PropValue::Float(*value),
        );
    }
    Some(fx)
}

/// The built-in presets, constructed in Rust rather than bundled as data — the
/// same choice 204 §3.4 and 206 §3.6 made, and for the same reason: no shipped
/// bytes means 23 §7.2's `AssetRightsManifest` gate is not engaged.
///
/// Built-ins are read-only: [`EffectPresetLibrary::upsert`] refuses to save
/// over one of these names and [`EffectPresetLibrary::remove`] refuses to
/// delete one, following `save_export_preset`'s shipped behaviour.
///
/// Deliberately short. A built-in that ships a look nobody asked for is
/// clutter; these three exist to establish both shapes (a multi-effect custom
/// stack, and a single-effect parameter preset) and to give the browser
/// something to render on first run.
pub fn built_in_presets() -> Vec<EffectPreset> {
    let mut out = Vec::new();
    if let (Some(grain), Some(vignette)) = (
        seeded("stylize.grain", &[("params.amount", 0.15)]),
        seeded(
            "stylize.vignette",
            &[("params.amount", -0.35), ("params.feather", 0.6)],
        ),
    ) {
        out.push(EffectPreset::new("Film Look", vec![grain, vignette], None));
    }
    if let (Some(vibrance), Some(sharpen)) = (
        seeded("color.vibrance", &[("params.amount", 0.35)]),
        seeded(
            "sharpen.unsharp",
            &[("params.amount", 0.6), ("params.radius", 2.0)],
        ),
    ) {
        out.push(EffectPreset::new("Punch Up", vec![vibrance, sharpen], None));
    }
    if let Some(blur) = seeded("blur.gaussian", &[("params.radius", 6.0)]) {
        out.push(EffectPreset::new("Soft Focus", vec![blur], None));
    }
    out
}

/// True when `name` is one of the read-only built-ins.
pub fn is_built_in(name: &str) -> bool {
    built_in_presets().iter().any(|p| p.name == name)
}

// ── Library operations (no undo unit — this is a config file) ────────────────

impl EffectPresetLibrary {
    /// An empty library stamped with this build's schema version.
    pub fn new() -> Self {
        EffectPresetLibrary {
            version: LIBRARY_VERSION,
            presets: Vec::new(),
            favourites: Vec::new(),
        }
    }

    /// Built-ins first, then the user's own presets — the order the browser
    /// renders and the order a "preset named X" lookup resolves in.
    pub fn catalogue(&self) -> Vec<EffectPreset> {
        let mut all = built_in_presets();
        all.extend(self.presets.iter().cloned());
        all
    }

    /// Look a preset up by name across built-ins and user presets.
    pub fn get(&self, name: &str) -> Option<EffectPreset> {
        self.catalogue().into_iter().find(|p| p.name == name)
    }

    /// Save a user preset, replacing a same-named one **in place** so the
    /// user's ordering survives an overwrite. Refuses a built-in name.
    pub fn upsert(&mut self, preset: EffectPreset) -> Result<(), EffectPresetError> {
        validate(&preset)?;
        if is_built_in(&preset.name) {
            return Err(EffectPresetError::BuiltInName(preset.name));
        }
        match self.presets.iter_mut().find(|p| p.name == preset.name) {
            Some(slot) => *slot = preset,
            None => self.presets.push(preset),
        }
        Ok(())
    }

    /// Delete a user preset. Refuses a built-in name; `NotFound` otherwise.
    pub fn remove(&mut self, name: &str) -> Result<(), EffectPresetError> {
        if is_built_in(name) {
            return Err(EffectPresetError::BuiltInName(name.to_string()));
        }
        let before = self.presets.len();
        self.presets.retain(|p| p.name != name);
        if self.presets.len() == before {
            return Err(EffectPresetError::NotFound(name.to_string()));
        }
        Ok(())
    }

    /// Rename a user preset, keeping its position. Refuses if either name is a
    /// built-in, and collapses onto an existing user preset of the new name
    /// (same rule as [`Self::upsert`]).
    ///
    /// The renamed preset is re-located by name *after* any clashing entry is
    /// dropped: removing an earlier element shifts every later index, and
    /// clamping the stale index instead would rename the wrong preset.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), EffectPresetError> {
        if is_built_in(from) {
            return Err(EffectPresetError::BuiltInName(from.to_string()));
        }
        if is_built_in(to) {
            return Err(EffectPresetError::BuiltInName(to.to_string()));
        }
        if to.trim().is_empty() {
            return Err(EffectPresetError::EmptyName);
        }
        if !self.presets.iter().any(|p| p.name == from) {
            return Err(EffectPresetError::NotFound(from.to_string()));
        }
        if from != to {
            self.presets.retain(|p| p.name != to);
        }
        let idx = self
            .presets
            .iter()
            .position(|p| p.name == from)
            .expect("the source preset was present a moment ago");
        self.presets[idx].name = to.to_string();
        Ok(())
    }

    pub fn is_favourite(&self, id: &str) -> bool {
        self.favourites.iter().any(|f| f == id)
    }

    /// Star / unstar an effect id. Returns the new state. Appending (rather
    /// than sorting) is what makes the list a user *ordering*.
    pub fn toggle_favourite(&mut self, id: &str) -> bool {
        if self.is_favourite(id) {
            self.favourites.retain(|f| f != id);
            false
        } else {
            self.favourites.push(id.to_string());
            true
        }
    }
}

// ── Load-time resolution (spec 30 §2.6 / 39 §2.2) ────────────────────────────

/// Resolve one preset against *this* build's catalogue, in place.
///
/// Mirrors `timeline::load::finalize_effect_ids`:
/// 1. **Backfill** an absent id from the legacy `kind`, and keep `kind`
///    consistent with an id that maps to one of the seven v1 kinds.
/// 2. **Migrate** a known id whose stored `version` predates the manifest's.
/// 3. **Inert preservation** for an id with no manifest: flag it, leave its
///    params untouched so re-serialization is byte-identical, and report it.
///
/// One deliberate difference from the document path: it does **not** clear
/// `enabled`. `graph::compile` skips an effect when `!enabled || inert`, so
/// `inert` alone is sufficient to keep it out of the render — while clearing
/// `enabled` in a *library* would persist a fabricated "off" into the user's
/// own file the first time they opened it on a build lacking the effect, and
/// the build that *does* have the effect would then resurrect it disabled.
/// Symmetrically, an entry that resolves here has `inert` cleared, so a preset
/// that round-trips A → B → A is unchanged.
///
/// Returns the unresolved ids, in stack order, for a caller to diagnose once.
pub fn finalize(preset: &mut EffectPreset) -> Vec<String> {
    let mut unresolved = Vec::new();
    for effect in &mut preset.effects {
        if effect.id.is_empty() {
            effect.id = effect.kind.effect_id();
        }
        if let Some(k) = effect.id.legacy_kind() {
            effect.kind = k;
        }
        match manifest(effect.id.clone()) {
            Some(m) => {
                effect.inert = false;
                if effect.version == 0 {
                    effect.version = m.version;
                } else if effect.version < m.version {
                    if let Ok(v) = migrate(&effect.id, effect.version, &mut effect.params.base) {
                        effect.version = v;
                    }
                }
            }
            None => {
                effect.inert = true;
                unresolved.push(effect.id.as_str().to_string());
            }
        }
    }
    unresolved
}

// ── Applying a preset — ONE undo unit ────────────────────────────────────────

/// Commands that apply `preset` to `owner`'s stack, for the caller to wrap in
/// ONE `Command::Batch`. Applying a preset is one user verb and must be one
/// undo step — the same contract `ops::paste_clip_attributes` documents.
///
/// * Effects are **appended** to the existing stack, in the preset's own order.
///   Appending (not replacing) is the Kdenlive "apply a custom effect" verb;
///   wholesale replacement already exists as K-B15's Paste Attributes.
/// * A preset's grade **replaces** the scope's grade, because a grade is a slot
///   and not a stack. A preset with no grade leaves the scope's grade alone —
///   "do not touch", the same rule `AttrSelector`'s `false` flags follow.
///
/// The `.rev()` is load-bearing, not stylistic. `ops::add_effect_scoped`
/// derives its insertion index from the project it is handed, which is the
/// **pre-batch** state for every call in this loop, so N ascending indices
/// would each clamp back to the same `len` and land the stack reversed.
/// Emitting the last entry first at that one fixed index leaves the preset's
/// order intact once the whole batch has applied, and `Command::Batch`'s
/// inverse (the reversed batch of inverses) unwinds it exactly.
/// `apply_appends_in_preset_order` and `apply_then_undo_restores_the_stack`
/// pin both halves.
pub fn apply_commands(
    p: &TimelineProject,
    owner: VfxOwner,
    preset: &EffectPreset,
) -> Result<Vec<TimelineCmd>, EditError> {
    // Resolve the owner before building anything, so an apply that cannot
    // land refuses whole rather than half.
    ops::effect_stack(p, owner)?;
    let mut cmds = Vec::with_capacity(preset.effects.len() + 1);
    for effect in preset.effects.iter().rev() {
        cmds.push(ops::add_effect_scoped(p, owner, effect.clone(), None)?);
    }
    if let Some(grade) = preset.grade.as_ref() {
        cmds.push(ops::set_grade_scoped(p, owner, Some(grade.clone()))?);
    }
    Ok(cmds)
}

// ── Persistence (`<config>/effect_presets.json`) ─────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum LibraryStoreError {
    #[error("could not resolve the app config directory")]
    NoConfigDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// What a load actually did, so the caller can diagnose **once** rather than
/// per frame (206 §4.2 rule 3).
#[derive(Debug, Default)]
pub struct LibraryLoad {
    pub library: EffectPresetLibrary,
    /// `(preset name, effect id)` for every entry this build could not resolve.
    pub inert: Vec<(String, String)>,
    /// Set when the file could not be parsed and was moved aside instead of
    /// being overwritten (206 §4.2 rule 5).
    pub quarantined: Option<PathBuf>,
}

/// `<config>/Photonic/effect_presets.json` — the same directory family as
/// `preferences.json`, `export_presets.json` and `layouts.json`, never the
/// project file.
pub fn library_path() -> Option<PathBuf> {
    crate::crash_dir().map(|d| d.join("effect_presets.json"))
}

/// Load the user library from the resolved config path.
pub fn load_library() -> Result<LibraryLoad, LibraryStoreError> {
    let path = library_path().ok_or(LibraryStoreError::NoConfigDir)?;
    load_library_from(&path)
}

/// Save the user library to the resolved config path.
pub fn save_library(library: &EffectPresetLibrary) -> Result<(), LibraryStoreError> {
    let path = library_path().ok_or(LibraryStoreError::NoConfigDir)?;
    save_library_to(&path, library)
}

/// Path-parameterized load, for tests (and for a future "import a preset pack"
/// action) without touching the real user config dir — the same test hook
/// `export::presets::load_custom_presets_from` ships.
///
/// **Never fails on content.** A missing file is an empty library; a file that
/// will not parse is renamed aside and reported, never overwritten and never
/// fatal. Only a genuine IO error propagates.
pub fn load_library_from(path: &Path) -> Result<LibraryLoad, LibraryStoreError> {
    if !path.exists() {
        return Ok(LibraryLoad {
            library: EffectPresetLibrary::new(),
            ..Default::default()
        });
    }
    let text = std::fs::read_to_string(path)?;
    let mut library: EffectPresetLibrary = match serde_json::from_str(&text) {
        Ok(l) => l,
        Err(_) => {
            let bad = quarantine(path)?;
            return Ok(LibraryLoad {
                library: EffectPresetLibrary::new(),
                inert: Vec::new(),
                quarantined: Some(bad),
            });
        }
    };
    let mut inert = Vec::new();
    for preset in &mut library.presets {
        for id in finalize(preset) {
            inert.push((preset.name.clone(), id));
        }
    }
    Ok(LibraryLoad {
        library,
        inert,
        quarantined: None,
    })
}

/// Path-parameterized save (see [`load_library_from`]). Stamps the current
/// schema version.
pub fn save_library_to(
    path: &Path,
    library: &EffectPresetLibrary,
) -> Result<(), LibraryStoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = library.clone();
    out.version = LIBRARY_VERSION;
    let json = serde_json::to_string_pretty(&out)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Move an unparseable library aside to `<name>.bad` (suffixing `.1`, `.2`, …
/// on collision) so the evidence and the content both survive. Returns the
/// path it was moved to.
fn quarantine(path: &Path) -> Result<PathBuf, LibraryStoreError> {
    let base = path.with_extension("json.bad");
    let mut target = base.clone();
    let mut n = 1u32;
    while target.exists() {
        target = base.with_extension(format!("bad.{n}"));
        n += 1;
    }
    std::fs::rename(path, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::history::Command;
    use crate::timeline::anim::PropValue;
    use crate::timeline::effect_manifest::MANIFESTS;
    use crate::timeline::{
        Clip, ClipSource, EffectKind, FrameRate, Sequence, Tick, Track, TrackKind,
    };

    /// A unique scratch directory under the OS temp dir. `photonic-core` has no
    /// `tempfile` dev-dependency (and adding one would churn `Cargo.lock` under
    /// `--locked`); `std::env::temp_dir()` is the in-crate precedent
    /// (`export.rs`'s PDF tests). Removed by [`Scratch`]'s `Drop`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "photonic_effect_presets_{}_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed),
                tag
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ── Built-ins ───────────────────────────────────────────────────────────

    #[test]
    fn every_built_in_resolves_against_this_builds_catalogue() {
        let builtins = built_in_presets();
        assert!(
            !builtins.is_empty(),
            "the built-in table produced nothing — every id it names must exist \
             in MANIFESTS"
        );
        for mut p in builtins {
            assert!(validate(&p).is_ok(), "built-in {} fails validation", p.name);
            let unresolved = finalize(&mut p);
            assert!(
                unresolved.is_empty(),
                "built-in {} names ids this build has no manifest for: {unresolved:?}",
                p.name
            );
        }
    }

    /// Derived, not a literal list: every param path a built-in writes must be
    /// declared by that effect's own manifest. `EffectParams::set` inserts an
    /// unknown path silently, so without this a typo ships as a dead param.
    #[test]
    fn built_in_param_paths_are_declared_by_their_manifest() {
        for p in built_in_presets() {
            for fx in &p.effects {
                let m = manifest(fx.id.clone())
                    .unwrap_or_else(|| panic!("{}: no manifest for {}", p.name, fx.id.as_str()));
                for (path, _) in &fx.params.base.entries {
                    assert!(
                        m.params.iter().any(|s| s.path == path.as_str()),
                        "{}: {} writes undeclared param {:?}",
                        p.name,
                        fx.id.as_str(),
                        path.as_str()
                    );
                }
            }
        }
    }

    /// The fixture is non-vacuous: at least one built-in must actually differ
    /// from the manifest defaults, or "presets" would be a no-op feature.
    #[test]
    fn at_least_one_built_in_differs_from_the_manifest_defaults() {
        let differing = built_in_presets()
            .iter()
            .flat_map(|p| p.effects.clone())
            .filter(|fx| {
                let seeded = ClipEffect::from_manifest(fx.id.clone()).unwrap();
                seeded.params.base != fx.params.base
            })
            .count();
        assert!(
            differing > 0,
            "no built-in overrides any manifest default — the preset table is vacuous"
        );
    }

    #[test]
    fn both_preset_shapes_ship_and_are_derived_not_stored() {
        let builtins = built_in_presets();
        let param_presets: Vec<_> = builtins
            .iter()
            .filter(|p| p.parameter_preset_for().is_some())
            .collect();
        let stacks: Vec<_> = builtins
            .iter()
            .filter(|p| p.parameter_preset_for().is_none())
            .collect();
        assert!(
            !param_presets.is_empty() && !stacks.is_empty(),
            "built-ins must cover both a single-effect parameter preset and a \
             multi-effect custom stack"
        );
        // Adding a grade to a one-effect preset re-classifies it, with no
        // stored discriminant to go stale.
        let mut one = param_presets[0].clone();
        assert!(one.parameter_preset_for().is_some());
        one.grade = Some(Grade::new());
        assert!(one.parameter_preset_for().is_none());
    }

    // ── Library management ──────────────────────────────────────────────────

    fn user_preset(name: &str) -> EffectPreset {
        EffectPreset::new(name, vec![ClipEffect::new(EffectKind::Blur)], None)
    }

    #[test]
    fn validate_rejects_empty_names_and_empty_presets() {
        assert_eq!(
            validate(&EffectPreset::new("  ", vec![], None)),
            Err(EffectPresetError::EmptyName)
        );
        assert_eq!(
            validate(&EffectPreset::new("Nothing", vec![], None)),
            Err(EffectPresetError::Empty("Nothing".into()))
        );
        // A grade with no effects is a legitimate preset.
        assert!(validate(&EffectPreset::new("Grade only", vec![], Some(Grade::new()))).is_ok());
    }

    #[test]
    fn built_ins_are_read_only() {
        let name = built_in_presets()[0].name.clone();
        let mut lib = EffectPresetLibrary::new();
        assert_eq!(
            lib.upsert(user_preset(&name)),
            Err(EffectPresetError::BuiltInName(name.clone()))
        );
        assert_eq!(
            lib.remove(&name),
            Err(EffectPresetError::BuiltInName(name.clone()))
        );
        assert_eq!(
            lib.rename(&name, "Mine"),
            Err(EffectPresetError::BuiltInName(name))
        );
        assert!(lib.presets.is_empty());
    }

    #[test]
    fn upsert_replaces_in_place_and_remove_reports_unknown_names() {
        let mut lib = EffectPresetLibrary::new();
        lib.upsert(user_preset("A")).unwrap();
        lib.upsert(user_preset("B")).unwrap();
        let mut replaced = user_preset("A");
        replaced.effects.push(ClipEffect::new(EffectKind::Glow));
        lib.upsert(replaced).unwrap();
        assert_eq!(
            lib.presets
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B"],
            "an overwrite must not move the preset to the end of the user's order"
        );
        assert_eq!(lib.presets[0].effects.len(), 2);
        assert_eq!(
            lib.remove("nope"),
            Err(EffectPresetError::NotFound("nope".into()))
        );
        lib.remove("A").unwrap();
        assert_eq!(lib.presets.len(), 1);
    }

    /// The index-shift case. Renaming `B` onto `A`'s name, where `A` sits
    /// *before* `B` and two more presets sit after, must rename **B** and drop
    /// **A**. Re-using the index captured before the clashing entry was removed
    /// renames `C` instead — this fixture is built with four entries precisely
    /// so that off-by-one is visible in the resulting name list.
    #[test]
    fn rename_over_an_earlier_preset_renames_the_right_one() {
        let mut lib = EffectPresetLibrary::new();
        for (name, kind) in [
            ("A", EffectKind::Blur),
            ("B", EffectKind::Glow),
            ("C", EffectKind::Invert),
            ("D", EffectKind::Sharpen),
        ] {
            lib.upsert(EffectPreset::new(name, vec![ClipEffect::new(kind)], None))
                .unwrap();
        }
        lib.rename("B", "A").unwrap();
        assert_eq!(
            lib.presets
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "C", "D"],
            "renaming B onto A must consume A and keep C/D untouched"
        );
        assert_eq!(
            lib.get("A").unwrap().effects[0].kind,
            EffectKind::Glow,
            "the surviving \"A\" must carry B's content"
        );
        assert_eq!(
            lib.rename("nope", "X"),
            Err(EffectPresetError::NotFound("nope".into()))
        );
        // Renaming to the same name is a no-op, not a self-delete.
        lib.rename("C", "C").unwrap();
        assert_eq!(lib.presets.len(), 3);
    }

    #[test]
    fn favourites_are_an_ordering_not_a_set_and_survive_unknown_ids() {
        let mut lib = EffectPresetLibrary::new();
        assert!(lib.toggle_favourite("stylize.glow"));
        assert!(lib.toggle_favourite("blur.gaussian"));
        // A star from a build that has an effect this one does not: preserved.
        assert!(lib.toggle_favourite("future.effect"));
        assert_eq!(
            lib.favourites,
            vec!["stylize.glow", "blur.gaussian", "future.effect"]
        );
        assert!(lib.is_favourite("future.effect"));
        assert!(manifest(EffectId::new("future.effect")).is_none());
        assert!(!lib.toggle_favourite("blur.gaussian"));
        assert_eq!(lib.favourites, vec!["stylize.glow", "future.effect"]);
    }

    // ── Unknown ids: inert-and-preserved ────────────────────────────────────

    #[test]
    fn an_unknown_effect_id_loads_inert_and_round_trips_verbatim() {
        let json = r#"{
          "version": 1,
          "presets": [
            {
              "name": "From a newer build",
              "effects": [
                {
                  "kind": "film_look",
                  "id": "stylize.film_look",
                  "version": 3,
                  "enabled": true,
                  "params": { "base": [["params.strength", {"t": "float", "v": 0.75}]] }
                }
              ]
            }
          ],
          "favourites": []
        }"#;
        let dir = Scratch::new("unknown_id");
        let path = dir.path("effect_presets.json");
        std::fs::write(&path, json).unwrap();

        let loaded = load_library_from(&path).unwrap();
        assert!(loaded.quarantined.is_none());
        assert_eq!(
            loaded.inert,
            vec![(
                "From a newer build".to_string(),
                "stylize.film_look".to_string()
            )],
            "an id with no manifest must be reported once, not dropped"
        );
        let fx = &loaded.library.presets[0].effects[0];
        assert!(fx.inert, "unknown id must load inert");
        assert!(
            fx.enabled,
            "the library path must not fabricate `enabled: false` — that would \
             persist a choice the user never made"
        );
        assert_eq!(fx.version, 3, "a future version must not be rewritten");
        assert_eq!(
            fx.params.base.get("params.strength"),
            Some(&PropValue::Float(0.75)),
            "unknown params must be preserved untouched"
        );

        // Save → load is byte-stable for the unknown entry.
        let path2 = dir.path("again.json");
        save_library_to(&path2, &loaded.library).unwrap();
        let again = load_library_from(&path2).unwrap();
        assert_eq!(again.library.presets, loaded.library.presets);
    }

    #[test]
    fn finalize_clears_inert_for_an_id_this_build_does_know() {
        let mut p = user_preset("Known");
        p.effects[0].id = EffectId::new_static("blur.gaussian");
        p.effects[0].inert = true;
        assert!(finalize(&mut p).is_empty());
        assert!(!p.effects[0].inert);
        assert!(p.effects[0].enabled);
    }

    // ── Store ───────────────────────────────────────────────────────────────

    #[test]
    fn a_missing_file_is_an_empty_library_not_an_error() {
        let dir = Scratch::new("missing");
        let loaded = load_library_from(&dir.path("nope.json")).unwrap();
        assert_eq!(loaded.library, EffectPresetLibrary::new());
        assert!(loaded.quarantined.is_none());
    }

    #[test]
    fn an_unparseable_file_is_quarantined_never_overwritten() {
        let dir = Scratch::new("quarantine");
        let path = dir.path("effect_presets.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let loaded = load_library_from(&path).unwrap();
        let bad = loaded.quarantined.expect("must report the quarantine");
        assert!(bad.exists(), "the original bytes must survive");
        assert_eq!(
            std::fs::read_to_string(&bad).unwrap(),
            "{ this is not json",
            "quarantine must move the file, not rewrite it"
        );
        assert!(!path.exists());
        assert!(loaded.library.presets.is_empty());

        // A second bad file does not clobber the first quarantine.
        std::fs::write(&path, "also not json").unwrap();
        let second = load_library_from(&path).unwrap().quarantined.unwrap();
        assert_ne!(second, bad);
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{ this is not json");
    }

    #[test]
    fn a_newer_schema_version_still_loads() {
        let dir = Scratch::new("newer_version");
        let path = dir.path("effect_presets.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"version": {}, "presets": [], "favourites": ["stylize.glow"]}}"#,
                LIBRARY_VERSION + 7
            ),
        )
        .unwrap();
        let loaded = load_library_from(&path).unwrap();
        assert!(loaded.quarantined.is_none());
        assert_eq!(
            loaded.library.favourites,
            vec!["stylize.glow"],
            "refusing a newer library would lose a user's whole library \
             permanently; a config file is re-creatable, a document is not"
        );
    }

    #[test]
    fn library_round_trips_through_the_store() {
        let dir = Scratch::new("round_trip");
        let path = dir.path("effect_presets.json");
        let mut lib = EffectPresetLibrary::new();
        lib.upsert(EffectPreset::new(
            "Mine",
            built_in_presets()[0].effects.clone(),
            Some(Grade::new()),
        ))
        .unwrap();
        lib.toggle_favourite("blur.gaussian");
        save_library_to(&path, &lib).unwrap();
        let back = load_library_from(&path).unwrap();
        assert_eq!(back.library, lib);
        assert_eq!(back.library.version, LIBRARY_VERSION);
    }

    // ── Applying — one undo unit ────────────────────────────────────────────

    /// A document carrying one video clip. `TimelineCmd::apply` takes a
    /// `Document`, so the fixture is one — the same shape `ops.rs`'s own tests
    /// use.
    fn doc_with_one_clip() -> (Document, VfxOwner) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("S", FrameRate::FPS_30, 320, 180);
        let mut track = Track::new(TrackKind::Video, "V1");
        let clip = Clip::new(
            ClipSource::SolidColor {
                color: crate::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
            Tick(0),
            Tick(1000),
        );
        let clip_id = clip.id;
        track.clips.push(clip);
        seq.video_tracks.push(track);
        project.insert_sequence(seq);
        let mut doc = Document::new("t", 1.0, 1.0);
        doc.timeline = Some(project);
        (doc, VfxOwner::Clip(clip_id))
    }

    fn project(doc: &Document) -> &TimelineProject {
        doc.timeline.as_ref().unwrap()
    }

    fn stack_ids(p: &TimelineProject, owner: VfxOwner) -> Vec<String> {
        ops::effect_stack(p, owner)
            .unwrap()
            .iter()
            .map(|e| e.id.as_str().to_string())
            .collect()
    }

    /// The order test. A naive ascending-index emission reverses the stack,
    /// because `add_effect_scoped` reads the pre-batch length every time —
    /// this asserts against the preset's own order, and the preset used has
    /// three *distinct* ids so a reversal cannot hide.
    #[test]
    fn apply_appends_in_preset_order() {
        let (mut doc, owner) = doc_with_one_clip();
        // Seed an existing effect so "append" is distinguishable from "insert".
        let seed = ClipEffect::from_manifest(EffectId::new_static("color.invert")).unwrap();
        TimelineCmd::AddEffect {
            owner,
            index: 0,
            effect: Box::new(seed),
        }
        .apply(&mut doc);

        let preset = EffectPreset::new(
            "Three",
            vec![
                ClipEffect::from_manifest(EffectId::new_static("blur.gaussian")).unwrap(),
                ClipEffect::from_manifest(EffectId::new_static("stylize.grain")).unwrap(),
                ClipEffect::from_manifest(EffectId::new_static("stylize.vignette")).unwrap(),
            ],
            None,
        );
        assert!(
            preset.effects.windows(2).all(|w| w[0].id != w[1].id),
            "fixture must use distinct ids or a reversal would be invisible"
        );

        let cmds = apply_commands(project(&doc), owner, &preset).unwrap();
        for c in &cmds {
            c.apply(&mut doc);
        }
        assert_eq!(
            stack_ids(project(&doc), owner),
            vec![
                "color.invert",
                "blur.gaussian",
                "stylize.grain",
                "stylize.vignette"
            ]
        );
    }

    /// Undo identity: the batch's inverse (the reversed batch of inverses, per
    /// `Command::Batch`) restores the stack exactly, including the position of
    /// the entry that was already there.
    #[test]
    fn apply_then_undo_restores_the_stack() {
        let (mut doc, owner) = doc_with_one_clip();
        let seed = ClipEffect::from_manifest(EffectId::new_static("color.invert")).unwrap();
        TimelineCmd::AddEffect {
            owner,
            index: 0,
            effect: Box::new(seed),
        }
        .apply(&mut doc);
        let before = ops::effect_stack(project(&doc), owner).unwrap().to_vec();

        let preset = &built_in_presets()[0];
        assert!(
            preset.effects.len() > 1,
            "the undo fixture needs a multi-effect preset to be meaningful"
        );
        let cmds = apply_commands(project(&doc), owner, preset).unwrap();
        assert_eq!(cmds.len(), preset.effects.len());

        // Wrap exactly as the caller does: ONE `Command::Batch`, one undo unit.
        let batch = Command::Batch(cmds.into_iter().map(Command::Timeline).collect());
        let pre = doc.clone();
        let inverse = batch.inverse(&doc).expect("batch must be invertible");
        batch.apply(&mut doc);
        assert_eq!(
            ops::effect_stack(project(&doc), owner).unwrap().len(),
            before.len() + preset.effects.len()
        );
        inverse.apply(&mut doc);
        assert_eq!(
            ops::effect_stack(project(&doc), owner).unwrap(),
            before.as_slice()
        );
        assert_eq!(
            doc.timeline, pre.timeline,
            "undo must restore the project exactly, not just the stack length"
        );
    }

    #[test]
    fn a_preset_grade_replaces_the_scope_grade_and_no_grade_leaves_it_alone() {
        let (doc, owner) = doc_with_one_clip();
        let with_grade = EffectPreset::new("G", vec![], Some(Grade::new()));
        let cmds = apply_commands(project(&doc), owner, &with_grade).unwrap();
        assert!(matches!(cmds.as_slice(), [TimelineCmd::SetGrade { .. }]));

        let without = &built_in_presets()[0];
        assert!(without.grade.is_none());
        let cmds = apply_commands(project(&doc), owner, without).unwrap();
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, TimelineCmd::SetGrade { .. })),
            "a preset with no grade must not touch the scope's grade"
        );
    }

    #[test]
    fn applying_to_an_owner_that_does_not_resolve_refuses_whole() {
        let (doc, _) = doc_with_one_clip();
        let ghost = VfxOwner::Clip(crate::timeline::ClipId::new());
        assert!(apply_commands(project(&doc), ghost, &built_in_presets()[0]).is_err());
    }

    #[test]
    fn an_inert_entry_is_carried_onto_the_clip_never_silently_dropped() {
        let (doc, owner) = doc_with_one_clip();
        let mut unknown = ClipEffect::new(EffectKind::Blur);
        unknown.id = EffectId::new("future.effect".to_string());
        let mut preset = EffectPreset::new("Mixed", vec![unknown], None);
        assert_eq!(finalize(&mut preset), vec!["future.effect".to_string()]);
        assert_eq!(preset.inert_ids(), vec!["future.effect".to_string()]);

        let cmds = apply_commands(project(&doc), owner, &preset).unwrap();
        assert_eq!(
            cmds.len(),
            1,
            "an unknown effect must still be applied (inert), not dropped"
        );
        match &cmds[0] {
            TimelineCmd::AddEffect { effect, .. } => {
                assert!(effect.inert);
                assert_eq!(effect.id.as_str(), "future.effect");
            }
            other => panic!("expected AddEffect, got {other:?}"),
        }
    }

    /// Inheritance pin (26 §10 K-B4 / 30 §2.3 / K-B residual). The gate lives
    /// only in `ops::add_effect_scoped`; `apply_commands` must not invent a
    /// parallel check. Catalogue is `ALL_SCOPES` so track/master/asset apply
    /// succeeds; `CLIP_ONLY` unit semantics stay true for future curation.
    #[test]
    fn apply_commands_inherits_applicability_gate() {
        use crate::timeline::effect_manifest::Applicability;
        use crate::timeline::ids::{ClipId, TrackId};

        let clip_only = Applicability::CLIP_ONLY;
        assert!(
            clip_only.allows(VfxOwner::Clip(ClipId::nil())),
            "CLIP_ONLY must still allow clip"
        );
        assert!(
            !clip_only.allows(VfxOwner::Track(TrackId::nil())),
            "CLIP_ONLY must still refuse track — curation uses this constant"
        );
        assert!(
            MANIFESTS
                .iter()
                .all(|m| m.applies == Applicability::ALL_SCOPES),
            "catalogue is ALL_SCOPES for K-B1; if a curated CLIP_ONLY lands, \
             add_effect_scoped already refuses — keep this assert honest or \
             add a live ApplicabilityDenied fixture for that id"
        );

        // Track-scoped apply through the preset path must succeed under
        // ALL_SCOPES (same route as a direct add_effect_scoped call).
        let (doc, _) = doc_with_one_clip();
        let p = project(&doc);
        let track_id = p.sequences.values().next().unwrap().video_tracks[0].id;
        let preset = EffectPreset::new(
            "Blur",
            vec![ClipEffect::from_manifest(EffectId::new_static("blur.gaussian")).unwrap()],
            None,
        );
        let cmds = apply_commands(p, VfxOwner::Track(track_id), &preset)
            .expect("ALL_SCOPES blur must apply on track via preset path");
        assert_eq!(cmds.len(), 1);

        // Unknown / unmanifested ids stay allowed (forward-compat) — same as
        // add_effect_scoped; the gate only consults known manifests.
        let mut unknown = ClipEffect::new(EffectKind::Blur);
        unknown.id = EffectId::new("future.scope_test".to_string());
        let inert_preset = EffectPreset::new("Future", vec![unknown], None);
        assert!(
            apply_commands(p, VfxOwner::Track(track_id), &inert_preset).is_ok(),
            "unmanifested ids must not invent a gate"
        );
    }
}
