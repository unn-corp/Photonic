use crate::commands::KeyBinding;
use crate::hotbar::{HotbarBucket, HotbarMode};
use crate::panels::{DrawerGroup, RightDrawerGroup};
use crate::tools::Tool;
use photonic_core::write_atomic_file;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Standalone, portable representation of the user's shortcut overrides.
/// Keep this deliberately narrower than `AppPreferences`: importing a keymap
/// must not overwrite unrelated preferences such as theme or autosave settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeymapFile {
    version: u32,
    keymap: HashMap<String, KeyBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppPreferences {
    // APPEARANCE
    pub dark_mode: bool,
    pub ui_scale: f32, // 0.75, 1.0, 1.25, 1.5, 2.0

    // CANVAS
    pub show_grid: bool,
    pub grid_size: u32, // 8, 16, 32, 64
    pub snap_to_grid: bool,
    pub grid_color: [f32; 4], // RGBA as f32 (matches egui color picker API)
    pub show_rulers: bool,
    /// Object-aware snapping: align a dragged node's edges/centers to nearby
    /// nodes during a move drag (#66). Additive with `snap_to_grid`.
    #[serde(default = "default_true")]
    pub snap_to_objects: bool,
    /// Also snap to the artboard/canvas edges, center, and margins (#211).
    #[serde(default = "default_true")]
    pub snap_to_artboard: bool,
    /// Also snap to path anchor points (#211). Off by default — dense paths add
    /// many candidates.
    #[serde(default)]
    pub snap_to_anchors: bool,
    /// Snap pull radius in screen pixels (converted to canvas units via zoom).
    #[serde(default = "default_snap_tolerance")]
    pub snap_tolerance_px: f32,
    /// Draw the dashed smart-guide lines + distance labels while snapping.
    #[serde(default = "default_true")]
    pub snap_show_guides: bool,
    /// Measurement unit used for ruler labels and the live cursor readout.
    #[serde(default)]
    pub document_units: photonic_core::DocumentUnit,
    /// Icon keyline template overlay (#208): draws the classic Material/Apple
    /// keyline safe-area shapes (square, circle, portrait, landscape) centered on
    /// the artboard so icon geometry can be aligned to a consistent grid.
    #[serde(default)]
    pub show_keyline_grid: bool,
    /// Snap drawing/moving to whole document pixels (#208). Additive with grid /
    /// object snapping; makes icon geometry land on crisp integer coordinates.
    #[serde(default)]
    pub snap_to_pixel: bool,

    // TOOL DEFAULTS
    pub default_fill_color: [f32; 4],
    pub default_stroke_enabled: bool,
    pub default_stroke_color: [f32; 4],
    pub default_stroke_width: f32,

    // BEHAVIOR
    pub console_open_on_start: bool,
    /// Prefer winit's X11/XWayland backend on Linux. This is an opt-in
    /// workaround for winit 0.30 Wayland's missing file drag-and-drop support
    /// (#198), and takes effect on the next launch.
    #[serde(default)]
    pub force_x11_backend: bool,
    /// Arrow-key nudge distance in document pixels (Shift multiplies by 10).
    #[serde(default = "default_nudge_distance")]
    pub nudge_distance: f64,
    /// Periodically write open documents to disk on a timer. On for titled files
    /// (recorded on an "Autosave" history branch) and untitled files (to a
    /// recovery folder). See the autosave loop in `app::PhotonicApp::draw`.
    #[serde(default = "default_true")]
    pub autosave_enabled: bool,
    /// Seconds between autosave passes. Default 5 minutes.
    #[serde(default = "default_autosave_interval_secs")]
    pub autosave_interval_secs: f64,

    // HISTORY — bound on the project undo/redo history persisted in the .photon
    // file. The user picks the unit: a step count, or a serialized-size budget
    // (in MB) applied to the history payload specifically (separate from the
    // document's own size). Once the history exceeds the cap, the oldest steps
    // are discarded to make room (with a warning the first time).
    #[serde(default)]
    pub history_limit_mode: HistoryLimitMode,
    /// Max retained undo steps when `history_limit_mode == Steps`.
    #[serde(default = "default_history_max_steps")]
    pub history_max_steps: usize,
    /// Max serialized history size in MB when `history_limit_mode == Size`.
    #[serde(default = "default_history_max_mb")]
    pub history_max_mb: f64,
    /// Check GitHub for a newer release once on launch and prompt if available.
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    /// Last app version this user actually ran. Drives the "What's New" popup:
    /// when it differs from the current build, show notes for the gap. Empty on
    /// a fresh install (no popup the very first time).
    #[serde(default)]
    pub last_seen_version: String,
    /// Consent for opt-in crash reporting (#59). `None` = never asked (off until
    /// the one-time dialog is answered), `Some(false)` = declined, `Some(true)` =
    /// allowed to offer pre-filled bug reports. Off by default — nothing is ever
    /// sent without an explicit, user-reviewed action.
    #[serde(default)]
    pub crash_reporting_consent: Option<bool>,

    // HOTBAR — tools pinned to the sidebar by the user
    #[serde(default)]
    pub pinned_tools: Vec<Tool>,

    // DRAWER UI — Canva-style left icon rail + single animated drawer.
    /// Which drawer group is open, or `None` when only the rail shows. Defaults
    /// to the Inspector so launch looks ~like the old always-on panel.
    #[serde(default = "default_open_drawer")]
    pub open_drawer: Option<DrawerGroup>,
    /// Target (fully-open) width of the drawer panel, in logical px.
    #[serde(default = "default_drawer_width")]
    pub drawer_width: f32,
    /// Which group is open in the right rail, or `None` when only the right rail
    /// shows. Defaults to Layers so launch looks ~like the old always-on panel.
    #[serde(default = "default_open_right_drawer")]
    pub open_right_drawer: Option<RightDrawerGroup>,
    /// Target (fully-open) width of the right drawer panel, in logical px.
    #[serde(default = "default_right_drawer_width")]
    pub right_drawer_width: f32,
    /// When true, drawer open/close transitions are instant (no width tween) —
    /// honours the user's reduced-motion preference.
    #[serde(default)]
    pub reduced_motion: bool,

    // HOTBAR — the always-on adaptive second toolbar row (#154 Phase 4).
    /// Static (curated default order) or Adaptive (ranked by the user's usage).
    #[serde(default)]
    pub hotbar_mode: HotbarMode,
    /// Per-bucket usage scores: bucket key → (item id → frequency-with-decay).
    /// Bumped each time a hotbar item is invoked in that bucket; drives the
    /// Adaptive ordering. Empty = cold start (falls back to static order).
    #[serde(default)]
    pub hotbar_usage: HashMap<String, HashMap<String, f32>>,

    // KEYBOARD — user shortcut overrides, keyed by `commands::CommandId`.
    // Empty by default (every command uses its registry default). User remaps in
    // the Keyboard Shortcuts settings page populate this and persist to disk.
    #[serde(default)]
    pub keymap: HashMap<String, KeyBinding>,
}

