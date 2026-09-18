# 04 — UI: Mode, Timeline, Program Monitor, Keyboard

**Depends on:** 01-data-model.md, 02-engine.md, 03-render-color-pipeline.md (§5 present path). **Decisions:** D-02 (locked), D-10.
**Owns:** `AppMode`, `photonic-gui/src/app/timeline/` module family, program-monitor canvas path, left/right rail mode-adaptivity, keyboard model. Scope per 00 §5: the AppMode mechanism, bottom timeline panel, program monitor, mode-adaptive panels, keyboard model.

D-02 is a hard constraint on this whole doc: **mode switch preserves the existing layout.** No full-screen video workspace. Video mode = existing shell (toolbar, left rail, canvas, right rail) + a bottom-docked timeline panel + canvas repurposed as program monitor + rail contents swapped per mode. Every design choice below defaults to "smallest change to the existing shell" over "new screen."

```
Vector mode (today, unchanged)          Video mode (this doc)
┌─────────────────────────────┐         ┌─────────────────────────────┐
│ toolbar (File Edit Tools...) │         │ toolbar (...+ Video toggle)  │
├───┬───────────────────┬─────┤         ├───┬───────────────────┬─────┤
│ L │                   │  R  │         │ L │                   │  R  │
│ a │      canvas       │  a  │         │ a │  program monitor  │  a  │
│ i │   (vector doc)     │  i  │         │ i │  (EngineFrame +   │  i  │
│ l │                   │  l  │         │ l │  transport bar)   │  l  │
│   │                   │     │         │   │                   │     │
│(6 groups,             │(3   │         │(5 video groups,       │(5   │
│ Tools..Document)      │groups)        │ MediaPool..NodeEditor)│groups,
├───┴───────────────────┴─────┤         ├───┴───────────────────┴─────┤
│  (lua console, if open)     │         │  (lua console, if open)     │
└─────────────────────────────┘         │  timeline panel (NEW)       │
                                         │  headers | ruler+lanes      │
                                         └─────────────────────────────┘
```

Same rect budget, same rail chrome, same tab bar — only the central content and the rail's *group list* change, plus one new bottom panel. This is the literal shape of D-02.

---

## 1. AppMode

```rust
// crates/photonic-gui/src/app/mode.rs (new)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AppMode {
    #[default]
    Vector,
    Video,
}
```

Field on `PhotonicApp` (`crates/photonic-gui/src/app/mod.rs:632` struct body, sibling of `active_tool`): `pub mode: AppMode`. Per-tab, not global — lives on the tab record next to `pub dirty: bool` (`app/mod.rs` tab struct, same block as `title`/`recovery_path`/`last_saved_node`), because a user may have a vector-only tab and a video-project tab open at once and mode is a property of *what the tab contains*, not a global UI toggle. `switch_tab` (existing fn, called at the tab-switch site `app/mod.rs:~1972` `self.switch_tab(target, doc, history, view)`) restores `self.mode` from the incoming tab's stored mode as part of its existing state-restore work (it already restores `selected_id`, `view` — one more field).

### 1.1 Where `draw()` branches

`PhotonicApp::draw()` (`app/mod.rs:1961`) sequence today: crash-recovery prompt → welcome early-return (`:2371`, `:2477`) → tab bookkeeping → modals → **top toolbar** (`:2524`) → central canvas (`:3119`). The welcome early-return is the only existing screen-level bifurcation and it stays exactly as-is (mode is meaningless before a document exists).

Per D-02, `AppMode` does **not** add a second top-level branch parallel to welcome. It threads through as a value read at three specific points, all *after* the welcome return and *after* tab bookkeeping:

1. **Toolbar** (`:2524` block) — reads `self.mode` to swap the row of mode-specific buttons (§1.2) but keeps File/Edit/Tools drawer toggles and the tab bar unconditionally.
2. **Left/right rail contents** (`panels/mod.rs` `draw_drawer`, `DrawerGroup::ALL`/`RightDrawerGroup::ALL`) — reads `self.mode` to pick which group set is offered (§4). The rail *chrome* (icon strip, drawer show/hide animation, resize) is unchanged code.
3. **Central panel** (`:3119` `egui::CentralPanel::default()...show(ctx, |ui| {...})`) — after the existing fit-pending/raster-brush/cursor-overlay preamble, branches on `self.mode` and (in video mode) the central-content state: `Vector` runs the existing tool if-chain (`:4766`+) unchanged; `Video` has **two content states** — (a) **monitor** (default): the program-monitor + timeline-panel path (§2, §3); (b) **node canvas**: when a composition or the project graph is open for editing, the central panel shows the egui-snarl node canvas with a live composed-output inset (70/30 split, 08 §6.1), with a Back-to-Timeline button and `Esc` returning to the monitor state. This is the only real `match` on mode/content in the function; everything else is data (which groups/buttons to list), not control flow, honoring "branch panel contents, not the whole screen."

