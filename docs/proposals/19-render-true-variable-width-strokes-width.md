# Render True Variable-Width Strokes (#19)

> Status: **implemented** (on-canvas + headless rendering). SVG export retains
> the average-width fallback — a true outlined-`<path>` export is tracked as a
> follow-up (see *Remaining work* below).

## What this PR implements

- `Stroke::width_profile_id: Option<Uuid>` (`style.rs`) links a stroke to a
  `WidthProfile` in `Document::width_profiles`. Additive + `#[serde(default)]`,
  so existing `.photon` documents load unchanged.
- `tessellate_stroke_variable(path, widths)` (`tessellator.rs`) flattens the
  path, offsets each vertex by the linearly-interpolated half-width along its
  normal, and triangulates a filled ribbon — true varying width, not the average.
- The renderer (`renderer.rs`) resolves the profile in the doc-lock read phase
  (`NodeSnapshot::stroke_widths`) and dispatches to the variable tessellator in
  `append_stroke`, scaling samples to match stroke-align width doubling. Uniform
  strokes (no profile) take the original path unchanged.
- `apply_width_profile` (MCP, `handlers/document.rs`) now sets `width_profile_id`
  in addition to the legacy average `width`, so applying a profile renders with
  real variable width immediately.
- Unit tests in `tessellator.rs` cover linear sampling and ribbon geometry
  (start span ≈ first sample, end span ≈ last sample).

### Remaining work (follow-up)

- SVG export of a variable-width stroke as an outlined `<path>` via
  `outline_stroke` (SVG has no native variable-width stroke). Currently exports
  `stroke-width` = average as a documented fallback.
- Asymmetric per-side widths and cubic interpolation for calligraphic smoothness.

---

> Original design scaffold follows.

## Summary

`WidthProfile` (`document.rs:256-281`) stores width samples at even-t intervals along a
path and is looked up by ID from `Document::width_profiles`. The `average_width()` helper
(`document.rs:275`) collapses the profile to a single scalar, which is what the renderer
currently uses. `tessellate_stroke` in `tessellator.rs:308` accepts only a uniform `width: f32`,
so variable-width art looks flat and the Width Tool cannot show real output. This proposal
makes the tessellator consume per-segment widths.

## Scope

**In:**
- Consume `WidthProfile::widths` in `tessellate_stroke` to produce a variable-width
  outline (offset by interpolated half-width on each side).
- Support the existing data shape: a `Vec<f64>` of width samples at uniform t-intervals.
- Implement a clean interpolation model (linear between samples, clamped to min width ε).
- `Stroke::width` remains the fallback when no profile is applied.
- `outline_stroke` (`crates/photonic-core/src/ops/stroke_outline.rs`) must also honor
  variable widths when expanding a stroke to a fill.
- SVG fallback: export as an outlined `<path>` (not `stroke-width`), since SVG has no
  native variable-width stroke.

**Out:**
- Asymmetric per-side widths (left ≠ right) — data model extension, defer.
- GUI Width Tool interaction (M1 milestone, separate issue).
- Brush pressure mapping / Wacom input (separate).

## Proposed Approach

1. **Link profile to `Stroke`:** add an optional field to `Stroke` in `style.rs`:
   ```rust
   pub width_profile_id: Option<uuid::Uuid>,
   ```
   When `Some(id)`, the renderer looks up the profile in `Document::width_profiles` and
   passes the interpolated samples to the tessellator instead of using `stroke.width`.

2. **New tessellator entry point:** add to `tessellator.rs`:
   ```rust
   pub fn tessellate_stroke_variable(
       path: &PathData,
       widths: &[f64],       // sampled at uniform t
       cap: LineCap,
       join: LineJoin,
       miter_limit: f32,
   ) -> Mesh
   ```
   Walk the path's parametric segments, evaluate `t` at each vertex position, interpolate
   the half-width (`w(t)/2`) linearly from adjacent samples, and offset the path's normal
   vectors on each side. Produce a closed outline polygon tessellated by lyon.

3. **`build_geometry` snapshot:** extend `NodeSnapshot` (`renderer.rs:703`) with:
   ```rust
   stroke_widths: Option<Vec<f64>>,  // None = uniform, Some = variable
   ```
   In the doc-lock read phase, look up `path_node.stroke.width_profile_id` in
   `doc.width_profiles` and clone the `widths` vec if found.

4. **Dispatch in draw loop:** in `build_geometry`, if `stroke_widths` is `Some`, call
   `tessellate_stroke_variable`; otherwise keep the existing `tessellate_stroke` call.

5. **`outline_stroke` op** (`ops/stroke_outline.rs`): add a `widths: Option<&[f64]>`
   parameter that, when `Some`, produces the variable-width expanded path instead of a
   fixed-width expansion.

6. **SVG export** (`export.rs`): when `width_profile_id` is set, expand the stroke to a
   filled `<path>` outline using `outline_stroke` rather than emitting `stroke-width`.

## Affected Modules

- `crates/photonic-core/src/style.rs` — `Stroke`: add `width_profile_id` field
- `crates/photonic-core/src/document.rs` — `WidthProfile` (already complete)
- `crates/photonic-render/src/tessellator.rs` — `tessellate_stroke_variable`
- `crates/photonic-render/src/renderer.rs` — `NodeSnapshot`, dispatch in draw loop
- `crates/photonic-core/src/ops/stroke_outline.rs` — variable-width expansion
- `crates/photonic-core/src/export.rs` — SVG outlined path fallback

## Risks & Open Questions

- **Interpolation quality:** linear interpolation between samples can produce kinks at
  tightly curved path segments. May need cubic interpolation for smooth calligraphic
  profiles.
- **lyon tessellator API:** `tessellate_stroke` currently uses lyon's
  `StrokeTessellator`; variable-width offsets require either a custom polygon or
  lyon's `path_offset` approach. Need to prototype which is simpler.
- **End-cap geometry:** variable-width strokes need caps that taper rather than cap at
  a uniform width; the cap shape at t=0 and t=1 depends on the profile value there.
- **Round-trip:** `.photon` serializes `Stroke` — adding `width_profile_id` is
  additive and backward-compatible with `#[serde(default)]`.

## Acceptance Criteria

- [ ] A stroke with a non-uniform `WidthProfile` renders with visibly varying width
      on-canvas and in headless export.
- [ ] `average_width()` is no longer the rendering fallback (only used as a legacy hint).
- [ ] SVG export produces an outlined `<path>` that matches on-canvas appearance.
- [ ] Round-trips through `.photon` save/load without data loss.
- [ ] Uniform strokes (no `width_profile_id`) are unaffected.

## Effort Estimate

**M** — tessellator change is the core; the extra plumbing in renderer + export is straightforward.