fn default_nudge_distance() -> f64 {
    1.0
}

fn default_autosave_interval_secs() -> f64 {
    300.0
}

fn default_open_drawer() -> Option<DrawerGroup> {
    Some(DrawerGroup::Tools)
}

fn default_drawer_width() -> f32 {
    220.0
}

fn default_open_right_drawer() -> Option<RightDrawerGroup> {
    Some(RightDrawerGroup::Layers)
}

fn default_right_drawer_width() -> f32 {
    280.0
}

/// How the project-history retention limit is measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HistoryLimitMode {
    /// Cap by number of undo steps. Retained only for back-compat deserialization
    /// of older preference files; retention is size-only now (#197).
    Steps,
    /// Cap by serialized size of the history payload (MB). The only mode used.
    #[default]
    Size,
}

/// Hard ceiling applied in Size mode so memory stays bounded regardless of how
/// large the byte budget is. The size cap does the real trimming; this just
/// prevents an unbounded step count.
pub const HISTORY_SIZE_MODE_STEP_CEILING: usize = 100_000;

fn default_history_max_steps() -> usize {
    200
}

fn default_history_max_mb() -> f64 {
    50.0
}

fn default_true() -> bool {
    true
}

fn default_snap_tolerance() -> f32 {
    6.0
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            dark_mode: true,
            ui_scale: 1.0,
            show_grid: false,
            grid_size: 16,
            snap_to_grid: false,
            grid_color: [0.31, 0.31, 0.47, 0.24], // muted violet, semi-transparent
            show_rulers: false,
            snap_to_objects: true,
            snap_to_artboard: true,
            snap_to_anchors: false,
            snap_tolerance_px: 6.0,
            snap_show_guides: true,
            document_units: photonic_core::DocumentUnit::Px,
            show_keyline_grid: false,
            snap_to_pixel: false,
            default_fill_color: [0.22, 0.47, 0.87, 1.0],
            default_stroke_enabled: false,
            default_stroke_color: [0.0, 0.0, 0.0, 1.0],
            default_stroke_width: 1.0,
            console_open_on_start: false,
            force_x11_backend: false,
            nudge_distance: 1.0,
            autosave_enabled: true,
            autosave_interval_secs: 300.0,
            history_limit_mode: HistoryLimitMode::Size,
            history_max_steps: 200,
            history_max_mb: 50.0,
            auto_check_updates: true,
            last_seen_version: String::new(),
            crash_reporting_consent: None,
            pinned_tools: Vec::new(),
            open_drawer: Some(DrawerGroup::Tools),
            drawer_width: 220.0,
            open_right_drawer: Some(RightDrawerGroup::Layers),
            right_drawer_width: 280.0,
            reduced_motion: false,
            hotbar_mode: HotbarMode::default(),
            hotbar_usage: HashMap::new(),
            keymap: HashMap::new(),
        }
    }
}

