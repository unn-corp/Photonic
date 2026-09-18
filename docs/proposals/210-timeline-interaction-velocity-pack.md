# 210 — Timeline Interaction Velocity Pack

> **Status: Accepted — partially implemented.**  
> Shipped: bookmarks category + `video.add_bookmark` and on-clip gain envelope
> paint (wave-1, 2026-08-03); §5 drag ergonomics checklist closed 2026-08-06.
> **Open residual: multi-clip drag-move** (§5), with group move behind K-A5
> ([194](194-k-a5-general-and-nested-clip-groups.md)).  
> CapCut-class editors (OpenCut classic) feel fast because of **on-clip
> affordances** and drag ergonomics, not because of a deeper data model.
> Photonic already owns most of the model; this pack closes the **feel** gap.
> Clean-room under [207](207-opencut-harvest-index.md) §2.

**Owner refs:**  
- [04](../specs/video-editor/04-ui-mode-timeline.md) timeline UI  
- [09](../specs/video-editor/09-audio-mixer.md) `ClipAudio.params.gain_db`  
- [15](../specs/video-editor/15-thumbnails-waveforms.md) waveforms  
- [35](../specs/video-editor/35-model-decisions.md) markers  
- K-A4 snap targets (**done**), K-A10 fixed playhead / edge pan (**done**),  
  K-A2 marker system (**done**)  

**Territory:** `panels-video` (+ optional thin `ops` for envelope edit).  
**Effort:** M. **Format impact:** none required; optional additive fields only
if bookmarks are not expressed as markers.

---

## 1. Problem and user outcome

**Today.** Photonic has:

- Waveforms and thumbnails on clip bodies (15).  
- Full marker model + panel (K-A2).  
- Snap target completeness (K-A4).  
- Fixed playhead + edge-zone auto-pan while dragging (K-A10).  
- Animatable `ClipAudio.params.gain_db` (09).  

What still feels “pro tool / incomplete CapCut” for social editors:

1. **No on-clip gain envelope** — volume automation is not painted or dragged on
   the clip body; users must find the mixer/inspector.  
2. **Bookmarks as a first-class mental model** — OpenCut exposes “bookmarks” as
   a lightweight jump list; Photonic has markers but the **quick bookmark** verb
   (one key, no category dialog) may be under-exposed.  
3. **Drag ergonomics polish** — edge pan exists; multi-select group resize feel,
   rubber-band selection of keyframes, and snap *indicator* fidelity may still
   lag CapCut-class polish (verify at impl time against 04 / 13).

**After 210.** A user can:

1. See a **gain polyline** over audio (and A/V) clips and drag handles to write
   `gain_db` keyframes — one undo unit per gesture (coalesced).  
2. Press a single shortcut to **drop a bookmark** at the playhead (implemented
   as a sequence marker in a reserved “Bookmarks” category, not a second type).  
3. Rely on edge auto-scroll / snap indicators that match 04/13 during multi-clip
   drags (regression suite + GUI paths).

---

## 2. Current state (code-oriented)

| Capability | State | Residual |
|---|---|---|
| Waveform paint | Delivered (15) | Envelope **on top of** waveform not present |
| `ClipAudio.params.gain_db` + `AnimProps` | Model + mixer | No on-clip editor |
| Markers + categories + clip markers | K-A2 done | No dedicated “bookmark” verb / filter preset |
| Edge pan / fixed playhead | K-A10 done | Multi-select resize feel TBD |
| Snap targets | K-A4 done | Indicator polish per 13 |

**Normative reuse:** do **not** invent `Bookmark` as a parallel type to
`Marker` (35 §1 already unified guides/markers). Bookmarks = markers in a
seeded category.

---

## 3. Spec — clip gain envelope

### 3.1 Paint

