# 213 — Social-First Editing Velocity (AS-1 CapCut-Class UX)

> **Status: Implemented (wave complete 2026-08-03).**  
> OpenCut’s product thesis is CapCut simplicity for social clips. Photonic’s
> [AS-1](../specs/video-editor/00-overview.md) is the same story with a pro
> engine underneath. This proposal is a **velocity checklist** so AS-1 does not
> require pro-NLE literacy. Clean-room under
> [207](207-opencut-harvest-index.md) §2.

**Shipped:** empty-timeline Import hero + social format chips; coach marks
(Import → Split → Export); auto-place import (prefs default on); Captions/Export
on timeline toolbar; Social labels on format bar; monitor Import CTA; structural
test `ui_as1_social_path_structural_arm`; **caption Looks chips** (Clean /
Karaoke / Social); **Quick Grade** strip (Exposure / Contrast / Saturation) on
the Color Controls drawer with full correctors remaining below.

**Owner refs:**  
- [00](../specs/video-editor/00-overview.md) AS-1 Social clip  
- [29](../specs/video-editor/29-qa-spec.md) acceptance stories  
- [05](../specs/video-editor/05-import-export.md) import/export  
- [06](../specs/video-editor/06-captions-ai.md) captions  
- [04](../specs/video-editor/04-ui-mode-timeline.md), [13](../specs/video-editor/13-ux-components.md)  
- [210](210-timeline-interaction-velocity-pack.md), [211](211-keyframe-graph-editor.md),
  [212](212-keymap-schema-migrations.md)  

**Territory:** mostly `panels-video` + onboarding. **Effort:** M cumulative.  
**Format impact:** none.

---

## 1. Problem and user outcome

Photonic can already execute AS-1 *in principle* (import → cut → caption →
reframe → grade → export). The risk is **friction density**: too many pro
panels, weak empty states, and multi-step paths for actions CapCut folds into
one gesture.

**After 213.** A new user (or agent driving MCP) can complete AS-1 in a guided
path with:

- obvious first actions on empty timeline,  
- one-click aspect-ratio social presets,  
- captions that feel one step away from import,  
- export presets labelled for social platforms,  
- no requirement to open colour page, node graph, or mixer for the happy path.

Pro surfaces remain available; they are not the default path for AS-1.

---

## 2. Happy-path storyboard (normative checklist)

Each row is a **user outcome**. “Owner” is the existing doc/CAP; residual is
what 213 tracks.

| Step | User outcome | Owner | Residual for 213 |
|---|---|---|---|
| 0 | Empty video mode shows **drop media / Import** hero, not a blank grid | 04, 13 | Empty-state card + sample project link |
| 1 | Drag/drop or file picker imports to pool **and** optionally auto-places on V1 | 05, G-15 | “Add to timeline” default toggle |
| 2 | Blade / split + ripple delete discoverable on clip context + keys | 04, 16 | Onboarding tooltips once |
| 3 | **Auto captions** entry from timeline toolbar (not buried) | 06 | One toolbar button + progress |
| 4 | Caption style presets (karaoke / clean / social) | 06 | Preset chips if missing |
| 5 | Sequence format switch 16:9 ↔ 9:16 with per-clip reframe handles visible | CAP-012, 05 | Format chip on monitor |
| 6 | Animated vector title template insert (or honest “templates coming” if P6) | 05 §4b | Do not lie — if unsupported, hide CTA |
| 7 | Quick grade: exposure/contrast/sat only strip | 07 | “Quick grade” strip vs full colour page |
| 8 | Export **Social 9:16** / **Social 16:9** presets one click | 05, K-F* | Labels + default folder |
| 9 | Entire path scriptable via MCP for AS-1 harness | 10, 29 | Close any missing tool on the path |

---

## 3. UX principles (normative)

1. **Progressive disclosure.** Default video layout: media pool, monitor,
   timeline, inspector (clip). Colour, nodes, scopes behind explicit open.  
2. **One primary CTA** in empty and export states.  
3. **Social labels** on formats and export presets (human names, not only
   codec strings).  
4. **Agent parity.** Every GUI CTA in the AS-1 path has an MCP equivalent
   (CAP-019).  
5. **No fake features.** If vector title library is NotSupportedV1, the empty
   state must not advertise it (00 / MCP already refuse).

---

## 4. Onboarding (lightweight)

- First-run in video mode: 3-step coach marks (Import → Split → Export),
  dismissible, stored in `AppPreferences` (not document).  
- “Reset coach marks” in prefs.  
- Reduced motion: static highlights (41).

Non-goal: full tutorial video player, account-gated lessons.

---

## 5. Metrics for “done” (acceptance)

Not vanity analytics — **harness and timebox**:

| Gate | Criterion |
|---|---|
| A1 | 29 QA-1 script arm AS-1 still 100% green |
| A2 | New GUI path test: empty → import fixture → split → export preset invoked (structural, like `video_ui_paths`) |
| A3 | Manual: cold user completes AS-1 in ≤ 10 minutes with coach marks (checklist artifact per 29 manual-row rules) |
| A4 | No AS-1 step requires opening Fusion/node page or full colour page |

---

## 6. Non-goals

- Becoming a mobile editor.  
- Template marketplace.  
- Generative stock media (OpenCut fal.ai path).  
- Replacing pro NLE features.  
- Web deployment.

---

## 7. Relationship to other harvest items

| Item | Role in AS-1 |
|---|---|
| 210 envelope + bookmarks | Faster audio trim + chapter jumps for social |
| 211 graph editor | **Not** required for AS-1 happy path (progressive disclosure) |
| 212 keymap migrations | Ensures social shortcuts reach upgrades |
| 208/209 | Quality under the hood; invisible to AS-1 unless glow/blur used |

---

## 8. Provenance

Product thesis shared with CapCut-class tools (including OpenCut’s “simple +
private + free” positioning). Implementation is Photonic UI + existing CAPs
only — no OpenCut screens or copy required.
