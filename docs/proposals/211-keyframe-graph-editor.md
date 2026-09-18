# 211 — Timeline Keyframe Graph Editor

> **Status: Accepted and Implemented (wave-1, 2026-08-03) — largely
> pre-existing.**  
> The bezier graph editor was already shipped when this proposal was drafted;
> wave-1 extended it with the clip-audio gain row. See ROADMAP §0's OpenCut
> harvest table.  
> CapCut-class editors expose a **curve graph** for easing, not only discrete
> keyframe dots. Photonic already stores interpolation and named ease presets
> (K-B12). Clean-room under [207](207-opencut-harvest-index.md) §2.

**Owner refs:**  
- [01 §6](../specs/video-editor/01-data-model.md) `AnimProps` / `Keyframe` / `Interp`  
- K-B12 `EasePreset` (**done** — name, not stored form)  
- K-B11 keyframe interchange (**done**)  
- [13](../specs/video-editor/13-ux-components.md) UX inventory  
- [04](../specs/video-editor/04-ui-mode-timeline.md)  

**Territory:** `panels-video` (+ keyframe editor panel). **Effort:** L.  
**Format impact:** **none** if cubic handles map to existing `Interp::Bezier`
control points already in the model; additive only if the model lacks
per-keyframe cubic params (verify at impl — see §2).

---

## 1. Problem and user outcome

**Today.** Users can:

- Add/remove keyframes and pick **named** easing presets (Hold / Linear /
  CSS-like cubics via `EasePreset`).  
- Copy/paste keyframe paths (K-B11).  

They **cannot**:

- See the value curve over time as a graph.  
- Drag bezier handles to shape a custom ease between two keys.  
- Box-select and scale a group of keys in value or time.

**After 211.** Selecting an animatable property (clip transform, effect param,
gain, etc.) opens a **graph strip** (docked above the track body or in the
effect controls region) where:

1. X = clip- or sequence-local time, Y = normalized or unit value.  
2. Keys are knots; Bezier segments show handles.  
3. Dragging handles writes the same `Interp` representation the evaluator
   already uses — preview and export match.  
4. Preset buttons apply `EasePreset` names (K-B12) without storing a parallel
   curve type.

---

## 2. Current state — model check (impl must re-verify)

| Piece | Expected state |
|---|---|
| `Keyframe` + time + value | Shipped |
| `Interp` / cubic ease | Shipped (`anim.rs`) |
| `EasePreset` as name over `Interp` | Shipped K-B12 |
| Per-segment editable bezier **handles as data** | **Verify** — if only presets exist without free handles, 211 needs a **small additive** model extension for custom cubics |
| Graph UI | **Not shipped** |

**Gate for format impact:** if free handles are missing, add optional
`cubic: Option<[f32; 4]>` (or two 2D handle offsets) on the segment with
`#[serde(default)]`, migration free (39 §2.2). Prefer **not** introducing a
second animation system.

---

## 3. UI contract

### 3.1 Layout

- **Toggle:** Effect controls / clip inspector “Graph” disclosure, or timeline
  bottom strip when a keyframed property is focused.  
- **Height:** user-resizable; default ~120–160 px; collapsed = 0.  
- **Time domain:** follows timeline zoom/scroll (shared `TimelineView`) so the
  graph aligns with clip keys on the track.  
- **Value domain:** auto-fit with padding; optional lock to param
  `numericRange` from the effect manifest / prop registry.

### 3.2 Interactions

| Input | Behaviour |
|---|---|
| Click empty | Insert key at time (value = evaluated curve) |
| Drag knot | Move time/value; snap to frame / other keys (K-A4 targets where applicable) |
| Drag handle | Edit cubic; adjacent segment updates live |
| Box select | Multi-key selection; drag moves all; Scale tool optional v2 |
| Right-click | Ease preset menu (K-B12 list) + Delete + Copy/Paste (K-B11) |
| Delete/Backspace | Remove selected keys (one undo batch) |

### 3.3 Accessibility (41)

- Keyboard: select prev/next key, nudge value (↑↓), nudge time (←→), modify
  handles with modifier+arrows.  
- Hit targets ≥ 41 minimum.  
- Reduced motion: no handle spring animations.

### 3.4 Multi-property

v1: **one property curve at a time** (the focused inspector field).  
v2 (out of scope): multi-channel overlay (e.g. x/y position).

---

## 4. Undo and MCP

- Every committed knot/handle edit is **one** undo unit (coalesce during drag).  
- MCP: prefer existing `copy_keyframes` / `paste_keyframes` /
  `set_effect_param` / generic keyframe tools. Add
  `set_keyframe_interp` only if no tool can write bezier handles today.

---

## 5. Non-goals

- After-effects style graph editor with all properties stacked (v1).  
- Expression-driven curves (K-B6 param expressions stay in inspector).  
- Physics springs / overshoot presets (explicitly omitted by K-B12).  
- OpenCut React graph-editor port.

---

## 6. Tests

| ID | Case | Layer |
|---|---|---|
| T1 | Handle drag → evaluator at mid-segment matches CPU sample of cubic | unit |
| T2 | EasePreset apply matches named curve within ε | unit |
| T3 | Drag coalesce = one history entry | history |
| T4 | Graph time origin matches clip-local vs sequence-local for clip props | unit |
| T5 | Keyboard nudge path (41) | GUI paths |
| T6 | Round-trip save/load if model extended | serde |

---

## 7. Provenance

Visual keyframe graphs are industry-standard (Premiere Effect Controls,
After Effects, CapCut, OpenCut classic). Photonic’s differentiator is binding
the graph to **existing** `AnimProps` / MCP tools, not inventing a parallel
channel model.

---

## 8. Delivery slices

1. Read-only graph paint for one numeric property.  
2. Knot move/insert/delete.  
3. Bezier handles (after model verification).  
4. Preset menu + keyboard.  
5. Box select / multi-key move.
