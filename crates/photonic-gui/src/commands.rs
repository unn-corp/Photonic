//! Command registry + key bindings for customizable keyboard shortcuts and the
//! searchable command palette (Ctrl/Cmd+K).
//!
//! Every user-facing editor action that can carry a keyboard shortcut is given a
//! stable [`CommandId`] (`&'static str`) and a default [`KeyBinding`] here. The
//! user's overrides live in `AppPreferences::keymap` (a `HashMap<String,
//! KeyBinding>` keyed by command id); `AppPreferences::resolve_binding` layers
//! the user map over these registry defaults. Tool activations are surfaced in
//! the palette too via [`TOOL_COMMANDS`].
//!
//! A `KeyBinding` serializes to/from a compact string like `"ctrl+shift+g"` so
//! the keymap round-trips through the JSON preferences file as a plain object.

use crate::Tool;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::OnceLock;

/// Stable identifier for a command. Used as the keymap key and palette id.
pub type CommandId = &'static str;

/// A single keyboard shortcut: a key plus modifier flags. `ctrl` and `command`
/// are both treated as the "primary" modifier so a binding works on Linux/Windows
/// (Ctrl) and macOS (Cmd) without per-platform tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBinding {
    pub key: egui::Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub command: bool,
}

impl KeyBinding {
    /// A primary-modifier (Ctrl/Cmd) + key binding, e.g. Ctrl+Z.
    pub const fn ctrl(key: egui::Key) -> Self {
        Self {
            key,
            ctrl: true,
            shift: false,
            alt: false,
            command: false,
        }
    }
    /// Ctrl/Cmd + Shift + key, e.g. Ctrl+Shift+G.
    pub const fn ctrl_shift(key: egui::Key) -> Self {
        Self {
            key,
            ctrl: true,
            shift: true,
            alt: false,
            command: false,
        }
    }
    /// Ctrl/Cmd + Alt + key, e.g. Ctrl+Alt+Y.
    pub const fn ctrl_alt(key: egui::Key) -> Self {
        Self {
            key,
            ctrl: true,
            shift: false,
            alt: true,
            command: false,
        }
    }
    /// A bare key with no modifiers, e.g. Delete.
    pub const fn plain(key: egui::Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: false,
            alt: false,
            command: false,
        }
    }
    /// Shift + key, no primary modifier, e.g. Shift+Z.
    pub const fn shift(key: egui::Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: true,
            alt: false,
            command: false,
        }
    }

    /// Alt + key, no primary modifier, e.g. Alt+Left.
    pub const fn alt(key: egui::Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: false,
            alt: true,
            command: false,
        }
    }

    /// True if this binding fires for the given live modifier state. Ctrl and Cmd
    /// are interchangeable (primary). Shift/Alt must match exactly.
    pub fn matches(&self, m: egui::Modifiers) -> bool {
        let want_primary = self.ctrl || self.command;
        let have_primary = m.ctrl || m.command || m.mac_cmd;
        want_primary == have_primary && self.shift == m.shift && self.alt == m.alt
    }

    /// Storage form, e.g. `"ctrl+shift+g"`. Lower-cased; key uses egui's name.
    pub fn to_storage_string(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl || self.command {
            parts.push("ctrl".to_string());
        }
        if self.shift {
            parts.push("shift".to_string());
        }
        if self.alt {
            parts.push("alt".to_string());
        }
        parts.push(self.key.name().to_ascii_lowercase());
        parts.join("+")
    }

    /// Parse the storage form back into a binding. Case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut key: Option<egui::Key> = None;
        for tok in s.split('+') {
            let t = tok.trim();
            if t.is_empty() {
                continue;
            }
            match t.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" | "super" | "meta" => ctrl = true,
                "shift" => shift = true,
                "alt" | "option" | "opt" => alt = true,
                _ => {
                    let found = egui::Key::ALL
                        .iter()
                        .copied()
                        .find(|k| k.name().eq_ignore_ascii_case(t))
                        .or_else(|| egui::Key::from_name(t));
                    key = key.or(found);
                }
            }
        }
        Some(Self {
            key: key?,
            ctrl,
            shift,
            alt,
            command: false,
        })
    }

    /// Human-readable label for the UI, e.g. `"Ctrl+Shift+["`.
    pub fn display(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl || self.command {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        parts.push(display_key(self.key).to_string());
        parts.join("+")
    }
}

