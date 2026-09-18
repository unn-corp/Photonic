---
version: alpha
name: Photonic
description: Vector + raster illustration editor, egui/Rust desktop app, "Deep Violet" dark-first theme
colors:
  primary: "#6E56CF"          # electric violet accent — active states, selection, brand mark
  primary-hover: "#9077E0"    # accent_light — hover/glow variant of primary
  primary-dim: "#3D3080"      # accent_dim (dark) — selection fill, active widget bg
  secondary: "#8A8AA8"        # text_muted (dark) — secondary labels, hints (5.51:1 vs surface-elevated, WCAG AA)
  surface: "#0C0C15"          # bg_panel (dark) — default panel/window surface
  surface-base: "#07070B"     # bg_base (dark) — outermost window fill, canvas surround
  surface-elevated: "#13131F" # bg_elevated (dark) — hover bg, drawer-inset surfaces
  surface-widget: "#1A1A28"   # bg_widget (dark) — text inputs, active widget fill
  on-surface: "#E8E8F2"       # text_primary (dark)
  border: "#1E1E32"           # decorative panel chrome — declared contrast-exempt
  border-interactive: "#666690" # interactive control boundary — 3.16:1 vs surface-widget, WCAG SC 1.4.11
  error: "#F87171"            # error_fg_color (dark)
  warning: "#FBBF24"          # warn_fg_color (dark)
  success: "#64C87A"          # audit-log "ok" green, used sparingly for confirmations
  # Light theme ("Soft Lavender") — same roles, light values. Referenced as
  # colors.light-* since the frontmatter schema is single-palette; the app
  # ships both and switches at runtime (prefs.dark_mode).
  light-primary: "#6E56CF"
  light-surface: "#F3F0FF"
  light-surface-base: "#FAF9FF"
  light-surface-elevated: "#EAE4FF"
  light-surface-widget: "#FFFFFF"
  light-on-surface: "#19143C"
  light-border: "#D2C8F0"
  light-secondary: "#6E6496"
  light-error: "#B02D2D"
  light-warning: "#8F5E00"
typography:
  # egui has no font-weight axis by default (one family: Inter-like default,
  # Noto fallback for CJK/emoji — see tests/no_tofu_glyphs.rs). "Weight" below
  # means RichText::strong()/weak() (semibold-equivalent / 70%-opacity-equivalent),
  # not a literal font file swap.
  body-md:
    fontFamily: "egui default proportional (Inter-like), 'Noto Sans' fallback"
    fontSize: 14px
    fontWeight: 400
    lineHeight: 1.3
    letterSpacing: 0
  body-sm:
    fontFamily: "egui default proportional"
    fontSize: 12px
    fontWeight: 400
    lineHeight: 1.25
    letterSpacing: 0
  label-section:
    fontFamily: "egui default proportional"
    fontSize: 11px
    fontWeight: 400
    lineHeight: 1.2
    letterSpacing: 0.4px
  label-strong:
    fontFamily: "egui default proportional"
    fontSize: 14px
    fontWeight: 600
    lineHeight: 1.3
    letterSpacing: 0
  mono-data:
    fontFamily: "egui default monospace"
    fontSize: 12px
    fontWeight: 400
    lineHeight: 1.3
    letterSpacing: 0
rounded:
  none: 0px
  sm: 3px    # widget rounding (buttons, inputs, dropdowns) — theme.rs Rounding::same(3.0)
  md: 4px    # window/menu/popup rounding — theme.rs Rounding::same(4.0)
  lg: 8px    # rail/drawer "card" rounding — app/mod.rs CARD_ROUNDING
  full: 9999px
spacing:
  xxs: 2px
  xs: 4px
  sm: 6px
  md: 8px
  lg: 14px
  xl: 24px
components:
  rail-icon-button:
    size: 30px 30px
    rounded: "{rounded.sm}"
    padding: "{spacing.xs}"
    iconSize: 18px
  drawer-card:
    backgroundColor: "{colors.surface}"
    borderColor: "{colors.border}"
    rounded: "{rounded.lg}"
    padding: 10px 8px
    minWidth: 160px
    maxWidth: 420px
  panel-section-header:
    textColor: "{colors.secondary}"
    typography: "{typography.label-section}"
    textTransform: uppercase
  selected-toolbar-item:
    backgroundColor: "{colors.primary-dim}"
    borderColor: "{colors.primary}"
    rounded: "{rounded.sm}"
  primary-accent-text:
    textColor: "{colors.primary}"
    typography: "{typography.label-strong}"
---

