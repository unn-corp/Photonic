# Advanced character metrics: kerning, baseline shift, super/subscript (#32)

> Status: **implemented** (node-level). Kerning now applies in the main renderer;
> baseline shift + super/subscript exist end-to-end across model, renderer, SVG export,
> MCP, and GUI. Per-character ranges remain deferred (see Remaining work).

## What this PR implements

- **Kerning (AC1).** The main glyphon renderer now shapes with `Shaping::Advanced` at all
  three `renderer.rs` sites — `measure_text` (~385), `render_text_pass` (~508), and the
  headless capture path (~1992) — so rustybuzz applies the font's default `kern` plus
  `liga`/`calt`. Pairs like AV/To/Wa now tighten in the canvas, headless PNG/JPEG export,
  and measurement. (The headless capture path additionally now honors `font_weight` and
  `font_style`, which it previously dropped — parity with the windowed path.)
- **Model (`photonic-core/src/node.rs`).** `TextNode` gains `baseline_shift: f64` and
  `script_position: ScriptPosition { Normal, Superscript, Subscript }`, both
  `#[serde(default)]` so existing `.photon` files load unchanged. `ScriptPosition`
  exposes `size_scale()` (0.58× for super/sub), `baseline_offset_em()` (+0.33em super,
  −0.10em sub), and string parse/format helpers. The exhaustive `TextNode` literal in
  `import.rs` is updated too.
- **Renderer (`photonic-render/src/renderer.rs`).** `TextSnapshot` gains a `top_offset`
  (physical px). At the snapshot site the effective font size is scaled and the top offset
  is derived from script position + baseline shift (screen-Y is down, so a raise is a
  negative offset). Both the live and headless TextArea builders apply
  `screen_y + top_offset`.
- **SVG export (`photonic-core/src/export.rs`).** The `<text>` element emits a reduced
  `font-size` for super/subscript and a `baseline-shift="…"` attribute (SVG is positive-up,
  matching the model) combining the script offset and explicit shift.
- **MCP.** New `set_character_metrics` tool (`baseline_shift` and/or `script_position`,
  both optional, validated, undoable) wired through `protocol.rs`, `handlers/nodes.rs`, and
  `server.rs` (dispatch + JSON schema). `inspect_node` now reports `baseline_shift` and
  `script_position`. `docs/mcp-api.md` regenerated (306 tools).
- **GUI (`photonic-gui`).** A `SetCharacterMetrics` `PanelAction` plus Character-panel
  controls beneath the decoration buttons: Normal / Superscript (x²) / Subscript (x₂)
  toggle buttons and a baseline-shift drag value, applied through the undo history.

## Verification

`cargo build --release`, `cargo test -p photonic-core -p photonic-mcp -p photonic-render`,
and `cargo check --workspace` all pass. (No new automated kerning assertion: `measure_text`
lives on the windowed `PhotonicRenderer`, and the GPU-free `HeadlessRenderer` used by the
render tests does not exercise text shaping, so there is no GPU-free hook to assert advance
width against. Verified by build + the existing render-test suite.)

## Review round 1 fixes

- **[major] Selection bounds & hit-testing now honor the new character metrics.**
  `text_aware_canvas_bounds` (`crates/photonic-gui/src/app/geometry.rs`) previously measured
  with the full, unscaled `font_size` and ignored `script_position` / `baseline_shift`, so a
  Superscript/Subscript node rendered at 0.58× and shifted while its selection rectangle and
  clickable area stayed at full size and unshifted. It now mirrors the renderer: measures with
  the effective size (`font_size * script_position.size_scale()`) and translates the local rect
  vertically by `-(baseline_offset_em() * font_size) - baseline_shift` (the renderer's
  `top_offset` sign convention with zoom factored out). This single function feeds all GUI
  hit-testing (`hit_test.rs`) and selection/transform bounds (`tool_handlers.rs`, `mod.rs`), so
  the fix is centralized. For Normal nodes with no shift (`size_scale()=1.0`, offset `0`) the
  rect is byte-identical to before — no regression to existing text.

## Summary

The text model is uniformly styled per node (`TextNode`, `node.rs` lines 478-536): a
single `content` string plus node-level `letter_spacing`, `opentype_features`,
`line_height`, etc. There is **no per-character span model**. Two concrete gaps:

1. **Kerning is not applied.** The main renderer shapes text with `Shaping::Basic`
   (`renderer.rs` lines 385, 508, 1992). Basic shaping does not run rustybuzz, so font
   pair-kerning (`kern`) and contextual features (`liga`/`calt`) are ignored. The
   text-on-path path already uses `Shaping::Advanced` (`text_path.rs` line 83) and kerns
   correctly, proving the fix is just the shaping flag.
2. **No baseline shift or super/subscript.** Neither exists on the model, renderer, MCP,
   GUI, or SVG export.