/// Friendlier glyphs for keys whose egui name is verbose.
fn display_key(k: egui::Key) -> &'static str {
    match k {
        egui::Key::OpenBracket => "[",
        egui::Key::CloseBracket => "]",
        egui::Key::Semicolon => ";",
        egui::Key::Quote => "'",
        egui::Key::Plus => "+",
        egui::Key::Minus => "-",
        egui::Key::Equals => "=",
        egui::Key::Comma => ",",
        egui::Key::Period => ".",
        egui::Key::Slash => "/",
        egui::Key::Backslash => "\\",
        _ => k.name(),
    }
}

impl Serialize for KeyBinding {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_storage_string())
    }
}

impl<'de> Deserialize<'de> for KeyBinding {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        KeyBinding::parse(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid key binding: {s:?}")))
    }
}

/// A registered command: a stable id, a human label, and an optional default
/// shortcut. `default == None` means "no default key" (still palette-reachable).
pub struct CommandDef {
    pub id: CommandId,
    pub label: &'static str,
    pub default: Option<KeyBinding>,
}

use egui::Key;

/// Every shortcut-bearing editor action. Ids are stable and used as keymap keys.
pub static REGISTRY: &[CommandDef] = &[
    // ── Edit ──────────────────────────────────────────────────────────────
    CommandDef {
        id: "edit.undo",
        label: "Undo",
        default: Some(KeyBinding::ctrl(Key::Z)),
    },
    CommandDef {
        id: "edit.redo",
        label: "Redo",
        default: Some(KeyBinding::ctrl(Key::R)),
    },
    CommandDef {
        id: "edit.copy",
        label: "Copy",
        default: Some(KeyBinding::ctrl(Key::C)),
    },
    CommandDef {
        id: "edit.paste",
        label: "Paste",
        default: Some(KeyBinding::ctrl(Key::V)),
    },
    CommandDef {
        id: "edit.paste_in_place",
        label: "Paste in Place",
        default: Some(KeyBinding::ctrl_shift(Key::V)),
    },
    CommandDef {
        id: "edit.duplicate",
        label: "Duplicate",
        default: Some(KeyBinding::ctrl(Key::D)),
    },
    CommandDef {
        id: "edit.delete",
        label: "Delete Selection",
        default: Some(KeyBinding::plain(Key::Delete)),
    },
    // ── Selection ─────────────────────────────────────────────────────────
    CommandDef {
        id: "selection.select_all",
        label: "Select All",
        default: Some(KeyBinding::ctrl(Key::A)),
    },
    CommandDef {
        id: "selection.deselect",
        label: "Deselect All",
        default: Some(KeyBinding::ctrl_shift(Key::A)),
    },
    // ── Object / arrange ──────────────────────────────────────────────────
    CommandDef {
        id: "object.group",
        label: "Group",
        default: Some(KeyBinding::ctrl(Key::G)),
    },
    CommandDef {
        id: "object.ungroup",
        label: "Ungroup",
        default: Some(KeyBinding::ctrl_shift(Key::G)),
    },
    CommandDef {
        id: "object.ungroup_all",
        label: "Ungroup All",
        default: None,
    },
    CommandDef {
        id: "object.bring_forward",
        label: "Bring Forward",
        default: Some(KeyBinding::ctrl(Key::CloseBracket)),
    },
    CommandDef {
        id: "object.send_backward",
        label: "Send Backward",
        default: Some(KeyBinding::ctrl(Key::OpenBracket)),
    },
    CommandDef {
        id: "object.bring_to_front",
        label: "Bring to Front",
        default: Some(KeyBinding::ctrl_shift(Key::CloseBracket)),
    },
    CommandDef {
        id: "object.send_to_back",
        label: "Send to Back",
        default: Some(KeyBinding::ctrl_shift(Key::OpenBracket)),
    },
    CommandDef {
        id: "object.flip_horizontal",
        label: "Flip Horizontal",
        default: Some(KeyBinding::ctrl_shift(Key::H)),
    },
    CommandDef {
        id: "object.flip_vertical",
        label: "Flip Vertical",
        default: Some(KeyBinding::ctrl_shift(Key::J)),
    },
    // ── View ──────────────────────────────────────────────────────────────
    CommandDef {
        id: "view.outline_mode",
        label: "Toggle Outline Mode",
        default: Some(KeyBinding::ctrl(Key::Y)),
    },
    CommandDef {
        id: "view.pixel_preview",
        label: "Toggle Pixel Preview",
        default: Some(KeyBinding::ctrl_alt(Key::Y)),
    },
    CommandDef {
        id: "view.overprint_preview",
        label: "Toggle Overprint Preview",
        default: Some(KeyBinding::ctrl_shift(Key::Y)),
    },
    CommandDef {
        id: "view.toggle_guides",
        label: "Toggle Guides",
        default: Some(KeyBinding::ctrl(Key::Semicolon)),
    },
    CommandDef {
        id: "view.toggle_grid",
        label: "Toggle Grid",
        default: None,
    },
    CommandDef {
        id: "view.toggle_keyline_grid",
        label: "Toggle Icon Keyline Grid",
        default: None,
    },
    CommandDef {
        id: "view.toggle_snap_pixel",
        label: "Toggle Snap to Pixel",
        default: None,
    },
    CommandDef {
        id: "assets.import_design_tokens",
        label: "Import Design Tokens…",
        default: None,
    },
    CommandDef {
        id: "document.export_icon_set",
        label: "Export Icon Set…",
        default: None,
    },
    CommandDef {
        id: "view.fit",
        label: "Fit to View",
        default: None,
    },
    CommandDef {
        id: "view.toggle_audit",
        label: "Toggle Audit Log",
        default: None,
    },
    // ── Palette ───────────────────────────────────────────────────────────
    CommandDef {
        id: "palette.open",
        label: "Open Command Palette",
        default: Some(KeyBinding::ctrl(Key::K)),
    },
    // ── Mode switch (video-editor-module 04-ui-mode-timeline.md §1.2) ───────
    CommandDef {
        id: "mode.toggle_video",
        label: "Toggle Video Mode",
        default: Some(KeyBinding::ctrl_shift(Key::V)),
    },
    CommandDef {
        id: "mode.enter_video",
        label: "Enter Video Mode",
        default: None,
    },
    CommandDef {
        id: "mode.exit_video",
        label: "Exit Video Mode",
        default: None,
    },
    // ── Video transport / timeline (04 §5.1) ─────────────────────────────────
    CommandDef {
        id: "video.play_pause",
        label: "Play/Pause",
        default: Some(KeyBinding::plain(Key::Space)),
    },
    CommandDef {
        id: "video.play_reverse",
        label: "Play Reverse",
        default: Some(KeyBinding::plain(Key::J)),
    },
    CommandDef {
        id: "video.pause",
        label: "Pause",
        default: Some(KeyBinding::plain(Key::K)),
    },
    CommandDef {
        id: "video.play_forward",
        label: "Play Forward",
        default: Some(KeyBinding::plain(Key::L)),
    },
    CommandDef {
        id: "video.step_back",
        label: "Step Back One Frame",
        default: Some(KeyBinding::plain(Key::ArrowLeft)),
    },
    CommandDef {
        id: "video.step_forward",
        label: "Step Forward One Frame",
        default: Some(KeyBinding::plain(Key::ArrowRight)),
    },
    CommandDef {
        id: "video.prev_edit_point",
        label: "Previous Edit Point",
        default: Some(KeyBinding::shift(Key::ArrowLeft)),
    },
    CommandDef {
        id: "video.next_edit_point",
        label: "Next Edit Point",
        default: Some(KeyBinding::shift(Key::ArrowRight)),
    },
    // K-A4: jump the playhead between snap targets (clip edges, markers, zone
    // in/out, keyframes, sequence start) rather than only between edit points.
    CommandDef {
        id: "video.prev_snap",
        label: "Previous Snap Point",
        default: Some(KeyBinding::alt(Key::ArrowLeft)),
    },
    CommandDef {
        id: "video.next_snap",
        label: "Next Snap Point",
        default: Some(KeyBinding::alt(Key::ArrowRight)),
    },
    CommandDef {
        id: "video.set_in",
        label: "Set In Point",
        default: Some(KeyBinding::plain(Key::I)),
    },
    CommandDef {
        id: "video.set_out",
        label: "Set Out Point",
        default: Some(KeyBinding::plain(Key::O)),
    },
    CommandDef {
        id: "video.split_at_playhead",
        label: "Split Clip at Playhead",
        default: Some(KeyBinding::plain(Key::S)),
    },
    // Clip editing (NLE parity QW-1/QW-3/QW-4). Delete/Backspace both remove the
    // timeline selection — Backspace is handled as a second hardwired
    // accelerator in `app/monitor.rs` since the keymap holds one binding per id.
    CommandDef {
        id: "video.delete_clip",
        label: "Delete Selected Clip",
        default: Some(KeyBinding::plain(Key::Delete)),
    },
    CommandDef {
        id: "video.ripple_delete",
        label: "Ripple Delete Selected Clip",
        default: Some(KeyBinding::shift(Key::Delete)),
    },
    CommandDef {
        id: "video.copy",
        label: "Copy Clip",
        default: Some(KeyBinding::ctrl(Key::C)),
    },
    CommandDef {
        id: "video.cut",
        label: "Cut Clip",
        default: Some(KeyBinding::ctrl(Key::X)),
    },
    CommandDef {
        id: "video.paste",
        label: "Paste Clip at Playhead",
        default: Some(KeyBinding::ctrl(Key::V)),
    },
    CommandDef {
        id: "video.add_marker",
        label: "Add Marker at Playhead",
        default: Some(KeyBinding::plain(Key::M)),
    },
    // Proposal 210: CapCut-class bookmark = sequence marker in the Bookmarks
    // category (not a parallel type). `B` is free in video mode (blade is `C`).
    CommandDef {
        id: "video.add_bookmark",
        label: "Add Bookmark at Playhead",
        default: Some(KeyBinding::plain(Key::B)),
    },
    CommandDef {
        id: "video.prev_bookmark",
        label: "Go to Previous Bookmark",
        default: None,
    },
    CommandDef {
        id: "video.next_bookmark",
        label: "Go to Next Bookmark",
        default: None,
    },
    // K-A2 marker depth. `video.add_range_marker` is the keyboard route to a
    // RANGED marker — the unit "Export each ranged marker" (K-F2) fans out
    // over, and previously uncreatable from anywhere in the app.
    // Marker navigation is deliberately distinct from `video.{prev,next}_snap`:
    // snap points also include clip edges, keyframes and the zone, so walking a
    // review pass marker-by-marker is not the same gesture. No default binding
    // — every plain and modified arrow key in video mode is already taken.
    CommandDef {
        id: "video.add_range_marker",
        label: "Add Ranged Marker from Work Range",
        default: None,
    },
    CommandDef {
        id: "video.prev_marker",
        label: "Go to Previous Marker",
        default: None,
    },
    CommandDef {
        id: "video.next_marker",
        label: "Go to Next Marker",
        default: None,
    },
    // 3/4-point editing (spec 16, Premiere defaults). Insert/Overwrite lay down
    // the armed source at the playhead; Lift/Extract clear the timeline in/out.
    // Razor is a blade-mode toggle. Bound here + dispatched in `command_center`;
    // the video-mode keyboard poll that fires them each frame lives in
    // `app/monitor.rs` (a separate story's file — see this story's report).
    CommandDef {
        id: "video.insert_edit",
        label: "Insert Edit (3-point)",
        default: Some(KeyBinding::plain(Key::Comma)),
    },
    CommandDef {
        id: "video.overwrite_edit",
        label: "Overwrite Edit (3-point)",
        default: Some(KeyBinding::plain(Key::Period)),
    },
    CommandDef {
        id: "video.lift_edit",
        label: "Lift (clear timeline in/out)",
        default: Some(KeyBinding::plain(Key::Semicolon)),
    },
    CommandDef {
        id: "video.extract_edit",
        label: "Extract (ripple-clear timeline in/out)",
        default: Some(KeyBinding::plain(Key::Quote)),
    },
    CommandDef {
        id: "video.extract_frame",
        label: "Extract Frame to File",
        default: Some(KeyBinding::ctrl_shift(Key::E)),
    },
    CommandDef {
        id: "video.extract_frame_to_bin",
        label: "Extract Frame to Media Pool",
        default: None,
    },
    CommandDef {
        id: "video.toggle_razor",
        label: "Razor Tool (blade)",
        default: Some(KeyBinding::plain(Key::C)),
    },
    CommandDef {
        id: "video.toggle_snap",
        label: "Toggle Timeline Snapping",
        default: Some(KeyBinding::plain(Key::N)),
    },
    // K-A10: fixed playhead + edge auto-pan while dragging (view state).
    CommandDef {
        id: "video.toggle_fixed_playhead",
        label: "Toggle Fixed Playhead",
        default: None,
    },
    CommandDef {
        id: "video.zoom_in",
        label: "Timeline Zoom In",
        default: Some(KeyBinding::plain(Key::Plus)),
    },
    CommandDef {
        id: "video.zoom_out",
        label: "Timeline Zoom Out",
        default: Some(KeyBinding::plain(Key::Minus)),
    },
    CommandDef {
        id: "video.zoom_fit",
        label: "Timeline Zoom to Fit",
        default: Some(KeyBinding::shift(Key::Z)),
    },
    CommandDef {
        id: "video.playhead_home",
        label: "Playhead to Sequence Start",
        default: Some(KeyBinding::plain(Key::Home)),
    },
    CommandDef {
        id: "video.playhead_end",
        label: "Playhead to Sequence End",
        default: Some(KeyBinding::plain(Key::End)),
    },
    // ── NLE parity round-2 (spec 17) — keyboard-velocity editing riding on the
    // shipped split/trim/roll ops. G1 (add-edit-all-tracks / close-gap /
    // simplify), G2 (Q/W/E ripple-trims + Shift+Q/W rolls), G3 (Match Frame /
    // Reveal). Bound here; dispatched in `command_center`; the per-frame poll
    // that fires them lives in `timeline/mod.rs::draw_timeline_panel`
    // (`handle_timeline_shortcuts`) — the timeline panel owns these keys.
    CommandDef {
        id: "video.split_all_tracks",
        label: "Add Edit to All Tracks",
        default: Some(KeyBinding::ctrl_shift(Key::K)),
    },
    CommandDef {
        id: "video.close_gap",
        label: "Close Gap at Playhead",
        default: None,
    },
    CommandDef {
        id: "video.close_gaps",
        label: "Close All Gaps",
        default: None,
    },
    // K-A3 spacer / space operations (across all unlocked tracks).
    CommandDef {
        id: "video.insert_space",
        label: "Insert Space at Playhead (1s)",
        default: None,
    },
    CommandDef {
        id: "video.remove_space",
        label: "Remove Space at Playhead (1s)",
        default: None,
    },
    CommandDef {
        id: "video.remove_all_spaces_after",
        label: "Remove All Spaces After Playhead",
        default: None,
    },
    CommandDef {
        id: "video.remove_clips_after",
        label: "Remove All Clips After Playhead",
        default: None,
    },
    CommandDef {
        id: "video.simplify_sequence",
        label: "Simplify Sequence (remove through-edits)",
        default: None,
    },
    CommandDef {
        id: "video.trim_start_to_playhead",
        label: "Ripple Trim Start to Playhead",
        default: Some(KeyBinding::plain(Key::Q)),
    },
    CommandDef {
        id: "video.trim_end_to_playhead",
        label: "Ripple Trim End to Playhead",
        default: Some(KeyBinding::plain(Key::W)),
    },
    CommandDef {
        id: "video.extend_edit",
        label: "Extend Edit to Playhead",
        default: Some(KeyBinding::plain(Key::E)),
    },
    CommandDef {
        id: "video.roll_prev_to_playhead",
        label: "Roll Previous Edit to Playhead",
        default: Some(KeyBinding::shift(Key::Q)),
    },
    CommandDef {
        id: "video.roll_next_to_playhead",
        label: "Roll Next Edit to Playhead",
        default: Some(KeyBinding::shift(Key::W)),
    },
    CommandDef {
        id: "video.match_frame",
        label: "Match Frame (arm source at playhead)",
        default: Some(KeyBinding::plain(Key::F)),
    },
    CommandDef {
        id: "video.reveal_in_project",
        label: "Reveal in Media Pool",
        default: None,
    },
    CommandDef {
        id: "video.audition_source",
        label: "Audition Source Audio",
        default: Some(KeyBinding::alt(egui::Key::Space)),
    },
    CommandDef {
        id: "video.stop_source_audition",
        label: "Stop Source Audition",
        default: None,
    },
    CommandDef {
        id: "video.precision_trim",
        label: "Precision Trim Mode",
        default: Some(KeyBinding::shift(egui::Key::T)),
    },
    CommandDef {
        id: "video.enter_nested_sequence",
        label: "Enter Nested Sequence",
        default: Some(KeyBinding::alt(egui::Key::ArrowRight)),
    },
    CommandDef {
        id: "video.leave_nested_sequence",
        label: "Back to Parent Sequence",
        default: Some(KeyBinding::alt(egui::Key::ArrowLeft)),
    },
    CommandDef {
        id: "video.add_preview_zone",
        label: "Add Preview Zone from In/Out",
        default: None,
    },
    CommandDef {
        id: "video.remove_preview_zone",
        label: "Remove Preview Zone in In/Out",
        default: None,
    },
    CommandDef {
        id: "video.remove_all_preview_zones",
        label: "Remove All Preview Zones",
        default: None,
    },
    CommandDef {
        id: "video.render_preview",
        label: "Render Timeline Preview",
        default: None,
    },
    CommandDef {
        id: "video.stop_preview_render",
        label: "Stop Preview Render",
        default: None,
    },
    CommandDef {
        id: "video.open_transcript",
        label: "Open Transcript",
        default: None,
    },
    CommandDef {
        id: "video.remove_transcript_selection",
        label: "Ripple Delete Selected Transcript Words",
        default: None,
    },
    CommandDef {
        id: "video.find_fillers",
        label: "Preview Transcript Filler Words",
        default: None,
    },
    CommandDef {
        id: "video.edit_duration",
        label: "Edit Duration…",
        // No default — Ctrl+D is `edit.duplicate`; palette / context menu /
        // inspector open the form. Users can rebind in preferences.
        default: None,
    },
    // K-B14 freeze frame — hold the source frame under the playhead for the
    // selected clip's duration (zero-rate SpeedMap). Palette / context menu;
    // no default binding (Shift+F is Match Frame in some NLEs, and F is ours).
    CommandDef {
        id: "video.freeze_frame",
        label: "Freeze Frame",
        default: None,
    },
    // K-B17 alpha view — program-monitor present channel (view state, zero
    // undo units). The util.alpha_view / util.unpremultiply *effects* already
    // live in the catalogue for per-clip use.
    CommandDef {
        id: "video.alpha_view",
        label: "Toggle Alpha View",
        default: None,
    },
    CommandDef {
        id: "video.compare_effects",
        label: "Toggle Effect Compare (A|B)",
        default: None,
    },
    // K-A7 grab item / arrow-key nudge (Shift+G engage; arrows while grabbed;
    // Enter commit; Esc cancel). Arrow keys are handled specially while grab
    // is active so they don't step the playhead.
    CommandDef {
        id: "video.grab_item",
        label: "Grab Item (keyboard move)",
        default: Some(KeyBinding::shift(Key::G)),
    },
    CommandDef {
        id: "video.grab_commit",
        label: "Commit Grabbed Item Move",
        default: None, // Enter hardwired while grab active
    },
    CommandDef {
        id: "video.grab_cancel",
        label: "Cancel Grabbed Item Move",
        default: None, // Esc hardwired while grab active
    },
    // K-B15 Paste Attributes: copy the LOOK of the clip on the timeline
    // clipboard (`video.copy`, Ctrl+C) onto every selected clip, as ONE undo
    // step — Premiere's Ctrl+C → Ctrl+Alt+V flow, Kdenlive's "Paste Effects"
    // for the narrower form. No timing, source or trim is touched.
    //
    // No default binding on purpose: the video-mode key poll that fires
    // `video.*` bindings is a fixed id list in `app/monitor.rs`, which this
    // story does not own, so advertising Ctrl+Alt+V in the palette would show
    // a shortcut that does not fire. Reachable from the command palette
    // (Ctrl+K) and rebindable in preferences; wiring the accelerator and the
    // clip context menu is a filed follow-up.
    CommandDef {
        id: "video.paste_attributes",
        label: "Paste Attributes onto Selected Clips",
        default: None,
    },
    CommandDef {
        id: "video.paste_effects",
        label: "Paste Effects onto Selected Clips",
        default: None,
    },
    CommandDef {
        id: "keymap.import",
        label: "Import Keyboard Shortcuts…",
        default: None,
    },
    CommandDef {
        id: "keymap.export",
        label: "Export Keyboard Shortcuts…",
        default: None,
    },
];