- On clips that have audio (`ClipAudio` present / linked A): draw a horizontal
  **gain line** in DESIGN.md accent, mapped so  
  `y = lerp(bottom, top, normalize(gain_db))` with  
  `gain_db ∈ [GAIN_FLOOR, +12]` (floor matches 09 mute floor).  
- Sample the evaluated `AnimProps` at a density of ~1 point per 4–8 screen px
  (same zoom discipline as waveforms).  
- Sit **above** the waveform fill, **below** the name label.

### 3.2 Interaction

| Gesture | Result |
|---|---|
| Hover near line | Cursor → resize-NS; show dB tooltip |
| Drag on empty segment | Insert keyframe at pointer time + set gain |
| Drag existing knot | Move keyframe value (and optionally time with Shift) |
| Double-click knot | Remove keyframe (if >0 keys); restore base if last |
| Alt-drag | Edit base gain only (no keys) — optional |

**Undo:** one `TimelineCmd` / property-track batch per pointer-down→up
(39 §1 coalescing rules). MCP already can set anim props; add
`set_clip_gain_envelope` only if the generic keyframe tools are insufficient
(prefer reuse).

### 3.3 Non-goals for envelope

- Full multi-band EQ drawing on the clip.  
- Track-level fader on the track header (separate mixer UI — 09).  
- Rubber-band audio “volume points” that are not `AnimProps` keyframes.

---

## 4. Spec — bookmarks as marker UX

### 4.1 Model

- Seed a built-in `MarkerCategory` id reserved for bookmarks
  (e.g. name “Bookmarks”, stable id in `ops::seed_marker_categories`).  
- **Add bookmark** = `AddMarker` at playhead with that category, empty or
  auto name (`Bookmark N`).  
- **Bookmarks list** = markers panel filter preset “Bookmarks only” + optional
  transport menu “Next/Prev bookmark” (may alias next/prev marker filtered).

### 4.2 Commands

| Command id | Default | Behaviour |
|---|---|---|
| `video.add_bookmark` | e.g. `B` if free | Drop bookmark at playhead |
| `video.next_bookmark` | optional | Seek to next bookmark ≥ playhead |
| `video.prev_bookmark` | optional | Seek to previous |

Defaults subject to [212](212-keymap-schema-migrations.md) if they change
shipped bindings.

### 4.3 Non-goals

- Second storage type.  
- Bookmarks that do not export as markers / chapters.  
- OpenCut-style separate “bookmark drag on ruler” if markers already cover it —
  only add if K-A2 ruler UX is insufficient (impl judgment + 13).

---

## 5. Spec — drag ergonomics checklist

Verify and close gaps (each is a checkbox for acceptance, not necessarily new
architecture):

- [~] Edge auto-scroll fires for **multi-clip** and **group** moves (194 K-A5),
  not only single-clip — **auto-scroll is not the limiter.** `edge_auto_pan` runs
  in the shared `resp.dragged()` branch keyed only on pointer x
  (`app/timeline/mod.rs`), so it is drag-kind- and selection-count-agnostic. The
  gap is upstream: **multi-clip drag-move does not exist.** `commit_drag`'s
  `DragKind::Move` arm moves `state.clip` alone, so dragging one member of a
  multi-selection moves only that member. Group moves additionally await K-A5.
  Retargeted as a residual below rather than absorbed here — it is a new editing
  capability, not drag polish.
- [x] Snap indicator line matches 13 (accent, duration of snap) — the guide was
  painted unconditionally at the moving edge, so it read as "snapped" at every
  pointer position and carried no information. Now drawn **only while
  `nearest_snap` actually captures a candidate**, at the **snap target**, in full
  accent, spanning all lanes (candidates come from other tracks, the playhead and
  markers per 04 §2.5). The trim duration readout stays anchored to the moving
  edge — 13 §1.6 makes it the non-colour-dependent confirmation, so it must
  follow the edge even when nothing snaps.