impl AppPreferences {
    /// Write only the shortcut overrides as a human-readable JSON file.
    pub fn export_keymap(&self, path: &Path) -> Result<(), String> {
        let file = KeymapFile {
            version: 1,
            keymap: self.keymap.clone(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    /// Replace shortcut overrides from a portable keymap file and persist them.
    pub fn import_keymap(&mut self, path: &Path) -> Result<usize, String> {
        let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let file: KeymapFile = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        if file.version != 1 {
            return Err(format!("unsupported keymap file version {}", file.version));
        }
        let count = file.keymap.len();
        self.keymap = file.keymap;
        self.save();
        Ok(count)
    }
    /// The active binding for a command: the user override if present, otherwise
    /// the registry default. `None` means the command has no shortcut.
    pub fn resolve_binding(&self, id: &str) -> Option<KeyBinding> {
        if let Some(b) = self.keymap.get(id) {
            return Some(*b);
        }
        crate::commands::default_binding(id)
    }

    /// Any other command whose *resolved* binding equals `binding`, excluding
    /// `for_id`. Used for conflict warnings in the Keyboard Shortcuts UI.
    pub fn binding_conflict(&self, for_id: &str, binding: KeyBinding) -> Option<String> {
        for def in crate::commands::REGISTRY {
            if def.id == for_id {
                continue;
            }
            if self.resolve_binding(def.id) == Some(binding) {
                return Some(def.label.to_string());
            }
        }
        None
    }

    /// Resolve the configured history retention limits as
    /// `(max_steps, size_limit_bytes)` for [`photonic_core::CommandHistory::set_limits`].
    ///
    /// Retention is **size-only** (#197): the serialized byte budget does the
    /// real trimming and a high internal step ceiling is only a runaway backstop.
    /// The legacy `history_limit_mode`/`history_max_steps` fields are retained for
    /// back-compat deserialization but no longer govern retention.
    pub fn history_limits(&self) -> (usize, Option<u64>) {
        let bytes = (self.history_max_mb.max(0.1) * 1_048_576.0) as u64;
        (HISTORY_SIZE_MODE_STEP_CEILING, Some(bytes))
    }

    /// Current usage score for a hotbar item within a bucket (0 if unseen).
    pub fn hotbar_score(&self, bucket: HotbarBucket, item_id: &str) -> f32 {
        self.hotbar_usage
            .get(bucket.key())
            .and_then(|m| m.get(item_id))
            .copied()
            .unwrap_or(0.0)
    }

    /// Record one use of `item_id` in `bucket`: mildly decay every score in the
    /// bucket, then bump the used item by `+1`. Frequency with a recency bias.
    pub fn bump_hotbar_usage(&mut self, bucket: HotbarBucket, item_id: &str) {
        const DECAY: f32 = 0.95;
        let m = self
            .hotbar_usage
            .entry(bucket.key().to_string())
            .or_default();
        for v in m.values_mut() {
            *v *= DECAY;
        }
        *m.entry(item_id.to_string()).or_insert(0.0) += 1.0;
    }

    fn prefs_path() -> Option<std::path::PathBuf> {
        crate::welcome::config_dir().map(|d| d.join("preferences.json"))
    }

    /// Load from disk, falling back to Default on any error.
    pub fn load() -> Self {
        let path = match Self::prefs_path() {
            Some(p) => p,
            None => return Self::default(),
        };
        let json = match std::fs::read_to_string(&path) {
            Ok(j) => j,
            Err(_) => return Self::default(),
        };
        match serde_json::from_str(&json) {
            Ok(preferences) => preferences,
            Err(error) => {
                tracing::error!(
                    path = %path.display(),
                    error = %error,
                    "failed to parse application preferences; using defaults"
                );
                Self::default()
            }
        }
    }

    /// Serialize and atomically write to disk.
    ///
    /// Persistence failures are logged and leave any existing preferences file
    /// untouched. Keeping this method infallible preserves the existing call
    /// sites while making failed writes observable.
    pub fn save(&self) {
        let Some(path) = Self::prefs_path() else {
            tracing::error!("unable to persist application preferences: no config directory");
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                tracing::error!(
                    path = %parent.display(),
                    error = %error,
                    "failed to create application preferences directory"
                );
                return;
            }
        }
        let json = match serde_json::to_string_pretty(self) {
            Ok(json) => json,
            Err(error) => {
                tracing::error!(
                    path = %path.display(),
                    error = %error,
                    "failed to serialize application preferences"
                );
                return;
            }
        };
        if let Err(error) = write_atomic_file(&path, json.as_bytes()) {
            tracing::error!(
                path = %path.display(),
                error = %error,
                "failed to persist application preferences"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_backend_is_opt_in_and_backwards_compatible() {
        assert!(!AppPreferences::default().force_x11_backend);

        let mut old_preferences =
            serde_json::to_value(AppPreferences::default()).expect("preferences serialize");
        old_preferences
            .as_object_mut()
            .expect("preferences are a JSON object")
            .remove("force_x11_backend");

        let loaded: AppPreferences =
            serde_json::from_value(old_preferences).expect("older preferences deserialize");
        assert!(!loaded.force_x11_backend);
    }

    #[test]
    fn keymap_file_roundtrips_without_other_preferences() {
        let path =
            std::env::temp_dir().join(format!("photonic-keymap-{}.json", uuid::Uuid::new_v4()));
        let mut prefs = AppPreferences::default();
        prefs
            .keymap
            .insert("edit.undo".into(), KeyBinding::ctrl(egui::Key::U));
        prefs.export_keymap(&path).expect("export keymap");

        let mut imported = AppPreferences::default();
        imported.dark_mode = false;
        assert_eq!(imported.import_keymap(&path).expect("import keymap"), 1);
        assert_eq!(imported.keymap, prefs.keymap);
        assert!(!imported.dark_mode);
        let _ = std::fs::remove_file(path);
    }
}
