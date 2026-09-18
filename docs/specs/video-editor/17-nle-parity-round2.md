# 17 — NLE Parity Gap-List, Round 2 (post-Wave-2 residuals + newly-surfaced gaps)

> **Status: Historical; superseded for live status.** Use [ROADMAP.md](ROADMAP.md) for inventory/priority and [19](19-editing-velocity-shot-management.md) / [20](20-pro-workflows.md) for contracts. This file preserves original research and ranking rationale.

**Depends on:** 14-nle-parity.md (round-1 backlog), 16-insert-overwrite-editing.md (3/4-point ops), 15-thumbnails-waveforms.md, 04-ui-mode-timeline.md, 01-data-model.md, 09-audio-mixer.md.
**Owns:** the *second-pass* prioritized parity backlog for the video editor's editing surfaces (timeline + program monitor + the edit grammar + the pending pro-editing spine). Supersedes 14's ranking; does **not** re-own captions/color/fusion parity except where a timeline affordance crosses into them.
**Source:** re-audit (2026-07-10) of `crates/photonic-gui/src/app/timeline/{mod,layout,ruler,tracks,clips,interact,ops_bridge}.rs`, `app/monitor.rs`, `app/command_center.rs`, `panels/video/*`, and `photonic-core/src/timeline/clip.rs`, plus fresh Premiere 2024/2025 + CapCut depth research.

> **What changed since round 1.** Wave-1/Wave-2 cleared the entire quick-win band and most of the medium/larger spine. Verified DONE in code: Delete + ripple-delete (`command_center.rs:210-211` `video.delete_clip`/`video.ripple_delete`), track-lock **enforcement** (`interact.rs:98 hit_at` rejects locked candidates + `clips.rs:657 paint_locked_hatch`), copy/cut/paste (`:212-216`), M-key marker (`:219`), monitor scrub bar (`monitor.rs:920`), header **Solo** (`tracks.rs:117-130`), sync-lock (`tracks.rs` + `clips.rs:661`), the full 3/4-point spine (`:226-229` `video.insert_edit`/`overwrite_edit`/`lift_edit`/`extract_edit`), razor click-to-split (`:230`), thumbnails+waveforms, per-clip color labels, linked A/V, fx badge, playback-res + Fit/100% zoom, and a rebindable **Keyboard Shortcuts** page (`commands.rs` + `mod.rs:859` key-capture). Those are **excluded below.** What remains is (a) round-1 residuals never built, and (b) newly-surfaced Premiere power-user affordances.

---

## How to read this

Each gap carries: **Ref** (which reference NLE ships it — **Pr** Premiere / **CC** CapCut / **DR** Resolve) · **Impact** · **Territory** (the single build-wave lane that owns it) · **Files** · **Effort** (S ≈ hours, M ≈ 1–2 days, L ≈ multi-day/needs mini-spec) · **Class** (Quick win / Medium / Larger).

**Territories** (one agent per lane in a build wave):

| Territory | Root | Owns |
|-----------|------|------|
| `core-timeline` | `photonic-core/src/timeline/` | clip/track model + pure ops (`ops.rs`, `clip.rs`) |
| `timeline-panel` | `photonic-gui/src/app/timeline/` + `app/command_center.rs` + `commands.rs` | the timeline surface, its commands, keymap |
| `monitor` | `photonic-gui/src/app/monitor.rs` | program monitor, transport, source-monitor |
| `panels-video` | `photonic-gui/src/panels/video/` | inspector, keyframe, caption, title, effect-controls docks |
| `photonic-video-engine` | `crates/photonic-video/src/` | eval/compositing/decode/time-warp |
| `photonic-mcp` | `crates/photonic-mcp/src/` | headless tool parity for new ops |

---

## Round-1 reconciliation (14-nle-parity.md)