- [x] Trim handles remain hittable at minimum hit target (41) — 41 §4 rules trim
  handles to **12 × 24** ("paint 6px, hit 12 × 24"). Horizontal: new
  `EDGE_HIT_PX = 12.0` drives hit-testing while `EDGE_ZONE_PX = 6.0` stays the
  painted size. Vertical: **already satisfied, no code** — a clip rect is a track
  row tall and `ops_bridge::set_track_height` clamps that to `28.0..=240.0`.
  `hit_zone` caps the handle at a third of the clip width so widening it cannot
  swallow the body (move) zone; without that cap every clip under 24px would have
  become un-draggable. Covered by `hit_zone_honours_the_widened_trim_handle` and
  `widened_handle_never_swallows_the_body_zone`.
- [x] Vertical track reassignment while dragging shows a clear drop line — the
  ghost already rendered in the destination row, but read identically whether or
  not the drag had crossed tracks. The destination lane now takes an accent
  outline + wash while `dest_track != track`, painted under the ghost. Gated to
  `DragKind::Move`: every trim/roll/slip/slide kind commits back to `state.track`.
- [x] Esc cancels in-progress drag without partial commit — was unhandled for
  mouse drags (only the K-A7 *keyboard* grab honoured Esc). Esc now drops the
  transient `DragState`/`Marquee`, which is the whole cancel: the document is
  never touched mid-drag (only the ghost is painted) and `drag_stopped` commits
  nothing when it finds no state.

**Multi-clip drag-move — landed 2026-08-06.** Dragging any member of a
multi-selection now carries the whole selection by one delta in a single undo
step, via a new plural op `ops::move_clips`, its `ops_bridge` wrapper, and the
`move_clips` MCP verb (19 G-21's standing MCP-trail requirement).

Two things forced a plural op rather than a loop over `move_clip`:

1. **Collision.** `move_clip` validates against the *current* project, where
   every other selected clip is still in place — so a contiguous run has each
   clip refuse on the neighbour it is about to vacate. Overlap is therefore
   tested against non-moving clips only; the set is displaced as a body.
2. **Ordering.** This is 194 K-A5's finding, confirmed in code:
   `TimelineCmd::apply` debug-asserts `Sequence::validate()` after *every*
   command and `Command::Batch` applies members one at a time, so an unordered
   fan-out panics in debug on a transient overlap. Commands are emitted
   trailing-edge-first when moving later and leading-edge-first when moving
   earlier, so each clip lands in space its neighbour has already left and no
   intermediate state is ever invalid.

A blocked or before-zero move is refused **whole** — a half-applied selection is
not a state the user asked for. Link partners fold into the same moving set
rather than expanding per clip, so a partner that is also selected moves once.

**Vertical multi-move landed** via `track_delta` on `ops::move_clips` (same-kind
lane-index offset for every member; higher-lane-first apply order; GUI
`same_kind_track_delta` + MCP `track_delta`). **Residual:** group move via
[194 K-A5](194-k-a5-general-and-nested-clip-groups.md).

---

## 6. Tests

| ID | Case | Layer |
|---|---|---|
| T1 | Envelope paint samples match `AnimProps` eval at known keys | unit pure layout fn |
| T2 | Drag gesture writes one undo unit; undo restores | GUI / ops |
| T3 | Add bookmark creates marker in Bookmarks category | unit + MCP |
| T4 | Next/prev bookmark seek order | unit |
| T5 | video_ui_paths structural arm for envelope + bookmark commands | GUI paths |

---

## 7. Provenance

Ideas (on-clip volume line, lightweight bookmarks, aggressive edge-scroll)
appear in CapCut-class UIs including OpenCut classic. **Contracts above are
Photonic-native** and bind to existing Marker / AnimProps / K-A10 surfaces.

---

## 8. Delivery order inside 210

1. Bookmark verb + category seed (smallest).  
2. Gain envelope paint (read-only).  
3. Gain envelope edit.  
4. Drag ergonomics checklist fixes.
