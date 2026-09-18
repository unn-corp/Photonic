# 206 — K-G3 Layout Presets (mini-spec)

> Status: **proposed mini-spec — not accepted, no code authorization.** Written to
> satisfy the [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands)
> K-Band 5 exit condition: *"an accepted mini-spec exists **before** code, naming
> its data-model change, migration, undo unit, MCP surface and acceptance
> fixtures. No item here starts without one."* Acceptance of this document is what
> authorizes K-G3; nothing here authorizes an edit to a product crate.

Owner refs: [26 §15 K-G3](../specs/video-editor/26-kdenlive-mlt-parity.md#k-g3--layout-presets)
(the requirement and the storage hint), [04 §4](../specs/video-editor/04-ui-mode-timeline.md#4-mode-adaptive-panels)
(D-02, the mode-adaptive rail model this item arranges rather than replaces),
[39 §1.6](../specs/video-editor/39-document-lifecycle.md#16-what-is-not-undoable)
and [39 §2.2](../specs/video-editor/39-document-lifecycle.md#22-generalise-it)
(persisted-UI-state placement, and the unknown-preserving rule §3.3 borrows),
[41 §6](../specs/video-editor/41-accessibility.md#6-reduced-motion) (R-17),
[ROADMAP §10](../specs/video-editor/ROADMAP.md#10-definition-of-done) (done).

Siblings written in earlier rounds and deliberately not contradicted:
[195 K-C1](195-k-c1-clip-jobs-framework.md) §7.1 — *a setting that changes
behaviour must not arrive inside a stranger's project file*; this document
applies the same reasoning to layouts in [§3.1](#31-the-key-decision-a-layout-is-a-user-preference).
[204 K-G4](204-k-g4-project-templates.md) §3.5 and its exclusion list, which
already records that *"dock layouts are K-G3's item and belong in the same config
dir"* — [§3.4](#34-where-layouts-live) honours that and does not add a fourth
`config_dir` delegation. [203 K-G1](203-k-g1-project-profiles.md) §3.4, the same
storage precedent.

**Sibling written concurrently:** [205 K-G2](205-k-g2-project-notes.md) §4.5
independently found the same preferences-file defect this document records at
[§2.3](#23-what-does-not-exist--stated-plainly) point 3, and proposes the fix
K-G3 adopts by name in [§4.2](#42-forwardbackward-compatibility-of-a-stored-layout).
Whichever of the two lands first should carry it; the other must not write a
second, different fix.

**Territory:** `panels-video` (26 §15) · **Effort:** M · **Crate:** `photonic-gui`.

All citations verified against `feat/video-editor-module` @ `8a33f32`.

---

## 1. Problem and user outcome

**Today the window has exactly one arrangement, and it is whatever the user last
left it in.** The shell is a left icon rail + one animated left drawer, a right
icon rail + one animated right drawer, and — in video mode — a bottom timeline
panel. Which drawer is open on each side, and how wide each is, are four fields
in `AppPreferences` (`crates/photonic-gui/src/preferences.rs:106,109,113,116`),
each written the moment the user clicks a rail button
(`crates/photonic-gui/src/app/mod.rs:3473`, `:3681`). There is exactly one such
arrangement at a time and no way to name it, save it, or return to it.

That is a bigger cost here than in a single-mode editor, because Photonic is a
**dual-mode app**: `DrawerGroup` is one enum carrying six vector variants and
nine video variants (`crates/photonic-gui/src/panels/mod.rs:1209-1263`), and the
rail simply offers a different slice per mode (`:1298`, `:1427`). A colourist
wants Clip Inspector + Colour Controls + scopes and a short timeline. An audio
pass wants the mixer open and the timeline tall. An assembly pass wants the media
pool and nothing on the right. Those are three arrangements of the *same* rails,
reached today by four clicks and two drags each, every time.

**After K-G3** a user can:

1. Pick a named layout — **Edit**, **Colour**, **Audio**, **Effects** in video
   mode, **Design** in vector mode — from a picker in the toolbar, from the
   command palette, or with `Ctrl+F1`–`Ctrl+F4`.
2. Arrange the panels how they like and **Save layout as…**, then get that
   arrangement back on any project and after any restart.
3. Rename, reorder, re-slot and delete their own layouts; the built-ins are
   read-only and cannot be lost.
4. Press **Reset layout** and land exactly on the built-in default for the
   current mode — including widths, not just which panels are open.
5. Open a build that has added or removed a panel, or a layout file written by a
   newer build, and have their layouts **still load**, degraded per-field, with
   one diagnostic naming what could not be resolved — never a silent reset of
   every other preference they have set.

Point 5 is the engineering content of this item. Points 1–4 are three enum
variants and a picker; point 5 is the part that is easy to get wrong and
expensive to get wrong, and [§3.3](#33-the-stored-form-stable-string-ids-not-enum-variants)
and [§4.2](#42-forwardbackward-compatibility-of-a-stored-layout) are written for
it specifically.

**Explicitly not in the outcome:** drag-to-dock, tear-off panels, a second
window, or any change to *which* panels exist. A layout selects among the rail
sets 04 §4 already defines; it does not create a second shell. See
[§8.3](#83-deliberately-excluded).

---

## 2. Current state in code (exact)

### 2.1 The layout state that exists

| Thing | Where | Note |
|---|---|---|
| `AppPreferences` | `crates/photonic-gui/src/preferences.rs:9` | The one user-state struct; `<config>/preferences.json` (`:317`), `load()` `:322`, `save()` `:335` |
| `open_drawer: Option<DrawerGroup>` / `drawer_width: f32` | `preferences.rs:106`, `:109` | Defaults `Some(Tools)` / `220.0` (`:162`, `:166`) |
| `open_right_drawer: Option<RightDrawerGroup>` / `right_drawer_width: f32` | `preferences.rs:113`, `:116` | Defaults `Some(Layers)` / `280.0` (`:170`, `:174`) |
| `reduced_motion: bool` | `preferences.rs:120` | Read by both drawer tweens (`app/mod.rs:3531`, `:3692`) |
| `DrawerGroup` — 16 variants, 6 vector + 9 video + `History` | `panels/mod.rs:1209-1263` | Plain `Serialize`/`Deserialize` derive. **No `#[serde(other)]`, not `#[non_exhaustive]`** |
| `DrawerGroup::ALL` (6) / `VIDEO_ALL` (9) / `all_for_mode` | `panels/mod.rs:1271`, `:1285`, `:1298` | The mode slices 04 §4 specifies |
| `DrawerGroup::icon` / `title` / `has_content` | `panels/mod.rs:1306`, `:1328`, `:1361` | `has_content` is the only availability predicate; `Modify`/`Arrange`/`ClipInspector` need a selection |
| `RightDrawerGroup` — 5 variants | `panels/mod.rs:1392-1404` | Same derive, same gap. `ALL` (3) `:1409`, `VIDEO_ALL` (5) `:1418`, `all_for_mode` `:1427` |
| Live drawer state on `PhotonicApp` | `app/mod.rs:1458`, `:1462`, `:1465`, `:1468` | `open_drawer`, `last_drawer_group`, `open_right_drawer`, `last_right_drawer_group` — the `last_*` pair is render-during-close-tween state and is **not** persisted |
| Rail render + click → persist | `app/mod.rs:3443-3477` (left), `:3661-3686` (right) | Clicking a rail button writes `self.prefs.open_drawer` and calls `prefs.save()` immediately |
| Width capture on resize | `app/mod.rs:3623-3626`, `:3793-3796` | Written in memory only; flushed on the next toggle |
| Width clamps | `app/mod.rs:3540` (`160.0..=420.0`), `:3700` (`220.0..=480.0`) | Already defensive; [§4.2](#42-forwardbackward-compatibility-of-a-stored-layout) reuses them verbatim |
| Timeline panel | `app/mod.rs:3824-3830` | `TopBottomPanel::bottom("timeline")`, `resizable(true)`, `default_height(220.0)`, `min_height(120.0)`, gated on `self.mode == AppMode::Video` |
| Console panel | `app/mod.rs:3805-3816` | Two ids (`console` / `console_expanded`) with different defaults; `show_animated` on `lua_console.visible` |
| Mode toggle | `app/monitor.rs:452` `enter_or_exit_video_mode`; clears the left drawer at `:475` | Also lazily creates the timeline project — see [§5](#5-undo-unit-and-its-exact-inverse) |
| Tab switch | `app/tabs.rs:98-133` | Restores `mode`, `timeline_view`, `playhead`, `selected_id`, `timeline_selection` per tab |
| Command registry | `commands.rs:204-208` (`CommandDef { id, label, default }`), `:213` `REGISTRY` (80 ids), `:646` `TOOL_COMMANDS` | `resolve_binding` layers `prefs.keymap` over registry defaults (`preferences.rs:261`); `binding_conflict` `:270` |
| Settings pages | `app/mod.rs:291` `EDIT_OPTIONS` (6 entries), rendered `app/menu_drawer.rs:175-470` | Where a **Layouts** page lands ([§6.1](#61-gui-route)) |
| App-level store precedent | `crates/photonic-video/src/export/presets.rs:401,407,414,421,428,437` | `config_dir()` → `crash_dir()` (`photonic-core/src/diagnostics.rs:29`), plus **path-parameterized `_from`/`_to` test hooks** |
| GUI config dir | `crates/photonic-gui/src/welcome.rs:2078` | One-line delegation to `photonic_core::crash_dir` |
| Window geometry | `crates/photonic-app/src/main.rs:274-296` | `WindowState` → `<config>/window_state.json`. **Already owns OS-window position/size; a layout must not** |

### 2.2 A "workspace" concept already exists, and it is the counter-example

This is the single most important finding for this item, and it is a live one.

`Document.workspaces: Vec<Workspace>` (`crates/photonic-core/src/document.rs:766`)
with `Workspace { name, search_query }` (`:866-872`). It is **serialized inside
the `.photon`**, it is documented as *"Named panel workspace presets"* (`:764`),
and its four MCP tools are already shipped and in the generated docs:
`save_workspace`, `load_workspace`, `list_workspaces`, `delete_workspace`
(`crates/photonic-mcp/src/handlers/doc_automation.rs:512`, `:534`, `:551`,
`:564`; `docs/mcp-api.md:3623`, `:2755`, `:2732`, `:1627`). There is a GUI
surface too (`crates/photonic-gui/src/panels/document.rs:858-933`, labelled
*"Named panel filter presets. Load to switch panel layout."*).

What it actually stores is **one string**: the properties-panel search query
(`prop_search`), applied at `crates/photonic-gui/src/app/panel_actions.rs:5930-5934`.
It stores no drawer, no width, no visibility, no mode. It is not a layout.

Three concrete problems it demonstrates, each of which K-G3 must not repeat:

1. **It is a per-user UI preference living in the document.** Open a colleague's
   project and you inherit their panel filters. `docs/mcp-api.md:3625` says so in
   terms: *"Stored on document."*
2. **It mutates the document with no undo unit.** `panel_actions.rs:5919-5940`
   pushes/retains on `doc.workspaces` directly and sets `doc_modified = true`;
   `doc_automation.rs:512-573` does the same under the MCP lock. Neither goes
   through `CommandHistory`. `SPEC.md`'s *"every document mutation, without
   exception, is undoable"* — quoted at
   [39 §1.6](../specs/video-editor/39-document-lifecycle.md#16-what-is-not-undoable)
   — is violated here, and 39 §1.6's list of violations does not currently name
   it. It is a third one.
3. **It occupies the word.** Shipping a second, unrelated thing called a
   "workspace" would be a documentation defect on day one. K-G3 uses **layout**
   throughout, and [Follow-ups](#follow-ups) proposes what to do with the
   existing concept.

### 2.3 What does not exist — stated plainly

1. **No layout preset of any kind.** `grep -rn 'layout_preset\|LayoutPreset\|workspace_layout\|WorkspacePreset' crates/`
   returns clean (verified 2026-07-28).
2. **Panel sizes are not persisted at all today.** Photonic is **not** an
   `eframe` app — `grep -rn eframe Cargo.toml crates/*/Cargo.toml` is clean; the
   shell is raw `egui-winit` + `egui-wgpu` (`crates/photonic-app/Cargo.toml:21`,
   `crates/photonic-gui/Cargo.toml:17`) driven from
   `crates/photonic-app/src/main.rs`. egui's own `PanelState`
   (egui 0.29.1, `src/containers/panel.rs:30-47`) is stored in `ctx.data_mut`
   and nothing serializes egui memory to disk. So `TopBottomPanel("timeline")`'s
   height resets to `default_height(220.0)` on every launch, and always has.
   The two drawer widths *are* persisted, because they are ours
   (`preferences.rs:109`, `:116`), not egui's.
3. **No `DrawerGroup`/`RightDrawerGroup` string id, and no unknown-tolerant
   deserialization.** Both enums derive `Deserialize` plainly. `AppPreferences`
   has no `deny_unknown_fields`, so an *unknown field* is ignored safely — but an
   **unknown enum variant** is a hard parse error, and `AppPreferences::load`
   ends in `serde_json::from_str(&json).unwrap_or_default()`
   (`preferences.rs:331`). Consequence, verified by reproduction: a
   `preferences.json` containing `"open_drawer": "<any name this build does not
   know>"` discards **the entire preferences file** — theme, UI scale, snapping,
   nudge distance, autosave, history budget, pinned tools, hotbar usage and the
   whole `keymap` — silently, with no diagnostic. That is today's forward-compat
   behaviour for panel state, and it is the specific hazard K-G3 must not widen.
4. **No mode reconciliation on tab switch.** `switch_tab` (`app/tabs.rs:98-133`)
   restores `self.mode` from the target tab (`:126`) but touches neither
   `open_drawer` nor `open_right_drawer`. Switching from a video tab with
   **Media Pool** open to a vector tab leaves the left drawer rendering Media
   Pool with no rail button to close it (`DrawerGroup::all_for_mode` no longer
   offers it; the render path uses `effective_open` unfiltered by mode,
   `app/mod.rs:3386`, `:3529`). The same holds for the right rail with
   **Colour Controls** or **Audio Mixer**.
5. **Mode toggle only half-reconciles.** `enter_or_exit_video_mode` clears the
   left drawer (`app/monitor.rs:475`, with the comment *"A drawer open in the old
   mode is meaningless in the new one (04 §4)"*) but **not** the right one — so
   Colour Controls stays rendered in vector mode. 04 §4's sentence "`open_drawer`
   … is cleared on mode switch" describes half the shipped behaviour.
6. **The one existing precedent for panel-set evolution is a hard-coded
   special case.** `PhotonicApp::new` (`app/mod.rs:2173`) remaps a persisted
   left-rail `History` to the right rail at `:2188-2192`, *"so it doesn't leave an
   unreachable open drawer on the left"* — exactly the right instinct, done once,
   by hand, and duplicated verbatim in the second constructor at `:2220-2223`.
7. **No UI sidecar.** 39 §1.6's recommendation has not landed:
   `crates/photonic-core/src/migration.rs:200-203` is a documented **no-op**
   (`fn migrate_height_sidecar(_value: &mut Value) {}`), and `Track.height_px`
   is still a non-undoable document field.
8. **No MCP access to preferences.** `grep -rn 'AppPreferences\|preferences' crates/photonic-mcp/src/`
   returns clean. `AppState` carries the document and history; it has no handle
   to the GUI's preference state. This is load-bearing for [§6.2](#62-mcp-surface--none-in-v1-and-why).

---

## 3. Data-model change

### 3.1 The key decision: a layout is a **user preference**

**Layouts are user preferences. They are not document properties. They are not in
the `.photon`, they need no format-version change, and they produce no undo
unit.**

The argument, in four steps:

1. **A layout describes how *I* work, not what the project *is*.** Frame rate,
   track count and bin structure are facts about a project and travel with it
   (that is [203 K-G1](203-k-g1-project-profiles.md) and
   [204 K-G4](204-k-g4-project-templates.md)). "The mixer is open and 320 px
   wide" is a fact about a person and a monitor. Two editors opening the same
   project should see their own arrangements; that is the entire feature.
2. **[195 §7.1](195-k-c1-clip-jobs-framework.md)'s precedent applies directly.**
   K-C1 put its enable flag in `AppPreferences` rather than the document because
   *"if the enable flag were a document field, then opening a stranger's project
   would enable process execution."* K-G3's stakes are lower — a layout cannot
   execute anything — but the shape is identical: **a file you were sent must not
   silently reconfigure your application.** The rule is worth holding at low
   stakes precisely so it is not re-litigated at high ones.
3. **The counter-example is already in the tree and already hurts.**
   `Document.workspaces` ([§2.2](#22-a-workspace-concept-already-exists-and-it-is-the-counter-example))
   put per-user panel state in the document, and the cost is exactly the two
   things predicted: it travels to strangers, and it forced a document mutation
   with no undo unit because there is no sensible undo for a panel filter. K-G3
   choosing the document would reproduce a defect that is visible in the repo
   today.
4. **The one honest argument for the document is weak.** "A layout could be part
   of a project handover" — but that is a *template* concern, and 204 §11 already
   excluded it by name: *"Preferences/keymap/layout in a template. Those are
   app-level state, not project structure; dock layouts are K-G3's item and
   belong in the same config dir."* Sharing a layout with a colleague is
   file-copy plus [§8.3](#83-deliberately-excluded)'s import/export follow-up,
   not a document field.

**Consequences, stated so they read as answers and not as omissions:**

- **[§4](#4-migration-and-format-version-impact): `CURRENT_FORMAT_VERSION` stays
  at 5, and not one byte of `Document` changes.** ROADMAP §10 point 5 is
  **N/A**, with the round-trip obligation moved onto the new config file.
- **[§5](#5-undo-unit-and-its-exact-inverse): there is no undo unit,** and there
  must not be one. ROADMAP §10 point 4 is **N/A**, with a *test* proving the
  N/A rather than an assertion.

### 3.2 New types — `photonic-gui/src/layouts.rs` (a new module, not in `photonic-core`)

Nothing here is part of the document model. Nothing here is in `photonic-core`:
the types name GUI panels, and `photonic-core` must not learn what a drawer is.

```rust
// crates/photonic-gui/src/layouts.rs

/// One named arrangement of the shell's panels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutPreset {
    pub name: String,
    /// Which editor this layout is for. A filter, not a partition — see §3.5.
    pub mode: LayoutMode,                 // Vector | Video
    /// `Ctrl+F<slot>` reaches this layout. 1..=9; `None` = no direct key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u8>,
    pub left: DrawerSlot,
    pub right: DrawerSlot,
    /// Bottom timeline panel height in logical px. `None` = leave as-is.
    /// Video layouts only; ignored (and preserved) on a Vector layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_height: Option<f32>,
    /// Lua console visibility (`app/mod.rs:3814`).
    #[serde(default)]
    pub console_visible: bool,
    /// Floating windows that are not drawers (04 §4.1's recorded exception).
    #[serde(default)]
    pub scopes_open: bool,
}

/// One rail + its drawer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DrawerSlot {
    /// Stable panel id — `"media_pool"`, NOT a `DrawerGroup` variant. §3.3.
    /// `None` = drawer collapsed, rail only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
    /// Target width in logical px. Clamped on apply to the ranges the render
    /// code already enforces (`app/mod.rs:3540`, `:3700`).
    pub width: f32,
}

/// The on-disk file. `version` is advisory — see §4.2 rule 4.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LayoutFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub layouts: Vec<LayoutPreset>,
}
```

Plus a **pure resolver**, which is the whole testable surface:

```rust
/// The applied form of a preset against a concrete build and mode: every
/// string id resolved, every number clamped, every unresolvable thing recorded.
pub struct ResolvedLayout {
    pub left: (Option<DrawerGroup>, f32),
    pub right: (Option<RightDrawerGroup>, f32),
    pub timeline_height: Option<f32>,
    pub console_visible: bool,
    pub scopes_open: bool,
    /// Panel ids this build could not resolve. Diagnosed once, never dropped.
    pub unresolved: Vec<String>,
}

pub fn resolve(preset: &LayoutPreset, mode: AppMode) -> ResolvedLayout;
```

`resolve` takes no `egui::Context`, no `PhotonicApp`, and no document. That is
deliberate: it makes tests T1, T2 and T4 ([§7](#7-acceptance-fixtures-and-tests))
plain unit tests with no window and no GPU, which is the difference between this
item having real coverage and having a smoke test.

`DrawerGroup` and `RightDrawerGroup` each gain two total functions beside their
existing `icon`/`title` (`panels/mod.rs:1306`, `:1328`):

```rust
impl DrawerGroup {
    /// Stable serialization id. Never renamed once shipped, even if `title()` is.
    pub fn panel_id(self) -> &'static str;          // Tools => "tools", …
    /// `None` for any id this build does not know. Never panics, never guesses.
    pub fn from_panel_id(id: &str) -> Option<Self>;
}
```

### 3.3 The stored form: stable string ids, not enum variants

**A layout stores `"media_pool"`, not `DrawerGroup::MediaPool`.** This is the
decision the rest of [§4.2](#42-forwardbackward-compatibility-of-a-stored-layout)
rests on, so the reasoning is here rather than there:

- Serializing the enum reproduces [§2.3](#23-what-does-not-exist--stated-plainly)
  point 3 exactly: an id this build does not know becomes a *parse error*, and a
  parse error in a `Vec<LayoutPreset>` loses **every** layout, not one field of
  one layout. Adding `#[serde(other)]` to `DrawerGroup` would fix the parse but
  collapse every unknown id to one indistinguishable `Unknown`, so a round-trip
  through an older build silently rewrites `"multicam"` and `"transcript"` to the
  same thing — which 39 §2.2 forbids in terms (*"never drop, never guess"*).
- The pattern is already in this file and already load-bearing:
  `AppPreferences::keymap` is `HashMap<String, KeyBinding>` keyed by
  `CommandId` (`preferences.rs:151`, ids at `commands.rs:18`), precisely so an
  unknown command id in a user's file is inert instead of fatal. `panel_id` is
  the same idea for panels.
- It decouples the wire name from the display name. `title()` is user-facing
  copy and will change; `panel_id()` is a contract.

**Normative:** once a `panel_id` ships it is never reused for a different panel,
and a removed panel's id is never recycled. Renaming a panel changes `title()`
only.

### 3.4 Where layouts live

```
<config>/Photonic/                      photonic_core::crash_dir()   diagnostics.rs:29
├── preferences.json                                                 preferences.rs:317
├── recent_docs.json                                                  welcome.rs:2083
├── disk_roots.json                                                   welcome.rs:2087
├── export_presets.json                                               presets.rs:407
├── layouts.json                        ← NEW, all of K-G3's storage
├── crash-reports/                                                    diagnostics.rs:43
├── recovery/                                                         autosave.rs:14
└── templates/                                                        204 K-G4 (proposed)
```

**One aggregate `layouts.json`, mirroring `export_presets.json`
(`presets.rs:407`) rather than 204's per-file `templates/` directory.** 204 chose
a directory because *"a template is a whole `Document`"* and one corrupt byte
would lose all of them. A layout is ~10 scalars; the aggregate file is a few
kilobytes, is rewritten atomically as a unit, and gives the manager UI an
ordering for free. The corruption argument is answered by
[§4.2](#42-forwardbackward-compatibility-of-a-stored-layout) rule 5 (quarantine,
never overwrite) rather than by the directory layout.

**Why not inside `preferences.json`.** This is the blast-radius decision, and it
is the direct consequence of [§2.3](#23-what-does-not-exist--stated-plainly)
point 3. `AppPreferences::load` ends in `unwrap_or_default()`
(`preferences.rs:331`): *any* deserialization failure anywhere in that file
resets **every** preference the user has. Layouts are the one piece of user state
whose schema will keep changing as panels are added, so they are the piece most
likely to fail to parse — and putting them in `preferences.json` would couple
"my Colour layout mentions a panel this build removed" to "my entire keymap is
gone". A separate file with its own tolerant loader bounds the worst case to
"layouts fell back to built-ins".

`AppPreferences` gains **one** field, and it is a `String`, which cannot fail to
parse:

```rust
// crates/photonic-gui/src/preferences.rs — appended to AppPreferences
/// K-G3: name of the layout last applied, per mode. Empty = the built-in
/// default for that mode. A plain string on purpose: it can never be an
/// unparseable enum, so it cannot take the rest of this file down with it.
#[serde(default)]
pub active_layout_vector: String,
#[serde(default)]
pub active_layout_video: String,
```

`config_dir` resolution reuses `welcome::config_dir` (`welcome.rs:2078`) and adds
no fourth delegation — 203 §8 and 204 §3.5 both flag the existing duplication;
K-G3 switches to `app_config_dir()` if either lands first.

The store copies `export/presets.rs`'s shape verbatim, **including the
path-parameterized test hooks** (`load_custom_presets_from` `:428`,
`save_custom_presets_to` `:437`) so every test in [§7](#7-acceptance-fixtures-and-tests)
runs against a `tempfile` and never the real user config dir.

### 3.5 One layout list, mode as a filter

**Decision: one namespace. Each preset declares its `mode`; the picker shows only
presets matching the active mode.**

- The rails are already partitioned by *data*, not by code: `DrawerGroup` is one
  enum and `all_for_mode` (`panels/mod.rs:1298`) picks a slice. A layout naming
  `"media_pool"` is meaningless in vector mode because that *panel* is
  video-only, not because layouts are inherently mode-scoped. Modelling the
  filter on the preset matches the code that already exists.
- Two separate lists would double the manager UI, double the "reset" verb, and
  hide a user's Colour layout whenever they happened to be in the wrong mode —
  for no capability gain.
- `resolve(preset, mode)` treats a panel that is valid in the enum but absent
  from `all_for_mode(mode)` **exactly like an unknown id**: the drawer collapses,
  the id is preserved, one diagnostic. That single rule covers hand-edited files
  and future panel re-homing (the `History` left→right move at `app/mod.rs:2188`
  is precisely this case, and after K-G3 it stops needing a hard-coded arm).

**Applying a layout never switches mode.** A layout whose `mode` differs from the
active mode is shown disabled in the picker with the reason. This is not
squeamishness — mode entry is **not free**: `enter_or_exit_video_mode`
(`app/monitor.rs:452`) calls `ensure_timeline_project`, which issues two
`history.execute(Command::Timeline(…))` calls (`app/monitor.rs:507`, `:509`) to
create the project and its first sequence. Letting `Ctrl+F2` enter video mode
would make a chrome verb produce a document mutation and an undo entry, breaking
[§5](#5-undo-unit-and-its-exact-inverse)'s guarantee in the one case a user would
hit by accident.

### 3.6 The built-in set

Five built-ins, **constructed in Rust** — no bundled bytes, so
[23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest)'s
`AssetRightsManifest` gate is not engaged (the same choice 204 §3.4 made for
templates). They are read-only: saving over a built-in name is refused, and
deleting one is refused, following `save_export_preset`'s shipped behaviour
(built-in names refused with `NotSupportedV1`, `handlers/video.rs:4444`/`:4460` and `:4487`/`:4494`).

| Layout | Mode | Slot | Left drawer | Right drawer | Timeline | Extras |
|---|---|---|---|---|---|---|
| **Design** | Vector | 1 | Tools, 220 | Layers, 280 | — | — |
| **Edit** | Video | 1 | Media Pool, 260 | *collapsed* | 300 | — |
| **Colour** | Video | 2 | Clip Inspector, 240 | Colour Controls, 400 | 160 | scopes open |
| **Audio** | Video | 3 | Media Pool, 220 | Audio Mixer, 420 | 300 | — |
| **Effects** | Video | 4 | Effects, 260 | Layers, 280 | 220 | — |

**Design is today's defaults, named.** `default_open_drawer` = `Some(Tools)` and
`default_open_right_drawer` = `Some(Layers)` with widths 220/280
(`preferences.rs:162-176`) — so "Design" is not a new arrangement, it is the
existing one given a name, which makes "reset to default" definable as "apply the
built-in for this mode" rather than as a second, parallel notion of default.

Every width above is inside the existing clamps (`160.0..=420.0` left,
`220.0..=480.0` right, `app/mod.rs:3540`, `:3700`) and every timeline height is
at or above `min_height(120.0)` (`app/mod.rs:3827`). Deliberately: a built-in
must never be the thing that trips the clamp.

**No "Logging"/"Ingest" built-in in v1** — its natural shape is Media Pool +
Source Monitor, and `SourceMonitor` is a compile-clean stub
(`panels/mod.rs:1292`; 17 G-10 marks it Larger). Shipping a built-in that opens a
stub is shipping a broken default. See [§8.2 Q2](#82-open-questions-each-with-a-recommendation).

---

## 4. Migration and format-version impact

### 4.1 `CURRENT_FORMAT_VERSION` stays at 5. No `v6`, and nothing additive either.

`crates/photonic-core/src/document.rs:117` pins the version at 5. K-G3 is the
first Band-5 item to add **zero** serialized document fields: the four prior
mini-specs each appended something to the model additively inside v5
(193 to `ProjectVideoSettings`, 194 to `TimelineCmd` only, 195 `MediaAsset.derived_from`,
203 to `ProjectVideoSettings`). K-G3 touches `Document` not at all, because
[§3.1](#31-the-key-decision-a-layout-is-a-user-preference) put the state
somewhere else.

A no-op bump would be worse here than merely dishonest.
`crates/photonic-core/src/migration.rs:16` sets `COMPAT_WINDOW = 1` — *"how many
versions ahead of the current one a file may be and still load"*
(`migration.rs:14-16`). That one version of forward tolerance is a finite budget
spent on real model growth. Bumping to 6 for a change that reinterprets nothing
would consume it, force every v5 project through an empty `MigrationV5ToV6` step,
and make the migration list lie about what changed. **Bump only when data must be
reinterpreted.** Nothing here is data in the document.

The only serde work K-G3 owes the document is a **negative** test: after this item
lands, a `.photon` written before and after must be byte-identical for the same
edits, and `CURRENT_FORMAT_VERSION` must still be 5 (T12).

### 4.2 Forward/backward compatibility of a stored layout

This is the real risk in the item, so it is specified as rules with a failure
table rather than as prose.

**The failure matrix.** `resolve` (§3.2) is total; every row is a defined
outcome, and no row is "fails to load".

| Situation | Behaviour |
|---|---|
| `panel_id` known and in `all_for_mode(mode)` | Open it at the clamped width |
| `panel_id` known but **not** in this mode's set (hand-edited file, or a panel re-homed between rails as `History` was) | Treated as unresolved: **that drawer collapses**, the id is preserved, one diagnostic |
| `panel_id` **unknown** (newer build wrote it, or this build removed the panel) | Identical to the row above. The rest of the preset still applies |
| `width` out of range, negative, `NaN` or infinite | Clamped to `160.0..=420.0` / `220.0..=480.0` (`app/mod.rs:3540`, `:3700`); non-finite → the built-in default for that side |
| `timeline_height` below `min_height` or non-finite | Clamped to `>= 120.0` (`app/mod.rs:3827`); non-finite → `None` (leave as-is) |
| `slot` outside `1..=9`, or duplicated across presets | Slot dropped for that preset (no key), one diagnostic. Never silently reassigned |
| Unknown **object field** in a preset | Ignored by serde as today. Not preserved — see rule 2's boundary |
| `version` greater than this build's | **Loads anyway**, best-effort. See rule 4 |
| File is not valid JSON, or the top level is the wrong shape | Built-ins only; the file is **quarantined**, not overwritten. See rule 5 |

**Five normative rules.**

1. **A layout file never fails to load, and never fails a launch.** Degradation is
   per-field and always toward "collapsed / default", never toward "unusable
   window". This is why every numeric field is clamped rather than validated.
2. **Unresolvable panel ids are preserved verbatim and re-emitted on save.**
   Directly [39 §2.2](../specs/video-editor/39-document-lifecycle.md#22-generalise-it)'s
   rule — *"preserve the original serialized form verbatim… render inert…
   diagnose once… never drop, never guess"* — applied to a config file rather
   than to the document. The extension is deliberate and worth stating: 39 §2.2
   was written for `.photon`, but the property it protects (a round-trip through
   an older build is lossless) is exactly what a user with two Photonic versions
   on two machines needs from their layouts. `LayoutPreset` therefore stores
   `panel: Option<String>` and never a resolved enum, so preservation is free
   rather than a mechanism.
3. **Diagnose once per load, per `(preset, id)`, never per frame.** One `Info`
   diagnostic naming the layout and the id, coalesced through the existing
   `DiagnosticLog` (`photonic-core/src/diag.rs:515`) like every other load-time
   report. A layout referencing three dead panels is three entries and one toast,
   not one per repaint.
4. **A newer `version` loads anyway.** This is a *deliberate asymmetry* with the
   document policy, which refuses out-of-window files with
   `Project::VersionTooOld` (39 §2.3). The asymmetry is justified by what is at
   stake: a document carries irreplaceable user work and a wrong guess corrupts
   it, so refusing is correct there. A layout file carries preferences that are
   re-creatable in thirty seconds, and refusing it means a user who ran a newer
   build once loses every layout permanently. Best-effort load plus rule 2's
   preservation strictly dominates. `version` exists for diagnostics and for a
   future real migration, not as a gate.
5. **A file that cannot be parsed at all is quarantined, never overwritten.**
   Rename to `layouts.json.bad` (suffixing on collision), start from built-ins,
   emit one `Warning`. The failure mode this forbids is the current one: a bad
   parse followed by a silent `save()` that destroys the evidence and the
   content. Note that `AppPreferences::save` (`preferences.rs:335`) will happily
   do exactly that today after an `unwrap_or_default()` load — which is
   [Follow-ups](#follow-ups) item 3, and is why rule 5 is written as a rule and
   not as a nicety.

**In scope, because K-G3 owns this surface: make the two existing panel fields
tolerant.** `open_drawer` and `open_right_drawer` (`preferences.rs:106`, `:113`)
must stop being able to discard the whole preferences file
([§2.3](#23-what-does-not-exist--stated-plainly) point 3) — a hazard K-G3 would
otherwise *increase*, by making it far likelier that a user's stored panel cursor
names a panel from a different build.

**Adopt [205 K-G2](205-k-g2-project-notes.md) §4.5's fix verbatim rather than
inventing a second one:** an `#[serde(other)] Unknown` variant on *both*
`DrawerGroup` (`panels/mod.rs:1209`) and `RightDrawerGroup` (`:1392`), plus one
line in `AppPreferences::load` normalising `Some(Unknown)` to the field's default
(`preferences.rs:162`, `:170`). 205 reaches this from adding
`RightDrawerGroup::Notes`; K-G3 reaches it from adding layouts. It is the same
two enums and the same defect, and two mini-specs proposing two different
mechanisms for it would be worse than either. Whichever item lands first carries
the change; the other asserts it in T7 and adds nothing.

**Why `#[serde(other)]` is right there and wrong in `layouts.json` — this is not
an inconsistency, and a reviewer will check.** `preferences.json`'s field is a
**cursor**: one transient "which drawer is open right now", overwritten on the
next rail click (`app/mod.rs:3473`). Collapsing an unresolvable cursor to the
default loses nothing a user typed. `layouts.json`'s field is a **catalogue
entry** the user authored and named; collapsing three distinct unknown ids to one
`Unknown` would make a round-trip through an older build silently rewrite their
Colour layout — precisely the loss 39 §2.2 forbids. Different data, different
tolerance, and the string id ([§3.3](#33-the-stored-form-stable-string-ids-not-enum-variants))
is what buys the stronger one.

**Also in scope: one shared reconciliation point.** `resolve` is the natural home
for "make the rails consistent with the mode", so the same helper closes
[§2.3](#23-what-does-not-exist--stated-plainly) points 4 and 5 — a
`reconcile_drawers_for_mode` called from `enter_or_exit_video_mode`
(`app/monitor.rs:475`, replacing the bare `self.open_drawer = None` and covering
the right rail it currently misses), from `switch_tab` (`app/tabs.rs:126`, where
nothing does it today), and from `apply_layout`. It also subsumes the hand-rolled
`History` remap duplicated at `app/mod.rs:2188-2192` and `:2220-2223`. Precedent
for taking an adjacent pre-existing defect in scope: 194 §8.1 defects 2 and 3,
taken in scope for the same reason — the new feature is what makes them
reachable, and fixing them elsewhere would mean writing the same reconciliation
twice.

---

## 5. Undo unit and its exact inverse

**There is no undo unit, and there must not be one. ROADMAP §10 point 4 is
answered N/A, and this section is the argument, not the omission.**

1. **Nothing K-G3 does is a document mutation.** Applying a layout writes
   `self.open_drawer`, `self.open_right_drawer`, two widths in `self.prefs`, one
   egui panel height and two visibility bools. `Document` is not touched;
   `CommandHistory` is not touched. The repo rule is *one user verb = one undo
   unit* for document mutations; a verb that mutates no document correctly
   produces no unit.
2. **The exact inverse of "apply Colour" is "apply the layout you had", and it
   already has an affordance** — the picker, which lists every layout including
   the one just left, plus `Ctrl+F<slot>`. Naming the inverse is the requirement;
   the inverse here is a sibling verb, not a history entry.
3. **Putting it on `Ctrl+Z` would be actively wrong.** The undo stack is the
   user's *edit* history. A layout switch interleaved into it means that after a
   bad cut, Ctrl+Z restores a panel instead of the cut — and worse, a user who
   switched layouts three times must press Ctrl+Z four times to undo one edit.
   Every NLE that has ever put view state on the undo stack has been asked to
   take it back off.
4. **41 §8 point 5 already sets this rule for chrome**, in the neighbouring case:
   cancelling a drag *"leaves the document byte-identical and **adds no history
   entry**, asserted against the revision counter."* K-G3 adopts both the rule
   and its test shape verbatim (T5).
5. **The one path that *would* create an entry is fenced off.** Mode entry runs
   two `history.execute` calls (`app/monitor.rs:507`, `:509`), which is precisely
   why [§3.5](#35-one-layout-list-mode-as-a-filter) forbids a layout from
   switching mode. That fence exists to keep this section true.

**The obligation this section takes on instead of an inverse:** T5 asserts that
applying every built-in layout, in every order, leaves `history.revision()`
unchanged (the same counter `app/mod.rs:3739`/`:3797` read) **and** leaves
`doc.to_json()` byte-identical. A regression that quietly routes a layout apply
through `CommandHistory` fails that test, which is a stronger guarantee than the
undo-identity assertion a document-carrying item would owe.

For completeness: `Document.workspaces` ([§2.2](#22-a-workspace-concept-already-exists-and-it-is-the-counter-example))
*is* a document mutation with no undo unit, today, and remains one — K-G3 does
not touch it, and [Follow-ups](#follow-ups) item 1 owns it.

---

## 6. Surfaces

### 6.1 GUI route

Four entry points, one manager, all through machinery that already exists.

| Surface | Where | Detail |
|---|---|---|
| **Layout picker** | Toolbar row (`app/mod.rs:3152-3180` block, beside the File/Edit/Tools `selectable_label`s) | A small dropdown showing the active layout's name; lists presets for the current mode, with the other mode's greyed and reasoned (§3.5) |
| **Save layout as… / Reset layout** | Same dropdown footer | "Reset layout" applies the built-in for the current mode (§3.6) |
| **Layouts settings page** | `EDIT_OPTIONS` (`app/mod.rs:291`) gains a 7th entry, rendered in the same `draw_two_column_menu` block as Keyboard Shortcuts (`app/menu_drawer.rs:455`) | Rename, reorder, re-slot, delete, **Reset all layouts** |
| **Command palette** | `commands.rs` `REGISTRY` (`:213`) | New ids below |
| **Keyboard** | via `resolve_binding` (`preferences.rs:261`) | Defaults below |

New `CommandDef` entries in the existing `view.` namespace (`commands.rs:213`),
dispatched by the existing `dispatch_command` (`app/command_center.rs:82`) — no
parallel keyboard path, the rule 04 §5.1 sets:

| `CommandId` | Label | Default binding |
|---|---|---|
| `view.layout_slot_1` … `view.layout_slot_4` | "Layout 1" … "Layout 4" | `Ctrl+F1` … `Ctrl+F4` |
| `view.layout_next` | "Next Layout" | none |
| `view.layout_reset` | "Reset Layout" | none |
| `view.layout_save_as` | "Save Layout As…" | none |

`Ctrl+F1`–`Ctrl+F4` are verified free: `grep -n 'Key::F' crates/photonic-gui/src/commands.rs`
returns exactly one hit, `Key::F` (the plain tool key at `:610`); no F-key binding
exists in the registry. `egui::Key::F1`–`F12` exist in egui 0.29. The lint that
proves it stays true is T8, asserting `binding_conflict` (`preferences.rs:270`)
is `None` for each new default.

**Slots are stored on the preset, not positional.** `LayoutPreset.slot`
([§3.2](#32-new-types--photonic-guisrclayoutsrs-a-new-module-not-in-photonic-core))
rather than "the Nth preset in the list". A positional binding silently changes
meaning the moment the user saves a new layout, which is a bug factory; a stored
slot is also directly editable in the manager, which is what a user asking "how
do I put Audio on Ctrl+F2" wants.

**Accessibility obligations, inherited not invented.** Applying a layout is
chrome, so it honours `prefs.reduced_motion` (41 R-17) — the two drawer tweens
already do (`app/mod.rs:3531`, `:3692`), and a layout apply must not introduce a
second animation that ignores it (T10). A layout switch must not steal keyboard
focus or reorder focus within a panel (41 §8 point 6). The picker is a normal
button with a label, so the AccessKit obligation (41 §8 point 12) is met by
construction.

### 6.2 MCP surface — none in v1, and why

**K-G3 ships no MCP tool. This is a recorded parity exception under ROADMAP §10
points 2–3, not an oversight.** Three independent reasons, any one of which would
be sufficient:

1. **There is no capability behind it.** ROADMAP §10 point 3 scopes the
   obligation to *"automatable capability"*. An agent has no eyes. Every function
   a panel exposes — grade, mixer, effects, media pool — is already reachable as
   a tool; opening the drawer that contains it adds nothing an agent can act on.
   26 §16's K-H trail obligation is *"every landed verb ships its MCP tool"*, and
   the verb here is "arrange **my** window", which has no agent-side referent.
2. **The MCP process cannot apply a layout, and a tool that pretended to would be
   worse than no tool.** `grep -rn 'AppPreferences\|preferences' crates/photonic-mcp/src/`
   is clean: `AppState` holds the document and history and has no handle to GUI
   preference state. The existing `load_workspace` demonstrates exactly this
   failure — it mutates nothing and returns the string *"Apply search_query: …"*
   (`handlers/doc_automation.rs:534-547`), i.e. it is a tool that returns a hint
   and hopes something applies it. Shipping `apply_layout` with the same shape
   would add a second one.
3. **The one honest use case is presence, not capability** — an agent telling a
   user *"I switched you to the Colour layout"*. That is a chat message about
   something the agent cannot do, and there is a real cost to letting an agent
   rearrange a user's window unprompted: it is the most visible possible action
   with the least possible undo (there is none, by [§5](#5-undo-unit-and-its-exact-inverse)).

If a future round wants a surface, the defensible shape is **read-only
`list_layouts`** so an agent can *name* the layout that would suit the task and
let the user press the key. That is recorded as
[§8.2 Q5](#82-open-questions-each-with-a-recommendation), not shipped.

**`docs/mcp-api.md` is therefore unchanged by K-G3**, and the CI docs gate
(`.github/workflows/ci.yml:163-167`, regenerate then `git diff --exit-code`)
stays green with no regeneration. That is a claim the implementer must verify
rather than assume: any accidental schema touch trips it.

---

## 7. Acceptance fixtures and tests

**No rights-cleared content is required. K-G3 is not a gated item.** No media, no
fixture bytes, no ffmpeg, no GPU adapter, no bundled asset — so 23 §7.2's
`AssetRightsManifest` gate is not engaged and ROADMAP §7's K/E/X gate list, which
names K-G4 but not K-G3, is not touched. Every fixture is a JSON string built
in-test; every store test runs against a `tempfile` through the path-parameterized
hooks ([§3.4](#34-where-layouts-live)).

| # | Test | Where | Proves |
|---|---|---|---|
| T1 | Built-in table: `resolve` of each of the five built-ins in its own mode yields the exact `(left, right, timeline_height, extras)` in §3.6 | `photonic-gui/src/layouts.rs` unit tests | The product's defaults, as data |
| T2 | **Unknown panel id**: a preset with `"panel": "flux_capacitor"` resolves with that drawer collapsed, `unresolved == ["flux_capacitor"]`, the rest of the preset applied — and a save-after-load re-emits the id **byte-identically** | `layouts.rs` unit tests | §4.2 rules 1–2; 39 §2.2 applied to config |
| T3 | **Known-but-wrong-mode id**: a `Vector` preset naming `"media_pool"` behaves exactly as T2 | `layouts.rs` unit tests | §3.5's one rule covering re-homed panels |
| T4 | **Clamp table**: widths `0`, `-1`, `1e9`, `NaN`, `inf`; `timeline_height` `1`, `NaN` → each lands inside the ranges at `app/mod.rs:3540`, `:3700`, `:3827` | `layouts.rs` unit tests | §4.2 row 4–5; no layout can produce an unusable window |
| T5 | **No undo unit**: apply all five built-ins in six orders; `history.revision()` unchanged and `doc.to_json()` byte-identical throughout | `photonic-gui/tests/video_ui_paths.rs` | §5 — the N/A, proven rather than asserted (41 §8.5's shape) |
| T6 | **Blast radius**: a `layouts.json` containing a future `version` and three unknown ids leaves `AppPreferences::load()` fully intact (theme, keymap, pinned tools compared field-by-field) | `photonic-gui/tests/` (new `layout_store.rs`) | §3.4's whole reason for a separate file |
| T7 | **Preferences tolerance regression**: a `preferences.json` with `"open_drawer": "NotAPanel"` loads with every *other* field preserved and the drawer closed — the behaviour this build does **not** have (§2.3 point 3) | `preferences.rs` unit tests | §4.2's in-scope tolerance fix |
| T8 | **Binding hygiene**: each new `Ctrl+F1`–`Ctrl+F4` default returns `None` from `binding_conflict` (`preferences.rs:270`); a `keymap` override wins over the registry default via `resolve_binding` | `commands.rs` / `preferences.rs` unit tests | §6.1; rebinding stays free |
| T9 | **Mode reconciliation** (the two live defects, §2.3 points 4–5): (a) video tab with Media Pool open → `switch_tab` to a vector tab → left drawer not rendering a video panel; (b) Colour Controls open → `enter_or_exit_video_mode` → right drawer collapsed | `photonic-gui/tests/video_ui_paths.rs` | §4.2's shared reconciliation point |
| T10 | **Reduced motion**: applying a layout with `reduced_motion == true` produces two consecutive identical frames | `photonic-gui/tests/` (41 §8.10's harness) | 41 R-17 |
| T11 | **Store round-trip**: write → read → structurally equal via `save_layouts_to` / `load_layouts_from`; a corrupt file is renamed `.bad`, built-ins are returned, and the original bytes still exist on disk | `layout_store.rs` | §3.4; §4.2 rule 5 |
| T12 | **Document untouched**: `CURRENT_FORMAT_VERSION == 5`, and a `.photon` saved after a layout switch is byte-identical to one saved before | `photonic-core/tests/timeline.rs` (assertion) + `video_ui_paths.rs` | §4.1 |
| T13 | **Built-in immutability**: saving over `"Colour"` is refused; deleting a built-in is refused; **Reset all layouts** clears customs and leaves the five built-ins with their slots | `layouts.rs` unit tests | §3.6 |
| T14 | **Slot integrity**: duplicate and out-of-range slots are dropped (not reassigned) with one diagnostic each; `view.layout_slot_2` with no slot-2 preset is a no-op, not a panic | `layouts.rs` + `command_center` tests | §4.2 row 6 |

Note for the implementer: T2, T3 and T11 must assert **byte-identical
re-emission**, not structural equality. Structural equality passes while a
serializer quietly normalises an unknown id, which is exactly the loss 39 §2.2
forbids.

No test in this table requires a display, a GPU adapter or ffmpeg, so all fourteen
run in the standard `Build & Test` matrix job (`.github/workflows/ci.yml:104`) on
all three platforms with no skip convention.

---

## 8. Risks, open questions and deliberate exclusions

### 8.1 Risks

1. **Scope creep into a docking system.** "Layout presets" reads to some
   reviewers as drag-to-dock, tear-off panels and `egui_dock`. It is not:
   Photonic's shell is a fixed rail + drawer + bottom-panel arrangement with
   hard-coded geometry constants (`app/mod.rs:3396-3403`), and a layout selects
   *which* panels are open and how wide within that shell. `egui_dock` is not in
   the dependency graph (`grep -rn egui_dock Cargo.toml crates/*/Cargo.toml` is
   clean) and adopting it would replace the shell, not serve this item —
   a change 04 §4's D-02 ("rails stay, contents adapt") forecloses. Reviewers
   should reject any change here that re-parents a panel.
2. **The timeline height is egui's state, not ours.** `PanelState`
   (egui 0.29.1 `src/containers/panel.rs:30-47`) is stored via `ctx.data_mut`
   and its `store` is private (`:44`). Two viable routes: **primary** —
   `ctx.data_mut(|d| d.remove::<PanelState>(Id::new("timeline")))` (public,
   `src/util/id_type_map.rs:481`) so the next frame takes a `default_height`
   driven from the preset; **fallback** — construct and `insert_persisted`
   (`:376`) directly, since `PanelState.rect` is public. Prefer the first: it
   uses only `remove` plus our own `default_height` and needs no knowledge of
   egui's internal layout shape. Either way this is the one place an egui version
   bump can break K-G3, so it lives behind a single helper with T1 exercising it.
3. **Two things called "workspace" / "layout".** Mitigated by vocabulary (K-G3
   says *layout*, never *workspace*) and by [Follow-ups](#follow-ups) item 1. Left
   unaddressed, the first support question is "why does Save Workspace not save my
   layout".
4. **Layout apply mid-gesture.** Applying a layout while a panel resize or a
   timeline drag is in flight would resize the surface under the pointer. Apply is
   refused (no-op plus a status line) while a drag gesture is active — the same
   guard boundary `commit_drag` (`app/timeline/mod.rs:1803`) already implies.
5. **Prefs write amplification.** Rail clicks call `prefs.save()` synchronously
   (`app/mod.rs:3474`, `:3682`). A layout apply changes up to four preference
   fields; it must write **once**, at the end, not per field. Small, but the
   existing pattern invites the mistake.

### 8.2 Open questions (each with a recommendation)

- **Q1 — Global, or per-tab like `AppMode`?** 04 §1 made mode per-tab
  (`DocTab::mode`, restored at `app/tabs.rs:126`), and a reviewer will ask why
  layouts are not. **Recommendation: global.** Mode is a property of *what the
  tab contains* (a document either has a timeline or does not); a layout is a
  property of *the human at the keyboard*. Per-tab layouts mean switching tabs
  rearranges the window, which is the specific behaviour users complain about in
  per-document UI state. *Product call because it diverges from the neighbouring
  precedent, and the divergence should be deliberate.*
- **Q2 — Should a "Logging"/"Ingest" built-in ship?** **Recommendation: no in
  v1**, per §3.6 — it needs `SourceMonitor`, which is a stub
  (`panels/mod.rs:1292`). Add it when G-10 lands; adding a built-in later is a
  const array entry and needs no migration.
- **Q3 — Auto-apply a layout on mode entry?** **Recommendation: once, on the
  first video-mode entry of a fresh install, then never again** — apply **Edit**
  and remember the user's last layout per mode thereafter (that is what the two
  `active_layout_*` strings in §3.4 are for). Reuse the one-shot flag shape
  `video_shortcuts_intro_shown` already uses (`preferences.rs:135`,
  `maybe_show_first_entry_hints` at `app/monitor.rs:515`). Auto-applying on
  *every* mode switch would override a deliberate arrangement, which is the
  complaint this feature exists to answer. *Product call.*
- **Q4 — Does "Reset layout" reset widths too, or only which panels are open?**
  **Recommendation: both.** A half-reset is not a reset, and the width is usually
  the thing the user dragged into a bad state.
- **Q5 — A read-only `list_layouts` MCP tool?** **Recommendation: not in v1**
  (§6.2), revisit if an agent-authored suggestion ("this is a colour task —
  press Ctrl+F2") turns out to be wanted. It is additive and costs one handler.

### 8.3 Deliberately excluded

- **Drag-to-dock, tear-off panels, floating docks, a second window.** Risk 1. The
  app is a single `winit` window (`crates/photonic-app/src/main.rs`), and OS
  window geometry is already owned by `WindowState` (`main.rs:274-296`); a layout
  that moved the window would fight it.
- **Theme, `dark_mode`, `ui_scale`.** Appearance, not arrangement. Bundling them
  means switching to Colour changes your theme, which nobody asks for and
  everybody notices.
- **Timeline zoom/scroll, playhead, selection.** Per-tab *document view* state
  (`app/tabs.rs:116-118`) and session selection (35 §3.4). A layout is chrome; a
  scroll position is where you were in the work.
- **`Track.height_px`.** A document field awaiting 39 §1.6's sidecar, which is
  still a no-op stub (`migration.rs:200-203`). It is per-track document data, not
  a workspace arrangement, and K-G3 must not be read as having fixed it.
- **Import/export a single layout to send to a colleague.** Defensible later and
  cheap — `load_layouts_from` / `save_layouts_to` already make it a file-picker
  and a merge — but it needs a merge/collision policy that v1 does not, and the
  underlying file is copyable today.
- **Per-monitor or per-resolution layouts** ("my laptop layout vs my desk
  layout"). A real workflow, and a real design problem (what identifies a
  monitor?). Out of scope; the width clamps mean a desk-width layout on a laptop
  degrades to a narrow-but-usable one rather than breaking.
- **Migrating `Document.workspaces`.** [Follow-ups](#follow-ups) item 1. Deleting
  a document field is a format change and belongs with whoever owns 39 §1.6, not
  inside a GUI-chrome item.

---

## 9. Clean-room provenance

Per [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
and [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol):

- **What was read.** Kdenlive's user-facing documentation (`docs.kdenlive.org`,
  `CC-BY-SA-4.0`) for three *facts about published behaviour*: that a named
  window-layout mechanism exists, that a small set of task-named default layouts
  ships with the product, and that layouts are reachable by function-key
  shortcuts. Those are requirements-level facts, readable as a requirements
  source under 26 §2 and cited here rather than pasted. The names **Edit**,
  **Colour**, **Audio**, **Effects**, **Design** are ordinary task words in this
  domain, not borrowed identifiers.
- **What was not read.** The Kdenlive source tree, the MLT/`mlt++` source tree,
  and any GPL/LGPL derivative. Additionally and specifically: **KDE's `KXmlGui`,
  `KDockWidget` and Qt's `QMainWindow::saveState`/`restoreState` machinery were
  neither read nor emulated.** Nothing about how a reference product serializes a
  dock tree informs this design, and nothing could: Photonic's shell has no dock
  tree. It has two `SidePanel`s, a `TopBottomPanel` and an enum of panel ids
  (`panels/mod.rs:1209`, `:1392`), so the stored form is a handful of scalars and
  string ids — a shape forced by Photonic's own UI, not adopted from anywhere.
  No symbol, constant, file name, key binding table or test derives from a
  reference implementation. The implementer records the 23 §3.4 attestation for
  the `panels-video` territory and an independent reviewer checks provenance
  before merge (26 §2 point 2).
- **Where the design actually comes from.** Every mechanism is Photonic's own:
  the store shape from `export/presets.rs:401-446` (26 §15 K-G3 names it in
  terms — *"reusing the `export/presets.rs` custom-store pattern"*); the
  preference-not-document placement from 195 §7.1; the stable-string-id pattern
  from `AppPreferences::keymap` + `CommandId` (`preferences.rs:151`,
  `commands.rs:18`); the unknown-preserving rule from 39 §2.2; the clamp ranges
  from the existing render code (`app/mod.rs:3540`, `:3700`, `:3827`); the
  one-shot-flag shape from `video_shortcuts_intro_shown`; the reduced-motion
  obligation from 41 R-17; the built-ins-constructed-in-Rust choice from 204 §3.4.
- **Photonic-ahead register (26 §5, ROADMAP §9): untouched, not consumed.** K-G3
  changes no pixel path, no graph, no `ContentHash`, no cache key, no range, no
  `Tick`, no error type. It does not port a reference limitation: layouts here
  span **both editors in one window**, which no reference NLE has to solve
  because none of them is also a vector editor — [§3.5](#35-one-layout-list-mode-as-a-filter)
  is a Photonic-specific design, not an adopted one.
- **Bundled bytes: none.** Built-ins are Rust `const` data, so
  [23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest)'s
  manifest gate is not engaged and K-G3 is **not** legal- or fixture-gated.
  ROADMAP §7's K/E/X gate list names K-G4 and does not name K-G3; that stays
  true.
- **No new dependency.** Nothing in 26 §2's reject list, directly or
  transitively. Explicitly **not** `egui_dock` (risk 1). Everything needed —
  `serde_json`, `std::fs`, the existing egui panels — is already in the build.
- **No network, no telemetry, no content.** `layouts.json` contains panel ids,
  numbers, and names the user typed. **No file paths and no document content** —
  unlike `recent_docs.json`, which does carry paths. Worth stating because it
  means a layout file is safe to attach to a bug report.

---

## 10. Definition of done → ROADMAP §10

| # | ROADMAP §10 point | Answered by |
|---|---|---|
| 1 | Core op/engine service with unit tests | `photonic-gui/src/layouts.rs` — the pure `resolve` + the store, tests T1–T4, T11, T13, T14. **Stated honestly: there is no engine service.** K-G3 is a GUI-chrome item; the "core" is the resolver, and it is pure and testable precisely so this row is real |
| 2 | GUI route, or a recorded exception | §6.1 — picker, Layouts settings page (`EDIT_OPTIONS`, `app/mod.rs:291`), palette, `Ctrl+F1`–`Ctrl+F4`. No GUI exception |
| 3 | MCP tool/schema/generated docs | **Recorded exception, §6.2**, argued three ways. `docs/mcp-api.md` unchanged; the CI docs gate (`ci.yml:163-167`) must stay green with no regeneration |
| 4 | One user verb = one undo unit | **N/A, §5** — no document mutation, so no unit; the inverse is a sibling verb (the picker), and T5 asserts `history.revision()` unchanged plus a byte-identical document |
| 5 | Additive serde/migration round-trip | **N/A for the document, §4.1** — `CURRENT_FORMAT_VERSION` stays 5, `Document` unchanged, T12 asserts it. The round-trip obligation lands on `layouts.json` instead: T2, T3, T11 |
| 6 | IR/eval/golden/sync coverage for new pixel/audio paths | **None needed** — K-G3 adds no pixel or audio path and touches no `ContentHash` |
| 7 | Hard gates green; trend metrics not regressed | No hard gate is on this path. One trend note: a layout apply is a handful of field writes plus at most one egui panel-state removal, and the drawer tween already requests repaints while in flight (`app/mod.rs:3531-3537`) — it must not add a second animation or a per-frame write (risk 5) |
| 8 | Offline, privacy, licensing, content, product gates | §9 — no bytes, no dependency, no network. Privacy: `layouts.json` holds panel ids, numbers and user-typed names; **no paths, no document content** |
| 9 | No protected-surface regression | None touched. 04 §4's D-02 ("rails stay, contents adapt") is honoured — a layout selects among the existing rail sets. 41 R-17 reduced motion honoured (T10). Shortcut rebinding (ROADMAP §9) preserved: the new ids go through `resolve_binding` like every other (T8) |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | §1's five outcomes are the L4 script; outcome 5 (degradation) is L1–L3 via T2/T3/T6/T7/T11. Parity: §6.2's exception recorded |

---

## Follow-ups

Changes to other documents and subsystems that this item deliberately does
**not** make. Each needs its own change.

1. **`Document.workspaces` is misplaced and non-undoable.**
   (`photonic-core/src/document.rs:764-872`; GUI `panels/document.rs:858-933` +
   `app/panel_actions.rs:5919-5940`; MCP `handlers/doc_automation.rs:512-573`;
   `docs/mcp-api.md:1627,2732,2755,3623`.) It stores a per-user panel *filter
   query* inside the `.photon` and mutates the document outside `CommandHistory`.
   Recommended resolution, in order: (a) rename the user-facing concept to
   **panel filter preset** everywhere so it stops colliding with K-G3's
   vocabulary; (b) move the data to `layouts.json` as an optional
   `filter_query: Option<String>` on `LayoutPreset`, where it belongs; (c) delete
   `Document.workspaces` and the four MCP tools at the **next** format version —
   a `v6` change with a real migration, exactly the kind §4.1 says a bump is for.
   Until (c), the four tools stay wire-compatible.
2. **[39 §1.6](../specs/video-editor/39-document-lifecycle.md#16-what-is-not-undoable)
   lists two violations of *"every document mutation is undoable"*; there is a
   third** — `Document.workspaces` (item 1). 39 §1.6 should name it, and its
   sidecar recommendation is still unimplemented: `migration.rs:200-203` is a
   documented no-op and `Track.height_px` is still an in-document, non-undoable
   field.
3. **`AppPreferences::load` resets everything on any parse failure**
   (`preferences.rs:331`, `unwrap_or_default()`), and `save` (`:335`) will then
   overwrite the original file. K-G3 fixes the specific *unknown-enum-variant*
   hazard for the two drawer fields (§4.2) but not the general one. The general
   fix is `layouts.json`'s rule 5 applied to `preferences.json`: quarantine a
   file that fails to parse instead of silently starting from defaults and
   overwriting it. Owned by whoever owns app preferences.
4. **[04 §4](../specs/video-editor/04-ui-mode-timeline.md#4-mode-adaptive-panels)
   has two inaccuracies.** (a) Its citations `panels/mod.rs:1105` /
   `panels/mod.rs:1168` have drifted to `:1271` / `:1409`. (b) Its sentence
   *"`open_drawer` … is cleared on mode switch"* describes half the shipped
   behaviour: `enter_or_exit_video_mode` clears the **left** drawer
   (`app/monitor.rs:475`) and not the right, and `switch_tab`
   (`app/tabs.rs:98-133`) clears neither — so a video-only panel can render in
   vector mode with no rail button to close it. K-G3 fixes the code (§4.2, T9);
   04 §4's text should be corrected to match whichever lands.
5. **[26 §15 K-G3](../specs/video-editor/26-kdenlive-mlt-parity.md#k-g3--layout-presets)**'s
   Files line ("serialize panel/drawer visibility + sizes to the config dir,
   reusing the `export/presets.rs` custom-store pattern") is accurate and is
   adopted verbatim. It should additionally record (a) the preference-vs-document
   decision (§3.1) and (b) the `Document.workspaces` name collision, so the next
   reader does not rediscover both.
6. **[41-accessibility.md](../specs/video-editor/41-accessibility.md) §6** should
   note that layout switching is chrome under R-17 and that a layout apply must
   not steal or reorder focus (§8 point 6) — one line each, so a future layout
   animation cannot be added without the flag.
7. **ROADMAP.md §0** progress table — add a K-G3 row with its commit when the
   item lands, per the existing convention.
