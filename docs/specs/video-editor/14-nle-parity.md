# 14 — NLE Parity Gap-List (Timeline + Monitor vs Premiere / CapCut / Resolve)

> **Status: Superseded historical gap analysis.** Live status and contracts moved to [ROADMAP.md](ROADMAP.md), [19](19-editing-velocity-shot-management.md), and [20](20-pro-workflows.md). Preserve this file for round-one rationale; do not schedule from it.

**Depends on:** 04-ui-mode-timeline.md (timeline + monitor surfaces), 01-data-model.md (`Clip`, `Track`, `Sequence`), 09-audio-mixer.md (solo/meters), 13-ux-components.md.
**Owns:** the prioritized parity backlog for the video editor's *editing* surfaces. This doc does **not** own captions/color/fusion parity — only timeline, program monitor, and the edit grammar around them.
**Source:** synthesized from the timeline/monitor parity audit of `crates/photonic-gui/src/app/timeline/{mod,layout,ruler,tracks,clips,interact,ops_bridge}.rs` + `monitor.rs`, cross-checked against `command_center.rs`, `panels/video/*`, and `photonic-core/src/timeline/`.

> **Verified anchors (2026-07):** video command dispatch is `app/command_center.rs:160-183`; the keyboard-active id list is `app/monitor.rs:700-716`; the command-palette registry is `crates/photonic-gui/src/commands.rs`. Every timeline edit primitive already exists in `app/timeline/ops_bridge.rs` (`remove_clip` :430, `ripple_delete` :445, `split` :414, `add_marker` :593, `insert_asset_clip` :521, trim/roll/slip/slide :288-398). This is why several Tier-1 gaps are pure **wiring**, not new engine work.

---

## How to read this

Each item carries: **Ref** (which reference NLE ships it) · **Impact** (what the user loses today) · **Files** (Photonic touch-points) · **Effort** (S ≈ a few hours, M ≈ 1–2 days, L ≈ multi-day/needs design) · **Class** (Quick win / Medium / Larger).

- **Quick win** = high user-visible payoff on top of primitives that already exist. A builder can pick one, wire it, and ship in a session.
- **Medium** = a genuine new feature but self-contained; no cross-cutting redesign.
- **Larger** = new surface, new data model, or a cross-cutting workflow (3/4-point editing, source monitor, thumbnails). Needs its own mini-spec before code.

"Ref NLE has it" values: **Pr** = Premiere, **CC** = CapCut, **DR** = Resolve.

---

## Ranked top-10 (builder pick-order)

| # | Gap | Class | Effort | Why this rank |
|---|-----|-------|--------|---------------|
| 1 | Delete / Backspace removes selected clip (+ ripple-delete) | Quick win | S | Every NLE user taps Delete in the first minute; ops already exist — pure wiring. |
| 2 | Track lock actually blocks edits (+ hatching) | Quick win | S–M | Borderline bug: the padlock looks functional but does nothing. Trust-breaking. |
| 3 | Copy / Cut / Paste clips (paste at playhead) | Quick win | S–M | Universally expected; builds on `clip_for_asset`/`insert_asset_clip`. |
| 4 | M-key add marker at playhead (+ toolbar button + color UI) | Quick win | S | `add_marker` exists; only the double-click-on-24px-ruler entry point exists today. |
| 5 | Program-monitor scrub bar under the image | Quick win | S | Pr/CC both have it; reuse ruler scrub math. |
| 6 | Solo button on audio track headers | Quick win | S–M | Solo logic exists in the mixer; unreachable from the timeline header. |
| 7 | Per-clip color labels | Medium | M | Organization affordance every editor uses; needs a `Clip` field + paint + menu. |
| 8 | Linked A/V selection (clips move as a unit) | Medium | M | A/V desync is a constant footgun; default-on in Pr/CC. |
| 9 | Insert / Overwrite / Lift / Extract (3/4-point editing) | Larger | L | The spine of Pr/DR editing; wholly absent. Highest-ceiling feature. |
| 10 | Clip thumbnails + audio waveforms | Larger | L | Biggest *visual* parity gap; known P3 seam. Timeline reads as a wireframe without it. |

The rest of the backlog (source monitor, sync lock, sequence tabs, meters, etc.) sits below the line in §3.

---

## 1. Quick wins to do now