<!--
Contrast coverage table (41 §5.1 / R-10 / R-11). One row per `foreground |
background | role`. `crates/photonic-gui/tests/design_contrast.rs` evaluates every
non-exempt row to WCAG 2.1 (body-text ≥ 4.5:1, large-text/boundary/graphic/
focus-ring ≥ 3:1) and asserts every `colors:` token appears as a foreground here
or in a named `exempt:` row — so adding a token without a pair row fails the build.
-->
```contrast
secondary | surface-elevated | body-text
secondary | surface | body-text
on-surface | surface | body-text
on-surface | surface-base | body-text
on-surface | surface-elevated | body-text
on-surface | surface-widget | body-text
primary | surface | focus-ring
primary-hover | surface | body-text
border-interactive | surface-widget | boundary
border-interactive | surface | boundary
error | surface | body-text
warning | surface | body-text
success | surface | body-text
light-secondary | light-surface | body-text
light-on-surface | light-surface | body-text
light-on-surface | light-surface-base | body-text
light-on-surface | light-surface-widget | body-text
light-primary | light-surface | body-text
light-primary | light-surface-widget | focus-ring
light-error | light-surface | body-text
light-warning | light-surface | body-text
primary-dim | surface | exempt:accent selection/active fill, not text or boundary
surface | surface-base | exempt:background surface fill
surface-base | surface-base | exempt:outermost window fill
surface-elevated | surface | exempt:background surface fill
surface-widget | surface | exempt:input/widget background fill
border | surface | exempt:decorative panel chrome
light-surface | light-surface-base | exempt:background surface fill
light-surface-base | light-surface-base | exempt:outermost window fill
light-surface-elevated | light-surface | exempt:background surface fill
light-surface-widget | light-surface | exempt:input/widget background fill
light-border | light-surface | exempt:decorative panel chrome
```

## Overview

Photonic is a native desktop illustration tool (Rust + egui/eframe) aimed at people who want Illustrator-class vector precision without leaving a fast, keyboard-driven, dark-by-default workspace. The reference point is **a professional color-grading suite's dark control room crossed with a design tool's icon-driven toolbar** — think DaVinci Resolve's charcoal density and instrument-panel restraint, not Figma's airy whiteboard or Photoshop's grey neutrality. Every surface reads as a plate in a stack of dark cards; the single violet accent (`#6E56CF`, nicknamed "Deep Violet" in the codebase) is the only saturated color in the chrome, reserved for "this is active / this is selected / this is you" — so it stays legible against the vector art itself, which is the actual point of focus. There is a light theme ("Soft Lavender") for daytime/bright-room use, but dark is the shipped default (`prefs.dark_mode = true`) and the theme this document treats as canonical; the light values are a straight tonal inversion of the same roles, not a separate personality.

Audience: working illustrators, motion/brand designers, and (increasingly) AI agents driving the app headlessly via MCP — so chrome must stay legible at a glance and every control needs an unambiguous state (selected/hover/active/disabled), since agents and humans both read it.

## Colors

- **`primary` (`#6E56CF`)** — the one accent. Used for every *non-text* accent: selection highlight/stroke, active rail-icon fill (`primary-dim` bg + `primary` border), focused-widget border, focus ring, port-socket fill — all ≥3:1 against their surface. Per R-13 it is **not** used as text: at 3.61:1 on `surface` it clears the 3:1 graphic floor but not the 4.5:1 body-text floor. Text that used to be `primary` — the Photonic wordmark and links — uses **`primary-hover`** instead (see below). Never used for large fills.
- **`primary-hover` link/wordmark text** — the Photonic wordmark and any hyperlink render in `primary-hover` (`#9077E0`, 5.47:1 on `surface`), the smallest violet that passes body-text AA; `primary` keeps every non-text use.
- **`primary-hover` (`#9077E0`)** — lighter violet reserved for hover glow and hyperlink-hover; not currently wired into most hover states (widgets use `surface-widget` for hover bg instead) but reserved for glow/emphasis moments.
- **`primary-dim` (`#3D3080`)** — the accent at low value, used as the *filled* background for an active/selected state (selection box fill, active widget bg) so the saturated `primary` can stay reserved for the 1px border/stroke on top of it.
- **`surface-base` (`#07070B`)** — the outermost window fill; almost black. Sits behind everything, including the floating rail/drawer cards, so their rounded corners and 1px borders actually read as elevated.
- **`surface` (`#0C0C15`)** — default panel fill: rail cards, drawer cards, the properties drawer, floating windows.
- **`surface-elevated` (`#13131F`)** — one step up: widget hover backgrounds, `noninteractive.weak_bg_fill`, code/audit-log background.
- **`surface-widget` (`#1A1A28`)** — text inputs and the "hovered" widget fill; the lightest of the neutral surfaces, used sparingly so inputs pop against panels.
- **`border` (`#1E1E32`)** — **decorative** panel/card chrome only, and `noninteractive` widget outlines. Low contrast by design (this is a dense, chrome-heavy app; loud borders everywhere would compete with the art) and therefore declared contrast-exempt — it never carries state or delimits an interactive control.
- **`border-interactive` (`#666690`)** — the boundary of every *unfocused interactive* widget (`inactive.bg_stroke`), 3.16:1 vs `surface-widget` / 3.58:1 vs `surface`, meeting WCAG SC 1.4.11 for non-text UI-component boundaries. Any 1px edge that conveys "this is an operable control" uses this, not `border`.
- **`on-surface` (`#E8E8F2`)** — primary text, near-white with a cool tint.
- **`secondary` (`#8A8AA8`)** — muted/secondary label text (hints, disabled-adjacent copy, `RichText::weak()`); 5.51:1 on `surface-elevated`, meeting body-text AA.
- Small-caps drawer/section headings (SELECT, SHAPES, TOOLS, etc.) render in `secondary` at `RichText::small()` (the 11px/0.4px-letterspaced `label-section` style, see `components.panel-section-header`). The recession comes from the smaller, letter-spaced type — not from a fourth, dimmer grey; the previous `#50506E` header tone failed contrast and has been removed (R-12).
- **`error` (`#F87171`)**, **`warning` (`#FBBF24`)** — status colors for validation/audit-log rows; used only for text/icon tint, never as a fill.
- **`success` (`#64C87A`)** — confirmations only (e.g. audit-log OK rows). Introduced here for the export/render pipeline's "done" states; add it to `theme.rs` if the video module needs it as a first-class widget state.