Additionally, a new `egui::TopBottomPanel::bottom("timeline")` is registered only when `self.mode == AppMode::Video`, docked below the existing `lua_console` bottom panel and above the (now program-monitor) `CentralPanel`. egui panel order is registration order per frame — placing it after the console panel's `show_animated` call (`app/mod.rs:580` block, immediately before `CentralPanel` at `:3119`) and before `CentralPanel::show` gives correct stacking with zero change to any panel already registered.

### 1.2 Entry / exit paths

Four symmetric entry points, one shared exit:

- **Toolbar toggle** — new "Video" button in the toolbar row (`:2524` block, alongside the existing File/Edit/Tools `selectable_label`s), same `selectable_label` + `active_drawer`-flush idiom already used there (flushing prefs on drawer switch, `:2536`). Click sets `self.mode = Video` if `doc.timeline.is_some()` (creates one first — §1.3 — if not); click while already in Video sets `self.mode = Vector`.
- **Command palette** — new `CommandId`s `"mode.enter_video"` / `"mode.exit_video"` / `"mode.toggle_video"` registered in `commands.rs` next to the existing `TOOL_COMMANDS` table (`commands.rs:346`), dispatched through the existing `dispatch_command` (`app/command_center.rs:49`) — no new dispatch mechanism.
- **Welcome action** — extend `WelcomeAction` (`welcome.rs:313`) with `WelcomeAction::CreateNewVideo(VideoProjectSpec)` alongside `CreateNew(spec)`; handled in the same `match action` arm group (`app/mod.rs:2372`) by calling `create_document_from_spec` then immediately setting `self.mode = Video` and creating the `TimelineProject` (§1.3), mirroring how `CreateNew` already sets `doc_modified = true` in that same match.
- **Auto-enter on open** — in the `WelcomeAction::OpenFile` arm (`app/mod.rs:2381`) and the CLI/tab-open path, after `*doc = loaded`, check `loaded.timeline.is_some()` and set `self.mode = Video` if true. A project with a timeline always opens into video mode; this is the only *implicit* transition and it is D-02-safe because it's the same layout, just pre-selected.
- **First-run hint (SS-2 discoverability)** — the first time video mode becomes available in a session (and only until dismissed once, persisted in `AppPreferences`), a small callout anchors to the Video toolbar toggle ("New: edit video timelines — click here or Ctrl+Shift+V"); on first *entry*, a one-time overlay lists the core shortcuts (Space/JKL/I/O/S) with "press ? anytime to see these again" — `?` opens the shortcut sheet in video mode thereafter.
- **Exit** — toggling back to Vector never destroys `doc.timeline` (data persists per 01 §2, `Document::new()` only sets `timeline: None` for brand-new documents); it only hides the timeline panel and restores rail contents. Symmetric with entry: same toolbar button / command / one new `WelcomeAction`-adjacent affordance is unnecessary for exit since it's a toggle, not a distinct action.

### 1.3 First-action project creation

`TimelineProject` is created lazily and undoably (01 §2 note: "first video-mode action creates it") via `TimelineCmd::CreateProject` (01 §10) — issued by whichever entry path is first taken on a document with `timeline: None`. This keeps `AppMode::Video` and `doc.timeline.is_some()` in lockstep: the toolbar/palette/auto-enter code above never sets `self.mode = Video` without first guaranteeing the project exists, so §2/§3 code can assume `doc.timeline` is `Some` whenever `self.mode == Video` (documented invariant, checked with `debug_assert!` at the top of the video central-panel branch).

---

### 1.4 Autosave & crash recovery (CAP-022, D-12)

Photonic already runs timed autosave (`app/autosave.rs`, `prefs.autosave_enabled` default on, `autosave_interval_secs` default 300s, floor 15s): titled documents write to their real `.photon` file plus a named "Autosave" history branch; untitled documents write to `crash_dir()/recovery` and are offered on next launch. Because `Document.timeline` serializes inside the document (01 §2/§9), timeline projects ride this machinery with **zero new subsystems**. The deltas this module owes:

1. **Media survives recovery by reference discipline, not copying:** recovery files carry the same absolute + project-relative asset paths as normal saves (01 §9); a recovered untitled project relinks by content hash exactly like a moved project. The sidecar cache dir (`<project>.photon.cache/`) is rebuildable and never part of recovery.
2. **Autosave cost stays flat:** media is never embedded (SPEC constraint), so a timeline project's JSON stays small regardless of footage volume — no interval change needed for video mode. The P3 exit test (11 §6) asserts an autosave pass on the AS-2 reference project completes within the existing frame-budget tolerance (no visible hitch).
3. **Engine state is disposable by design:** playhead, caches, decode rings are session state (01 §11) — recovery restores the document, and the engine cold-starts from it. Nothing engine-side needs persisting.
4. **Recovery prompt copy** gains a video-aware line when the recovered document has a timeline ("Recovered video project — media will relink automatically; proxies rebuild in the background").

