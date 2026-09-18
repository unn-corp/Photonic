# 43 — Gesture & chrome UI-path coverage

**Status:** Authoritative for the residual **pointer-gesture / first-run chrome**
slice that unit/MCP verbs cannot fully exercise.  
**Date:** 2026-08-06  
**Branch:** `feat/video-editor-module`

## 1. Why this exists

Residual polish on 210 / K-A10 / K-B5 / 213 landed with **core + ops_bridge +
MCP** coverage and some pure layout unit tests, but a later live-UI audit still
flagged gaps that need **structural path tests** on the real GUI entry points:

| Gap | Owning surface | Pure entry (testable without egui paint) |
|-----|----------------|------------------------------------------|
| Esc cancels in-progress mouse drag | `timeline/mod.rs` drag loop | Clear `DragState` → no `commit_drag` |
| Trim handle 12px hit (41 R-9) | `interact::hit_zone` + `EDGE_HIT_PX` | `hit_zone(..., EDGE_HIT_PX)` |
| Snap guide only when captured | `nearest_snap` + paint gate | `nearest_snap` returns `None` outside threshold |
| Fixed playhead + edge pan (K-A10) | `TimelineView` | `fixed_playhead`, `center_on_playhead`, `edge_auto_pan*` |
| Compare-effects A\|B (K-B5) | `EngineBridge::toggle_compare_effects` | flag flips + engine cmd queued |
| Social coach Import→Split→Export (213) | prefs + auto-advance on import | step machine pure helpers |
| Multi-clip vertical (210 residual) | `ops_bridge::move_clips` | already covered; keep regression |
| Gain envelope paint (210) | `keyframe_editor::paint_gain_envelope` | paint is egui-only; samples via AnimProps unit elsewhere |
| Transport J/K/L + step + home (04 §3.2) | `monitor::video_play_*` | lib `transport_tests` |
| Split / razor / snap / freeze cmds | `commands::REGISTRY` + `ops_bridge::split` / `freeze_frame` | registry + one-undo path |
| Markers + bookmarks (210) | `ops_bridge::add_marker` / `add_bookmark` | marker name + Bookmarks category seed |
| Alpha view (K-B) | `EngineBridge::toggle_alpha_view` | flag flip (GPU headless skip ok) |

Paint-only chrome (snap accent stroke, coach card layout, compare wipe divider
drag) remains visual; this document **does not** require pixel-diff of those.
It requires every **decision and mutation path** the paint code depends on to
have a failing test when the contract breaks.

## 2. Contracts

### 2.1 Esc drag cancel (210 §5)

- While a `DragState` is in egui temp memory, **Escape removes it** (and any
  marquee) and returns before `drag_stopped` can call `commit_drag`.
- Document / history are **unchanged** after Esc (no partial commit).
- Test: pure `drag_cancel_on_escape(escape, had_drag) -> cancelled` plus an
  integration that a multi-clip move only commits when `commit_drag` is invoked
  (ops_bridge already covers commit; Esc is cancel-only).

### 2.2 Trim hit target (41 R-9 / 210)

- Hit testing uses `EDGE_HIT_PX` (12), paint uses `EDGE_ZONE_PX` (6).
- Body (move) zone survives on every clip width ≥ 13px (handle capped at ⅓ width).
- Locked tracks never produce a hit.

### 2.3 Snap guide

- `nearest_snap(value, candidates, threshold)` returns the **first** candidate
  within threshold (priority order), else `None`.
- Guide paint must call only when `Some(_)` — contract on the pure function.

### 2.4 Fixed playhead / edge pan (K-A10)

- `TimelineView::fixed_playhead` toggles without undo.
- `center_on_playhead` places playhead near lane mid-x.
- `edge_auto_pan_speed` is negative at left zone, positive at right, 0 in middle.

### 2.5 Compare effects (K-B5)

- `EngineBridge::toggle_compare_effects` flips `compare_effects` and enqueues
  `EngineCmd::SetCompareEffects`.
- Command id `video.compare_effects` is registered in `commands::REGISTRY`.

### 2.6 Social coach (213)

| Event | From step | To |
|-------|-----------|-----|
| Default prefs | — | step 0, not dismissed, auto_place true |
| First clip lands / import with clips | 0 | 1 (auto) |
| User Next | 0→1, 1→2 | dismissed on Done at 2 |
| User Skip | any | dismissed |

Pure helpers live next to the coach drawer so the path cannot drift.

## 3. Deliverables

1. This spec.
2. Pure helpers for coach step transitions (if not already extractable).
3. `crates/photonic-gui/tests/gesture_chrome_paths.rs` — integration tests on
   **public** APIs (`TimelineView`, `EngineBridge`, `commands`, prefs, and any
   newly `pub` pure interact helpers).
4. Lib-unit extensions in `interact` / `layout` where `pub(crate)` is enough.
5. Green `cargo test -p photonic-gui --test gesture_chrome_paths` and
   `cargo test -p photonic-gui --lib` filters for new cases.

## 4. Non-goals

- Full egui pointer simulation / Playwright-style GUI.
- Band-5 / K-A5 group drag.
- Visual regression of coach card or wipe divider.

## 5. Definition of done

- [x] Every row in §1 has at least one automated test on the shipped function
      (`gesture_chrome_paths.rs` + existing interact/layout lib tests).
- [x] Tests fail if `EDGE_HIT_PX` regresses to paint width, if snap returns a
      miss as a hit, if compare toggle is a no-op, if coach auto-advance breaks.
- [x] Transport / split / marker / freeze / alpha daily paths locked
      (`gesture_chrome_paths` + `monitor::transport_tests`).
- [x] Live GUI re-smoke after landing (vector + AS-1 / residual MCP audit).