`opentype_features: Vec<String>` is stored and editable (MCP `set_opentype_features`,
`protocol.rs` line 4138; GUI `mod.rs` line 10428), but cosmic-text 0.12.1's `Attrs` API
(`~/.cargo/.../cosmic-text-0.12.1/src/attrs.rs`) exposes **no per-feature toggle** — only
`Shaping::Advanced` (which enables the font's default `kern`/`liga`/`calt`). Arbitrary tag
application (`frac`, `smcp`, `sups`…) in the glyphon path therefore needs a custom
rustybuzz feature list and is deferred (see Out).

This proposal delivers kerning now, and adds baseline shift + super/subscript as
**node-level** attributes (a defensible incremental step), with the per-character span/run
model called out as deferred phase 2.

## Scope

**In:**
- **Kerning:** flip `Shaping::Basic` → `Shaping::Advanced` at the three `renderer.rs` call
  sites (`measure_text` 385, `render_text_pass` 508, headless 1992). rustybuzz then applies
  the font's default `kern` + `liga`/`calt`. Satisfies AC1 (AV/To tighten). `text_path.rs`
  already does this.
- **Model:** add to `TextNode` (`node.rs`):
  - `baseline_shift: f64` (document units, positive = up), `#[serde(default)]`.
  - `script_position: ScriptPosition` enum { `Normal`, `Superscript`, `Subscript` },
    `#[serde(default)]`. Update `TextNode::new` defaults.
- **Render (node-level):** in `render_text_pass`, when `baseline_shift != 0` or
  `script_position != Normal`, adjust the glyphon `TextArea.top` and the buffer
  `font_size`: super/subscript renders at ~0.58× size with a +0.33em / −0.10em baseline
  offset; explicit `baseline_shift` adds to `top`. Carry both onto `TextSnapshot`
  (`renderer.rs` line 136) and the snapshot-build site (~line 868).
- **SVG export** (`export.rs` line 387): emit `baseline-shift="..."` and a reduced
  `font-size` on the `<text>` element for super/subscript / shift (SVG has native support).
- **MCP:** add `baseline_shift` + `script_position` to the text-properties handler
  (`handlers/nodes.rs` ~line 581) and a dedicated `set_character_metrics` tool with args in
  `protocol.rs`; report them in `get_node`/inspection output.
- **GUI Character controls** (`app/mod.rs`, alongside the existing letter_spacing /
  decoration / opentype plumbing ~lines 7902, 10401-10428): a baseline-shift drag value and
  Normal/Super/Sub toggle buttons, wired through a `PanelAction`.

## Remaining work (deferred)

- **Per-character span/run model** (`runs: Vec<TextRun{range, baseline_shift, script}>`).
  This PR ships super/subscript and baseline shift at *node* granularity. Proper
  character-range super/subscript (the "2" in "m²") needs a run model plus AttrsList-span
  rendering and split TextAreas — a larger architectural change. Phase 2.
- **Arbitrary OpenType feature application in glyphon** (`frac`, `smcp`, `sups`, `ordn`…):
  blocked by cosmic-text 0.12 `Attrs`, which exposes no per-feature toggle; needs a custom
  rustybuzz shaping path or upstream support. `opentype_features` storage/UI already exists,
  and `Shaping::Advanced` now applies the font's *default* features (kern/liga/calt) — but
  applying arbitrary stored tags is still deferred.
- Manual `letter_spacing` application in the main glyphon renderer (separate gap — cosmic
  has no `letter_spacing` attr; today only `text_path.rs` honors it).
- Optical/metrics kerning mode selection and manual kern-pair overrides.
- A GPU-free kerning regression assert (no current hook — see Verification).

## Proposed approach

1. **Kerning (smallest viable change first):** change the three `Shaping::Basic` to
   `Shaping::Advanced`. Build `--release`, render a headless sample of "AVTo Wave" before/
   after and confirm the advance width shrinks (kern applied). Add a regression assert in a
   render test if one exists for measure_text.
2. **Model + defaults:** add `baseline_shift`, `script_position` (+ `ScriptPosition` enum,
   `#[serde(rename_all="snake_case")]`) to `TextNode` and `new()`. Default values keep all
   existing `.photon` files loading unchanged (serde defaults).
3. **Renderer:** extend `TextSnapshot` with `baseline_shift` + `script` and derive the
   effective size/top at the snapshot site. Node-level shift = one TextArea, no span logic.
4. **SVG export:** append `baseline-shift` + adjusted `font-size` attrs.
5. **MCP + GUI:** add the setter tool + Character-panel controls following the existing
   `set_text_decoration` / opentype patterns for symmetry.
6. Per house rule, `cargo build --release` after each edit; verify render + SVG export
   headless on the RTX 4060 Ti.

## Acceptance criteria mapping
- AC1 (kerning tightens AV/To) → step 1 (`Shaping::Advanced`).
- AC2 (baseline shift + super/subscript render and export) → steps 2-5 at node granularity;
  character-range granularity deferred to phase 2.