## 2. Timeline panel

New module family `crates/photonic-gui/src/app/timeline/` (mirrors the existing `panels/` split-by-concern pattern):

```
app/timeline/
  mod.rs        // draw_timeline_panel(ctx, app, doc, engine_status) — the bottom-panel entry point
  layout.rs     // TimelineView: zoom/scroll/tick↔pixel mapping (session state, §6)
  ruler.rs      // time ruler, playhead widget, marker row
  tracks.rs     // track header column, add/reorder/height-drag
  clips.rs      // clip rect rendering, thumbnails/waveform sampling from engine caches
  interact.rs   // select/move/trim/split/ripple-roll-slip-slide/snap/multi-select/drag-in
  ops_bridge.rs // intent → timeline/ops.rs call → CommandHistory, per clip below
```

### 2.1 Layout

Two-column grid inside the bottom panel: fixed-width track-header column (left, resizable via a splitter, default 160px, clamped 120–320px — same clamp discipline as the existing drawer widths) + scrollable clip-lane area (right). Above the lane area, a ruler strip (fixed height, ~24px) shared across all tracks (horizontal scroll/zoom is one value for the whole panel, not per-track — matches every reference NLE and avoids desync bugs).

**Zoom/scroll model** (session state, §6), built on 01 §1's `Tick`/`FrameRate`:

```rust
pub struct TimelineView {
    pub pixels_per_tick: f64,       // zoom; clamped [MIN_PPT, MAX_PPT]
    pub scroll_ticks: Tick,         // leftmost visible tick
    pub track_scroll_px: f32,       // vertical scroll across track rows
}
impl TimelineView {
    pub fn tick_to_x(&self, t: Tick, lane_left_px: f32) -> f32 {
        lane_left_px + ((t.0 - self.scroll_ticks.0) as f64 * self.pixels_per_tick) as f32
    }
    pub fn x_to_tick(&self, x: f32, lane_left_px: f32) -> Tick {
        Tick(((x - lane_left_px) as f64 / self.pixels_per_tick) as i64 + self.scroll_ticks.0)
    }
}
```

`pixels_per_tick` is stored as ticks (not frames) per 01 §1 rule ("no f32/f64 time in the data model") — this struct lives in GUI session state, not the document, so `f64` here is fine and is exactly the "UI converts at the edge" case 01 §1 calls out. Snapping (§2.3) calls `sequence.frame_rate.snap(t)` before commit, so displayed and committed positions always land on frame boundaries regardless of zoom-level rounding.

Zoom levels: `Ctrl+scroll` over the ruler (mirrors the existing vector-canvas `Ctrl`-free scroll-to-zoom convention at `app/mod.rs:4647` — using `Ctrl+scroll` here instead of bare scroll because bare vertical scroll pages the track list); `+`/`-` keys (§5); a zoom-to-fit button (fits `work_range` or full sequence extent to lane width).

### 2.2 Clip rendering

Per visible clip (from `Track::clips`, already sorted/non-overlapping per 01 §4 invariant — no re-sort needed, only a binary-search-by-start for the visible-range slice):

```rust
// timeline/clips.rs — visible-range query, called once per track per frame
fn visible_clips(track: &Track, view: &TimelineView, lane_width_px: f32) -> &[Clip] {
    let first_visible_tick = view.scroll_ticks;
    let last_visible_tick = view.x_to_tick(lane_width_px, 0.0);
    let start_idx = track.clips.partition_point(|c| c.start + c.duration < first_visible_tick);
    let end_idx = track.clips.partition_point(|c| c.start <= last_visible_tick);
    &track.clips[start_idx..end_idx]
}
```

`partition_point` on the already-sorted, non-overlapping `Vec<Clip>` is O(log n); this is the mechanism §7's culling risk mitigation refers to — it runs before any per-clip egui widget is built, so off-screen clips never reach the painter.