| R1 id | Gap | Status | Evidence |
|-------|-----|--------|----------|
| QW-1 | Delete / ripple-delete | ✅ DONE | `command_center.rs:210-211` |
| QW-2 | Track-lock enforcement + hatch | ✅ DONE | `interact.rs:98`, `clips.rs:657` |
| QW-3 | Copy / cut / paste | ✅ DONE | `command_center.rs:212-216` |
| QW-4 | M-key marker | ✅ DONE | `command_center.rs:219` |
| QW-5 | Monitor scrub bar | ✅ DONE | `monitor.rs:920` |
| QW-6 | Header Solo | ✅ DONE | `tracks.rs:117-130` |
| M-1 | Per-clip color labels | ✅ DONE | (HAVE list) |
| M-2 | Linked A/V | ✅ DONE | (HAVE list) |
| M-5 | Playback-res + Fit/100% zoom | ✅ DONE | (HAVE list) |
| M-9 | Sync Lock | ✅ DONE | `tracks.rs` + `clips.rs:661` |
| L-1 | Insert/Overwrite/Lift/Extract | ✅ DONE | `command_center.rs:226-229` |
| L-4 | Thumbnails + waveforms | ✅ DONE | (HAVE list) |
| **M-3** | **Source-patch UI + target highlight** | ✅ DONE | Header patch boxes and `target_video_track` / `target_audio_track` state route Insert, Overwrite, and Paste; Paste validates kind, enabled/locked state, and occupancy before deterministic fallback (`command_center.rs::timeline_paste_clipboard`). |
| **M-4** | **Modal tool palette + cursor hints** | ⚠️ PARTIAL | razor toggle exists (`video.toggle_razor`) but no tool-mode segmented control, no per-zone cursor — **G-13** |
| **M-6** | **Navigator / horizontal scrollbar** | ❌ OPEN | `draw_mini_toolbar` has zoom/snap only — **G-8** |
| **M-7** | **Master meter beside monitor** | ❌ OPEN | grep `meter/peak/rms` clean in `monitor.rs` — **G-4** |
| **M-8** | **Effect-Controls unification** | ⚠️ PARTIAL | fx badge shipped; keyframe editor still a floating `egui::Window` (`keyframe_editor.rs:574`) split from `clip_inspector.rs` — **G-9** |
| **M-10** | **Track-select tools + wrench menu** | ❌ OPEN | no select-forward, no display popup — **G-14** |
| **L-2** | **Source Monitor / dual monitor** | ❌ OPEN | `monitor.rs` program-only; "no source monitor yet" (`command_center.rs:885`) — **G-10** |
| **L-3** | **Source in/out vs work-range semantics** | ⚠️ PARTIAL | `video.set_in/out` still write `work_range`; true source marks live only in §16's session `PendingSource` — folds into **G-10** |
| **L-5** | **Sequence tabs / multi-open** | ❌ OPEN | `draw_timeline_panel` renders single `active_sequence` — **G-17** |

**Net:** 13 of 20 round-1 items shipped after G-6; 7 residuals carry forward (renumbered G-*), joined by newly-surfaced Premiere-depth gaps.

---

## Historical ranked top-12 (original builder pick-order)

