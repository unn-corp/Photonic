//! Timed autosave for all open documents.
//!
//! Titled documents (those with a `.photon` path) are written to their real file
//! with the current state also recorded on a named "Autosave" branch of their
//! history — a non-destructive recovery point the user can switch to from the
//! Branches panel. Never-saved "Untitled" documents are written to a recovery
//! folder under `crash_dir()/recovery` and offered back on the next launch.

use super::*;
use std::path::PathBuf;

/// Directory holding recovery autosaves for untitled documents. `None` if the
/// platform config dir is unavailable.
pub(crate) fn recovery_dir() -> Option<PathBuf> {
    let dir = photonic_core::crash_dir()?.join("recovery");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Turn a tab title into a filesystem-safe stem for a recovery file.
fn sanitize_stem(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

// ─── Test hooks (CAP-022 crash-recovery integration test) ─────────────────────
//
// `write_photon_file`/`load_document` (defined in `app/mod.rs`) are private to
// the `app` module tree, and `apply_opened_history` is trivial enough to
// reproduce inline from `CommandHistory`'s already-public API — so no hook is
// needed for it. An external integration test crate
// (`crates/photonic-gui/tests/timeline_recovery.rs`) can only reach items
// through a fully `pub` module chain, so exercising the *real* write/load
// functions from there (rather than duplicating their logic) requires both:
// this pair of thin wrappers, and `app::autosave` itself being declared `pub`
// in `app/mod.rs` (was `pub(crate)`; documented there). Nothing else in the
// crash-recovery machinery changes visibility.

/// Test hook: the exact write call `autosave_all` makes for an untitled tab's
/// recovery snapshot (`write_photon_file(&recovery_path, doc, history)`).
pub fn recovery_write_for_test(
    path: &std::path::Path,
    doc: &Document,
    history: &mut CommandHistory,
) -> Result<(), String> {
    write_photon_file(path, doc, history)
}

/// Test hook: the exact load call `draw_recovery_prompt`'s Restore branch
/// makes for each discovered recovery candidate (`load_document(&path)`).
pub fn recovery_load_for_test(
    path: &std::path::Path,
) -> Result<(Document, Option<photonic_core::HistorySnapshot>), String> {
    load_document(path)
}

impl PhotonicApp {
    /// Run the autosave timer. Called once per frame from `draw`. Fires a full
    /// autosave pass every `autosave_interval_secs` while enabled.
    pub(crate) fn run_autosave(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        history: &mut CommandHistory,
    ) {
        if !self.prefs.autosave_enabled || self.tabs.is_empty() {
            return;
        }
        let now = ctx.input(|i| i.time);
        // Anchor the first interval to launch/enable time (no immediate save).
        let last = *self.last_autosave.get_or_insert(now);
        let interval = self.prefs.autosave_interval_secs.max(15.0);
        if now - last < interval {
            return;
        }
        self.last_autosave = Some(now);
        self.autosave_all(doc, history);
    }

    /// Autosave the active document plus every dirty parked tab.
    fn autosave_all(&mut self, doc: &mut Document, history: &mut CommandHistory) {
        let active = self.active_tab;
        let mut saved = 0u32;
        let mut recovered = 0u32;

        // ── Active document (live engine state in the `&mut` params) ──────────
        if self.tabs.get(active).is_some_and(|t| t.dirty) {
            if let Some(path) = self.current_file.clone() {
                // The saved commit is marked by the "Last Save" ring
                // (`last_saved_node`), not a branch label — branch names move with
                // the tip, so they'd drift off the saved point.
                if write_photon_file(&path, doc, history).is_ok() {
                    let node = history.current_node();
                    if let Some(t) = self.tabs.get_mut(active) {
                        t.dirty = false;
                        t.last_saved_node = Some(node);
                    }
                    saved += 1;
                }
            } else if let Some(rp) = self.ensure_recovery_path(active) {
                if write_photon_file(&rp, doc, history).is_ok() {
                    recovered += 1;
                }
            }
        }

        // ── Parked documents (owned engine state in each DocTab) ──────────────
        for i in 0..self.tabs.len() {
            if i == active || !self.tabs[i].dirty {
                continue;
            }
            if let Some(path) = self.tabs[i].current_file.clone() {
                let node = {
                    let tab = &mut self.tabs[i];
                    if write_photon_file(&path, &tab.document, &mut tab.history).is_ok() {
                        Some(tab.history.current_node())
                    } else {
                        None
                    }
                };
                if let Some(n) = node {
                    self.tabs[i].dirty = false;
                    self.tabs[i].last_saved_node = Some(n);
                    saved += 1;
                }
            } else if let Some(rp) = self.ensure_recovery_path(i) {
                let tab = &mut self.tabs[i];
                if write_photon_file(&rp, &tab.document, &mut tab.history).is_ok() {
                    recovered += 1;
                }
            }
        }

        if saved > 0 || recovered > 0 {
            self.file_status = Some(match (saved, recovered) {
                (s, 0) => format!("Autosaved {s} document(s)"),
                (0, r) => format!("Autosaved {r} recovery snapshot(s)"),
                (s, r) => format!("Autosaved {s} document(s), {r} recovery"),
            });
        }
    }

    /// The recovery-file path for the untitled tab at `idx`, creating and storing a
    /// stable one on first use. `None` if no recovery directory is available.
    fn ensure_recovery_path(&mut self, idx: usize) -> Option<PathBuf> {
        if let Some(rp) = self.tabs.get(idx).and_then(|t| t.recovery_path.clone()) {
            return Some(rp);
        }
        let dir = recovery_dir()?;
        self.untitled_counter += 1;
        let stem = sanitize_stem(&self.tabs[idx].title);
        let path = dir.join(format!(
            "{stem}-{}.{}",
            self.untitled_counter, PHOTON_FILE_EXTENSION
        ));
        if let Some(t) = self.tabs.get_mut(idx) {
            t.recovery_path = Some(path.clone());
        }
        Some(path)
    }
}