- **Rect**: `x = tick_to_x(clip.start)`, `width = pixels_per_tick * clip.duration`, height = `track.height_px` minus padding.
- **Thumbnail strip**: sampled frames from the engine's decoded-frame ring / a lightweight thumbnail cache keyed by `(asset, source_time)` — new small LRU in `photonic-video::media` (not listed in 02's cache table because it's a GUI-driven convenience cache, not part of frame-graph eval; spec position: add it as a `thumbnail_cache` sibling to the waveform/thumbnail sidecar entry in 02 §5's cache table, generated on-demand at low res when a clip enters the visible range, never blocking).
- **Waveform**: audio clips render the pyramid from 02 §5 / 09, downsampled to `pixels_per_tick`.
- **Name label** clipped to rect width; **transition badges** (small triangular overlay at the edge where `transition_in`/`transition_out` is `Some`) drawn on top of the trim handles (§2.3) so both are visible at once.
- **Selection state**: outline stroke, same red/blue-accent convention as the existing vector-canvas selection boxes (`app/mod.rs` diff/removed-box rendering pattern at `:4742` — reuse `Color32` constants, don't invent a new palette).

Disabled clips (`clip.enabled == false`) render at reduced opacity; offline media (01 §3, no reachable file) renders the diagonal-stripe placeholder pattern the data model already specifies, at thumbnail size.

### 2.3 Interactions → intent → op table

Every mutation is a **pure `timeline/ops.rs` function producing a `TimelineCmd`, pushed through `CommandHistory`** — never a direct mutation of `doc.timeline` from GUI code (mirrors the rule 01 §10 states for MCP parity, CAP-019). `interact.rs` only computes *what the user intends*; `ops_bridge.rs` is the sole place that calls `ops::*` and pushes to history.

| Interaction | Detection | `ops.rs` fn | `TimelineCmd` variant |
|---|---|---|---|
| Click clip | `egui::Sense::click` on clip rect | — (selection only) | none (§6: selection is session state) |
| Ctrl/Shift-click | modifier + click | — | none |
| Drag clip body | drag on clip rect, not near edge (edge = 6px hit zone) | `move_clip` | `MoveClip` |
| Drag left/right edge | drag inside 6px edge hit zone | `trim_clip` | `TrimClip` |
| `S` at playhead on selected clip(s) | keypress, §5 | `split_clip` | `SplitClip` |
| Delete key | keypress | `remove_clip` | `RemoveClip` |
| Drag with Ripple modifier held | drag + modifier (§2.4) | `ripple_edit` | `RippleEdit` |
| Drag edge with Roll modifier | drag + modifier | `roll_edit` | `RollEdit` |
| Drag with Slip modifier | drag + modifier | `slip_clip` | `SlipClip` |
| Drag with Slide modifier | drag + modifier | `slide_clip` | `SlideClip` |
| Drag from Media Pool onto lane | drag-and-drop payload (egui `dnd` or manual pointer-payload state) | `insert_clip` | `InsertClip` |
| Track header: enable/lock toggle | click icon | `set_track_prop` | `SetTrackProp` |
| Track header: height drag | drag on header/lane boundary | (GUI-only: `track.height_px` is persisted-but-not-undoable UI pref, 01 §4 marks it "UI-only but persisted") | — direct field write, no command |
| Context menu: Add Transition / Remove Effect / etc. | right-click menu | `set_clip_prop` / effect ops | `SetClipProp` / `AddEffect` / `RemoveEffect` |

Drag gestures (move/trim/slip/slide) **coalesce** into one undo step per gesture using the same anchor-and-coalesce mechanism `CommandHistory` already implements for canvas drags (`photonic-core/src/history/mod.rs` coalesce tests, e.g. `coalesce_streamed_updates_into_one_step`) — `ops_bridge.rs` opens a coalesce anchor keyed by `(TimelineCmd variant, clip id)` on drag-start (matching 01 §10's stated rule) and streams updates until pointer release.

### 2.4 Ripple/roll/slip/slide modifier scheme

Reference-NLE-standard, chosen for zero new keys beyond existing modifier semantics:

- **Ripple** (default trim behavior *without* a modifier is a plain overwrite-trim leaving a gap/overlap per clip's own bounds only): hold **no modifier** for a plain trim; hold **Shift** while dragging an edge to ripple-trim (trims the clip and shifts every downstream clip on the same track by the same delta).
- **Roll**: drag the boundary *between* two adjacent clips (hit-test on the shared edge, not a single clip's edge) — trims one clip's out-point and the neighbor's in-point together, net sequence duration unchanged. No modifier; distinguished purely by hit-testing on a shared boundary vs. a lone edge.
- **Slip**: hold **Alt** while dragging a clip body — changes `source_in` without moving `start`/`duration` (the content shifts under a fixed window).
- **Slide**: hold **Alt+Shift** while dragging a clip body — moves the clip's `start` while trimming neighbors to absorb the delta, clip's own in/out unchanged.

This reuses Shift (already the vector-canvas "constrain/nudge×10" modifier, `app/mod.rs:4586` nudge-distance ×10) and Alt (unclaimed on the canvas today) rather than inventing new chords, keeping muscle memory consistent for Shift and giving Alt a single, memorable meaning in timeline context.

### 2.5 Snapping

Toggleable magnet (toolbar icon in the timeline panel's own mini-toolbar + keybinding, §5). When on, drag/trim operations snap the moving edge to, in priority order: other clip edges on the same track → clip edges on other tracks within a pixel-distance threshold (converted from a fixed *screen*-pixel threshold to ticks via `pixels_per_tick`, so snap distance feels constant across zoom) → playhead position → markers (01 §4 `Sequence::markers`). Snap candidates are computed once per drag-start (not per-frame) from the visible clip set for performance (§7).

### 2.6 Multi-select, track controls, context menus

- **Multi-select**: marquee-drag on empty lane space (rect-intersect against clip rects, same pattern as the vector canvas's lasso/rect select); Ctrl/Shift-click adds/toggles. Selection set is `Vec<ClipId>` in session state (§6).
- **Track controls** (header column): enable/hide (video) or mute (audio) toggle, lock toggle, height-drag handle, track name (double-click to rename → `SetTrackProp`), Add Track / Remove Track buttons at the column's bottom.
- **Context menus**: right-click on a clip → egui's `Response::context_menu` — the established idiom in this codebase (`panels/layers_panel.rs:177,519`, `panels/history.rs:468`, `app/direct_select.rs` all use it) — offering Split, Delete, Add Transition In/Out, Enable/Disable, Open as Node Composition (→ 08), Detach Audio (video clips with embedded audio → creates a linked audio clip).

---

## 3. Program monitor

The **existing canvas** (`egui::CentralPanel` at `app/mod.rs:3119`) is the program monitor while `self.mode == AppMode::Video` — not a second viewport. D-02 requires this literally: same rect, same `last_canvas_rect` bookkeeping, same fit/zoom/pan session state (`view: &mut CanvasView`), reused rather than duplicated.

**Single context-driven monitor (normative):** there is one preview surface. What it shows is retargeted by focus and transport state (`PreviewTarget` in [24-preview-media-load.md](24-preview-media-load.md)) — sequence under the playhead by default; media-pool / Match Frame **source peek** replaces the same surface rather than opening a dual source|program layout. Play always wins over selection retarget. Load/preview performance budgets and Draft/Full tiers are owned by 24.

### 3.1 Composition with existing canvas rendering

Today the canvas area's GPU content comes from `state.renderer` (document geometry + text + glow passes, `main.rs:594` `render_frame`) composited *before* egui runs, then egui draws overlays. For the video-mode monitor:

- The engine publishes `EngineFrame { texture: Arc<wgpu::Texture>, .. }` (02 §1) on its watch/triple-buffer channel. Presentation follows **03 §5's normative mechanism**: `present_engine_frame(frame, target)` (owned by `photonic-render`) runs a full-screen conversion pass (unpremultiply + sRGB OETF encode — the app's window surface is deliberately non-sRGB, so the encode is explicit shader work, 03 §5) into an intermediate texture, which is registered with egui via `egui_wgpu::Renderer::register_native_texture` and displayed with `ui.image(...)` inside the `CentralPanel`. When `self.mode == AppMode::Video`, the renderer's per-frame update (`main.rs` step 1, `state.renderer.update()`) skips the vector geometry pass entirely; no blit into the frame's color target occurs — the monitor image is an egui image widget, which also gives free interaction (hover coordinates for eyedroppers, drag for reframe handles).
- **Format conversion for display** happens only inside `present_engine_frame` (03 §5) — at monitor presentation, never earlier, so scopes/exports upstream of display stay in linear space per SS-3. This doc fixes *where* (presentation) and *what widget* (egui image in the CentralPanel); the shader math is 03's.
- egui text/glow overlay passes (`render_text_pass`, `render_gaussian_glow_pass`, `main.rs` steps 2b/2c) are skipped in video mode (no vector text/glow nodes exist on an `EngineFrame`); caption/safe-area/transport overlays are drawn by egui in the same slot instead (§3.3), keeping the "GPU pass then egui" ordering identical, just with different content per mode.
- If no frame is yet available (engine still compiling/decoding first frame after a seek), the monitor holds the last-presented texture (standard NLE behavior) rather than flashing black; `EngineStatus` (02 §1) exposes enough to show a small "buffering" spinner overlay.

### 3.2 Transport controls

A slim overlay bar along the bottom edge of the canvas rect (inside the `CentralPanel`, an `egui::Area` or a bottom-aligned `ui.horizontal` — not a separate panel, so it doesn't compete with the timeline panel for D-02's "preserve layout" budget): play/pause, step back/forward one frame, loop toggle, in/out markers, current-timecode readout (`frame_rate.frame_at(playhead)` formatted `HH:MM:SS:FF`), work-range scrubber ticks. All controls call `EngineCmd` variants (02 §1: `Play`, `Pause`, `Seek`, `Step`, `SetLoop`) — the GUI never simulates playback locally, it only reflects `EngineStatus`.

**JKL** (§5) is the same transport, exposed as keyboard rather than click: J = play reverse (repeated presses increase reverse speed, standard convention), K = pause, L = play forward (repeated presses increase speed). Space = play/pause toggle (single-speed).

### 3.3 Safe-area / format overlays and reframe

- **Safe-area overlay**: toggleable guide lines (action-safe/title-safe standard percentages) drawn by egui directly over the canvas rect, positioned from the active `SequenceFormat.width/height` (01 §4) mapped into canvas-rect coordinates the same way the existing vector canvas maps document space → screen space (`view.canvas_to_screen`, reused verbatim — the monitor "document" is just `width × height` at world origin, no artboard concept needed).
- **Format overlay**: when the sequence's rendered aspect ratio doesn't match the canvas rect's aspect ratio, letterbox/pillarbox bars (solid fill, standard NLE treatment) — computed once per format/rect-size change, not per frame.
- **Reframe manipulation (CAP-012)**: in video mode, selecting a clip whose `reframe` map (01 §5) has (or can have) an entry for the active `SequenceFormat` index shows an on-canvas transform handle set — same handle *widgets* the vector canvas already uses for scale/rotate on a selected shape (reuse, don't reinvent: the existing selection-transform-handle drawing code becomes the `ClipTransform`-editing surface here too, parametrized over "what am I editing" rather than duplicated). Dragging a handle computes a new `ClipTransform`, writes it into `clip.reframe[active_format]` via `SetClipProp`-style op (01 §10), coalesced per gesture like any other drag.

---

## 4. Mode-adaptive panels

D-02: rails stay, contents adapt. Concretely, `DrawerGroup::ALL` (`panels/mod.rs:1105`) and `RightDrawerGroup::ALL` (`panels/mod.rs:1168`) — currently fixed `const` arrays — become **mode-dependent functions**:

```rust
impl DrawerGroup {
    pub fn all_for_mode(mode: AppMode) -> &'static [DrawerGroup] {
        match mode {
            AppMode::Vector => &Self::ALL,                    // unchanged: Tools, Inspector, Modify, Arrange, Assets, Document
            AppMode::Video  => &Self::VIDEO_ALL,               // new const, §4.1
        }
    }
}
```

This is the minimal-diff route: the six existing vector variants and their `icon()`/`title()`/`has_content()` impls are untouched; new video variants are added to the same enum (so `draw_drawer`'s existing exhaustive dispatch just grows more match arms — same pattern the doc-comment at `panels/mod.rs:1076` already describes: "every section is reachable through exactly one group"). `open_drawer: Option<DrawerGroup>` (`app/mod.rs:1131`) is cleared on mode switch (a vector `open_drawer` value is meaningless in video mode and vice versa) — one line added at the mode-toggle sites from §1.2.

### 4.1 New groups and their doc owners

| New `DrawerGroup` variant (left rail) | Contents | Interior owned by |
|---|---|---|
| `MediaPool` | Import, bins, asset list, probe metadata, proxy status/toggle | 05-import-export.md |
| `ClipInspector` | Selected clip's transform/speed/effects-stack/transition params (mirrors today's `Inspector` group but for `Clip`/`ClipEffect` instead of `SceneNode`) | this doc owns the *panel shell*; effect param widgets sourced from `prop_registry` (01 §6.2) same as vector props today |
| `Effects` | Effect browser/catalog, drag-to-apply onto selected clip | 08-fusion-node-flows.md (shares `EffectKind` catalog with grade/node ops) |
| `Captions` | Caption track list, cue text/timing editor, style panel | 06-captions-ai.md |
| `NodeEditor` | Node **palette** (searchable add-node list), node inspector (selected node's params), graph info — NOT the graph canvas itself, which lives in the central panel's node-canvas content state (§1.1 point 3, 08 §6.1): an egui-snarl canvas cannot work inside a narrow drawer | 08-fusion-node-flows.md |

`DrawerGroup::VIDEO_ALL` const lists these five, in that order (Media Pool first — it's the entry point for populating a project, matching every reference NLE's left-most-panel convention).

| New `RightDrawerGroup` variant (right rail) | Contents | Interior owned by |
|---|---|---|
| `AudioMixer` | Track fader strips, master bus meters, per-track EQ/comp/automation entry points | 09-audio-mixer.md |
| `ColorControls` | Wheels/curves/HSL qualifier/LUT browser for the selected clip's grade | 07-color-grading.md (its §5 position, adopted over an earlier left-rail draft) |

**Scopes are NOT in any drawer**: waveform/vectorscope/histogram live in a separate floating/dockable panel (an `egui::Window`, resizable, GPU-rendered) — wide plots don't fit rail-drawer widths. 07 §5 owns the scopes panel interior; this is the one deliberate exception to the rails-stay-rails rule, recorded in §9.

`RightDrawerGroup::VIDEO_ALL` = `[Layers, ColorControls, AudioMixer, Chat, History]` — `Layers` stays (a timeline's clips still benefit from a flattened list view for keyboard-driven selection; cheap to keep, no reason to hide it) and `Chat`/`History` are mode-agnostic already (AI chat and undo history apply equally to timeline commands, since `TimelineCmd` is just another `Command` variant per 01 §10).

`DrawerGroup::has_content` (`panels/mod.rs:~1143`) gains matching arms for the new variants (`MediaPool`/`Effects`/`NodeEditor` always available in video mode; `ClipInspector` requires a clip selection, same pattern as today's `Modify`/`Arrange` requiring `selection_count >= 1`; `ColorControls` requires a clip selection likewise).

---

## 5. Keyboard model

### 5.1 Video-mode bindings

| Key | Action | `CommandId` |
|---|---|---|
| `Space` | Play/pause toggle | `video.play_pause` |
| `J` | Play reverse (repeat = faster) | `video.play_reverse` |
| `K` | Pause | `video.pause` |
| `L` | Play forward (repeat = faster) | `video.play_forward` |
| `←` / `→` | Step one frame back/forward (paused) | `video.step_back` / `video.step_forward` |
| `Shift+←` / `Shift+→` | Jump to previous/next clip edge or marker | `video.prev_edit_point` / `video.next_edit_point` |
| `I` / `O` | Set in-point / out-point at playhead (`work_range`) | `video.set_in` / `video.set_out` |
| `S` | Split selected clip(s) at playhead | `video.split_at_playhead` |
| `N` | Toggle snapping | `video.toggle_snap` |
| `+` / `-` | Timeline zoom in/out | `video.zoom_in` / `video.zoom_out` |
| `Shift+Z` | Zoom to fit | `video.zoom_fit` |
| `Home` / `End` | Playhead to sequence start/end | `video.playhead_home` / `video.playhead_end` |

All registered as `CommandId` entries in `commands.rs` (same table `TOOL_COMMANDS`/existing shortcuts live in) with `default: Some(KeyBinding::plain(Key::...))`, dispatched via the existing `dispatch_command`/`binding_pressed` mechanism (`app/command_center.rs:41,49`) — no parallel keyboard-handling path. Users can rebind through the existing preferences UI (`AppPreferences::resolve_binding`) with zero new plumbing.

### 5.2 Conflicts with existing vector-tool shortcuts, and the resolution rule

Three real collisions were found by inspection, not hypothesized:

1. **`Space`** is already live as canvas-pan-while-held (`app/mod.rs:4569` `space_held = ui.input(|i| i.key_down(egui::Key::Space))`, sets grab cursor and pans on drag).
2. **`S`** is already live as WASD-canvas-pan's south component (`app/mod.rs:4665` `i.key_down(egui::Key::S)` inside the WASD velocity-pan block).
3. **Arrow keys** are already live as vector-selection nudge (`app/mod.rs:4585` arrow-key nudge block, gated by `viewport_kb(ctx)`).

**Resolution rule (spec position): gate the entire existing vector-canvas input block behind `self.mode == AppMode::Vector`, and add the video bindings as a sibling block behind `self.mode == AppMode::Video`, at the same call site.** Concretely, the space-pan (`:4569-4583`), arrow-nudge (`:4585-4610`), and WASD-pan (`:4661+`) blocks all move inside (or stay inside, guarded by) an `if self.mode == AppMode::Vector { ... }` wrapper already implied by §1.1's central-panel mode branch — they are *canvas-tool* input and the video-mode canvas isn't running the tool if-chain at all, so this isn't extra work, it's the natural consequence of §1.1's branch. This resolves all three collisions by construction (mutual exclusion by mode) rather than by per-key priority hacks, and it's the only rule that scales to future key additions in either mode without re-litigating conflicts each time.

One shortcut is intentionally *not* mode-exclusive: **`Delete`** (remove clip / remove node) and **`Ctrl+Z`/`Ctrl+Shift+Z`** (undo/redo) fire identically in both modes since `CommandHistory` is unified (01 §10) — their existing bindings and dispatch are reused with zero change; only the *target* (selected clip vs. selected node) differs, resolved by which selection state is populated (§6).

---

## 6. Session state (not document state)

Per 01 §11 ("Playhead, selection, in/out *while scrubbing* → session state... work_range persists, playhead does not"), the following live on `PhotonicApp` (or its per-tab record) as plain fields, never touching `CommandHistory`:

```rust
// app/mod.rs, PhotonicApp fields (or a nested TimelineSessionState alongside `view: CanvasView`)
pub mode: AppMode,                          // §1, actually per-tab (§1)
pub timeline_view: TimelineView,            // zoom/scroll, §2.1 — per-tab
pub playhead: Tick,                          // per-sequence would be ideal; v1: per-tab, tracks active_sequence
pub timeline_selection: Vec<ClipId>,        // §2.6 — per-tab
pub timeline_snap_enabled: bool,             // persisted to AppPreferences like other UI toggles (prefs.save() pattern, `:2537`), not to the document
```

This mirrors exactly how `selected_id: Option<NodeId>` already lives on the per-tab record (`app/mod.rs` tab struct) alongside `view: CanvasView` (also session-only, also per-tab) — no new pattern, same shape extended to timeline concepts. `work_range` is the one exception explicitly called out in 01 §4 as *document* state (`Sequence.work_range`) because in/out points are meaningful export/preview bounds a user wants to persist across sessions — `playhead` is not, by the same reasoning JKL-scrubbing position isn't.

---

## 7. Risks and test hooks

- **egui perf with hundreds of clip rects.** Mitigation: (a) visible-range culling before iterating clips (binary-search the sorted `Vec<Clip>` for the scroll window, per §2.2 — never iterate off-screen clips); (b) batch clip-rect painting through `egui::Shape` vecs pushed in one `ui.painter().extend(...)` call per frame rather than per-clip painter calls, matching the existing removed-box batch-painting pattern at `app/mod.rs:4742`; (c) thumbnail/waveform textures are pre-baked at a fixed resolution independent of zoom (resampled by the GPU on blit, not regenerated per zoom level) so zoom changes never trigger cache misses.
- **Immediate-mode drag semantics.** egui recomputes widget state every frame; a multi-clip ripple/roll drag must NOT re-derive "which clips are downstream" from scratch every frame at large clip counts. Mitigation: compute the affected-clip set once at drag-start (mirrors the snap-candidate precompute in §2.5) and cache it in the coalesce-anchor's transient drag state, not recomputed per frame.
- **Mode-switch race with in-flight engine playback.** Switching `Vector → Video → Vector` while the engine is mid-play must pause playback (`EngineCmd::Pause`) as part of the exit transition (§1.2) — otherwise the engine keeps publishing `EngineFrame`s to a monitor no longer being drawn, wasting GPU/decode work. Spec position: exit-to-Vector always issues `Pause` first, unconditionally (cheap no-op if already paused).
- **Test hooks for 11-testing-phasing.md**: (a) golden-frame comparison must run in video mode with the timeline panel + program monitor both rendered (not headless-only) to catch overlay-compositing regressions the pure-engine golden tests can't see; (b) a scripted-input harness (reuse whatever drives existing GUI interaction tests, if any exist, or `egui::Context::run` with synthetic `RawInput` sequences) exercising the full intent→op table in §2.3 exists as a P2 exit criterion, since CAP-002/003 acceptance tests are specified at the pointer-input level (SPEC.md CAP-002/003 "perform via pointer input"), not just at the `ops.rs` unit level.

---

## 8. Summary of new/changed surfaces

| Surface | Change |
|---|---|
| `app/mode.rs` | new: `AppMode` enum |
| `app/mod.rs` PhotonicApp / tab struct | + `mode`, `timeline_view`, `playhead`, `timeline_selection`, `timeline_snap_enabled` fields |
| `app/mod.rs:1961` `draw()` | central-panel branch on `self.mode` (§1.1); vector canvas input blocks (`:4569`, `:4585`, `:4661`) gated to `Vector`; new bottom timeline panel registered for `Video` |
| `app/timeline/` (new dir) | `mod.rs`, `layout.rs`, `ruler.rs`, `tracks.rs`, `clips.rs`, `interact.rs`, `ops_bridge.rs` |
| `panels/mod.rs` `DrawerGroup`/`RightDrawerGroup` | + video-mode variants; `ALL` consts become `all_for_mode(mode)` |
| `commands.rs` | + `mode.*` and `video.*` `CommandId`s with default bindings |
| `welcome.rs` `WelcomeAction` | + `CreateNewVideo(VideoProjectSpec)` |
| `main.rs` `render_frame` / renderer `update()` | video-mode branch presenting `EngineFrame` in place of vector geometry pass (depends on 03's `present_engine_frame`) |

---

## 9. Open design positions (resolved, not deferred)

- **Roll vs. plain trim disambiguation** is hit-test-based (§2.4), not a modifier key — chosen because reference NLEs converge on this and it avoids a fifth modifier combination.
- **Track height drag is non-undoable** (§2.3 table). **Resolution ([39 §1.6](39-document-lifecycle.md#16-what-is-not-undoable)): move `height_px` out of `Document` into a per-document sidecar view-state file** rather than amending [SPEC.md](SPEC.md)'s "every document mutation, without exception, is undoable" constraint. The constraint is a good one and worth defending; `height_px` genuinely is not a document mutation — it is a view preference that happens to be persisted, and no user expects undo to change a track's height. Weakening an absolute rule for one field invites the next. Sequence tabs, breadcrumbs, selection and preview-zone render state belong in the same sidecar.
- **`DrawerGroup`/`RightDrawerGroup` extend in place** rather than introducing a parallel `VideoDrawerGroup` enum — chosen because `draw_drawer`'s dispatch, `has_content`, and persistence (`Serialize`/`Deserialize` on the existing enum) all already generalize to "more variants," and a parallel enum would duplicate `PropPanelCtx` plumbing for no benefit.
- **Node canvas lives in the central panel, not a drawer** (adjudicated with 08 §6.1): an egui-snarl graph canvas needs the central rect; the `NodeEditor` drawer is palette + inspector only. The central panel gains a second video-mode content state with Back-to-Timeline/`Esc` exit (§1.1 point 3).
- **Color controls in the right drawer; scopes float** (adjudicated with 07 §5): wide scope plots are the one deliberate floating-panel exception to D-02's rails-stay-rails rule; grade controls fit the right-drawer width and sit opposite the left-rail media/effects browsers, mirroring Resolve's left-media/right-inspector muscle memory.