/// Tool-activation commands surfaced in the palette. Labels come from
/// `Tool::label()` so they never drift from the toolbar.
pub static TOOL_COMMANDS: &[(CommandId, Tool)] = &[
    ("tool.select", Tool::Select),
    ("tool.direct_select", Tool::DirectSelect),
    ("tool.pan", Tool::Pan),
    ("tool.rectangle", Tool::Rectangle),
    ("tool.rounded_rect", Tool::RoundedRect),
    ("tool.ellipse", Tool::Ellipse),
    ("tool.polygon", Tool::Polygon),
    ("tool.star", Tool::Star),
    ("tool.spiral", Tool::Spiral),
    ("tool.line", Tool::Line),
    ("tool.arc", Tool::Arc),
    ("tool.grid", Tool::Grid),
    ("tool.polar_grid", Tool::PolarGrid),
    ("tool.pen", Tool::Pen),
    ("tool.shape_builder", Tool::ShapeBuilder),
    ("tool.text", Tool::Text),
    ("tool.scissors", Tool::Scissors),
    ("tool.knife", Tool::Knife),
    ("tool.eraser", Tool::Eraser),
    ("tool.magic_wand", Tool::MagicWand),
    ("tool.lasso", Tool::Lasso),
    ("tool.pencil", Tool::Pencil),
    ("tool.smooth", Tool::Smooth),
    ("tool.width", Tool::Width),
    ("tool.raster_brush", Tool::RasterBrush),
    ("tool.raster_eraser", Tool::RasterEraser),
];