Not live queue. See [ROADMAP.md §4](ROADMAP.md#4-corrected-priority-bands).

| # | Gap | Territory | Class | Effort |
|---|-----|-----------|-------|--------|
| 1 | Add Edit to All Tracks + Close Gap (all) + Simplify Sequence | `timeline-panel` | Quick win | S |
| 2 | Keyboard trims: Q/W ripple-trim-to-playhead + E extend-edit + Shift+Q/W | `timeline-panel` | Quick win | M |
| 3 | Match Frame (F) + Reveal in Project | `timeline-panel` | Quick win | S–M |
| 4 | Master audio meter beside the program monitor | `monitor` | Quick win | M |
| 5 | Replace With Clip / Replace Edit (3-point, keeps duration+fx) | `core-timeline` | Medium | M |
| 6 | Source-patch boxes + target-track highlight in headers | `timeline-panel` | Medium | M |
| 7 | Adjustment-layer clips (create UI + composite over lower tracks) | `photonic-video-engine` | Medium | M |
| 8 | Timeline navigator / horizontal scrollbar thumb | `timeline-panel` | Medium | M |
| 9 | Effect-Controls unification (dock keyframe editor into inspector) | `panels-video` | Medium | M |
| 10 | Source Monitor (dual-monitor) + true source in/out marks | `monitor` | Larger | L |
| 11 | Speed / time-remap ramps (variable speed + on-clip rubber band) | `core-timeline` | Larger | L |
| 12 | Title / text / graphics clips + responsive design (Pin-To, intro/outro) | `panels-video` | Larger | L |

Territory spread for a 6-lane wave: `timeline-panel` ×5 (G1/2/3/6/8), `monitor` ×2 (G4/10), `core-timeline` ×2 (G5/11), `panels-video` ×2 (G9/12), `photonic-video-engine` ×1 (G7), `photonic-mcp` ×1 (G21, below the line). Lanes are file-disjoint except where noted.

---

## 1. Quick wins to do now

### G-1 — Add Edit to All Tracks + Close Gap (all tracks) + Simplify Sequence · `timeline-panel`
- **Ref:** Pr / DR. **Impact:** `video.split_at_playhead` (`command_center.rs:204`) slices only the **targeted** clip. Premiere's cut-to-the-beat move is **Add Edit to All Tracks (Ctrl+Shift+K)** — one keystroke slices every clip under the playhead. There is also no **Close Gap** (remove all gaps across all tracks in one command) and no **Simplify Sequence** (strip disabled clips / vertical gaps). Today gap removal is per-clip only.
- **Files:** `app/command_center.rs` (add `video.split_all_tracks` → iterate every unlocked track's clip under the playhead through existing `ops_bridge::split` in one `commit_batch`; `video.close_gaps` → new tiny op that left-shifts each track's post-gap clips; `video.simplify_sequence`); `commands.rs` (bind Ctrl+Shift+K); `app/monitor.rs:700-716` (keyboard-active ids). Close-Gap's shift arithmetic is a small pure op in `photonic-core/src/timeline/ops.rs`.
- **Effort:** S. **Class:** Quick win. Split + ripple primitives already exist; this is fan-out + one gap-collapse op.

### G-2 — Keyboard trims: Q/W ripple-trim-to-playhead, E extend-edit, Shift+Q/W · `timeline-panel`
- **Ref:** Pr (muscle-memory core). **Impact:** trimming is drag-only. Pros never drag — **Q** ripple-trims the clip *start* up to the playhead, **W** ripple-trims the *end* from the playhead (no gap), **E** extends the selected edit to the playhead, **Shift+Q/W** roll the previous/next cut to the playhead. Playhead edit-to-edit nav already exists (`video.prev_edit_point`/`next_edit_point` = Up/Down); the *trims* do not.
- **Files:** `app/command_center.rs` (`video.trim_start_to_playhead`/`trim_end_to_playhead`/`extend_edit`/`roll_prev`/`roll_next` — each computes a delta from the playhead and routes through the existing `ops_bridge` ripple-trim/roll wrappers `:288-398`); `commands.rs` (Q/W/E, Shift+Q/W defaults); `app/monitor.rs:700-716`.
- **Effort:** M. **Class:** Quick win. Reuses the entire trim engine; it is delta arithmetic + command wiring. **Watch-out:** respect track lock; no-op cleanly when the playhead is outside the selected clip.

### G-3 — Match Frame (F) + Reveal in Project · `timeline-panel`
- **Ref:** Pr / DR. **Impact:** no way to get from a timeline clip back to its source. **Match Frame (F)** should park the source at the exact same frame the timeline playhead is on (feeds Replace/Source-monitor); **Reveal in Project** should select+scroll the source asset in the media pool. Both are constant reference moves; grep for `match_frame`/`reveal` is clean.
- **Files:** `app/command_center.rs` (`video.match_frame` → resolve the clip under the playhead, compute source-frame = `source_in + (playhead − clip.start) * speed`, arm the source mark; `video.reveal_in_project` → set media-pool selection to `clip.source.asset()`); context menu entries in `app/timeline/mod.rs` clip menu; `commands.rs` (F). **Effort:** S–M. **Class:** Quick win (Match Frame is richest once G-10's source monitor exists, but the arm-the-source-mark half ships now and feeds G-5).

### G-4 — Master audio meter beside the program monitor · `monitor`
- **Ref:** Pr / DR / CC. **Impact:** the transport has no meter; peak/RMS live only inside the mixer drawer. A slim master meter column beside the picture is standard. `DESIGN.md` already carves the filled-meter exception but it is unwired on the monitor.
- **Files:** `app/monitor.rs` (a thin master-meter column between the letterboxed image and the right edge, fed by the same peak/RMS the mixer reads in `panels/video/audio_mixer.rs`). **Effort:** M (S if the mixer already exposes a shared master-peak snapshot). **Class:** Quick win.

---

## 2. Medium parity features

### G-5 — Replace With Clip / Replace Edit · `core-timeline`
- **Ref:** Pr. **Impact:** no "swap the shot, keep everything" affordance. Replace drops a new source into a timeline clip's **exact slot** — same duration, transitions, keyframes, effects/grade preserved (fill from Source In / Match-Frame / bin selection). Reference editors lean on this hourly; grep `replace_edit`/`replace_with` is clean.
- **Files:** `photonic-core/src/timeline/ops.rs` (`replace_clip_source(seq, track, clip, new_source, new_src_in)` — keeps `start`/`duration`/effects, rebinds `ClipSource` + `source_in`); `app/timeline/ops_bridge.rs` wrapper; `app/command_center.rs` (`video.replace_with_clip`, fill from armed source (G-3) or media-pool selection); Alt-drag-onto-clip path in `app/timeline/mod.rs` drop handler. **Effort:** M. **Class:** Medium. **Watch-out:** if the new source is shorter than the slot, decide trim-vs-hold (Pr trims to slot).

### G-6 — Source-patch boxes + target-track highlight in headers · `timeline-panel` (round-1 M-3)
**Status (2026-07-10): ✅ DONE.** Track-header patch targets are wired through Insert, Overwrite, and Paste. Video/audio clipboard entries resolve independently; invalid, disabled, locked, stale, wrong-kind, or occupied explicit targets fall back in stable timeline order. Planner tests cover mixed A/V routing and occupancy created earlier in the same paste.

- **Ref:** Pr / DR. **Impact:** `resolve_target_track` (`interact.rs:355`) already computes an *explicit patch target*, but there is **no UI to set it and no highlight** — Insert/Overwrite/Paste silently use "first enabled" every time. B-roll can't be routed to V2 without dragging. Spec 16 §4 explicitly defers this ("Target track indicator on track headers… click to set target").
- **Files:** `app/timeline/tracks.rs::draw_header` (a small V1/A1-style patch tab + highlight per kind); timeline session state `target_video_track`/`target_audio_track: Option<TrackId>`; feed it into `resolve_target_track`'s `explicit` arg from `command_center` insert/overwrite/paste paths. **Effort:** M. **Class:** Medium. Unblocks the full value of G-1/G-5 and the 3/4-point spine.

### G-7 — Adjustment-layer clips (create UI + composite) · `photonic-video-engine`
- **Ref:** Pr / DR / CC. **Impact:** the data model already has `ClipSource::Adjustment` (`clip.rs:153`) — a clip whose effects/grade apply to everything on lower tracks beneath its span — but there is **no way to create one** (all `ClipSource::Adjustment` uses are test helpers) and the compositor treats it as an empty fill (`clips.rs:527` "just leaves that fill showing"). Grading/vignetting a whole section without touching individual clips is impossible today.
- **Files:** `crates/photonic-video/src/graph/eval*.rs` (composite an adjustment clip's effect stack over the accumulated lower-track raster across its time span — the load-bearing part); `app/command_center.rs` + `app/timeline/mod.rs` menu (`video.add_adjustment_clip` at playhead on the target track); `panels/video/clip_inspector.rs` (effect stack already edits `ClipEffect`). **Effort:** M. **Class:** Medium. **Watch-out:** eval order — adjustment must see the *composited* lower tracks, not one clip.

### G-8 — Timeline navigator / horizontal scrollbar thumb · `timeline-panel` (round-1 M-6)
- **Ref:** Pr / DR / CC. **Impact:** navigation is Ctrl+scroll zoom / Shift+scroll pan / fit-button only (`mod.rs::handle_scroll_zoom`). On a long sequence with a plain mouse there is nothing to grab — no draggable horizontal thumb, no navigator strip showing viewport-in-sequence.
- **Files:** `app/timeline/mod.rs` (a bottom scrollbar/navigator drawing the viewport window over the full sequence extent, draggable body = pan, draggable ends = zoom → writes the same `view.scroll_ticks`/zoom `handle_scroll_zoom` computes). **Effort:** M. **Class:** Medium.

### G-9 — Effect-Controls unification (dock keyframe editor) · `panels-video` (round-1 M-8)
- **Ref:** Pr (single Effect Controls). **Impact:** the fx badge shipped, but the Effect-Controls mental model is split: the curve/keyframe editor is a floating `egui::Window` (`keyframe_editor.rs:574 draw_window`) while fixed Motion/Opacity/Speed live in `clip_inspector.rs`. Two surfaces for one concept.
- **Files:** `panels/video/clip_inspector.rs` (host the keyframe curve inline / as a collapsible section for the selected clip); `panels/video/keyframe_editor.rs` (offer a docked render path alongside `draw_window`). **Effort:** M. **Class:** Medium. **Watch-out:** keep the float option; don't regress the existing curve editor.

### G-13 — Modal tool palette + per-zone cursor hints · `timeline-panel` (round-1 M-4)
- **Ref:** Pr (V/A/B/C/Y/U/P/T) / CC. **Impact:** the drag-time grammar is rich (`interact.rs::resolve_drag_kind`) but **undiscoverable** — razor is the only *armed* mode (`video.toggle_razor`); there is no tool-mode segmented control and no cursor change per hovered zone (trim/roll/slip). New users can't find slip/slide/roll.
- **Files:** `app/timeline/mod.rs::draw_mini_toolbar` (segmented Select/Razor/Slip/Slide control that biases `resolve_drag_kind`); `interact.rs` (armed-mode bias); egui cursor icon per zone. **Effort:** M. **Class:** Medium. **Watch-out:** additive only — keyboard modifiers stay authoritative.

### G-14 — Track-select-forward tool + wrench/display menu · `timeline-panel` (round-1 M-10)
- **Ref:** Pr (track-select-forward / all-tracks) / all (display settings). **Impact:** marquee select exists (`interact.rs::apply_marquee`) but no "select everything from here right / on all tracks" tool, and the mini-toolbar has no wrench popup to toggle thumbnails/waveforms/track-name display now that L-4 shipped.
- **Files:** `interact.rs` (select-forward variant), `app/timeline/mod.rs::draw_mini_toolbar` (wrench popup toggling the thumbnail/waveform display flags). **Effort:** M. **Class:** Medium.

### G-15 — Proxy workflow polish: Attach Proxies + Toggle button + Ingest Settings · `photonic-video-engine`
- **Ref:** Pr. **Impact:** proxies exist in the media pool, but the *polish* is missing — **Attach Proxies** (link externally/camera-made low-res by name/timecode, no re-transcode), a **Toggle Proxies** button in the monitor transport (one click flips the whole project proxy↔full for viewing), and **Ingest Settings** (auto-create proxies on import). Grep `attach_prox`/`toggle_prox`/`ingest` is clean.
- **Files:** `crates/photonic-video/src` (attach/ingest plumbing); `app/monitor.rs` transport (a Toggle-Proxies button beside playback-res). **Effort:** M. **Class:** Medium.

---

## 3. Larger / deferred

### G-10 — Source Monitor (dual-monitor) + true source in/out marks · `monitor` (round-1 L-2 + L-3)
- **Ref:** Pr / DR (Source + Program). **Impact:** the single biggest workflow gap. `monitor.rs` is program-only; there is no surface to load a raw clip, scrub it with its **own** In/Out, and audition a sub-selection before it hits the timeline. Worse, `video.set_in/out` still write `Sequence.work_range` while the tooltips say "Set In/Out Point," so a Pr user reads them as source marks and is confused when nothing consumes them (round-1 L-3). The 3-point loop (two marks in Source, one in Timeline → Insert/Overwrite) has no home even though the ops (G's L-1) exist.
- **Files:** new source-monitor surface in `app/monitor.rs` (mode-adaptive per 04's D-02 — must fit the shell, share the transport with the program monitor); separate true source In/Out marks from `work_range`; feed §16's `PendingSource`; relabel the work-area bar. **Effort:** L. **Class:** Larger — needs its own mini-spec. Consumes G-3 (Match Frame) and G-6 (patch).

### G-11 — Speed / time-remap ramps · `core-timeline`
- **Ref:** Pr / CC (every Reels/action edit). **Impact:** `SpeedMap` is `Constant`-only (`clip.rs:192`, comment: "Keyframed speed ramps are a post-v1 non-goal"); the inspector exposes a single constant % (`clip_inspector.rs:231`). There is no variable-speed ramp — the on-clip speed rubber band with bezier easing that makes slow-mo→fast-mo smooth is absent.
- **Files:** `photonic-core/src/timeline/clip.rs` (`SpeedMap::Ramp(Vec<SpeedKey>)` + exact-rational integration over segments — the model gate); `crates/photonic-video/src/graph` (time-warped source sampling); `app/timeline/clips.rs` + a keyframe surface (on-clip white speed band, Ctrl-click adds a key, drag segments = %, split keys = ramp region, bezier handles ease). **Effort:** L. **Class:** Larger. **Watch-out:** thumbnail slicing already assumes speed (`clips.rs:893`) — extend, don't break.

### G-12 — Title / text / graphics clips + responsive design · `panels-video`
- **Ref:** Pr (Type tool + Properties + Responsive Design) / CC (text is central). **Impact:** there is **no title/text/graphics clip type creatable on the timeline** — modern editing is text-heavy and Photonic has nothing (the photo-editor text tools are unrelated). Critically, Photonic ships one-click aspect-switch + auto-reframe (HAVE), and **without Responsive-Design Position (Pin-To frame edges) reframing breaks any lower-third**, and without Responsive-Time (protected intro/outro) trimming a title destroys its entrance/exit. So this gap is amplified by an existing strength.
- **Files:** a new title/graphics editor in `panels/video/` (Type-tool-on-monitor placement, per-layer transform/appearance); a text/graphic `ClipSource` (model addition in `core-timeline`); engine render of the title clip (`photonic-video-engine`); Pin-To + intro/outro on the clip. **Effort:** L. **Class:** Larger — needs a mini-spec; touches 3 territories (own it in `panels-video`, coordinate model+render). **Watch-out:** pair Pin-To directly with the reframe engine.

### G-16 — Nested-sequence UI (nest selection, open nested) · `core-timeline`
- **Ref:** Pr / DR. **Impact:** `ClipSource::NestedSequence` exists in the model (`clip.rs:146`, cycle-checked) and renders a "Sequence" label (`clips.rs:751`), but there is **no command to nest a selection** into a sub-sequence, and `mod.rs` "Open as node composition" is a disabled stub. Core to long-form organization (act/scene per sequence).
- **Files:** `photonic-core/src/timeline/ops.rs` (`nest_selection` → new sequence, move clips in, replace with one `NestedSequence` clip, cycle-check); `app/timeline/mod.rs` menu + double-click-to-open. **Effort:** L. **Class:** Larger. Couples with G-17.

### G-17 — Sequence tabs / multiple open sequences · `timeline-panel` (round-1 L-5)
- **Ref:** Pr / DR. **Impact:** `draw_timeline_panel` renders only `active_sequence` (`mod.rs:509`); the mini-toolbar has no tab strip or even a sequence-name label. Can't keep multiple sequences open; a nested sequence (G-16) can't open as a tab.
- **Files:** `app/timeline/mod.rs` panel header (a sequence tab strip), timeline state (active-among-many). **Effort:** M–L. **Class:** Larger.

### G-18 — Text-based (transcript) editing · `panels-video`
- **Ref:** Pr 2024 signature. **Impact:** Photonic already has auto-caption infra (HAVE) — the transcript exists — but deleting a word in the transcript does **not** ripple-delete the matching clip range, and there is no filler-word ("um/uh") one-click filter. Highest value for dialogue/interview/podcast; the leap is wiring transcript spans to timeline ripple.
- **Files:** `panels/video/caption_editor.rs` (select-text → ripple map); `core-timeline` (word-span → clip-range → `extract_edit`). **Effort:** L. **Class:** Larger (nice-to-have).

### G-19 — Dedicated Trim Mode (split-screen trim monitor) · `monitor`
- **Ref:** Pr (Shift+T). **Impact:** no split outgoing/incoming trim monitor with dynamic looping playback, numeric-keypad frame offsets (`+5`/`-3`), and asymmetrical trim. A precision-trim surface young NLEs lack. **Files:** `app/monitor.rs` (a trim-mode split view driving the same ripple/roll ops). **Effort:** L. **Class:** Larger (nice-to-have).

### G-20 — Multicam · `photonic-video-engine`
- **Ref:** Pr / DR. **Impact:** no multi-camera source sequence, no audio/timecode/marker sync, no live 1–9 angle cutting in the program monitor. Genre-specific but a hard wall for that genre. **Files:** `crates/photonic-video/src` (multicam source sequence + sync); `app/monitor.rs` (multicam display + number-key cutting). **Effort:** L. **Class:** Larger (nice-to-have).

### G-21 — MCP parity for the new ops · `photonic-mcp`
- **Ref:** internal parity (CAP-019). **Impact:** MCP has no tools for G-1/G-3/G-5/G-7/G-11/G-16 (grep `match_frame`/`replace_edit`/`add_adjustment`/`nest_sequence`/`close_gap`/`add_edit` clean in `photonic-mcp/src`). Headless acceptance can't drive the new editing verbs.
- **Files:** `crates/photonic-mcp/src` (mirror each new `ops_bridge` verb as a tool). **Effort:** M (grows with the above). **Class:** Larger — track alongside whichever ops land.

---

## Solid parity worth preserving (do not regress)

Round-1's protected surfaces still hold, now joined by the Wave-2 additions: the full trim grammar (trim/ripple/roll/slip/slide, one undo each), lock enforcement + hatch, sync-lock, Solo, copy/paste, 3/4-point Insert/Overwrite/Lift/Extract, razor click-to-split, thumbnails+waveforms, per-clip labels, linked A/V, monitor scrub, playback-res + Fit/100% zoom, the aspect/reframe preset bar, and the rebindable Keyboard Shortcuts page. Treat all as protected when building the above.

---

## Bottom line

Round 1 converted a wireframe into a real NLE; round 2 is about the **pro editing spine and text era**. The residual gaps cluster in four bands: (1) **keyboard-velocity wiring** riding on shipped ops — Add-Edit-all-tracks, Q/W/E trims, Match-Frame, close-gap (Quick wins, mostly `timeline-panel`); (2) **shot-management** — Replace-edit, source-patch UI, adjustment layers, navigator, Effect-Controls unification (Medium, spread across `core-timeline`/`photonic-video-engine`/`panels-video`); (3) **the two-monitor + variable-speed + titles frontier** (Larger, mini-spec each); and (4) **MCP parity** trailing each new verb. Clear §1 first — it makes a Premiere editor's muscle memory work — then §2 gives the shot-management depth that §16's spine already earned the right to.