## Typography

egui ships one proportional family (Inter-like) plus a Noto Sans fallback stack for glyph coverage (`tests/no_tofu_glyphs.rs` enforces no tofu boxes), and one monospace family for numeric/code readouts. There is no weight axis to select from a font file — "weight" in practice means `RichText::strong()` (near-600) vs default (400) vs `RichText::weak()` (400 at reduced opacity via `secondary` color), plus `RichText::small()` for the 11-12px label sizes. Keep the video module inside these three sizes (`body-md` 14px default UI text, `body-sm`/`label-section` 11-12px for dense metadata and section headers, `mono-data` 12px monospace for timecodes/frame counts/numeric readouts) — do not introduce a fourth size without a documented reason; egui's density depends on this restraint.

## Layout

Spacing runs on a tight, mostly-even scale (`spacing` tokens): 2/4/6/8/14/24px, built from observed `ui.add_space()` calls rather than a formal 8pt grid — expect small asymmetric gaps (2px after a header rule, 4px between rail icons, 6-8px around card padding). The whole shell is a **rail + floating drawer-card** system, not fixed panels: a slim icon rail (`RAIL_WIDTH` = pad 7 + icon 30 + pad 7 + gap 4 = 48px) sits flush to each screen edge, and clicking a rail icon tweens open a floating rounded "card" drawer (160-420px left / 220-480px right, 0.18s cubic-out ease, reduced-motion collapses the tween to instant). Cards float with a small margin off the window edge/canvas (`CARD_FLOAT_Y` 6px top/bottom, `DRAWER_FLOAT_X` 4px off the canvas) so their rounded corners and border are visible against the near-black base — flush placement with no gap is a known anti-pattern here (`app/mod.rs` comments call this out explicitly). New video-module chrome (timeline panel, transport bar, program monitor) should follow the same idiom: floating card, not flush dockable pane, unless the panel map in `04-ui-mode-timeline.md §4.1` requires a specific dock behavior (it does — timeline is bottom-docked, not floating; treat the dock's *frame* the same as a drawer card: rounded top corners, 1px border, `surface` fill).

## Elevation & Depth

Flat by design: `window_shadow`/`popup_shadow` are explicitly `Shadow::NONE` everywhere. Depth is communicated by **surface value + border**, not shadow — `surface-base` → `surface` → `surface-elevated` → `surface-widget` is the light-value ramp from "background" to "thing you're about to type into," each with the shared 1px `border` color. Any new elevated surface (a floating scopes panel, a modal export dialog) should pick the next step up this ramp rather than introduce a drop shadow.

## Shapes

Two rounding values cover the entire app: `sm` (3px) for every interactive widget (buttons, inputs, dropdowns, selectable labels), `md` (4px) for windows/menus/popups. The rail/drawer "card" system adds one more: `lg` (8px), reserved specifically for the outermost floating-card corners (and only the outward-facing corners — rail cards round only their outer two corners, e.g. the left rail rounds only its right edge, since its other edge is flush to the window). Video-module floating panels (scopes, node-editor palette drawer) should use `lg` the same way; inline controls inside them (sliders, buttons, dropdowns) stay at `sm`.

## Components

- **Rail icon button** — 30×30px square, phosphor glyph at 18px, `sm` rounding, selected state = `primary-dim` fill + `primary` 1px border (egui `.selected(true)` on `Button`). This is the idiom for every "toggle one of N exclusive panels" control — reuse verbatim for any new rail-style mode switcher (e.g. a program-monitor overlay toggle strip) rather than inventing a new toggle-button look.
- **Selectable label (toolbar/tool-list item)** — `ui.selectable_label(is_active, label)`; used for the tool palette rows, top menu-bar items (File/Edit/Tools), and format/mode tab strips. This is the correct widget for the video module's format tab strip and mode toggle (04 §4.1) — do not build a custom tab widget.
- **Section header** — small-caps, `secondary` at `RichText::small()`, `label-section` typography, 4px space above / 2px below (`theme::section_header()` helper). Reuse for every drawer subsection (inspector panel category labels, effects browser groups, etc).
- **Collapsing header** — `egui::CollapsingHeader::new(title)`, the standard idiom for a foldable property group inside a drawer (Transform, Fill, Stroke, Effects, …). The clip inspector and color-controls drawer should use this for each control group (Lift-Gamma-Gain, Curves, HSL, LUT) rather than always-expanded stacks.
- **Context menu** — `Response::context_menu(...)`, right-click driven; used for canvas object actions today. Reuse for timeline clip right-click menus (split, ripple delete, reveal in media pool) per 04's edit-menu requirements.
- **Card / drawer frame** — `egui::Frame` with `inner_margin` (content gutter), `outer_margin` (float gap), `rounding` (per-corner, outward corners only when docked to an edge), and a 1px `noninteractive.bg_stroke` border. This is the frame recipe for every new floating panel in the video module (scopes, node palette).
- **Swatch button** — fixed 26×26px square color patch that opens the shared sRGBA color popup (`crate::color_popup::ColorPopup`); reuse for the color-grading wheels' current-value chips and the LUT browser's thumbnail chips.
- **Audit/log row coloring** — status text tinted `success`/`error` inline in an otherwise neutral grid row (no full-row fill). Reuse this restrained pattern for render/export queue status and caption-generation job status — do not fill entire rows with status color.
- **Node-editor port sockets** — port-type color coding reuses existing functional tokens, no new hues: `Image` port = `primary` (#6E56CF), `Mask` port = `success` (#64C87A), `Value` port (post-v1) = `warning` (#FBBF24). Socket = 8px filled circle, `border`-color ring when unconnected, full token fill when wired. This is functional data-coding (like status tints), not chrome accent — documented so it isn't flagged as an accent-rule violation.

## Do's and Don'ts

- **Do** keep `primary` violet reserved for "active/selected/you're focused here" — one accent color, used consistently, is what makes the instrument-panel reference read correctly.
- **Do** build new floating panels as rounded `surface`-fill cards with a 1px `border` stroke and no shadow — that is how this app signals elevation.
- **Do** reuse `selectable_label` for any exclusive-choice row/tab and the rail-icon-button pattern for any exclusive-choice icon strip; don't invent a third toggle idiom.
- **Do** keep section headings at `secondary` + `RichText::small()` (the `label-section` style) so they recede below their own section's content through size and letter-spacing, not a below-AA grey.
- **Don't** introduce drop shadows, gradients-as-chrome, or a second accent hue — the flat, single-accent, dark-panel-stack look is the whole personality; a second saturated color (e.g. a distinct "video module blue") would read as a bolted-on module rather than a native part of the app.
- **Don't** dock new panels flush with no gap against a neighboring panel/edge — always leave the small float margin (`CARD_FLOAT_Y`/`DRAWER_FLOAT_X`) so rounded corners and borders are visible; this codebase has explicit prior-art comments warning against the flush anti-pattern.
- **Don't** add a fourth text size or a bold font file — weight is `strong()`/default/`weak()` on the one family, not a font swap.
- **Don't** fill full rows/cells with status color (error/warning/success) — tint the text/icon only, matching the audit-log idiom. **Two named exceptions** (video module, deliberate): (1) **audio meter bars** fill with a green→amber→red level gradient — meters are instrumentation, not chrome, and every DAW user expects filled ballistics; (2) the **export pre-flight error banner** uses a dim `error`-tinted background fill behind its text (offline-media blocking state) — a blocking pre-flight failure must not be miss-able as a tinted line. Anything else wanting a status fill needs a documented exception here first.