/// Resolve a tool-activation command id to its [`Tool`].
pub fn tool_for_command(id: &str) -> Option<Tool> {
    TOOL_COMMANDS
        .iter()
        .find(|(cid, _)| *cid == id)
        .map(|(_, t)| *t)
}

/// The registry default binding for a command (ignores user overrides).
pub fn default_binding(id: &str) -> Option<KeyBinding> {
    REGISTRY.iter().find(|d| d.id == id).and_then(|d| d.default)
}

/// A flattened command for the palette + settings list (core + tool commands).
#[derive(Clone)]
pub struct CommandEntry {
    pub id: String,
    pub label: String,
    /// `true` for tool-activation entries (no remappable default binding).
    pub is_tool: bool,
}

/// All commands the palette can list and run: editor commands, tool
/// activations, then MCP operations whose schema can be invoked without
/// required arguments. Argumented MCP tools stay available through the MCP
/// client, where their schemas can be filled correctly.
pub fn all_commands() -> Vec<CommandEntry> {
    let mut v: Vec<CommandEntry> = REGISTRY
        .iter()
        .map(|d| CommandEntry {
            id: d.id.to_string(),
            label: d.label.to_string(),
            is_tool: false,
        })
        .collect();
    for (id, t) in TOOL_COMMANDS {
        v.push(CommandEntry {
            id: id.to_string(),
            label: format!("Tool: {}", t.label()),
            is_tool: true,
        });
    }
    // The MCP schema is the canonical operation registry. Keeping this derived
    // means newly-added AI operations automatically become palette-searchable.
    v.extend(mcp_command_entries().iter().cloned());
    v
}