### QW-1 — Delete / Backspace removes the selected clip (+ Shift for ripple-delete)
- **Ref:** Pr / CC / DR (all). **Impact:** the *only* way to delete a clip today is the right-click menu (`mod.rs:1090` Delete / `:1094` Ripple delete). A user who selects a clip and taps Delete gets nothing.
- **Files:** `app/command_center.rs:160-183` (add `"video.delete_clip"` / `"video.ripple_delete"` arms → call existing `ops_bridge::remove_clip` / `ripple_delete`); `app/monitor.rs:700-716` (add both ids to the keyboard-active list); `crates/photonic-gui/src/commands.rs` (register palette entries + default binds Delete / Shift+Delete or Backspace). Selection source is the timeline's current clip selection (`mod.rs` selection state).
- **Effort:** S. **Class:** Quick win. Ops exist; this is command + keymap wiring. One undo step already guaranteed by `commit`.
- **Watch-outs:** respect track lock once QW-2 lands; no-op cleanly on empty selection; ripple-delete must reuse `ripple_delete`'s same-track shift semantics (`ops_bridge.rs:445`).

### QW-2 — Track lock blocks edits and shows hatching
- **Ref:** Pr / CC / DR. **Impact:** `locked` is drawn (`tracks.rs:118`) and toggled (`ops_bridge::toggle_locked` :228) but **never consumed** — `mod.rs:648 hit_at`, `mod.rs:808 start_clip_drag`, and the context menu (`mod.rs:1062`) don't check it, and `photonic-core/src/timeline/ops.rs` has no locked guard. You can move/trim/roll/slip/split/delete on a "locked" track. The control looks functional and silently does nothing.
- **Files:** `app/timeline/mod.rs` (`hit_at` :648 and `start_clip_drag` :808 must early-out when the target track's `locked`; gate the context-menu edit actions :1062-1094); `app/timeline/clips.rs::paint_lane` (add diagonal-hatch fill for locked lanes — today the only cue is a dimmer header title, `tracks.rs:148`). Lock state reads from `sequence.rs:272 Track::locked`.
- **Effort:** S–M (guard S; hatching a few lines). **Class:** Quick win — arguably a bug fix, do it alongside QW-1 so Delete honors it.
- **Watch-outs:** also block asset-drop onto locked tracks (`mod.rs:249-293`); keep the header lock toggle itself always clickable.

### QW-3 — Copy / Cut / Paste clips
- **Ref:** Pr / CC / DR. **Impact:** no clipboard at all (grep for copy_clip/paste_clip/clipboard is clean). Can't duplicate a graded/trimmed clip.
- **Files:** new small clipboard buffer on the app/timeline state (hold cloned `Clip`(s) + source track kind); `app/command_center.rs` (`"video.copy"` / `"video.cut"` / `"video.paste"`); `app/monitor.rs:700-716` + `commands.rs` (Ctrl+C/X/V). Paste places at playhead on the target track using existing `clip_for_asset`/`insert_asset_clip` construction pattern (`ops_bridge.rs:502,521`).
- **Effort:** S–M. **Class:** Quick win.
- **Watch-outs:** paste target = current target track (see M-3) or the source's track kind; snap paste to playhead frame; Cut = copy + `remove_clip` in one undo step (use `commit_batch`, `ops_bridge.rs:32`).

### QW-4 — M-key adds a marker at the playhead (+ toolbar button + color picker)
- **Ref:** Pr / CC / DR. **Impact:** marker workflow is otherwise solid — add via double-click empty ruler (`ruler.rs:269-287`), retime-drag (:188-249), rename/remove menu (:305-331) — but adding requires aiming a double-click on the 24px ruler instead of tapping M at the playhead. Marker color is read from the model (`ruler.rs:159-168`) with **no UI to set it**, and there's no Add-Marker button in the mini-toolbar.
- **Files:** `app/command_center.rs` (`"video.add_marker"` → `ops_bridge::add_marker` :593 at current playhead); `app/monitor.rs:700-716` + `commands.rs` (bind M); `app/timeline/mod.rs::draw_mini_toolbar` (:498-569, add a marker button); marker context menu (`ruler.rs:305-331`, add a color swatch that writes via the existing `set_marker_field` path :624).
- **Effort:** S. **Class:** Quick win.

### QW-5 — Program-monitor scrub bar under the image
- **Ref:** Pr (program monitor) / CC (preview). **Impact:** `draw_transport_bar` (`monitor.rs:586-681`) is buttons + timecode + toggles only; all scrubbing goes through the timeline ruler (`ruler.rs:253-267`). No draggable position bar under the picture.
- **Files:** `app/monitor.rs` (add a thin draggable position strip between the letterboxed image `:440-450` and the transport bar; drive the same shared playhead the ruler drives — `ruler.rs:253-267` is the reference for frame-snapped scrub). Reuse sequence duration for the track extent.
- **Effort:** S. **Class:** Quick win.
- **Watch-outs:** single shared playhead — writing from the monitor bar must update the same state the ruler reads, no second playhead.

### QW-6 — Solo button on audio track headers
- **Ref:** Pr / CC / DR (audio headers carry M **and** S). **Impact:** headers give video show/hide (`tracks.rs:92-101`) and audio mute (`♪`/`×`) but **no Solo**. Solo logic fully exists in the mixer drawer (`panels/video/audio_mixer.rs:106-113` solo-safe resolution) but is unreachable from the timeline.
- **Files:** `app/timeline/tracks.rs` (add an S toggle next to mute in the audio-track header branch :92-101); wire to the solo state the mixer already resolves against (`panels/video/audio_mixer.rs`; solo lives on the audio strip model `audio.rs:24`, not `Track` — `sequence.rs:270-272` is enabled+locked only). Reuse the mixer's solo-safe resolution so header + mixer stay in sync.
- **Effort:** S–M (S is the button; M if the strip↔track id mapping needs a lookup). **Class:** Quick win.

---

## 2. Medium parity features

### M-1 — Per-clip color labels
- **Ref:** Pr / CC / DR. **Impact:** clips color only by selected/normal/disabled (`clips.rs:83-91`); no assignable label for organization.
- **Files:** add an optional `color_label` to `Clip` (`photonic-core/src/timeline/clip.rs`) + a set-command in `ops.rs`/`ops_bridge.rs`; `clips.rs::paint_lane` reads it for the lane tint; context menu (`mod.rs:1062`) adds a label submenu (preset swatches).
- **Effort:** M. **Class:** Medium. **Watch-outs:** label must not override the selected/disabled visual states; persist in the doc model.

### M-2 — Linked A/V selection (clips move together)
- **Ref:** Pr / CC (default-on). **Impact:** clips are fully independent (grep "linked" is clean); dragging a video clip (`mod.rs` `DragKind::Move`) never carries its paired audio, and there's no linked-selection toggle. Editors constantly desync A/V.
- **Files:** a `link_group`/paired-id concept on `Clip` (`clip.rs`) set at import when a media file yields both A+V; `mod.rs` drag/commit path (`start_clip_drag` :808, `commit_drag` :963-1060) moves the linked partner in the same `commit_batch`; a Linked-Selection toggle in `draw_mini_toolbar` (:498-569) to temporarily break the link (Alt-drag to override, Pr-style).
- **Effort:** M. **Class:** Medium. **Watch-outs:** trim/ripple/roll must decide whether they propagate across the link (Pr: trim is independent, move is linked); keep undo a single step.

### M-3 — Source patching / target-track indicators
- **Ref:** Pr / DR. **Impact:** no V1/A1 patch boxes, no target-track highlight in the header column (`tracks.rs::draw_header`). Needed the moment Insert/Overwrite (L-1) land, and useful now as the paste target for QW-3.
- **Files:** `app/timeline/tracks.rs` (a target-track marker + highlight in the header), a `target_track` per kind on timeline state. **Effort:** M. **Class:** Medium (do with L-1). 

### M-4 — Modal edit tools / tool palette + cursor + Razor
- **Ref:** Pr (V/A/B/C/Y/U/P/H/T) / CC. **Impact:** the drag-time grammar is rich (`interact.rs::resolve_drag_kind`: Alt=Slip, Alt+Shift=Slide, Shift=Ripple-trim, flush-edge=Roll, edge=Trim, body=Move) but **undiscoverable** — no Tools panel, no cursor change per mode, and Razor is S-key/context-menu only (`mod.rs:398`/`:1084`). The mini-toolbar only shows a passive "Ripple" label while Shift is held (`:557-568`).
- **Files:** `app/timeline/mod.rs::draw_mini_toolbar` (add a tool-mode segmented control incl. a clickable Razor that arms split-on-click, calling `ops_bridge::split` :414); `interact.rs` (let an armed mode bias `resolve_drag_kind`); cursor hints via egui cursor icon per hovered zone.
- **Effort:** M. **Class:** Medium. **Watch-outs:** don't break the existing modifier grammar — the palette is an *additive* discoverability layer, keyboard modifiers stay authoritative.

### M-5 — Playback-resolution + image-zoom selectors on the monitor
- **Ref:** Pr / DR (playback res) / all (Fit/100%/200%). **Impact:** proxy is plumbed engine-side (`monitor.rs:334 apply_proxy_mode`) but there's no user-facing Full / 1/2 / 1/4 dropdown and no Fit/100%/200% image zoom (image always letterboxes, `:440-450`).
- **Files:** `app/monitor.rs` (two dropdowns in the monitor header near the aspect bar `:547-582`; wire res → `apply_proxy_mode`, zoom → the letterbox transform at `:440-450`). **Effort:** S–M. **Class:** Medium.

### M-6 — Horizontal scrollbar / zoom navigator
- **Ref:** Pr / DR / CC. **Impact:** navigation is Ctrl+scroll zoom, Shift+scroll pan, plain scroll vertical (`mod.rs::handle_scroll_zoom` :571-605) + fit/±buttons (:512-544). No draggable horizontal thumb, no navigator strip. On a long sequence with a plain mouse there's nothing to grab.
- **Files:** `app/timeline/mod.rs` (a bottom scrollbar/navigator drawing viewport-in-sequence with draggable ends → sets the same view offset/zoom `handle_scroll_zoom` computes). **Effort:** M. **Class:** Medium.

### M-7 — Master audio meter beside the monitor
- **Ref:** Pr / DR / CC. **Impact:** transport has no meter (`draw_transport_bar` :586-681; grep meter/peak/rms clean in monitor+timeline). Meters exist only inside the mixer drawer. `DESIGN.md:165` even carves out the filled-meter exception but it isn't wired here.
- **Files:** `app/monitor.rs` (a slim master meter column beside the program image, fed by the same peak/RMS the mixer reads). **Effort:** M. **Class:** Medium.

### M-8 — Effect-Controls unification + fx badge on clips
- **Ref:** Pr (single Effect Controls). **Impact:** clips show transition triangles (`clips.rs:119-124`) and keyframe diamonds (`mod.rs:170-179` via `keyframe_editor::paint_clip_automation`) but no generic "has effects/grade" fx dot, and the Effect-Controls model is split across two surfaces: the curve/keyframe editor is a floating `egui::Window` (`keyframe_editor.rs:574`) while fixed Motion/Opacity live in `clip_inspector.rs`.
- **Files:** `clips.rs::paint_lane` (fx badge when a clip has effects/grade); longer-term, dock the keyframe editor into the inspector drawer to present one Effect-Controls mental model. **Effort:** M. **Class:** Medium (badge alone is S — could be pulled into §1).

### M-9 — Sync Lock
- **Ref:** Pr / DR. **Impact:** header has only enable + lock (`tracks.rs`); no sync-lock. Ripple ops shift only the same track (`ops_bridge::ripple_trim` :308, `ripple_delete` :445) — the "keep these tracks in sync during ripple/insert" concept doesn't exist. Grep sync_lock clean.
- **Files:** `sequence.rs Track` (add `sync_lock`), `tracks.rs` header toggle, ripple/insert ops shift all sync-locked tracks in one batch. **Effort:** M. **Class:** Medium (couples with L-1). 

### M-10 — Track-select tools + per-timeline display (wrench) menu
- **Ref:** Pr (track-select-forward/all-tracks) / all (display settings). **Impact:** marquee select exists (`interact.rs::apply_marquee`) but no track-select-forward/all-tracks tool, and the mini-toolbar has no wrench menu to toggle thumbnails/waveforms/track-name display (`mod.rs:498-569`).
- **Files:** `interact.rs` (select-forward variant), `mod.rs::draw_mini_toolbar` (wrench popup — becomes meaningful once L-4 thumbnails/waveforms exist). **Effort:** M. **Class:** Medium.

---

## 3. Larger / deferred

### L-1 — Insert / Overwrite / Lift / Extract (3/4-point editing)
- **Ref:** Pr / DR (the spine of their editing). **Impact:** editing is drag-from-media-pool only (`mod.rs:249-293` → `insert_asset_clip`). No Insert, Overwrite, Lift, or Extract anywhere (grep clean). Mark source in/out → target a track → Insert/Overwrite is the core reference workflow and is wholly absent. Depends on L-2 (source in/out) + M-3 (target track).
- **Files:** new commands in `command_center.rs` + `commands.rs`; new ops in `photonic-core/src/timeline/ops.rs` (insert = ripple-open + place; overwrite = place over; lift = remove leave-gap; extract = ripple-remove); `ops_bridge.rs` wrappers. **Effort:** L. **Class:** Larger — needs its own mini-spec. **Prereqs:** L-2, M-3, and clarifying the I/O semantics (L-3).

### L-2 — Source Monitor / dual-monitor layout
- **Ref:** Pr / DR (Source + Program). **Impact:** `monitor.rs` is program-only (`draw_video_monitor`, header :1-2). No surface to load, preview, and mark in/out on a clip before it hits the timeline. A Premiere user's entire pre-edit workflow has no home.
- **Files:** new source-monitor surface (mode-adaptive per 04's D-02 constraint — must fit the existing shell, not a new screen); shares transport with the program monitor. **Effort:** L. **Class:** Larger — prereq for L-1.

### L-3 — Fix I/O semantics: source in/out vs work-range
- **Ref:** Pr. **Impact:** the transport "I"/"O" buttons (`monitor.rs:664-669`) call `video_set_in`/`video_set_out` → `set_work_range_bound` (:267-304) → `ops_bridge::set_work_range` — a render/preview work-area (Pr's *work area bar*), **not** the In/Out marks that drive 3-point editing. Tooltips say "Set In/Out Point," so a Pr user reads them as source marks and is confused when no Insert/Overwrite consumes them.
- **Files:** `monitor.rs:664-669` + `:267-304` (separate source in/out marks from work-range; likely relabel work-range and add true source marks feeding L-1/L-2). **Effort:** M. **Class:** Larger (semantic redesign; do with L-1/L-2).

### L-4 — Clip thumbnails + audio waveforms
- **Ref:** Pr / CC / DR. **Impact:** the single biggest *visual* parity gap. `clips.rs:3-5` and the `// P3 seam` at `:132-133` state it: clips paint as a rounded rect + name label only. Reference clips show head/tail thumbnails (video) and waveform envelopes (audio); the Photonic timeline reads as a wireframe by comparison.
- **Files:** `clips.rs::paint_lane` (thumbnail strip + waveform render), a thumbnail/peak cache (decode head/tail frames + precomputed audio peaks), engine hooks to produce them. Gated behind the wrench menu (M-10). **Effort:** L. **Class:** Larger (known P3 seam) — needs a caching/decoding mini-spec.

### L-5 — Sequence tabs / multiple open sequences
- **Ref:** Pr / DR. **Impact:** `draw_timeline_panel` (`mod.rs:44`) renders only `active_sequence`; the mini-toolbar (`:498-569`) has no tab strip or even a sequence-name label. Can't keep multiple sequences open; nested sequences can't open as a tab (`mod.rs:1108` "Open as node composition" is a disabled P8 stub).
- **Files:** `mod.rs` timeline panel header (a sequence tab strip), timeline state (active-among-many). **Effort:** M–L. **Class:** Larger.

---

## Solid parity worth preserving (do not regress)

The **drag-time trim grammar** and **transport** are at or above reference quality — treat these as protected surfaces when implementing the above:

- JKL shuttle w/ speed ramping + pause (`monitor.rs:197-222`), Space, frame-step, Home/End, Loop (`draw_transport_bar` :586-681).
- Current/total timecode readout, accent monospace (`monitor.rs:650-661`).
- Safe-area guides action-90% / title-80% (`monitor.rs:737-756`).
- Zoom in/out + zoom-to-fit, Ctrl+scroll zoom-to-cursor (`mod.rs:340-358`, `handle_scroll_zoom`).
- Snap toggle w/ drag-time snap guide + candidate priority same-track→other-track→playhead→markers (`mod.rs:547-554`, `interact.rs::build_snap_candidates`).
- Full trim vocabulary — trim, ripple-trim, roll, slip, slide, cross-track move, marquee — each one undo step (`interact.rs` + `commit_drag` `mod.rs:963-1060`).
- Track header rename/lock/enable/height-drag/add-remove-reorder (`tracks.rs`).
- Media-pool drag-drop w/ frame-snapped insertion caret (`mod.rs:249-293`).
- One-click aspect/reframe preset bar above the monitor — a CapCut-flavored extra (`monitor.rs:547-582`, `ops_bridge::ASPECT_PRESETS`).
- Empty-state affordances in both surfaces + first-run shortcut sheet (`mod.rs::draw_empty_affordance`, `monitor.rs:508-524`, `:784-828`).

---

## Bottom line

The trim grammar and transport are reference-grade. The gaps cluster in three bands: (1) **wiring debt** — Delete, copy/paste, marker-at-playhead, monitor scrub, header solo, and the lock-enforcement bug are all quick wins riding on primitives that already exist; (2) **self-contained features** — color labels, linked A/V, tool palette, meters, navigator; (3) **the pro editing spine** — 3/4-point editing + source monitor + true source in/out, plus the visual-parity workhorse of thumbnails/waveforms. Clear §1 first: it converts a wireframe-feeling editor into one that behaves the way a Premiere/CapCut user expects within the first minute.
