# 41 — Accessibility: Keyboard, Contrast, Motion, and Non-Colour Encoding

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** GUI owner, UX reviewer, DESIGN.md owner, CI owner

**Depends on:** [DESIGN.md](../../../DESIGN.md) (token source of truth), [01-data-model.md](01-data-model.md) §4.1 (marker categories — this document places an amendment on it), [04-ui-mode-timeline.md](04-ui-mode-timeline.md) §5 (keyboard model and the mode-gating rule), [06-captions-ai.md](06-captions-ai.md) §5 (caption animation), [07-color-grading.md](07-color-grading.md), [08-fusion-node-flows.md](08-fusion-node-flows.md), [09-audio-mixer.md](09-audio-mixer.md), [13-ux-components.md](13-ux-components.md) (per-component a11y notes — this document is the contract those notes lack), [33-timeline-preview-render.md](33-timeline-preview-render.md) §6, [36-error-model.md](36-error-model.md) §4.

**Owns:** [27 MC-7](27-spec-audit.md#mc-7--p2--accessibility) in full, and [13 §16 Finding 1](13-ux-components.md) (the custom drag controls with no keyboard path).

---

## 1. The gap

[13 §0](13-ux-components.md) sets the ceiling in one line: *"egui's accessibility story is limited (no OS-level a11y tree in v1 per repo precedent); notes here are keyboard-reachability and color-contrast, not screen-reader semantics."* Fourteen per-component Accessibility subsections follow, committing to tab-reachability for clip and track-header controls, tooltips on icon-only controls, disabled controls stating *why*, shape-not-colour for transition badges and offline stripes, and a keyboard fallback for effect-stack reorder.

Everything else it **flags and declines to resolve**: colour wheels and the curve editor have "no keyboard equivalent specified anywhere in 07"; the pan knob and EQ handles are "the same underlying gap"; node marquee and align/distribute need "keyboard-reachable equivalents" with no proposal. There is no focus-order rule, no contrast number, no hit-target number, no reduced-motion policy, and no rule about colour-only encoding beyond one sentence.

**The code has moved ahead of the spec, unevenly, and in two places wrongly.** Verified against `crates/photonic-gui`:

- **Colour wheels** (`panels/video/color_page.rs:632` `wheel_dial`) — handles `dragged()` and `double_clicked()` only. It allocates with `Sense::click_and_drag()`, so it *is* in the tab ring, and does nothing when focused. The three `DragValue` readouts beside it are a path to the value, not to the control.
- **Curve editor** (`color_page.rs:793`) — arrow-nudge and `Delete` already exist (`:855-880`), which 13 §9.6 says they do not. But they are gated on `!keyboard_captured(ui)`, and `keyboard_captured` is `ui.memory(|m| m.focused().is_some())` (`:74`). **The nudge therefore works only when nothing has focus, and stops the moment the curve widget is focused.** That is exactly backwards. Point *selection* is still pointer-only, so a keyboard user can never acquire a point to nudge.
- **Node editor** (`node_editor.rs:1383-1455`) — `Tab` cycles nodes, arrows nudge, `Delete` removes, `Esc` cancels a wire. All of it sits behind `if !ui.rect_contains_pointer(canvas_rect) { return; }` (`:1379`). **Keyboard access requires holding the mouse in the right place.** The marquee 13 §12 specifies does not exist in code at all.
- **`accesskit` appears nowhere.** `egui = "0.29"` with `egui-winit`/`egui-wgpu` driven directly; there is no `eframe`. Since AccessKit is a non-default feature on `egui-winit` enabled *by eframe*, Photonic emits **no accessibility tree of any kind** today.
- **Reduced motion already exists and is honoured in two places** — `prefs.reduced_motion`, the drawer tween, and the radial wheel. It is a convention with no contract, so nothing stops the next animation from ignoring it.

So the gap is not "accessibility is missing". It is that **there is no owner, no number, and no test** — and the two most common failure modes of that situation, a keyboard path gated on the pointer and a keyboard path disabled by focus, are both already in the tree.

---

## 2. Position on screen readers

### 2.1 What egui can actually do, as of 2026-07

- **egui 0.35** is current; the repo is pinned to **0.29**. Any AccessKit work is gated on that upgrade.
- egui integrates **AccessKit**, but as an **optional feature** on `egui-winit`, enabled by default only in `eframe`. Photonic does not use `eframe`, so it must be turned on deliberately.
- AccessKit ships adapters for **Windows (UIA), macOS, and Unix (AT-SPI)**. The Unix adapter is real and shipping, and also the least exercised of the three.
- The semantics egui gives you: `Response::widget_info`, `Response::labelled_by`, `WidgetType`, and the focus API. Built-in widgets populate `WidgetInfo` themselves.
- The semantics egui does **not** give you: anything for a custom-painted region. `allocate_painter` produces a node with no role, name or value. **Every custom control in this module is invisible to assistive technology unless it calls `widget_info` explicitly** — the colour wheel, curve editor, node canvas, timeline, scopes and meters, i.e. the module's entire distinctive surface.
- `Response::labelled_by` with an `Id` naming no widget **panics**. Treat it as a footgun.
- **`egui_kittest` is AccessKit-based** and queries by accessible label. This is the lever that makes the work pay even if no screen reader is ever pointed at the app: *a correct accessibility tree is a testable UI contract that runs headless in CI.*

### 2.2 The position

| Tier | Commitment |
|---|---|
| **T1 — Keyboard operability** | **Committed, v1, no exceptions.** Every function reachable by pointer is reachable by keyboard |
| **T2 — A correct accessibility tree** | **Committed, v1.** Enable `accesskit`; every custom control emits `widget_info` with role, name and value. Enforced by kittest queries in CI |
| **T3 — A verified screen-reader editing experience** | **Explicitly deferred, and not promised** |

**Why T3 is deferred, said plainly.** A colour-grading timeline is a spatial, continuous, real-time instrument. Making a chroma disc, a Bézier curve and a 4K monitor genuinely *usable* through a linear speech channel is a research problem, not a sprint — and a user who trusts an unverified accessibility claim and loses an afternoon to it has been actively harmed. T1 and T2 are the honest, verifiable subset, and the prerequisite for T3 whenever someone takes it on.

**Rejected:** promising full screen-reader support (unverifiable at our test capacity); egui's built-in experimental screen reader (forks our semantics away from the AccessKit tree kittest tests — one tree, one contract); and waiting for the egui upgrade before starting (T1 costs nothing on 0.29).

---

## 3. Keyboard access

**R-1 — Declaration order is focus order, and must match visual order.** egui derives the tab ring from widget declaration order. If the ring is wrong, the layout code is wrong.

**R-2 — Regions are reachable in O(1); items are not reached by Tab.** `F6`/`Shift+F6` cycles video-mode regions: media pool → timeline → program monitor → transport → right drawer → node canvas. `Tab` moves *within* the focused region.

This **amends [13 §1.6](13-ux-components.md)**, which requires every clip control to be Tab-reachable. Taken literally a 500-clip sequence becomes a 500-stop tab ring — denial of service with good intentions. Correction: **track-header controls are in the ring; clips are not.** Clip selection is by *navigation* — `↑`/`↓` change track, `Shift+←`/`Shift+→` move by edit point (already in [04 §5.1](04-ui-mode-timeline.md)), `Enter` selects under the playhead. This is how every NLE does it and the only model that survives scale.

**R-3 — Focus is always visible.** A 2px `primary` ring painted outside the widget rect, never fill-only, never suppressed by a custom paint path.

**R-4 — No focus traps without an exit.** Any surface consuming `Tab` internally must release on `Esc`.

**R-5 — Keyboard handling is never gated on pointer position.** Gate on `Response::has_focus()`, never `rect_contains_pointer`, never `memory().focused().is_none()`. **Both anti-patterns exist today** (`node_editor.rs:1379`, `color_page.rs:855`) and this rule names them as bugs.

**R-6 — `Esc` is a three-level ladder**, consuming exactly one level per press: (1) cancel and revert an in-flight gesture, pushing **no** history entry; (2) clear the focused surface's selection; (3) surrender focus to the containing region, or dismiss the topmost transient overlay. `Esc` never closes a document, never discards unsaved work, and never dismisses a [36](36-error-model.md) `Fatal` modal.

**R-7 — Cancellation is total** — no partial mutation, no orphaned wire, no undo entry.

### 3.1 A keyboard path for every custom drag control

**Colour wheels.** Arrows move the chroma tip 1% per press; `Shift` 10%; `Ctrl` 0.1%; `Home` resets to neutral (the existing double-click reset); `Enter` moves to the numeric readouts; `Esc` surrenders focus. **The keyboard path routes through the same `chroma_to_deltas` mapping as the drag**, so a nudge and a drag to the same tip produce bit-identical values — two paths that agree by accident will stop agreeing.

**Curve editor.** `Tab`/`Shift+Tab` cycles the selected point in ascending x — **this is the missing piece**, since today a point can only be acquired with the pointer. Arrows nudge 0.005, `Shift` 0.05, `Ctrl` 0.001 (nudge already implemented). `Insert` adds a point at the largest x-gap; `Delete` removes (endpoints not removable, already implemented); `Home`/`End` select first/last; `Esc` deselects then releases. **The `keyboard_captured` gate is deleted and replaced with `resp.has_focus()`** — its purpose (make typing safe) is legitimate but solved at the wrong layer, since focus-scoped handling *is* that mechanism.

**Node canvas.** `Tab`/`Shift+Tab` cycles nodes in `(y, x)` order; **`Space` toggles the focused node's membership in the selection — this is the keyboard marquee**; `Ctrl+A` selects all; arrows nudge 8px, `Shift` 1px; `Delete` removes the selection except `Output`; `Esc` follows the ladder. **All gated on canvas focus, not `rect_contains_pointer`.** Wire creation: `Ctrl+Enter` enters connect mode, arrows cycle ports, `Tab` cycles targets, `Enter` commits, `Esc` cancels — with [08](08-fusion-node-flows.md) port-type filtering, so an incompatible connection is unreachable rather than rejected after the fact.

*Rejected: a keyboard rubber band.* It is a two-corner modal interaction needing its own state, cancel semantics and tutorial, to reproduce what additive `Space`-toggle gets in fewer keystrokes. Marquee is a *pointer* affordance; the keyboard's equivalent is enumeration, not imitation.

**Align/distribute get toolbar buttons and command-palette entries** operating on the selection. Keyboard selection without keyboard operations on it is a dead end, which is why they ship together.

**Timeline editing** ([04 §5.1](04-ui-mode-timeline.md) covers transport thoroughly and editing-by-drag not at all). Added in the `Alt` family, mode-gated by 04 §5.2: `Alt+←`/`→` move by frame, `Alt+Shift` by second, `Alt+↑`/`↓` by track; `,`/`.` trim the selected edge by one frame, `Shift` by five; `[`/`]` extend/retract in/out **to the playhead** — a long drag in one keystroke. **Keyboard moves run through the same snapping engine as drags**, honour the snap toggle, and paint the same guide.

**The rest:** pan knob and EQ handles take the same arrow/`Shift`/`Ctrl`/`Home` scheme as the wheels — [13 §11.8](13-ux-components.md) is right that this is one findable pattern, not three separate bugs. Splitters become focusable with arrow resize. Scopes, meters and waveforms are read-only and stay out of the tab ring, but **must still emit `widget_info`** — a control that cannot be operated still has to be readable.

**R-8 — No new pointer-only interaction ships.** A PR adding a `dragged()` branch without a focus-gated keyboard branch is incomplete. This is the rule that stops §3 decaying the week after it lands.

---

## 4. Minimum hit-target size

**R-9 — Every interactive target is at least 24 × 24 logical pixels**, from **WCAG 2.2 SC 2.5.8 (AA)**. Not 44 (SC 2.5.5, AAA) — 44px targets in a track header would push timeline density below the point where the panel does its job, and a spec mandating a number the app cannot honour gets ignored wholesale. **28 × 28 recommended for standing chrome.**

The *visual* element may be smaller than its hit rect: inflate with `Rect::expand` at interaction time, paint at the design size.

Exemptions, and only WCAG's own — **inline** (sized by surrounding text), **equivalent** (same function on a conforming target, and **the exemption must name the equivalent**), **essential**, and **spacing** (sub-24px acceptable if 24px circles centred on each target do not overlap).

| Target | Today | Ruling |
|---|---|---|
| Clip trim handles | 6px hit zone | Paint 6px, hit **12 × 24**; also discharged by the `,` `.` `[` `]` equivalents |
| Marker diamonds | Glyph-sized | Inflate to 24 × 24; overlaps resolve by hover list, not a smaller target |
| Node ports | Socket-sized | Inflate to 20px under the spacing exemption; keyboard equivalent above |
| Track height-drag handle | Row-edge strip | Minimum 8px under spacing exemption; keyboard equivalent via the splitter rule |
| Curve control points | 10px hit radius | Acceptable under spacing exemption; keyboard equivalent above |

`egui::style::Spacing::interact_size` defaults to 40 × 18 — **raise the height to 24** so every stock widget clears the floor without per-call-site work. One line in `theme.rs`, and the cheapest item in this document.

---

## 5. Contrast

**R-10 — WCAG 2.1 AA, measured:** 4.5 : 1 body text · 3 : 1 large text · 3 : 1 interactive boundaries and state indicators (SC 1.4.11) · 3 : 1 meaningful graphical objects (status strips, meters, badges, scope traces) · 3 : 1 focus indicator against both the control and its surround.

**R-11 — The gate is a test, not a review.** A unit test parses DESIGN.md's `colors:` frontmatter and evaluates a declared table of `(foreground, background, role)` pairs — the same doc-drift mechanism [40](40-spec-verification.md) generalises. **Adding a token without adding its pair rows fails the test**, so the table cannot fall behind the palette. Purely decorative separators are exempt **and must be declared exempt by name**; silence is not an exemption.

### 5.1 The existing palette fails

Measured from DESIGN.md's own hex values, **seven token/surface pairs are below AA**:

| Pair | Ratio | Verdict |
|---|---|---|
| `secondary` #7A7A9A on `surface-elevated` | **4.45** | Fail |
| `secondary` on `surface-widget` | **4.15** | Fail |
| section-header #50506E on `surface` | **2.51** | Fail badly |
| section-header on `surface-elevated` | **2.38** | Fail badly — and this is every drawer heading in the app |
| `border` #1E1E32 on `surface` | **1.19** | Fail as a control boundary; exempt as decoration |
| `light-error` #C83C3C on `light-surface` | **4.49** | Fail (by 0.01 — a fail is a fail) |
| `light-warning` #B47800 on `light-surface` | **3.33** | Fail |

`on-surface` (15.99), `error` (7.04), `warning` (11.66), `success` (9.35) and the light-theme body text all pass comfortably.

**R-12 — Fix the tokens; do not exempt the failures.** Five hex values, all inside the existing hue family. The alternative — a table of exemptions — converts a design system into a list of excuses.

| Token | From | To |
|---|---|---|
| `secondary` | `#7A7A9A` | **`#8A8AA8`** — 5.82 / 5.51 / 5.14 across the three surfaces |
| section-header `#50506E` | — | **deleted**; use `secondary` at `RichText::small()`. Heading recession should come from **size and letterspacing, not a contrast failure** — a fourth grey buys 0.13 units of hierarchy at the price of an AA violation |
| `border-interactive` (new) | — | **`#666690`** — 3.16 vs `surface-widget`. `border` #1E1E32 stays for panel chrome, declared decorative-exempt |
| `light-warning` | `#B47800` | **`#8F5E00`** |
| `light-error` | `#C83C3C` | **`#B02D2D`** |

**R-13 — `primary` #6E56CF is not a text colour.** At 3.61 : 1 it is right for its actual job (selection stroke, focus ring — all ≥ 3 : 1) and wrong for the one DESIGN.md also assigns it: links and the wordmark. **Those move to `primary-hover` #9077E0** (5.47 : 1). `primary` keeps every non-text use.

**R-14 — `primary` on `primary-dim` is 2.02 : 1, and that is fine.** The selected-state idiom is measured against what the border *separates from* — surrounding `surface`, at 3.61 : 1. Recorded so a future audit does not "fix" a non-problem.

---

## 6. Reduced motion

**R-16 — Reduced motion governs chrome. It never governs content.**

That settles the [06 §5.2](06-captions-ai.md) question, and settles it the other way from the obvious answer: **caption animations are NOT suppressed.** They are baked into the exported master; suppressing them in preview would break WYSIWYG in exactly the place [06](06-captions-ai.md) works hardest to preserve it. A preview that lies about the output is a worse accessibility outcome than a preview that moves.

What reduced motion changes for captions is the **editor's** behaviour, which is chrome: the caption editor's looping autoplay is disabled and the cue renders frozen at its midpoint; a "freeze animation preview" toggle defaults on under the flag; and applying `Typewriter` or `FadeWords` surfaces a one-line note that motion-sensitive viewers cannot turn this off. An authoring tool informs the author about their audience; it does not silently overrule them.

**R-17 — Every animation with a non-zero duration reads `prefs.reduced_motion`.** No exceptions. Drawer tween and radial wheel are **already correct**. Remaining: timeline zoom/scroll smoothing → instant; playhead follow → page-jump at the boundary; monitor frame-swap crossfade → hard cut; first-run overlay and shortcut sheet → instant; toast enter/exit → instant appear, **dwell time unchanged** (never shorten the time to read something); snap-guide flash → a solid guide held for the snap's duration (**strictly better for everyone** — a flash is the least legible way to signal a snap); recording/buffering pulse → static colour plus text; transport spinner → static glyph plus the word "Buffering"; [33](33-timeline-preview-render.md) chunk-strip → discrete state changes, no animated wipe.

**R-18 — Reduced motion never removes information.** Every entry keeps the signal in static form or replaces it with text. If the only way to know something is happening is that it moves, the design is wrong for everyone and reduced motion merely made it visible.

**R-19 — Seed from the OS** (Windows `SPI_GETCLIENTAREAANIMATION`, macOS `accessibilityDisplayShouldReduceMotion`, GNOME `enable-animations`) **on first run only**; an explicit user setting always wins.

---

## 7. Colour-only information

**R-20 — Colour is never the sole carrier of meaning.** The test is mechanical: *render greyscale; is the state still readable?*

| Where | Colour-only today | Non-colour affordance |
|---|---|---|
| Offline media, transition badges, track enable/mute/lock | — | **Already conformant** — stripes, corner triangles, icons. 13 §1.6 got these right; they are the model for the rest |
| Clip disabled | ~50% opacity | Reads as "dim", not "off" — add a diagonal strike on the name label |
| **Marker categories** ([01 §4.1](01-data-model.md)) | colour is the **only** ruler distinguisher | **`MarkerCategory` gains `glyph`** (diamond/circle/square/triangle/flag/bar — six is the limit at ruler scale), plus category name in tooltip and list. **This is a [01](01-data-model.md) data-model amendment and a serialization change**, flagged as such rather than smuggled in as a UI detail |
| **Preview-chunk strip** ([33 §6](33-timeline-preview-render.md)) | red/amber/green only | **Fill pattern per state:** not-rendered = outline only; rendering = diagonal hatch; rendered = solid; **stale = cross-hatch** (stale and not-rendered are different conditions and must not share an appearance). Plus a text count in the gutter. This also relieves 33's open question about a new "rendering" colour token — with a pattern carrying state, it isn't needed |
| **Node port types** | three socket colours | **Socket shape:** circle = Image, square = Mask, diamond = Value; colour becomes reinforcement. Also closes [13 §16 Finding 3](13-ux-components.md) — the missing Mask token stops being load-bearing |
| Audio meters + loudness ceiling | gradient + coloured zone | Numeric LUFS/peak readout plus a 1px ceiling tick. The gradient is a sanctioned DESIGN.md fill exception; it may not also be the only signal |
| Diagnostic badges ([36 §4](36-error-model.md)) | red/amber tint | Severity glyph per `Severity`. `Diagnostic` already carries severity as a variant, so this is a render decision, not a model change |
| Karaoke active/inactive | colour swap | **Content, not chrome — out of scope by R-16.** The `Underline` mode already exists as the non-colour option; the editor should say so when a colour-only mode is chosen |

**R-21 — New states declare their non-colour affordance at design time.** A component spec listing a state colour and no second channel is incomplete, and this is the line a UX reviewer cites when rejecting it.

---

## 8. Acceptance

1. **No pointer-only function exists in video mode** — an enumerated inventory of every `dragged()`/`clicked()` site in `panels/video/` maps to a keyboard path, checked in as a fixture so a new drag site with no mapping fails CI.
2. **A full editing pass completes with the mouse unplugged**, scripted through `egui_kittest`: import → place three clips → trim two edges → move one clip a track down → grade with a wheel offset and a curve point → connect two nodes → align three → export. Key events only.
3. **No keyboard handler is gated on pointer position or global focus-emptiness** — a grep gate rejects `rect_contains_pointer` and `memory().focused().is_none()` inside key-handling blocks. The two known violations must be green before this passes.
4. **Keyboard and pointer produce identical values** for wheel, curve, pan knob and EQ handle — one shared mapping function per control, asserted bit-identical.
5. **`Esc` obeys the ladder** — cancelling a clip, node or wire drag leaves the document byte-identical and **adds no history entry**, asserted against the revision counter.
6. **Focus order matches visual order** per panel; **`F6` reaches every region** in declared order and wraps.
7. **Every focusable control paints a visible focus ring**, snapshot-tested in both themes.
8. **No interactive target is under 24 × 24** unless declared exempt, and every "equivalent" exemption **names its equivalent**.
9. **The contrast test passes** across the full declared table in both themes with the §5.1 replacements applied; a new token without pair rows fails.
10. **Every animation honours `reduced_motion`**, asserted by two consecutive identical frames — and **caption animation is excluded by name**, with a test asserting it is *still animating* under the flag so a future well-meaning change cannot quietly break WYSIWYG.
11. **Every §7 state survives greyscale**; the chunk strip's four states are mutually distinguishable desaturated.
12. **The AccessKit tree is populated** — every custom control produces a node with a non-empty name and a non-default role; kittest addresses wheel, curve, canvas, timeline and transport **by label**. An unnamed node in a video-mode frame fails.
13. **`Response::labelled_by` is never called with an unregistered `Id`** — a debug assertion at every call site, since this panics.
14. **The claim matches the product** — release notes state keyboard operability and an accessibility tree, and **do not claim screen-reader support** until T3 is actually tested against a real AT.

---

## 9. Sequencing

| Order | Item | Gate |
|---|---|---|
| 1 | **Fix the two inverted gates** — `color_page.rs:855` (`keyboard_captured` → `has_focus`), `node_editor.rs:1379` (`rect_contains_pointer` → canvas focus) | None. Two lines, restoring keyboard paths that were *written and then disabled by their own guard* |
| 2 | **DESIGN.md token fixes** + contrast test + `interact_size` height → 24 | None. Five hex values, one test, one theme line |
| 3 | **Reduced-motion contract** — audit, wire the remainder, add the frame-identity test | None |
| 4 | **Focus order, focus ring, `F6` regions, the `Esc` ladder** | The foundation §3.1 stands on |
| 5 | **Keyboard paths for the custom controls** + the timeline `Alt`/`,.`/`[]` family | 1, 4 |
| 6 | **Hit-target audit** — inflate, declare exemptions, rect-walk test | 5 discharges the "equivalent" exemptions |
| 7 | **Non-colour affordances** — chunk-strip patterns, port shapes, severity glyphs, `MarkerCategory.glyph` | The glyph field **landed in [01 §2](01-data-model.md) on 2026-07-20**; it still needs a serialization migration. The rest are render-only |
| 8 | **egui 0.29 → 0.35** | Independent of 1–7, blocking 9 |
| 9 | **Enable `accesskit`; emit `widget_info`; convert acceptance to kittest label queries** | 8 |

**Steps 1 and 2 are the highest value per line in this document** and neither depends on anything. A curve nudge that stops working when the widget is focused, and a shortcut that requires the mouse to hover, are not gaps in coverage — they are the specific, verifiable defects that having no owning contract produced. Everything after is the contract that stops the next two from being written.

Step 9 is where this stops being a keyboard spec and becomes testable UI infrastructure: the same AccessKit tree a screen reader consumes is the one `egui_kittest` queries — the argument that should carry it past the first sprint where something else looks more urgent.