fn mcp_command_entries() -> &'static Vec<CommandEntry> {
    static ENTRIES: OnceLock<Vec<CommandEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        photonic_mcp::server::tool_list()
            .as_array()
            .into_iter()
            .flatten()
            .filter(|tool| !mcp_tool_has_required_args(tool))
            .filter_map(|tool| tool.get("name").and_then(|v| v.as_str()))
            .map(|name| CommandEntry {
                id: format!("mcp.{name}"),
                label: format!("MCP: {name}"),
                is_tool: false,
            })
            .collect()
    })
}

fn mcp_tool_has_required_args(tool: &serde_json::Value) -> bool {
    tool.get("inputSchema")
        .and_then(|schema| schema.get("required"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|required| !required.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_binding_roundtrips_through_string() {
        let cases = [
            KeyBinding::ctrl(Key::Z),
            KeyBinding::ctrl_shift(Key::G),
            KeyBinding::plain(Key::Delete),
            KeyBinding::ctrl(Key::OpenBracket),
            KeyBinding::ctrl_shift(Key::CloseBracket),
            KeyBinding::ctrl(Key::Semicolon),
        ];
        for b in cases {
            let s = b.to_storage_string();
            let back = KeyBinding::parse(&s).expect("parse");
            assert_eq!(b, back, "round-trip failed for {s}");
        }
    }

    #[test]
    fn storage_string_is_lowercase_plus_joined() {
        assert_eq!(
            KeyBinding::ctrl_shift(Key::G).to_storage_string(),
            "ctrl+shift+g"
        );
        assert_eq!(KeyBinding::plain(Key::Delete).to_storage_string(), "delete");
    }

    #[test]
    fn serde_roundtrip_as_string() {
        let b = KeyBinding::ctrl(Key::K);
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json, "\"ctrl+k\"");
        let back: KeyBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn registry_ids_are_unique() {
        let mut ids: Vec<&str> = REGISTRY.iter().map(|d| d.id).collect();
        ids.extend(TOOL_COMMANDS.iter().map(|(id, _)| *id));
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            assert!(seen.insert(id), "duplicate command id: {id}");
        }
    }

    #[test]
    fn palette_includes_only_argumentless_mcp_operations() {
        let tool_list = photonic_mcp::server::tool_list();
        let mcp_tools = tool_list.as_array().expect("MCP tool list is an array");
        let argumentless_count = mcp_tools
            .iter()
            .filter(|tool| !mcp_tool_has_required_args(tool))
            .count();
        let palette_mcp_count = all_commands()
            .iter()
            .filter(|entry| entry.id.starts_with("mcp."))
            .count();
        assert_eq!(palette_mcp_count, argumentless_count);
        assert!(palette_mcp_count > 0);
        assert!(
            !all_commands()
                .iter()
                .any(|entry| entry.id == "mcp.create_shape"),
            "tools requiring arguments must not be invokable with an empty payload"
        );
    }

    #[test]
    fn matches_distinguishes_shift() {
        let z = KeyBinding::ctrl(Key::Z);
        let plain = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let with_shift = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        assert!(z.matches(plain));
        assert!(!z.matches(with_shift));
    }
}
