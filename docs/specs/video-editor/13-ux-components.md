# 13 — UX Components: Video Editor Module

> **⚠ Corrections, 2026-07-20.** This document is in ROADMAP tier 2 (normative) and is heavily cited from code — 30+ `13 §N` references across `panels/video/` and `app/` — so its stale sections mislead implementers who are visibly reading it as the contract. The 2026-07-20 remediation pass corrected 00–11 and **skipped this document**. Audited findings:
>
> - **§12.4 mandates `egui-snarl`; the code evaluated and rejected it.** `node_editor.rs:24-30` records the reason — only egui-snarl 0.5 targets egui 0.29, and *"it wants to own the graph and mutate it in place, which fights the rule that the core `NodeGraph` is authoritative and every edit flows through `graph_ops` → undo."* `snarl` is in no `Cargo.toml`; the canvas is hand-drawn. This was a **correctness decision protecting undo integrity**, not a library preference — §12.1/§12.4 should adopt the hand-drawn canvas as the design and drop the "fallback" framing.
> - **§14's Format Tab Strip was not built.** The shipped surface is a fixed-preset "Frame" bar over a hardcoded `ASPECT_PRESETS` constant (`ops_bridge.rs:86-93`), where a click means *activate-or-add*. There is no per-format enumeration, no "+" picker, no custom w×h, and **no remove affordance** — so §14.3's guard for the last/active format guards an operation that does not exist. Two shipped presets (4:3, 21:9) appear in neither §14 nor [05 §4.1](05-import-export.md).
> - **§5.1 restricts Speed to `SpeedMap::Constant`.** Both the model (`clip.rs:333`) and the shipped UI (`clip_inspector.rs:297` — "Speed ramp (variable speed)") have ramps. This is [27 A-5](27-spec-audit.md#a-5--p2--10s-set_clip_speed-row-contradicts-the-shipped-code) surviving in a second document, and it is the same failure: telling a reader a shipped capability is unavailable.
> - **§13.3 cites `EngineCmd::CancelExport`, which is not in the enum** (`session.rs:144-178`). [27 SD-7](27-spec-audit.md#3-sd---spec-versus-code-drift) named only doc 02; cancel runs through the job registry.
> - **The inventory is incomplete**, despite §0 claiming "every net-new GUI surface". Missing: the **keyframe/curve editor** (`keyframe_editor.rs`, three surfaces — floating, docked, timeline-lane paint), and it is the module's **third** custom-drawn 2D drag control where §9.4 names only two · **Titles drawer** · **Source Marks drawer** · **Timeline Navigator** (`draw_navigator`) · **Multicam** and **Transcript** drawers. The last three are **empty-bodied stubs with rail icons**, reachable by a user and rendering nothing — this document needs an empty-state contract for a reachable-but-unimplemented drawer, which is exactly what a component inventory exists to catch.
> - **§16 findings 2, 3 and 4 are resolved** and still filed as open (eyedropper `GradeQualifier`, the node-port socket colours now in DESIGN.md, and the two named fill exceptions). Note finding 3 was resolved **against this document's own recommendation** — it proposed a muted blend and the implementation reused `success` green. Findings 5, 6 and 7 remain genuinely open; finding 6 is [27 U-8](27-spec-audit.md#5-u---under-specified-contracts).
> - **Two `MC-*` gaps were assigned here and have no section.** Error surfaces now have a contract in **[36](36-error-model.md)**; accessibility now has one in **[41](41-accessibility.md)**. This document should gain the corresponding surface-level sections or ROADMAP should reassign them explicitly.


**Status:** Draft 0.1. **Depends on:** DESIGN.md (repo root — tokens cited below), 01-data-model.md, 02-engine.md, 04-ui-mode-timeline.md (panel-map authority, §4.1), 05–09 (per-domain interiors). **Scope:** every net-new GUI surface the video module introduces. Does not restate vector-mode UI (unchanged) or engine/data-model contracts (01/02 own those) — this doc is the component inventory an implementer builds from and a UX reviewer audits against.

Every component below extends DESIGN.md's existing token set (dark "Deep Violet" theme, `sm`=3px/`lg`=8px rounding, flat no-shadow floating-card idiom) — no new colors, no new rounding values, no new elevation mechanism. Where a component needs a state color not yet in DESIGN.md (e.g. a "recording/buffering" pulse), it is called out explicitly as a DESIGN.md addition, not invented ad hoc.

---

## 0. Conventions used below

- **Owning spec doc** — which of 04–10 is normative for this component's data/behavior; this doc owns only the UI layer.
- **egui construction** — cites the exact existing idiom to reuse (file:line where traceable), per DESIGN.md's "Do reuse, don't invent a third toggle idiom" rule.
- **States** — every component lists default/hover/active/disabled/error/loading only where that state is reachable; omitted states don't apply (e.g. a static ruler has no "disabled").
- **A11y** — egui's accessibility story is limited (no OS-level a11y tree in v1 per repo precedent); notes here are keyboard-reachability and color-contrast, not screen-reader semantics, since no other Photonic panel commits to the latter either.

---

## 1. Timeline Panel

**Purpose:** the primary multi-track editing surface — arrange, trim, split, and inspect clips against time. **Owning spec doc:** 04 §2 (layout/interactions), 01 §4–5 (Track/Clip data), 09 §8.1 (waveform rendering), 06 §4 (caption lane interactions).

### 1.1 Anatomy

Two-column grid inside a bottom-docked `egui::TopBottomPanel` (04 §1.1, registered only in `AppMode::Video`): fixed-width **track-header column** (left, default 160px, splitter-resizable) + scrollable **clip-lane area** (right). A shared **ruler strip** (~24px tall) sits above the lanes, spanning the full lane width — one zoom/scroll value for the whole panel (04 §2.1).

| Subcomponent | Detail |
|---|---|
| **Ruler** | Tick marks at a zoom-adaptive interval (seconds/frames), current-timecode readout, marker diamonds (`Sequence::markers`, 01 §4) at their `at: Tick` position, click-to-seek. |
| **Track headers** | Per track: enable/hide (video) or mute (audio) toggle icon, lock toggle icon, name label (double-click to rename), height-drag handle at the row's bottom edge, solo button (audio only, mirrors mixer strip solo). Add Track / Remove Track buttons anchor the column's bottom. |
| **Clip lane area** | One horizontal lane per `Track`, height = `track.height_px`. Clips render as rounded rects (`sm` rounding) filled `surface-elevated`, selected state adds `primary` 1px border + `primary-dim` fill wash. |
| **Clip rect anatomy** | Name label (clipped to width), thumbnail strip (video/image clips, sampled per 04 §2.2), waveform (audio clips, min/max envelope + RMS overlay per 09 §8.1), transition badges (small triangular overlay at edges where `transition_in`/`transition_out` is `Some`), 6px trim-handle hit zones at each edge (invisible until hover). |
| **Playhead** | A full-height vertical line (accent `primary`) spanning ruler + all lanes, draggable from the ruler, arrow-key/JKL-steppable (04 §5.1). |
| **Snapping indicators** | When a drag is within snap threshold of a candidate (clip edge / other-track clip edge / playhead / marker, priority order per 04 §2.5), a thin accent-colored guide line flashes at the snap target for the duration of the snap; magnet icon in the panel's mini-toolbar toggles snapping on/off (`N` key). |
| **Caption lane** | One lane per `CaptionTrack` (06 §4), cues render as rects with cue text preview; word-level boundaries show as thin internal tick marks on hover/select, not always-on (avoids visual noise at low zoom). |
| **Panel mini-toolbar** | Docked at the panel's top-left, above track headers: zoom-to-fit button, snap toggle, ripple-mode indicator (reflects whether Shift is held, live). |

### 1.2 States

| Element | States |
|---|---|
| Clip | default / hover (edge cursor changes to resize on trim-zone hover) / selected (accent outline) / multi-selected (same outline, all members) / disabled (`clip.enabled == false`, reduced opacity ~50%) / offline media (diagonal-stripe placeholder pattern, 01 §3) / dragging (semi-transparent ghost + snap guides) |
| Track header | default / locked (lock icon filled, row content dimmed + non-interactive) / disabled-hidden (video) or muted (audio) |
| Playhead | idle / scrubbing (cursor = grab, timecode readout updates live) |

### 1.3 Interactions

Pointer + keyboard table is normative in 04 §2.3/§2.4/§5.1 (click/drag/trim/ripple/roll/slip/slide/split/snap/JKL/step) — this doc doesn't restate it, only the **visual feedback per interaction**: drag shows a ghost rect + snap guide; trim shows a resize cursor + live duration tooltip; split shows a preview cut line at the playhead before commit; ripple/roll/slip/slide each show a distinct cursor glyph (four-arrow / roll-arrows / slip-arrows / slide-arrows) so the active modifier is legible without a HUD.

### 1.4 egui construction

- Panel shell: `egui::TopBottomPanel::bottom` per 04 §1.1, frame recipe matching the drawer-card idiom (`surface` fill, 1px `border` stroke, `lg` rounding on the top two corners only — mirrors the left-rail's "round only the outward corners" pattern at `app/mod.rs:2746`).
- Clip rects: batch-painted via `ui.painter().extend(...)` in one call per frame (04 §7 perf mitigation), not per-clip `ui.add` calls — matches the existing removed-box batch pattern at `app/mod.rs:4742`.
- Track header rename: inline `egui::TextEdit` on double-click, same idiom as layer rename in `panels/layers_panel.rs`.
- Clip context menu: `Response::context_menu(...)` — established idiom (`panels/layers_panel.rs:177,519`, `panels/history.rs:468`) — offering Split / Delete / Add Transition In/Out / Enable-Disable / Open as Node Composition / Detach Audio (04 §2.6).
- Splitter (track-header column width): reuse whatever drag-splitter idiom exists for `SidePanel` resizing (`resizable(true).min_width().max_width()`), not a custom drag handle.

### 1.5 Design tokens

`surface-elevated` clip fill, `primary`/`primary-dim` selection, `border` for lane separators, `secondary` for muted track-header text, `label-section` typography for the mini-toolbar labels, `sm` rounding on clip rects, `mono-data` typography for the ruler timecode and duration tooltips (numeric readouts get the monospace family per DESIGN.md's Typography section).

### 1.6 Accessibility

Every clip/track-header control must be reachable via Tab (existing egui focus order) since JKL/arrow/Shift+arrow are pointer-adjacent, not global-keyboard substitutes for selection. Snap-guide flash must not be the *only* signal — the moving edge's own position readout (tooltip) is the non-color-dependent confirmation. Transition badges and offline-media stripes must read at 100% zoom without color (shape-only), since `error`/`warning` red/amber are not colorblind-safe as sole indicators — this repeats DESIGN.md's "tint text/icon, never fill the row" rule at object scale.

---

## 2. Transport Bar

**Purpose:** playback control overlay on the program monitor. **Owning spec doc:** 04 §3.2.

### 2.1 Anatomy

A slim `egui::Area` or bottom-aligned `ui.horizontal`, floating along the bottom edge of the canvas rect — **not** a separate panel (04 §3.2, preserves D-02's rect budget). Left-to-right: play/pause toggle, step-back/step-forward buttons, loop toggle, in/out marker buttons, current-timecode readout (`HH:MM:SS:FF`, `mono-data`), work-range scrubber (a thin horizontal strip below the button row showing in/out ticks + playhead position, click/drag to seek).

### 2.2 States

Play/pause button swaps glyph (not label) on toggle. Loop toggle uses the rail-icon-button selected idiom (`primary-dim` fill + `primary` border when active) at a smaller inline size. Buffering: the whole bar dims slightly and a small spinner glyph appears beside the timecode when `EngineStatus` indicates no frame is yet available (04 §3.1) — the monitor image itself holds the last-presented frame, so the bar's spinner is the only "something is loading" signal.

### 2.3 Interactions

Space = play/pause (also a global keybinding, 04 §5.1). Click-drag on the scrubber strip seeks live (coalesced seek requests per 02 §4). In/out buttons set `work_range` at the playhead (`I`/`O` keys mirror them).

### 2.4 egui construction

Icon buttons at rail-icon-button scale (30×30, `sm` rounding) but laid out horizontally in a floating `Area`, not the vertical rail idiom — this is the one place a horizontal icon strip is correct (transport controls are universally left-to-right in every NLE; don't force the vertical rail pattern here). Timecode readout: `RichText` with `mono-data` typography, `on-surface` color, no background (floats directly over the monitor image — needs a semi-opaque backing strip, `surface-base` at ~85% alpha, so it stays legible over bright footage; this is a DESIGN.md-consistent extension of the flat/no-shadow rule using alpha instead of elevation).

### 2.5 Design tokens

`surface-base` at reduced alpha for the bar backing, `primary` for the active loop-toggle border, `mono-data` for timecode, `warning` (amber) tint on the buffering spinner glyph only (not a fill).

### 2.6 Accessibility

Play/pause must remain reachable via Space regardless of pointer focus (already true per 04 §5.1's global binding). Buttons need `.on_hover_text()` tooltips (existing app-wide convention, e.g. `panels/toolbar.rs`) since transport icons alone are not self-explanatory to a first-time user.

---

## 3. Program Monitor Overlays

**Purpose:** non-destructive viewing aids drawn over the canvas-as-monitor. **Owning spec doc:** 04 §3.1/§3.3, 05 §4.4/§4.5.

### 3.1 Anatomy

All overlays are egui-drawn directly over the `ui.image(...)` monitor widget (04 §3.1), never baked into the rendered frame:

| Overlay | Detail |
|---|---|
| **Safe-area guides** | Two toggleable rectangles: action-safe (~90% of frame) and title-safe (~80%), thin outline strokes, on by default for non-square formats (05 §4.5). |
| **Letterbox/pillarbox bars** | Solid-fill bars (`surface-base`, matching the window's own black-adjacent tone — video letterboxing is conventionally black, and `surface-base` at `#07070B` is near-black, so it reads correctly without a special-cased "true black" token) when sequence aspect ≠ canvas-rect aspect (05 §4.1). |
| **Reframe handles** | On-canvas transform handle set (position/scale/rotation) — reuses the existing vector-canvas selection-transform-handle widget verbatim, parametrized to edit `ClipTransform` instead of a `SceneNode` transform (04 §3.3). Same handle glyphs, same drag behavior, same accent-colored handle dots. |
| **Buffering spinner** | Small centered or corner-anchored spinner glyph shown only when no `EngineFrame` is yet available and no prior frame exists to hold (04 §3.1's "hold last frame" rule means this is rare — first-open-of-a-sequence only). |
| **Mobile-frame chrome** | Phone-frame decoration (notch/rounded-corner/safe-area guide) drawn around the monitor image when the mobile-preview toggle is active (05 §4.4) — decoration only, zero engine cost. |

### 3.2 States

Safe-area guides: on/off per independent toggle (action-safe and title-safe toggle separately). Reframe handles: only visible when a clip is selected AND the active format has (or can have) a `reframe` entry — otherwise absent, not disabled-grayed (matches the existing vector-canvas convention of hiding rather than disabling selection handles with no selection).

### 3.3 Interactions

Reframe handle drag writes `clip.reframe[active_format]` live, coalesced per gesture (05 §4.2) exactly like a vector-object transform drag. Safe-area/mobile-frame toggles live in the monitor's own small toolbar strip (top-right corner of the canvas rect, unobtrusive — not the transport bar, which is playback-only).

### 3.4 egui construction

Guide rectangles and letterbox bars: `ui.painter().rect_stroke`/`rect_filled` directly on the `CentralPanel`'s `Ui`, positioned via the same `view.canvas_to_screen` mapping the vector canvas already uses (04 §3.3 explicitly mandates reuse, not reinvention). Reframe handles: literal reuse of the existing handle-drawing function, parametrized over "what am I editing."

### 3.5 Design tokens

Safe-area guide stroke: `secondary` at reduced opacity (visible but recessive — a guide, not a selection). Reframe handle dots: `primary` (matches every other selection-handle accent in the app). Letterbox bars: `surface-base`. Buffering spinner: `secondary` with a `primary` accent sweep (standard spinner treatment, not a new token).

### 3.6 Accessibility

Safe-area toggle state must be discoverable without trial-and-error — label the monitor toolbar icons with hover text ("Action-safe guide", "Title-safe guide"), matching the app-wide tooltip convention.

---

## 4. Media Pool

**Purpose:** import, browse, and manage project media. **Owning spec doc:** 05 §1–2. **Left-rail drawer group:** `MediaPool` (04 §4.1, first in `DrawerGroup::VIDEO_ALL`).

### 4.1 Anatomy

Standard left-rail drawer card (160–420px, per DESIGN.md's drawer-card component). Interior:

- **Bin tree** — left-aligned mini-tree (mirrors the existing layer-panel tree affordance, 05 §2.1), flat list with parent refs rendered indented.
- **Grid/List toggle** — small segmented control (two `selectable_label`s or icon-button pair) at the drawer's top, persisted as a session preference.
- **Grid view** — thumbnail-forward cards, each showing: thumbnail (or diagonal-stripe offline placeholder), duration badge (bottom-right corner overlay), kind icon (top-left corner overlay for audio/vector/LUT assets that have no visual thumbnail), status badge (Offline/Proxy Building, top-right).
- **List view** — metadata-forward rows: Name, Kind, Duration, Resolution, Frame Rate, Codec, Channels, File Size, Status, Bin — sortable columns (05 §2.1), matches the app's existing sortable-grid idiom (`audit_panel`'s header-grid pattern, `panels/mod.rs:1423`).
- **Search + kind-filter chips** — text filter bar (same `TextEdit` + clear-button idiom as the drawer's shared property-search bar, `panels/mod.rs:1224`) plus toggle chips for Video/Audio/Image/Vector kind filtering.
- **Import affordance** — "Import Media…" button at the drawer's top, drag-drop accepted anywhere in the panel body.

### 4.2 States

Per-row/card status cycles through: **Importing** (spinner, greyed thumbnail) → **Probing** → **Indexing** (video only) → **Ready** → optionally **Proxy Available**/**Proxy Building** → **Offline** (red badge + diagonal-stripe thumbnail) → **Relinking** (dialog open) — derived states per 05 §2.4, never manually set. Multi-select (shift/ctrl-click, matches layer-panel convention) drives batch actions.

### 4.3 Interactions

Drag onto a bin (reparent), drag onto the timeline (add to pool + insert clip, 05 §1.1). Context menu (`Response::context_menu`) per row: Reveal in Finder/Explorer, Generate Proxy/Remove Proxy, Replace Media, Duplicate, Delete from Pool, Convert/Compress…, Copy Content Hash (05 §2.3). Offline row → right-click "Relink…" opens a file picker (05 §2.2).

### 4.4 egui construction

`egui::Grid` for list view (matches `draw_audit_panel`'s header/row-grid pattern verbatim). Grid-view thumbnail cards: a wrapped flex layout (egui doesn't have native CSS-grid wrap; use `ui.horizontal_wrapped()` with fixed-size thumbnail buttons). Status badges: small `RichText` overlays positioned via `Ui::put` at a corner offset within the card's `Rect`, colored per the status-color rule (tint only, never fill — DESIGN.md Do's/Don'ts).

### 4.5 Design tokens

`surface-elevated` for card/row background, `error` tint for the Offline badge text/icon, `warning` tint for Proxy Building, `success` tint for Ready-with-proxy (a genuinely new use of `success` — first component in the app to need a persistent "good" status color beyond the audit log's transient rows; confirm this addition lands in DESIGN.md's `colors.success` entry, already reserved there).

### 4.6 Accessibility

Grid-view thumbnails need hover tooltips carrying the full filename (thumbnails alone don't disambiguate two similarly-thumbnailed clips) — same rule as any icon-only control elsewhere in the app.

---

## 5. Clip Inspector

**Purpose:** selected clip's transform/speed/effects-stack/transition params. **Owning spec doc:** 04 §4.1 (`ClipInspector` drawer group — "mirrors today's `Inspector` group but for `Clip`/`ClipEffect` instead of `SceneNode`"), 01 §5–6 (param registry).

### 5.1 Anatomy

Left-rail drawer, requires a clip selection (`has_content` gated, mirrors `Modify`/`Arrange`'s `selection_count >= 1` rule, 04 §4.1 closing note). Sections as `egui::CollapsingHeader` blocks (exact idiom `panels/inspector.rs` already uses for Transform/Fill/Stroke/Effects — reuse verbatim, don't invent a new foldout widget):

- **Transform** — position/scale/rotation/anchor/opacity fields, each with a small keyframe-diamond toggle beside it (adds/removes a `PropertyTrack` entry, 01 §6) — same visual language the node editor's inspector uses (§12.5 below), one keyframe-indicator convention app-wide.
- **Speed** — ratio field (`SpeedMap::Constant`, 01 §5.1); reversed-speed shown as a negative-ratio toggle, not a separate "reverse" checkbox (matches the data model's single `Ratio` field).
- **Reframe** — per-active-format transform readout + "Reset reframe for this format" button (05 §4.2); a small format-index chip row shows which formats already have overrides (dot indicator per format, filled if overridden).
- **Effects stack** — ordered list of applied `ClipEffect`s (drag-to-reorder, matches the layer-panel's drag-reorder idiom, #169 precedent cited in `panels/mod.rs`), each row: enable toggle, effect name, expand-to-params chevron, remove button. Params render via the same generic `PropPath`-driven widget builder the vector Inspector already uses for effect params.
- **Transitions** — `transition_in`/`transition_out` summary (kind + duration), edit opens a small inline params block; "None" state shows an "Add Transition" affordance instead of an empty section.

### 5.2 States

Every numeric field: default / hover (drag-to-scrub cursor, matches existing vector-property drag-scrub convention) / keyframed (diamond filled solid vs. hollow-outline when not) / animated-and-varying-at-current-tick (diamond gets a thin `primary` ring — "this value is being interpolated right now," distinct from "this property has keyframes somewhere on the timeline"). Effects stack row: enabled/disabled (dim + strikethrough-adjacent treatment, not literal strikethrough — matches disabled-clip opacity convention).

### 5.3 Interactions

Drag-to-reorder effects (pointer drag on the row, drop indicator line between rows). Click keyframe diamond: add/remove keyframe at playhead. Numeric field drag-scrub + type-to-edit, matching every existing vector property field.

### 5.4 egui construction

`egui::CollapsingHeader` per section (`panels/inspector.rs` idiom, verbatim reuse). Effects-stack reorder: same drag-and-drop primitive the Layers panel's #169 reorder feature uses — do not build a second drag-reorder implementation.

### 5.5 Design tokens

`label-section` for CollapsingHeader titles, `primary` for the "actively animating" keyframe ring, `secondary` for disabled-effect row text.

### 5.6 Accessibility

Drag-to-reorder needs a keyboard fallback (up/down buttons or arrow-key-while-focused) since pointer-only reorder locks out keyboard-primary users — flagged here as a requirement, not yet resolved by any cited existing idiom (see Findings §16).

---

## 6. Effects Browser

**Purpose:** catalog of available clip effects, drag-to-apply. **Owning spec doc:** 04 §4.1 (`Effects` drawer group), 08 §2 (shared `EffectKind` catalog).

### 6.1 Anatomy

Left-rail drawer: search-filterable list grouped by family (matches the node editor's palette grouping for consistency — Filters / Keys / Generators, a subset of 08 §6.2's full node-family list since the effects browser only surfaces `EffectKind`-backed, non-graph effects). Each entry: icon + name + one-line description (hover tooltip for the full description, matching the vector tool-palette's `tool_row` pattern exactly — `tool_row(ui, active, tool, chosen)` at `panels/tools_panel.rs:103`).

### 6.2 States

Entry: default / hover / dragging (ghost chip follows cursor) / disabled (an effect incompatible with the current selection's clip kind — e.g. `ChromaKey` on an audio-only clip — grayed, hover text explains why, never hidden, matching the rail-icon "disabled not hidden" convention).

### 6.3 Interactions

Drag onto a selected clip in the timeline OR onto the Clip Inspector's effects-stack section (both are valid drop targets, matching how most NLEs accept either). Double-click as a keyboard/no-drag fallback: applies to the current clip selection directly (mirrors the tool-palette's click-not-drag convention for the common case).

### 6.4 egui construction

Reuse `tool_row`'s exact structure (`section_header` + `selectable_label`-style row + hover tooltip, `panels/tools_panel.rs:91-112`) for the list; egui's native drag-source/drop-target primitives (`egui::DragAndDrop` or manual pointer-payload state, matching 04 §2.3's Media-Pool-to-timeline drag mechanism) for the drag path.

### 6.5 Design tokens

Section headers: `label-section` dim-muted style (`section_header()` idiom, `#50506E`). Disabled entries: `secondary` text at further-reduced opacity.

### 6.6 Accessibility

Double-click-to-apply (§6.3) is the accessibility fallback for the drag-only path — document it as load-bearing, not incidental, since drag-and-drop alone excludes switch/keyboard-only users.

---

## 7. Caption Track Lane + Caption Style Editor

**Purpose:** view/edit caption cues inline on the timeline; edit style cascade (track/cue/word). **Owning spec doc:** 06 §4 (editing UX), 06 §5 (rendering semantics the style editor must preview accurately), 01 §7 (data model).

### 7.1 Anatomy — track lane (embedded in §1 Timeline Panel)

Already covered structurally in §1.1's "Caption lane" row; this section owns its **editing** anatomy: inline text editor (double-click or Enter-on-selected-cue opens a token-per-word overlay directly on the cue rect — not a separate modal, keeping the edit surface spatially anchored to the timeline), split-point insertion (click between word tokens while in inline-edit mode), cue-edge and word-edge retime handles (small drag grips at cue/word boundaries, visually distinct from clip trim handles — thinner, positioned at the caption lane's own vertical center rather than full-height).

### 7.2 Anatomy — style editor panel

A distinct panel (not the caption lane itself) reachable from a selected cue/word/track: cascade-scope selector (Track / Cue / Word segmented control, 06 §4's `StyleTarget`), then fields: font family, size, weight, fill color (swatch button, 26×26 idiom from DESIGN.md's Components section), stroke color + width, background (color + corner-radius + padding, toggleable), karaoke-highlight mode picker (FillSweep / WordPop / Underline, `selectable_label` trio) + active/inactive color swatches, animation picker (None / FadeWords / SlideUp / Typewriter), position (drag handle on the monitor, mirrors reframe handles' on-canvas-drag pattern) + max-width. "Clear override" button per field when editing Cue/Word scope, showing the resolved fallback value ghosted when cleared.

### 7.3 States

Cue: default / selected / editing (inline token overlay open) / has-style-override (small dot indicator on the cue rect, distinguishes "this cue has its own style" from "inherits track style" — helps a user spot outliers). Word token (in inline-edit mode): default / being-retimed (drag in progress, live duration readout). Style field: default / overridden-at-this-scope (label gets a filled dot) / inherited (label shows the resolved value in `secondary` tone, "clear" button absent since there's nothing to clear).

### 7.4 Interactions

Table is normative in 06 §4 (inline edit / split / merge / retime cue / retime word / style cascade). This doc adds: the inline word-token overlay uses click-to-place-cursor + type, matching a normal text field's feel despite being drawn over a timeline rect rather than in a `TextEdit` widget — implementer should still back it with `egui::TextEdit` state where possible (custom-position `TextEdit` via `Ui::put`) rather than hand-rolling text-input from scratch.

### 7.5 egui construction

Cascade scope selector: three `selectable_label`s in a row (exclusive-choice idiom, DESIGN.md Components). Karaoke-mode picker: same. Fill/stroke/background swatches: `crate::color_popup::ColorPopup::swatch_f32` (the exact widget the rail's persistent fill swatch already uses, `app/mod.rs:2816`) — one color-swatch component reused for every color field the video module introduces, not a new picker.

### 7.6 Design tokens

Override-indicator dot: `primary`. Inherited-value text: `secondary`. Style editor sections: `CollapsingHeader` per field group (Font / Fill & Stroke / Background / Karaoke / Animation / Position), same foldout idiom as the Clip Inspector.

### 7.7 Accessibility

Karaoke color pairs (active/inactive) must be checked for contrast against typical caption backgrounds — flagged as a content-authoring concern the UI should warn about (a low-contrast active/inactive pair is a user choice, not a bug, but a soft warning akin to the export dialog's disk-space guardrail is reasonable scope).

---

## 8. TTS/Voiceover Panel

**Purpose:** generate spoken audio from typed text. **Owning spec doc:** 06 §6.

### 8.1 Anatomy

A mini-panel (06 §6 calls it exactly that) — reachable from the timeline's audio-track context or a dedicated entry point in the `MediaPool`/`ClipInspector` drawer (placement not fixed by 06; recommend surfacing it as a section within the `MediaPool` drawer, since its output becomes a media-pool asset, consistent with "Media Pool first" being the entry point for populating a project, 04 §4.1). Contents: multiline text box, voice picker (dropdown populated from `TtsProvider::voices()` — never hardcoded, 06 §2.1), a generically-built param panel from each voice's `ParamSpec` list (float sliders / enum dropdowns, built at runtime — the UI must not assume a fixed param set), "Also caption this voiceover" checkbox, Generate button, target-track selector.

### 8.2 States

Generate button: default / disabled (empty text or no provider configured — hover text explains which) / generating (spinner + cancel affordance, `ProviderProgress` states surfaced as short status text: "Uploading…", "Processing…") / done (success toast + the panel resets for the next generation, doesn't stay in a "done" frozen state). A previously-generated clip selected on the timeline shows a "Regenerate" variant of this panel (pre-filled with the clip's original text/voice/params, 06 §6's `TtsCmd::Regenerate` path).

### 8.3 Interactions

Generate submits the job; panel stays interactive (non-blocking, matches export's non-modal progress convention, 05 §3.8 step 8). Cancel button tied to the job's `CancelToken`.

### 8.4 egui construction

Param panel: same generic `PropPath`/param-builder pattern as effect params (§5) — the voice's `ParamSpec` list is structurally identical in shape (key/label/kind/range/default) to an `EffectKind` registry entry, so reuse the same widget-building function rather than writing a second one.

### 8.5 Design tokens

Generate button: standard `sm`-rounded button, `primary-dim`/`primary` on hover-active per the theme's own widget-hover rule (no special treatment needed — it's a normal action button). Progress status text: `secondary`, `body-sm`.

### 8.6 Accessibility

Voice picker must show voice names, not opaque IDs (06 §2.1's `VoiceDescriptor.name` exists exactly for this) — never surface a raw provider voice ID in the UI.

---

## 9. Color Controls Drawer

**Purpose:** grade a selected clip — wheels, curves, HSL qualifier, LUT. **Owning spec doc:** 07 §5 (UI), 07 §1–4 (data/math). **Right-rail drawer group:** `ColorControls` (04 §4.1, requires a clip selection).

### 9.1 Anatomy

Right-rail drawer (220–480px). Ordered sections (mirrors 07 §4.4's default op order — WhiteBalance → Exposure → Contrast → primary CDL/Wheels → secondaries → Curves → LUT — so the UI's visual order teaches the underlying signal-flow order):

- **Grade op stack header** — `Grade.bypass` toggle (keyboard `D`, 07 §5) + before/after split toggle, pinned at the drawer's top (always visible regardless of which op section is expanded).
- **Op list** — ordered, drag-reorderable (same drag-reorder idiom as the Clip Inspector's effects stack, §5.1) rows, each a `CollapsingHeader`: per-op enable toggle, kind label, expand to reveal that op's controls, remove button, "Add mask" affordance (power-window mask, §9.1.1 below).
- **Wheels widget** (`GradeOpKind::Wheels`) — three circular lift/gamma/gain dials, Resolve-style: drag within the disc for hue/sat offset, drag radius for luminance offset, numeric readout beside each dial, double-click a dial to reset to neutral.
- **Curve editor** (`GradeOpKind::Curves`) — draggable spline control points over a live histogram backdrop (reads the pre-curve luma histogram from the scopes compute pass, §10 below), per-channel tabs (RGB / R / G / B / Hue-Hue / Hue-Sat) as a `selectable_label` row, snap-to-grid toggle.
- **Qualifier picker** (`GradeOpKind::HslQualifier`) — eyedropper button (reuses `eyedropper_btn` verbatim, `panels/mod.rs:1347` — identical icon/tooltip/Esc-cancel convention as every other eyedropper in the app) sampling off the program monitor to seed hue/sat/lum center; hue/sat/lum range sliders + softness slider; "Highlight" toggle previews the isolated matte (white=qualified/black=excluded) in place of the graded image.
- **LUT browser** (`GradeOpKind::Lut3d`) — thumbnail grid scanning the configured LUT folder + a "recently used" strip, drag-drop onto the op stack creates a `Lut3d` op, intensity slider (default 100%).
- **Power-window mask editor** — shape toggle (Ellipse/Rectangle), on-canvas position/size/rotation handles (same handle widget as reframe, §3.1 — a third reuse of the one transform-handle primitive), softness + invert controls inline in the drawer.

### 9.1.1 Wheels widget detail

The one genuinely new interactive widget this module introduces (no existing egui or Photonic precedent for a 2D-drag-within-a-disc control). Spec: a circular disc (`surface-widget` fill, `border` outline), drag anywhere within it maps radial position to hue (angle) + saturation (distance from center) offset, a draggable ring at the disc's edge maps to luminance offset (drag up/down or a thin peripheral ring-slider — implementer's choice, both are Resolve-precedented). Center dot marks neutral; a thin line from center to the current drag position shows the live offset vector. Numeric readout (three small fields: the effective lift/gamma/gain triplet) beside each disc for precise entry, since drag-only precision is coarse.

### 9.2 States

Op row: enabled/disabled (dim, matches effects-stack convention), being-edited (expanded), has-mask (small mask-shape glyph badge on the row). Wheels dial: default / dragging (live numeric readout updates) / neutral (center dot only, no offset line drawn — visually confirms "this wheel does nothing right now"). Qualifier "Highlight" toggle: on/off, matches rail-icon selected-state visual (`primary-dim` fill).

### 9.3 Interactions

Eyedropper: click activates, next canvas click samples and seeds the qualifier, Esc cancels (identical to every existing eyedropper flow, `EyedropperTarget` pattern in `panels/mod.rs` — this is a strong argument for literally extending that enum with a `GradeQualifier` variant rather than building a parallel eyedropper mechanism; flagged in Findings §16). Curve control points: drag to move, double-click to add, right-click (or Delete while selected) to remove — standard curve-editor convention, no existing Photonic precedent to cite since the app has no prior curve widget (this is genuinely new UI, like the wheels).

### 9.4 egui construction

Op-stack `CollapsingHeader`s + drag-reorder: identical pattern to §5's effects stack — one drag-reorder implementation serving three lists app-wide (Layers, Effects stack, Grade op stack) once built. Wheels disc: custom `egui::Painter` circle + `Sense::drag()` on an allocated `Rect` — no existing widget to reuse, build once, document it as a reusable "radial dial" primitive since the audio EQ's frequency-response curve (§11) and the wheels disc are the module's two genuinely custom-drawn controls.

### 9.5 Design tokens

Disc fill `surface-widget`, disc border `border`, offset-vector line + numeric readouts `primary`, neutral center dot `secondary`. Curve spline line `primary`, control points `on-surface` dots with `primary` ring when selected. Qualifier highlight-mode monitor overlay: not a drawer element but worth noting here since it's this section's output — pure white/black, no accent tint (it's a technical matte view, not a themed UI element).

### 9.6 Accessibility

Wheels and curve editor are drag-primary controls with **no keyboard equivalent specified anywhere in 07** — flagged as a gap in Findings §16 (numeric readout fields beside the wheels are the partial mitigation: a keyboard user can type exact lift/gamma/gain/sat values even without dragging the disc, but curve control-point keyboard-nudging has no fallback at all).

---

## 10. Floating Scopes Panel

**Purpose:** waveform/vectorscope/histogram display for grading. **Owning spec doc:** 07 §5 (final bullet), 04 §4.1 ("the one deliberate floating-panel exception to rails-stay-rails").

### 10.1 Anatomy

A resizable `egui::Window` (GPU-rendered), parked beside the program monitor by default (Resolve's scopes-beside-monitor convention, 07 §5) — **not** a rail drawer, per 04 §4.1's explicit exception. Tabbed or stacked (implementer's choice, not specified in 07) among: Waveform (per-x-column luma/channel intensity plot), Vectorscope (Cb/Cr scatter with the standard skin-tone reference line graphic overlaid), Histogram (256-bin luma + optional per-channel bars).

### 10.2 States

Empty/no-selection: falls back to sequence-output scopes (pre-`CaptionOverlay`, 07 §5) rather than going blank — the panel should never show "nothing" while a sequence is loaded, only "scoping the program instead of a clip," communicated via a small label ("Program" vs. the clip's name) at the panel's top. Decimated refresh (1-in-2 frames under perf pressure, 07 §5): no visible UI change — this is a silent internal fallback, not a user-facing state, by design (07 explicitly frames it as responsiveness-preserving, not a degraded-mode the user needs to know about).

### 10.3 Interactions

Window drag/resize (native egui `Window` behavior). Tab/section switch between the three scope types. No direct manipulation of scope content (read-only visualization).

### 10.4 egui construction

`egui::Window::new("Scopes").resizable(true)` — same window-chrome idiom as `draw_audit_panel` (`panels/mod.rs:1368`), GPU compute-shader output blitted into an egui texture and displayed via `ui.image(...)`, matching how the program monitor itself presents `EngineFrame` (04 §3.1).

### 10.5 Design tokens

Window chrome: standard `md` (4px) window rounding per DESIGN.md (this is a `Window`, not a drawer card, so it gets the window-rounding token, not the `lg` card rounding — a deliberate distinction: floating scopes are a tool window, not a shell drawer). Scope plot backgrounds: `surface-base` (near-black, standard scope-display convention in every color tool). Waveform/histogram trace: `on-surface` or a neutral green (`success` token repurposed here is **not** recommended — scope traces are a technical readout, not a status signal; use plain `on-surface` white/near-white, matching Resolve/every NLE's scope convention).

### 10.6 Accessibility

Standard `Window` keyboard-close (Esc or title-bar close button) applies; no scope-specific concern beyond ensuring the label distinguishing "Program" vs. named-clip scoping (§10.2) is legible at the panel's minimum resize size.

---

## 11. Audio Mixer

**Purpose:** per-track/master gain, pan, mute/solo, fx chain, meters; plus on-timeline gain/fade automation. **Owning spec doc:** 09 §8 (UI spec — normative for this section), 09 §4 (signal flow the meters visualize). **Right-rail drawer group:** `AudioMixer` (04 §4.1).

### 11.1 Anatomy — mixer panel (right-rail drawer)

One vertical **channel strip** per `Sequence.audio_tracks` entry (order matches track order) + one **master strip**, laid out left-to-right within the drawer (drawer width 220–480px caps how many strips show before horizontal scroll is needed — acceptable, matches how every DAW mixer scrolls past ~4-6 visible strips). Per strip (09 §8):

- Vertical fader (dB scale, -inf..+12, default 0dB) — drag idiom matches any existing vertical slider in the app, just oriented and scaled per this spec.
- Pan knob (equal-power) — a small rotary control (reuse the wheels-disc primitive's simpler cousin: a 1D rotary drag rather than the 2D wheels disc, same underlying `Sense::drag()`-on-`Rect` construction technique).
- Dual meter (peak fast-ballistics + RMS ~300ms window, VU-like) with a clip-indicator LED (latches red above -0.3 dBTP, click to reset) — a thin vertical bar beside the fader, not overlapping it.
- Mute/solo buttons — small paired icon buttons, `selectable_label`-style toggle-active visuals, solo-active track gets a `warning`-tint highlight (distinct from the `primary` selected-state accent — solo is a "heads up, you're in a special listening mode" signal, not a normal selection, and every DAW convention uses yellow/amber for solo).
- FX-slot rack — ordered `AudioFxUnit` list (add/remove/reorder, same drag-reorder primitive as §5/§9's stacks), double-click a slot opens its kind-specific editor.
- Master strip additionally shows the live integrated-loudness (LUFS) readout, `mono-data` typography, labeled "approx — export value authoritative" (09 §8's explicit caveat — must be visible in the UI, not just the spec prose, so users don't mistake the live rolling estimate for the gated export measurement).

### 11.2 FX editors (double-click a rack slot)

- **Eq** — interactive frequency-response curve with draggable band handles (5 bands: low-shelf, band1-3, high-shelf, 09 §6.1) — a second genuinely custom-drawn widget (alongside the grade curve editor, §9) that should share implementation with it where the underlying "draggable point on a 2D plot" mechanic overlaps, even though the X/Y semantics differ (frequency/gain vs. time/value).
- **Compressor/Gate** — threshold/ratio curve (static reference plot, not draggable point-by-point like EQ — the curve is a *function* of the threshold/ratio/attack/release sliders, so it updates live as those sliders move rather than being directly manipulated) + live gain-reduction meter (a small horizontal or vertical bar showing current `gr_db`).
- **Limiter** — ceiling slider + GR meter, simplest of the three editors.

### 11.3 Anatomy — timeline overlays (embedded in §1 Timeline Panel)

- **Clip gain line** — drawn on the clip body (audio clips), draggable to set `gain_db` baseline or add an automation keyframe if the clip already has a `PropertyTrack` (09 §8).
- **Fade handles** — small triangular drag handles at clip in/out corners; drag sets `fade_in`/`fade_out` duration, right-click picks `FadeShape` (Linear/EqualPower/Log/SCurve) via a small context menu.
- **Waveform** — rendered behind the gain line (already covered in §1.1's clip-rect anatomy; noted here for cross-reference since 09 §8.1 owns the pyramid-sampling detail).
- **Automation lanes** — track-header expand (a chevron/disclosure triangle on the track header, matching the CollapsingHeader affordance's visual language even though it's not literally that widget) reveals `PropertyTrack` lanes for volume/pan and any pinned FX param; each lane shows the keyframe curve (Hold/Linear/Bezier per 01 §6) with draggable keyframe points, matching the interaction language 01 §6/§10 already establish for vector-property keyframing (same coalesce-by-id rule).

### 11.4 States

Mute: toggled/off. Solo: toggled/off, `warning`-tint when any strip is soloed (affects every strip's visual context — a soloed session should be glanceable at the drawer level, not just per-strip). Clip-indicator LED: unlit / lit-and-live / latched-red (persists until clicked, per 09 §8). Fader/pan/knob: default / dragging (live numeric tooltip) / automated (a small keyframe-lane indicator dot at the strip's base when the track has active automation, echoing the Clip Inspector's animated-property ring convention, §5.2).

### 11.5 Interactions

Fader/pan/knob drag with de-zippered live audio feedback (09 §5 — the UI doesn't control this, just triggers it). FX rack drag-reorder. Automation lane keyframe add (click empty lane space at a tick) / drag (move) / right-click (set interpolation).

### 11.6 egui construction

Vertical fader: custom `Slider` with `.vertical()` — egui supports vertical sliders natively, no custom widget needed here (unlike the pan knob/EQ curve, which do need custom painting). Meters: `ui.painter().rect_filled` bars driven by `get_audio_meters` (10 §3.12) polled state, redrawn every frame the drawer is visible. FX-rack drag-reorder: same shared primitive as §5/§9.

### 11.7 Design tokens

Fader track `surface-widget`, fader handle `on-surface`, meter fill gradient from `success`(low)→`warning`(near-clip)→`error`(clipping) — this is the one place a three-stop status gradient is appropriate (a meter is inherently a continuous status signal, unlike the "tint text only" rule for discrete status rows elsewhere) — call out in DESIGN.md as a named exception scoped to meters only. Solo-active highlight: `warning` background wash on the strip header (not the whole strip — matches "tint, don't fill" applied at strip-header granularity rather than row granularity).

### 11.8 Accessibility

Vertical faders support keyboard nudge once focused (egui `Slider` native arrow-key behavior) — no extra work needed, but must be verified given the custom dB-scale mapping doesn't break egui's default keyboard step. Pan knob and EQ curve handles need the same keyboard-fallback treatment flagged for wheels/curves in §9.6 — same underlying gap, different component (see Findings §16, this is one findable pattern, not three separate bugs).

---

## 12. Node Editor

**Purpose:** author per-clip compositions and the project graph. **Owning spec doc:** 08 §6 (UI, normative for this section), 04 §1.1 point 3 (central-panel content-state placement), 08 §5 (project-graph segmented control).

### 12.1 Anatomy — central panel (node canvas + viewer)

Replaces the program-monitor central-panel content state while a composition is being edited (04 §1.1, 08 §6.1). Majority of the rect is the **egui-snarl canvas**; a resizable inset (default 70/30 split, draggable, ratio persisted) is the **viewer** — live composed output, either the true `Output` or a pinned node (08 §6.7). A small segmented control `[ Clip: <name> | Project Graph ]` sits at the canvas's top (08 §5), and a "Back to Timeline" button (+ `Esc`) restores the plain monitor.

- **Nodes** — rounded rect bodies (`sm` rounding, `surface-elevated` fill, `border` outline; selected = `primary` outline), title bar with op name + optional per-node thumbnail-pin toggle (off by default, §12.3), 2-3 inline params on the body for the most load-bearing knobs (e.g. `Merge`'s mode dropdown + opacity slider, 08 §6.4), input/output port sockets colored by `PortType` (Image/Mask/Value — three distinct socket colors, a new small token set: recommend `primary` for Image, `secondary`-adjacent cool tone for Mask, and a third neutral for Value even though no v1 op emits one — reserve it now so it doesn't need inventing later).
- **Wires** — bezier connectors between sockets, colored to match the socket's port type; an in-progress drag from an empty socket previews the wire and snaps/refuses at the drop target per type-compatibility (08 §3.1 — refusal is instant, wire snaps back, no invalid edge ever representable).
- **Keyframe/animation badge** — corner badge on the node body when any param has ≥1 keyframe (08 §6.5), small diamond glyphs on animated-param rows in the inspector (same convention as §5's Clip Inspector).
- **Diagnostic badge** — red exclamation glyph on the specific node a type-mismatch or compile-fallback diagnostic points at (08 §6.6 — the diagnostic type must carry `GraphNodeId` so this is possible, a coordination requirement on 02).
- **Left-rail `NodeEditor` drawer** (not the canvas) — add-node search palette (type-to-filter, grouped: Sources / Compositing / Filters / Keys / Masks / Color / Generators / Time / Utility, 08 §6.2), selected node's full param inspector (all of `AnimProps<EffectParams>`, keyframe add/remove/easing), graph-level info (which clip/project is open, node/edge counts).

### 12.2 States

Node: default / selected (`primary` outline) / dragging / has-diagnostic (red exclamation badge, persists until the underlying issue resolves) / has-keyframes (corner badge) / thumbnail-pinned (small filled-pin icon replaces the default outline pin icon in the title bar). Wire: default / drag-preview (translucent, follows cursor) / refused (brief red flash at the attempted drop point, then the wire snaps back — a transient state, not persisted).

### 12.3 Interactions

Pan/zoom (native egui-snarl). Node drag (native). Wire drag with type-colored sockets that refuse incompatible drops (08 §3.1, §6.2). Add-node via palette search or canvas right-click "Add Node" menu (mirrors the same list). Box-select (marquee) + align/distribute — thin custom layers over snarl's selection primitives, matching the timeline's marquee-select for muscle-memory consistency (08 §6.2 explicitly calls this out). Per-node thumbnail-pin toggle click (explicit opt-in, 08 §6.3 — never automatic-for-all-visible-nodes, since each pin is a real per-frame eval+readback cost).

### 12.4 egui construction

egui-snarl crate (08 §9 pins the exact version; fallback `egui_node_graph2` if needed, UI-layer-only swap). Palette list: same `tool_row`/`section_header` idiom as the vector tool palette and the effects browser (§6) — one more reuse of that pattern, now used in four places (tool palette, effects browser, node palette, and implicitly the LUT browser's grouped list, §9). Inspector: same generic `PropPath`-driven param widget builder as Clip Inspector (§5) and TTS param panel (§8) — third reuse of that one builder.

### 12.5 Design tokens

Node body `surface-elevated`/`border`, selected `primary` outline, `sm` rounding (matches every other rect-shaped interactive element in the app — a node is, chrome-wise, just another card). Port-type socket colors: `primary` (Image), a new cool-neutral (Mask — recommend deriving from `secondary` at higher saturation, e.g. a muted teal-violet blend consistent with the app's one-hue-family discipline rather than introducing a second true hue; exact value is an implementation decision deferred to whoever builds this, flagged in Findings §16 as needing a DESIGN.md addition before v1 ships). Diagnostic badge: `error` red, small, corner-anchored, never a full node-body tint (consistent with "tint, don't fill").

### 12.6 Accessibility

Marquee/box-select and align/distribute need keyboard-reachable equivalents matching whatever the vector canvas already provides for the same operations (if the vector canvas's align/distribute has toolbar buttons in addition to drag-select, the node editor should too — don't make the node graph pointer-only when the vector canvas isn't).

---

## 13. Export Dialog

**Purpose:** configure and launch a sequence export. **Owning spec doc:** 05 §3 (schema, presets, walkthrough — normative for every field/step below).

### 13.1 Anatomy

A modal-adjacent dialog (05 §3.8 explicitly requires the *progress* stage to be non-modal, but the configuration stage can be a standard `egui::Window` modal since it's a one-time setup step, not an ongoing session). Two-column layout:

- **Left column — preset picker**: built-ins (locked icon, 05 §3.5's catalog: Social 9:16/1:1/16:9, Master AV1 High, Web H.264, WebM VP9 Alpha, ProRes Mezzanine, GIF, PNG Sequence) then custom presets (05 §3.6), search/filter box. Selecting populates all right-column fields; editing any field after selection marks the picker "Custom (based on X)" — never silently locks the user out of tweaking a preset.
- **Right column — fields** (05 §3.1/§3.8 step 3): container dropdown, video codec + quality-mode toggle (CRF slider ⟷ target-bitrate field, mutually exclusive — a segmented control switches which input is active, the other greys out rather than both showing simultaneously), audio codec + bitrate, resolution radio group (`SourceFormat` default / `Explicit` w×h / `Scale` factor), frame-rate radio (`MatchSequence` default / `Explicit`), alpha toggle (disabled unless the container+codec combination is in 05 §3.4's allow-list — grey with hover text explaining *why*, never just absent), faststart checkbox (MP4/MOV only, hidden for other containers), loudness-target dropdown (Off / -14 LUFS streaming / -23 LUFS broadcast / custom).
- **Format checklist** (05 §3.8 step 4): appears only when `Sequence.formats.len() > 1` — one checkbox per format ("☑ 16:9 ☑ 9:16 ☐ 1:1"), each checked format becomes a sub-job in the same export job.
- **Range** — work-range fields pre-filled from `Sequence.work_range`, "Entire sequence" override button.
- **Estimate strip** — read-only line: approximate output size + estimated render time (05 §3.8 step 6 — expectation-setting, explicitly not a hard promise, should read as an estimate visually, e.g. `secondary` tone with a "~" prefix, not presented with the same confidence as a fact).
- **Pre-flight banner** — appears above the Export button only if offline media is detected (05 §3.8 step 7); lists the specific offline assets with an inline "Relink…" shortcut; the Export button stays disabled until resolved.
- **Progress panel** (post-launch, non-modal, 05 §3.8 step 8) — frame/total, fps, ETA, cancel button; multi-format jobs show N progress bars or one aggregate with a sub-job label.

### 13.2 States

Export button: disabled (pre-flight failure) / enabled / hidden-behind-progress (once launched, the dialog's config form is replaced by the progress panel — not both shown at once, per the modal→non-modal transition 05 §3.8 implies). Alpha toggle: enabled / disabled-with-reason. Quality-mode toggle: CRF-active / bitrate-active (mutually exclusive, never both editable simultaneously).

### 13.3 Interactions

Preset select → field population. Field edit → preset-picker label updates to "Custom (based on X)". "Save as preset…" button persists the current field set as a named custom preset (05 §3.6). Format checklist toggles which sub-jobs run. Cancel (during progress) → `EngineCmd::CancelExport`.

### 13.4 egui construction

Two-column `egui::Window` with `ui.columns(2, ...)` or a manual `SidePanel`-in-`Window` split. Preset list: same `tool_row`-family list idiom (fourth reuse). Progress panel: matches the audit log's non-modal `Window` pattern, or a corner-anchored toast-adjacent panel — implementer's choice, but must not block interaction with the rest of the app (05 §3.8 step 8 is explicit: "GUI stays interactive").

### 13.5 Design tokens

Standard `md` window rounding, `surface` fill. Estimate strip: `secondary`, `body-sm`. Pre-flight banner: `error` tint background wash (this is one of the rare cases a banner-level fill is justified — a blocking pre-flight failure is exactly the "loud, can't-miss" case DESIGN.md's "don't fill rows" rule is scoped to avoid for routine status, not for a hard blocker; note this as a deliberate, narrow exception, not a precedent for filling rows generally).

### 13.6 Accessibility

Every disabled control (alpha toggle, Export button) needs hover text stating *why*, not just a greyed-out appearance — this is a repeated requirement across the whole module (rail icons, effects-browser entries, export fields) and should be treated as a standing rule: disabled ≠ silent.

---

## 14. Format Tab Strip

**Purpose:** switch/add/remove sequence aspect-ratio variants. **Owning spec doc:** 05 §4.1.

### 14.1 Anatomy

A horizontal tab strip above the program monitor (mirrors D-02: canvas stays the monitor, this strip is a thin addition above it, not a new panel — consistent with the transport bar's "overlay, not a panel" treatment, though the format strip is persistent chrome rather than a floating overlay, so it more resembles the existing document tab bar's construction, `draw_tab_bar`, than the transport bar's floating `Area`). One tab per `SequenceFormat` (name label, e.g. "16:9"), a trailing "+" tab opens the Add Format picker (16:9/9:16/1:1/4:5/Custom w×h list, 05 §4.1), each non-active, non-last tab has a small remove affordance on hover (guarded: can't remove the last format or the active format without switching first, matching existing guarded-delete UX, 05 §4.1).

### 14.2 States

Tab: default / active (selected-state visual, matches every other exclusive-choice tab in the app) / hover-showing-remove-affordance / disabled-remove (last format or active format, remove icon absent or greyed with explanatory hover text).

### 14.3 Interactions

Click switches `active_format` (`TimelineCmd::SetActiveFormat`, instant, undoable). "+" opens the add-format picker (a small popup/menu, not a full dialog — this is a lightweight action). Remove (hover affordance) prompts nothing extra if the format is unused; per 05 §4.1 the operation is simply guarded/refused for the two protected cases, not confirmation-gated for the general case (no destructive-confirmation dialog specified — matches the app's general pattern of confirmation only for genuinely data-losing, non-undoable actions; format removal is undoable via the normal history stack, so a confirm-dialog would be redundant friction).

### 14.4 egui construction

`selectable_label` row (the exclusive-choice idiom, same as the mode toggle §15 and the top menu-bar File/Edit/Tools tabs, `app/mod.rs:2533` `ui.selectable_label(file_active, "File")`) — do not build a custom tab widget, per DESIGN.md's explicit instruction to reuse this exact idiom for format/mode tab strips.

### 14.5 Design tokens

Active tab: `primary-dim` fill + `primary` underline-or-border (matches `selectable_label`'s native selected treatment, no override needed). Inactive tab text: `secondary`.

### 14.6 Accessibility

Tab strip must be keyboard-navigable (egui's native Tab-focus + Enter-to-activate on `selectable_label` already provides this — no extra work, just verify it isn't accidentally suppressed by custom click-handling).

---

## 15. Mode Toggle

**Purpose:** switch the whole shell between Vector and Video `AppMode`. **Owning spec doc:** 04 §1.2.

### 15.1 Anatomy

A single toolbar button, alongside the existing File/Edit/Tools `selectable_label`s in the top toolbar row (04 §1.2 — "same `selectable_label` + `active_drawer`-flush idiom already used there"). Label: "Video" (icon + text, matching the toolbar's existing mixed icon/text convention, e.g. `panels/toolbar.rs`'s logo+wordmark treatment).

### 15.2 States

Default (Vector mode active, button unselected) / active (Video mode active, `selectable_label` selected-state visual) / first-click-pending (no distinct visual state needed — `TimelineProject` creation on first entry, 04 §1.3, is fast enough to not need a loading state per the spec's silence on this; if project creation ever becomes perceptibly slow, add a brief disabled+spinner state, but nothing in 01/04 suggests this is needed for v1).

### 15.3 Interactions

Click toggles `self.mode` (creates `TimelineProject` on first entry if absent, 04 §1.3). Also reachable via command palette (`mode.enter_video`/`mode.exit_video`/`mode.toggle_video`, 04 §1.2) — the button and the command must stay in sync (both just flip the same `self.mode` field, so this is automatic, not a synchronization concern requiring extra plumbing).

### 15.4 egui construction

`ui.selectable_label(mode_active, "Video")` in the toolbar row, exact idiom as File/Edit/Tools (`app/mod.rs:2561` `ui.selectable_label(tools_active, "Tools")` — the mode toggle is a fourth entry in that same row-building code, not a separate widget tree).

### 15.5 Design tokens

Standard `selectable_label` selected-state visuals (`primary-dim`/`primary`), no new tokens.

### 15.6 Accessibility

None beyond what `selectable_label` + command-palette dual-entry already provides — this is the one component in the whole inventory with no open accessibility question, worth noting as the baseline every other exclusive-choice control should match.

---

## 16. Findings — UX issues in the existing spec docs

Observations only; docs 00–12 are not edited here per the task scope.

1. **No keyboard path for the three genuinely custom-drawn 2D drag controls** (color wheels §9.1.1, curve editors §9/§11.2, node-graph marquee/align in §12) — 07, 08, and 09 all specify these as pointer-drag-only. Numeric readout fields are a partial mitigation for the wheels (type an exact value instead of dragging) but curve control-point placement and node-canvas marquee-select have no stated fallback anywhere in 04–09. Recommend a follow-up decision in 07/08/09 (or a new cross-cutting a11y appendix) before implementation, not left implicit.

2. **The eyedropper mechanism isn't explicitly extended for grading.** `EyedropperTarget` (`panels/mod.rs:56-109`) is the existing, well-established enum every vector-canvas eyedropper flow uses (fill, stroke, glows, raster color-range, swatch recolor). 07 §5's qualifier-picker eyedropper describes the same interaction (sample off the canvas, Esc to cancel) but 07 never states whether it's a new `EyedropperTarget::GradeQualifier { clip_id }` variant on the *existing* enum or a parallel mechanism. Given DESIGN.md's "one toggle idiom, reused" principle and this doc's own recommendation (§9.3), 07 should be amended to explicitly extend `EyedropperTarget` rather than leaving the implementer to discover the existing enum mid-implementation.

3. **Mask-port socket color has no assigned token.** 08's node catalog defines three `PortType`s (Image/Mask/Value) that need visually distinct wire/socket colors, but no doc assigns concrete colors — this doc proposes a direction (§12.5) but the exact value needs to land in DESIGN.md as a real addition before P8 implementation, not be improvised per-implementer.

4. **The meter-gradient exception and the pre-flight-banner fill exception (both flagged inline, §11.7 and §13.5) are the only two places this doc recommends breaking DESIGN.md's "tint text, never fill" rule.** Worth a one-line addition to DESIGN.md's Do's/Don'ts itself (rather than leaving the exception only in this doc) so a future reviewer auditing against DESIGN.md doesn't flag them as violations — DESIGN.md should name its own narrow exceptions.

5. **04 §2.1 fixes the track-header column at "default 160px" but never states a min/max range**, unlike the left/right drawers which 04/DESIGN.md both give explicit clamp ranges (160-420 / 220-480). Recommend 04 add a clamp (this doc assumes splitter-resizable per convention but the bounds are unspecified — an implementer could ship an unbounded or accidentally-zero-width column).

6. **No stated behavior for what the Clip Inspector (§5) shows with a multi-clip selection.** 04/01 are silent on whether ripple/roll/multi-select (04 §2.6) extends to showing common-value editing in the inspector (the vector Inspector has an established multi-select story via `selection_count`/`selected_ids`, `panels/mod.rs` `PropPanelCtx`) — the video Clip Inspector should almost certainly mirror that existing pattern rather than only supporting single-clip inspection, but no doc states this explicitly. Flag for 04 or a P6 implementation note.

7. **Format tab strip removal (§14.3) and export dialog format-checklist (§13.1) both reference `Sequence.formats`, but no doc specifies a maximum count** — a user could add an unbounded number of custom formats, degrading both the tab strip (horizontal overflow — no scroll/overflow behavior specified) and the export checklist (long, unscannable list). Low-priority, but worth a soft cap or scroll affordance decision before the strip is built.
