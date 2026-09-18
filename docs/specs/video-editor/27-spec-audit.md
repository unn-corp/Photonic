# 27 — Spec-Set Audit: Contradictions, Drift, Unowned Capabilities, and Missing Coverage

**Status:** Draft — audit findings. **All `SD-*` rows and the P2 `A-*` findings were applied to their owning documents on 2026-07-20.** Every remaining P0 and P1 finding now has an **owning specification** (see §8); none is still homeless, which was the original complaint. No code authorization.
**Date:** 2026-07-20
**Audience:** Photonic product owner, maintainers, owner-doc authors, implementation agents

**Depends on:** the whole spec set ([SPEC.md](SPEC.md), [00](00-overview.md)–[26](26-kdenlive-mlt-parity.md), [ROADMAP.md](ROADMAP.md)).
**Owns:** the cross-cutting audit inventories `A-*` (contradictions), `SD-*` (spec-vs-code drift), `O-*` (unowned capabilities), `U-*` (under-specified contracts), and `MC-*` (never covered).
**Does not own:** any feature. Every finding is **assigned to an existing owner doc** for resolution; this document is the tracker, not the authority. It deliberately holds no design decisions of its own.

---

## 1. Why this document exists

[26](26-kdenlive-mlt-parity.md) closed the last external parity gap. Auditing the spec set to produce it surfaced a different class of problem: the design layer ([00](00-overview.md)–[13](13-ux-components.md)) is thorough on **mechanism** and thin on **operations**, several docs describe a pre-implementation world the code has overtaken, and a handful of contracts contradict each other in ways that are load-bearing for colour correctness and SS-3 determinism.

None of these belong to a feature backlog, so none of them had anywhere to live. They were being rediscovered by every reader and fixed by none. This document gives them IDs, evidence, and an owner.

**Verification status.** Every finding was checked against the cited document. The first draft of this audit then **failed its own standard on code claims**: an adversarial re-verification on 2026-07-20 refuted six findings that alleged defects in `crates/` (A-1, A-2, A-5, A-6, SD-9, MC-4, MC-9) and one repository claim (H-1). Those are now rewritten in place, each carrying a note saying what the earlier version got wrong — including one, A-1, where the real defect turned out to have the **opposite polarity** from the one filed.

The lesson is recorded rather than quietly fixed, because it generalises: **claims about documents are cheap to verify and claims about code are not, so an audit drifts toward asserting the latter on inference.** Anyone extending this document should re-derive every code citation before adding a finding. Line numbers are from 2026-07-20 and will drift.

### Severity

| | Meaning |
|---|---|
| **P0** | Produces wrong output, or blocks a locked guarantee (SS-1/SS-3/CAP-019). Fix before the affected subsystem ships. |
| **P1** | Will cause an implementer to build the wrong thing, or leaves a shipped surface undefended. |
| **P2** | Documentation drift. Cheap to fix, misleading until fixed. |

---

## 2. `A-*` — contradictions between documents

### A-1 · P0 · The live canvas composites in gamma; headless composites in linear

> **This finding was rewritten on 2026-07-20.** Its first version claimed blend math ran on premultiplied, linear-light operands against an sRGB-encoded canvas. **That was wrong in both halves** — `graph/ops.rs:230-244` already unpremultiplies and re-premultiplies, and `pipeline.rs:586-590` states the canvas *also* blends in linear ("both inputs are sRGB textures, so sampling decodes to linear and the target re-encodes"). The genuine defect is real, adjacent, and has the **opposite polarity**.

- **The defect:** `COMPOSITE_SHADER`'s correctness depends on its render target being **sRGB-encoded**, so the hardware decodes on sample and re-encodes on write and the blend arithmetic lands in linear light. That holds headless — `headless.rs:25` pins `FORMAT = Rgba8UnormSrgb`. It **does not hold on the live canvas**: `renderer/mod.rs:71-79` deliberately selects a **non-sRGB** surface (`Bgra8Unorm | Rgba8Unorm`), and `renderer/effects_renderer.rs:17` allocates every isolation/backdrop texture as `format: self.surface_format`. So on screen the same shader blends **gamma-encoded** values.
- **Consequence:** the 22 backdrop-read/isolation blend modes composite differently on the live canvas than in export — the precise inverse of the canvas-equals-export guarantee [03 §2.4](03-render-color-pipeline.md) claims (issue #145). Separable fixed-function modes are unaffected; this hits exactly the modes that route through the isolation pass.
- **Why no test catches it:** the golden corpus renders headless, where the format is correct by construction.
- **Recommended resolution (2026-07-20).** The intent is already recorded and is not in doubt: `pipeline.rs:586-590` says blending runs in linear, citing issue #145 and canvas-equals-export identity. Headless honours it; the live canvas defeats it. Three facts constrain the fix — the canvas renders **directly to the swapchain view** (`frame_manager.rs:115`), `view_formats` is empty everywhere, and **egui shares that surface** (`main.rs:517` constructs its renderer with `surface_format`), which is why a non-sRGB surface was chosen in the first place.
  **Render the document to an offscreen target matching headless (`Rgba8UnormSrgb`, or float), then present that into the egui surface.** This makes canvas and headless bit-identical *by construction* rather than by keeping two formats in agreement, decouples document rendering from swapchain-format availability and from egui's conventions, and is what the **video** path already does — `EngineFrame` is an offscreen `Rgba16Float` texture presented into the monitor. The vector canvas rendering direct-to-swapchain is the outlier, not the norm.
  Rejected alternative: an sRGB `view_formats` view of the swapchain. It is cheaper, but it leaves egui and the document sharing one target with opposing colour expectations — the fragile arrangement that produced this bug.
- **Owner:** [03](03-render-color-pipeline.md). **Fix:** adopt the offscreen target; state the operand rule (§below); add a canvas-vs-headless parity fixture using a non-separable blend mode.
- **⚠ This does NOT gate [26 K-0.3](26-kdenlive-mlt-parity.md#8-k-0--foundations).** An earlier draft coupled them; that was wrong. A-1 is a defect in the **vector/raster canvas** compositor, whose operand encoding depends on the swapchain format. K-0.3 is the **video graph's** `Merge`, which operates in `Rgba16Float` linear premultiplied throughout (D-09, `eval.rs:38 WORKING_FORMAT`) — an unambiguous space with an already-decided CPU reference (`ops.rs::merge_pixel`: unpremultiply → `blend_rgb` → backdrop-blend → premultiplied source-over). **K-0.3 is unblocked today**, and so is anything downstream of it.
- **Distinct from [26 E-9](26-kdenlive-mlt-parity.md#e-9--cpugpu-evaluator-equivalence-as-a-bug-class)**, which is that the *video graph's* GPU `Merge` ignores `mode` entirely (`graph/eval.rs:319`). Two different defects in two different compositors; both real, neither the one originally filed here.

### A-2 · P2 · `03`'s account of `COMPOSITE_SHADER` wiring is stale

> **✅ Applied to the owning document, 2026-07-20.**

> **Withdrawn as a P1 in favour of doc drift.** The first version asked whether D-10 could be considered met, on the strength of [03 §2.4](03-render-color-pipeline.md)'s claim that `blend_mode_index` is *“validated but unreferenced from any live pass.”* **That claim is false in the code.**

- **Verified live path:** `renderer/mod.rs:602` builds `composite_pipeline` from `COMPOSITE_SHADER` against the live `surface_format`; `scene_renderer.rs:384` sets it; `blend_mode_index` is called at `scene_renderer.rs:225` and `:267`. The canvas performs real per-layer isolation with backdrop ping-pong. **D-10 is satisfied at layer granularity.**
- **The residual is stale documentation:** 03's line citations have drifted (`COMPOSITE_SHADER` is `pipeline.rs:592`, not `:577`; `blend_mode_index` is `:544`, not `:529`), the `#[allow(dead_code)]` it cites no longer exists, and the “unreferenced from any live pass” sentence is simply out of date. Per-*node* segment isolation does remain unreferenced on the canvas (`segments_need_isolation` is called only from `headless.rs`).
- **Owner:** [03](03-render-color-pipeline.md). **Fix:** refresh the citations, delete the unreferenced claim, and narrow the open item to per-node segment isolation. [26 §8](26-kdenlive-mlt-parity.md#8-k-0--foundations)'s D-10 hedge should be removed to match.

### A-3 · P0 · Grade operators apply transfer functions to premultiplied alpha

- [07 §3](07-color-grading.md): *“All ops operate on **premultiplied** linear-Rec.709 `Rgba16Float` pixels unless stated otherwise.”* The CDL, contrast-pivot, curve-LUT and 3D-LUT formulae that follow then apply per-channel non-linear functions with `clamp01` and **no unpremultiply**.
- [03 §3.5](03-render-color-pipeline.md) does the opposite at export — it explicitly unpremultiplies **before** the OETF, because that is the correct order.
- **The defect:** for any partially transparent pixel — every vector title in AS-3, every keyed edge in AS-2 — CDL offset, contrast pivot and LUT lookup operate on alpha-attenuated values. Result: edge fringing, and a grade that visibly changes as opacity is keyframed. [07 §6](07-color-grading.md) mandates GPU/CPU operation-order parity, so **both paths will be wrong identically** and the goldens will agree with each other.
- **Owner:** [07](07-color-grading.md). **Fix:** specify unpremultiply → grade → repremultiply inside the `Grade` op, mirroring [03 §3.5](03-render-color-pipeline.md) step 2. Add a partial-alpha grade fixture to [11](11-testing-phasing.md) — the current corpus cannot catch this.

### A-4 · P1 · MCP binds one document; the GUI has tabs

- [10 §2](10-mcp-tools.md): *“**Single-document assumption holds.** `AppState.document` is one `Arc<Mutex<Document>>`… `EngineSession` is therefore also a singleton per `AppState`, **no `session_id` arg anywhere**.”*
- [04 §1](04-ui-mode-timeline.md): timeline state is *“**Per-tab, not global** — lives on the tab record… because a user may have a vector-only tab and a video-project tab open at once”*, with `playhead` per-tab.
- **The defect:** with two tabs open, every MCP video tool silently targets whichever document `AppState` holds. `play`/`seek`/`render_frame_at` cannot address the other tab, and **CAP-019's “MCP outputs equal GUI outputs” becomes unverifiable** whenever the GUI has two projects open. Neither document acknowledges the other.
- **Owner:** [10](10-mcp-tools.md), with [04](04-ui-mode-timeline.md) concurring. **Fix:** state that MCP binds to the **active tab** and define how a tab switch re-binds `EngineSession` — or add a `document_id` argument. The former is almost certainly right; it just has to be written down.

### A-5 · P2 · `10`'s `set_clip_speed` row contradicts the shipped code

> **✅ Applied to the owning document, 2026-07-20.**

- [10 §3](10-mcp-tools.md): `set_clip_speed` — *“`SpeedMap::Constant` only in v1 (01 §5.1); ramps rejected with `NotSupportedV1`.”*
- [01 §5.1](01-data-model.md) specifies `SpeedMap::Keyframed { keys }` with Linear/Bezier ramp integration and declares the current code contract normative. Verified in code: `SpeedMap::Keyframed` exists with exact-rational `Hold` integration and eased ramps.
- **The defect is documentation only.** The handler already builds `SpeedMap::Keyframed { keys: resolved }` (`handlers/video.rs:1862`), args accept a key list, and `mcp_parity_round2.rs::set_clip_speed_accepts_a_keyframed_ramp` covers it. `NotSupportedV1` is never returned for this. Two stale rows in [10](10-mcp-tools.md) describe a restriction that does not exist — which is worse than harmless, because it tells an agent author the capability is unavailable.
- **Owner:** [10](10-mcp-tools.md). **Fix:** delete both stale rows.

### A-6 · P2 · `05` specifies a `RelinkAsset` shape that was never adopted

> **✅ Applied to the owning document, 2026-07-20.**

- [01 §10](01-data-model.md): `RelinkAsset { asset, old_path, new_path }`.
- [05 §2.2](05-import-export.md): *“a `TimelineCmd` variant `RelinkAsset { asset, new_path }`… no data-model shape change needed.”*
- **The defect is documentation only.** The shipped command carries `old_path` (`timeline/commands.rs:379-383`) and its inverse swaps the two (`:2030-2036`), with tests. [05](05-import-export.md) describes a two-field shape that was never built — no live breakage, but an implementer following 05 would remove invertibility.
- **Owner:** [05](05-import-export.md). **Fix:** adopt [01](01-data-model.md)'s shape. Feeds [26 K-C6](26-kdenlive-mlt-parity.md#k-c6--relink-offline-media).

### A-7 · P1 · Scopes tap two different textures

- [03 §3.6](03-render-color-pipeline.md): scopes read *“after the `Grade` node, before `CaptionOverlay`/`Output`… a stable single tap point regardless of how many tracks fold above it”* — a sequence-level tap.
- [07 §5](07-color-grading.md): *“the **selected clip's** texture after its `Grade` node, before `CaptionOverlay` **and the track fold**”* — a per-clip, pre-fold tap, with a sequence-output fallback.
- These are **different textures whenever more than one track is active**, and a colourist reading the wrong one grades against a composite they are not adjusting.
- **Owner:** [03](03-render-color-pipeline.md) declares itself owner of readback points; [07](07-color-grading.md) owns scope interiors. **Fix:** 03 adopts 07's per-clip-with-fallback wording. Implements as [26 K-E2](26-kdenlive-mlt-parity.md#k-e2--per-clip-scope-tap).

### A-8 · P1 · Track height is “not undoable” against an absolute constraint

- [SPEC.md](SPEC.md): *“Every document mutation, **without exception**, is undoable through the existing command history.”*
- [01 §4](01-data-model.md): `height_px: f32, // UI-only but persisted`. [04 §2.3](04-ui-mode-timeline.md): *“direct field write, no command”*, re-affirmed at [04 §9](04-ui-mode-timeline.md) as a resolved position.
- **The defect:** `height_px` is serialized **inside `Document`**, so it *is* a document mutation. The constraint says “without exception” and there is an exception.
- **Owner:** [SPEC.md](SPEC.md) + [01](01-data-model.md). **Fix:** either move the field to session state (cleanest — it is genuinely UI state), or amend the constraint to name persisted-UI-preference fields as a bounded exception. The current position is defensible; it is just not what the SPEC says.

### A-9 · P2 · Two docs assert an HDR non-goal that D-09 repealed

> **✅ Applied to the owning document, 2026-07-20.**

- [07 §7](07-color-grading.md): *“**SDR-only v1** — confirmed, matches SPEC non-goal ‘HDR delivery (PQ/HLG output), 10-bit export pipelines’ (`SPEC.md:98`).”* **That sentence no longer exists in SPEC.md.**
- [05 §6.1](05-import-export.md): *“No color-space conversion choice is exposed in v1 (HDR/PQ/HLG is a SPEC non-goal).”*
- Both are overtaken by **D-09** and the S3 amendment ([23 §S3](23-legal-open-source-implementation-routes.md#s3--d-13-hdr-and-10-bit)), which put HLG/PQ decode, Rec.2020 working state, HDR scopes and 10-bit delivery **in** scope. [03](03-render-color-pipeline.md) carries the D-09 overlay correctly — the two *consuming* docs are stale, and they are precisely where HDR must be handled (scopes, export tagging).
- **Owner:** [07](07-color-grading.md), [05](05-import-export.md). **Fix:** replace both with the D-09 wording plus a pointer to [22 §7](22-dji-advanced-workflows.md#7-d-13--hdrhlg-10-bit-color-pipeline); add HDR rows to 05's tagging table.

### A-10 · P2 · The MCP tool count is stated four ways and matches nothing

> **⚠ Partly applied, 2026-07-20** — counts are now flagged non-authoritative; generating the tables from `tool_list()` remains open.

- [10 §3](10-mcp-tools.md) “**89 tools**”, [10 §5](10-mcp-tools.md) “**Total: 89 tools**”, [10 §9](10-mcp-tools.md) “once `schema_gen.rs` gains the **84** entries”. Actual: **110** `pub async fn` handlers in `handlers/video.rs`.
- [10](10-mcp-tools.md) also omits verbs the code ships — `insert_edit`, `overwrite_edit`, `lift_edit`, `extract_edit`, `close_gap`, `match_frame`, `link_clips`/`unlink_clips`, `attach_proxy`/`detach_proxy`, `insert_adjustment_clip`, `insert_text_clip`, the four bin tools, `remove_asset`, and the two title-template tools — while [05 §4b](05-import-export.md) promised the last two would be “added to 10's catalog at P6”.
- **Applied:** the counts are now flagged as non-authoritative and the catalogue points at `docs/mcp-api.md`. **Still open:** actually **generating §3's tables from `tool_list()`** under the existing doc-drift gate — the same mechanism that already keeps `docs/mcp-api.md` honest. **This fix currently has no tracker** — [26 X-4](26-kdenlive-mlt-parity.md#x-4--effect-manifest-as-a-versioned-schema) applies the same generate-and-drift-check pattern to the *effect manifest*, not to the MCP catalogue. Either widen X-4 or open an item under [10](10-mcp-tools.md).

### A-11 · P2 · Ring depths and cut-ahead lead conflict three ways

| Source | Ring | Cut-ahead |
|---|---|---|
| [02 §3](02-engine.md), [02 §8](02-engine.md#8-perf-budgets-verified-in-11) | 16 fwd / 4 back | **≥ 500 ms** |
| [25 §1](25-performance.md) | 24 fwd / 6 back | 24 frames |
| Code (`decode/ring.rs:19-21`, `playback/prefetch.rs`) | **24 / 6** | **24 frames** |

The code matches 25. Note also that a 24-frame lead meets 02's 500 ms budget only at ≤ 48 fps — **at 60 fps it is 400 ms**, so 25 silently narrows an 02 budget.
- **Owner:** [02](02-engine.md) defers to [25](25-performance.md). **Fix:** 02 drops its numbers and points to 25; 25 restates the lead **in milliseconds** with the frame-count derivation shown, so the 60 fps case is explicit.

### A-12 · P2 · Transcode tool named two ways

> **✅ Applied to the owning document, 2026-07-20.**

[05 §5](05-import-export.md) calls it `transcode_asset` (twice, including in the CAP-019 parity-risk row); [05 §7](05-import-export.md) and [10 §3](10-mcp-tools.md) call it `transcode_media`; the code registers **`transcode_media`**. 05's parity test therefore names a tool that does not exist. **Owner:** [05](05-import-export.md).

### A-13 · P2 · A stale coordination note reads as an open blocker

> **✅ Applied to the owning document, 2026-07-20.**

[07 §2](07-color-grading.md): *“**Required additive change to 01 §3:** extend `AssetKind` with `Lut3d`…”* — [01 §3](01-data-model.md) already reads `pub enum AssetKind { Video, Audio, Image, VectorDoc, Lut3d }`. Done; delete the note. **Owner:** [07](07-color-grading.md).

---

## 3. `SD-*` — spec-versus-code drift

All verified 2026-07-20. **All rows below were applied to their owning documents on 2026-07-20** — the table is retained as the record of what was wrong and where, so a future reader can tell drift from design. Remaining open work is noted per row where the fix surfaced a real gap rather than only a stale sentence.

| ID | Sev | Claim | Location | Reality |
|---|---|---|---|---|
| SD-1 | P2 | “`crates/photonic-video` **does not exist yet** (P2/P3 create it)” | [03 §1](03-render-color-pipeline.md) | Exists with `graph/`, `decode/`, `audio/`, `export/`, `playback/`, `media/`, `session.rs` and 13 integration tests |
| SD-2 | P2 | Workspace is “currently 7 crates, becomes 8” | [11 §7](11-testing-phasing.md) | Already 8 |
| SD-3 | **P1** | “Bump `CURRENT_FORMAT_VERSION` **2 → 3**; add no-op `V2ToV3` migration” | [01 §9](01-data-model.md), echoed in [11](11-testing-phasing.md) | `document.rs:110` = **4**; `docs/format-versions.md` documents v1–v4. [01](01-data-model.md) is also self-inconsistent — it separately describes a **v3→v4** `anchor_space` migration. An implementer following §9 would author a migration that already exists |
| SD-4 | P2 | CI “has **no `ffmpeg`**… fixtures cannot be generated in CI”, flagged as an open P3 task | [11 §2](11-testing-phasing.md), [11 §5](11-testing-phasing.md) | `.github/workflows/ci.yml` installs ffmpeg on **all three** platforms (apt / brew / choco) |
| SD-5 | **P1** | “**None** of `proptest`, `insta`, `criterion` exist in `Cargo.toml` today” | [11 §8](11-testing-phasing.md) | `proptest` and `criterion` are present. **`insta` is genuinely absent** — so [11 §3.2](11-testing-phasing.md)'s entire IR-snapshot strategy is unimplemented, and the doc's own wording hides that |
| SD-6 | P2 | `EngineCmd` variant list | [02 §1](02-engine.md) | Code additionally has `ScrubSeek`, `SetPreviewTarget`, `SetPreviewQuality`, `SeekSource`, `Shutdown`; `EngineFrame` gained `preview_asset`. None documented |
| SD-7 | **P1** | “Headless/MCP export uses the identical path (**engine owns it, not the GUI**)”; `CancelExport`/`GenerateProxies` listed as engine commands | [02 §1](02-engine.md), [02 §7](02-engine.md) | `Export`/`Probe` are **NotImplemented stubs**; real export runs from `handlers/video.rs:4063` (`run_export_job`) over a **dedicated** `EngineSession` on a frozen snapshot (`:4059-4062`) — so “the GUI owns it” is wrong, but so is “it bypasses the engine”. `CancelExport`/`GenerateProxies` are **not in the enum at all**. Tracked as [26 K-0.1](26-kdenlive-mlt-parity.md#8-k-0--foundations)/[K-0.8](26-kdenlive-mlt-parity.md#8-k-0--foundations); recorded here because 02 asserts the opposite |
| SD-8 | P2 | Frames published “via `triple_buffer`/watch” | [02 §1](02-engine.md) | `arc_swap` (`ArcSwap`/`ArcSwapOption`, `session.rs:322`). Cited downstream by [04 §3.1](04-ui-mode-timeline.md). *(An earlier draft also cited 03 §5; that document does not mention the mechanism.)* |
| SD-9 | P2 | Node cache is “budgeted pool, LRU”, `Output` “pinned” | [02 §5](02-engine.md), [03 §3.4](03-render-color-pipeline.md) | **The docs are right and an earlier draft of this finding was wrong.** `pool.rs` implements a genuine LRU with `last_used`, a `budget_bytes` cap and a `pinned` set, evicting “the least-recently-used **unpinned** entry” (`pool.rs:117,164-169`); `cache.rs:120` exposes `pin`/`unpin`. The only real residual: `cache.rs:110` flushes the 16 384-entry **rendered-validity bookkeeping map** wholesale, which forces re-render of textures that are *still resident in the pool*. Worth a doc sentence, not a redesign |
| SD-10 | P2 | Cache keys: rings `(asset, quality, pts)`; “sequence frames (final)” keyed by `doc_generation`-relevant hash | [02 §5](02-engine.md) | Ring is keyed by `Tick` within a per-`(AssetId, bool)` source; the GPU upload cache is `(AssetId, Tick, bool)`. **There is no sequence-frame cache**, and “`doc_generation`-relevant hash” is defined nowhere in the set |
| SD-11 | **P1** | `at_tc` accepts `HH:MM:SS:FF` “or `HH:MM:SS;FF` **for drop-frame**” | [10 §1](10-mcp-tools.md) | `parse_timecode` does `tc.rfind([':', ';'])` and treats both separators **identically** — no drop-frame renumbering; the ruler formats non-drop only. At 29.97 this drifts ≈ **3.6 s/hour** against a documented contract. Owned as [26 K-A12](26-kdenlive-mlt-parity.md#k-a12--timecode-as-a-first-class-concept) |
| SD-12 | P2 | Master-bus meter → mixer strip and `get_audio_meters` presented as delivered | [09 §5](09-audio-mixer.md), [13 §11.1](13-ux-components.md), [10 §3](10-mcp-tools.md) | `master_level()` returns `None` unconditionally; the mixer master strip is synthetic. This is G-4 + [26 K-0.6](26-kdenlive-mlt-parity.md#8-k-0--foundations) |
| SD-13 | P2 | Declared dependency adoptions: `rubato` “adopt for v1”, `subparse`, pinned `egui-snarl` | [09 §2](09-audio-mixer.md), [06 §7](06-captions-ai.md), [08 §7](08-fusion-node-flows.md) | None appear in any `Cargo.toml`; there is no node-editor module. Audio resampling is **asserted but absent** — `audio/mixer.rs` requires sources to already be at mix rate, with no resampler in the tree (the same gap [26 §4.2](26-kdenlive-mlt-parity.md#42-phase-gated-seams-that-k--items-depend-on) notes for non-1:1 clip speed) |
| SD-14 | P2 | Phase table P1–P8 with P7 “Color page”, P8 “Fusion + full mixer” | [00 §6](00-overview.md), [11 §6](11-testing-phasing.md), [12](12-agent-execution-plan.md) | Grade operators, DSP modules and grade/graph goldens all exist. The phase map no longer describes the build; [ROADMAP.md](ROADMAP.md) does |
| SD-16 | P2 | [03 §2.4](03-render-color-pipeline.md)'s `COMPOSITE_SHADER` account | [03 §2.4](03-render-color-pipeline.md) | Line citations drifted (`:577`→`:592`, `:529`→`:544`), the cited `#[allow(dead_code)]` is gone, and “validated but unreferenced from any live pass” is **false** — the pipeline is built at `renderer/mod.rs:602` and set at `scene_renderer.rs:384`. See [A-2](#a-2--p2--03s-account-of-composite_shader-wiring-is-stale) |
| SD-17 | P2 | [10 §1](10-mcp-tools.md)'s claim that the drift gate applies “once `schema_gen.rs` gains the 84 entries” | [10 §9](10-mcp-tools.md) | `tool_list()` already carries all 110; the work described as pending is done. Separate from [A-10](#a-10--p2--the-mcp-tool-count-is-stated-four-ways-and-matches-nothing)'s count mismatch |
| SD-15 | P2 | “D-01…**D-10** in SPEC.md” | [00 §4](00-overview.md) | SPEC carries **D-01…D-12**, including D-09 (colour) and D-12 (crash recovery) — both of which [00 §3](00-overview.md) itself depends on. **Fixed 2026-07-20** alongside this audit. Note [00 §5](00-overview.md)/[§6](00-overview.md) still say “serialization **v3**” and “P2 delivers v3 format”; those are part of SD-3, not this finding |

**Anti-drift annotation status ([40 §3.2](40-spec-verification.md#32-inline-spec-assert-assertions)).** Per 40 §2's rule — the checker verifies structure, tests verify behaviour, humans verify design — inline `spec-assert` comments were added to the owning docs for the **structurally-checkable** findings only:

- **Annotated (machine-checked in CI):** SD-1 ([03 §1](03-render-color-pipeline.md), crate/symbol exists), SD-2 ([11 §7](11-testing-phasing.md), one representative symbol per crate × 8), SD-3 ([01 §9](01-data-model.md), `const … == 4`), SD-4 ([11 §2](11-testing-phasing.md), `ci-step-contains ffmpeg`), SD-5 ([11 §8](11-testing-phasing.md), `dep-present proptest`/`criterion` + `dep-absent insta`), SD-8 ([02 §1](02-engine.md), `dep-present arc_swap` + `dep-absent triple_buffer`), SD-13 ([09 §2](09-audio-mixer.md)/[06 §7](06-captions-ai.md)/[08 §7](08-fusion-node-flows.md), `dep-absent rubato`/`subparse`/`egui-snarl`), SD-17 ([10 §9](10-mcp-tools.md), `ci-step-contains gen-mcp-docs.py`).
- **Structural but deferred to an anchored block ([40 §3.1](40-spec-verification.md#31-anchored-code-blocks)):** SD-6 (the `EngineCmd` variant list) and SD-10 (the cache-key type) are field/variant-order claims. The `spec-source` mechanism is implemented and fixture-tested (`tools/spec-extract/tests/cases/anchored/`), but a live anchored block requires the drift gate to run with `--spec-extract`; it is deliberately not wired into the single cheap lint-job step yet, and is **not** faked with per-variant `spec-assert`s (40 §2 forbids that).
- **Behaviour, not structure — left to tests, not annotated (40 §3.5: an unannotated block is an honest one):** SD-7 (`Export` is a NotImplemented stub — a runtime behaviour), SD-9 (the finding was false; nothing to pin), SD-11 (drop-frame separator handling — a parsing behaviour), SD-12 (`master_level()` returns `None` — a runtime value), SD-14 (a stale phase-model narrative — prose, not a symbol), SD-16 (needed a call-graph read — not expressible as a structural assertion).

---

## 4. `O-*` — capabilities with no owning design doc

SPEC declares **22** capabilities. Mapping each to the doc that *specifies* it (not merely mentions it) leaves four unowned. Each was confirmed by searching the **design layer** (docs 00–13): the CAP identifier appears in no design doc there. It does appear in the two *test* documents — [11-testing-phasing.md](11-testing-phasing.md) and `29-qa-spec.md`, which carries a dedicated section per capability. That is the problem, not a mitigation: a test document can only assert a contract someone else wrote, and here nobody did.

### O-1 · P2 · CAP-003 — ripple / roll / slip / slide semantics are under-documented, not unowned

> **Downgraded from P0 on 2026-07-20.** The first version claimed no document defines the four edits. **[04 §2.4](04-ui-mode-timeline.md) does**, and `29-qa-spec.md`'s CAP-003 section is a five-row scenario matrix that covers the track-boundary case among others; [19](19-editing-velocity-shot-management.md) adds a tool-mode resolution contract. The ops also ship with unit tests and a proptest (`photonic-core/tests/timeline.rs:576,1081,1135,1602`).

What remains is genuinely thinner than a four-verb edit grammar deserves: Unanswered: what slide does to a **locked** neighbour; behaviour at a track boundary; what happens when the neighbour is **shorter than the delta**; interaction with an adjacent **transition**; multi-selection semantics; whether ripple crosses **sync-locked** tracks (see [26 K-0.9](26-kdenlive-mlt-parity.md#8-k-0--foundations) — `sync_lock` is inert). [10 §3.4](10-mcp-tools.md) says only “one tool per `TimelineCmd` variant” with `delta_*` args.

**Owner:** [04](04-ui-mode-timeline.md). **Fix:** consolidate the scattered rules into one edit-semantics table and cover the residual cases above; the behaviour itself is already decided and tested.

### O-2 · P1 · CAP-005 — nested sequences

[01 §5](01-data-model.md) gives one line plus a cycle-check note; [02 §2](02-engine.md) a parenthetical “nested sequences compile recursively with cycle guard”. Unspecified: nested **audio routing**; **frame-rate mismatch** between inner and outer sequence; whether the nest's `work_range`, `markers` and `project_graph` apply; **trimming the nest vs changing the inner length**; and nested-sequence **caching** (a nested sequence is the single best case for the content-hash cache and nothing says so). [20 §7](20-pro-workflows.md) owns only the *UI*. **Owner:** [01](01-data-model.md) + [02](02-engine.md).

### O-3 · P1 · CAP-018 — undo/redo of everything

[01 §10](01-data-model.md) provides the command *enum* and a coalescing rule. Nothing owns undo **behaviour**: history depth, memory cap, what coalesces across domains, undo across a **mode switch**, or undo of a **completed background job's** committed result — [10 §6](10-mcp-tools.md) commits from a worker thread, and no document says what happens if the user undoes while a second job is in flight. See also [MC-4](#mc-4--p2--undo-bounds-exist-in-code-and-in-no-spec-doc). **Owner:** [01](01-data-model.md).

### O-4 · P1 · CAP-020 — save/reopen and backward compatibility

[01 §9](01-data-model.md) is four bullets on serialization. Nothing owns **forward**-compat: what happens when a newer file carries an unknown `IrOp`, `GraphOp`, `EffectKind`, `AudioFxKind`, `TransitionKind` or `GradeOpKind`. Only `GradeOpParams` has a `#[serde(other)]` inert-load path ([07 §1](07-color-grading.md)) — which is the right pattern and is applied **once**. Also unaddressed: how the v4 fields interact with `COMPAT_WINDOW`. Given [26 X-1](26-kdenlive-mlt-parity.md#x-1--mlt-xml--kdenlive-project-import)/[X-2](26-kdenlive-mlt-parity.md#x-2--opentimelineio-interchange) will import foreign projects, unknown-variant handling stops being hypothetical. **Owner:** [01](01-data-model.md).

---

## 5. `U-*` — under-specified contracts

Sections where a heading and a sentence stand in for a contract an implementer needs.

| ID | Sev | Gap | Owner |
|---|---|---|---|
| U-1 | **P1** | **Transition timing model.** [01 §5](01-data-model.md) says `transition_in` “overlaps previous clip”; [08 §2.0b](08-fusion-node-flows.md) gives five kinds over `t∈0..1`. Nothing states whether the overlap **consumes media handles** or shortens the sequence, what happens when the neighbour lacks sufficient `source_in` handle, how `transition_out` on A interacts with `transition_in` on B, or how the **audio** crossfade window is derived. Worse, [01 §4](01-data-model.md)'s “clips sorted, **non-overlapping**” invariant appears to *forbid* the overlap transitions require — and `Sequence::validate()` enforces it. No doc reconciles this | [01](01-data-model.md) |
| U-2 | **P1** | **Compile diagnostics have no type.** [02 §2](02-engine.md) promises “surface a diagnostic (never black-frame silently)”; [08 §5](08-fusion-node-flows.md) states as a hard requirement that “02's diagnostic type must carry `GraphNodeId`”; [10 §3](10-mcp-tools.md) says `get_graph` is how an agent reads it. **02 defines no diagnostic type** — no struct, no severity, no transport to `EngineStatus`. (Code has `CompileDiagnostic`; the contract is undocumented) | [02](02-engine.md) |
| U-3 | P1 | **`VectorStateKey`** is a cache key in [02 §2](02-engine.md)/[§3](02-engine.md)/[§5](02-engine.md) and [03 §2.5](03-render-color-pipeline.md), and is **defined nowhere**. “hash(referenced nodes' state + …)” is not definable without naming which `SceneNode` fields participate — exactly the bug class [03 §2.2](03-render-color-pipeline.md) had to fix for the tessellation cache | [02](02-engine.md) |
| U-4 | P2 | **`ProjectVideoSettings`** — “proxy prefs, cache limits, default rates” ([01 §2](01-data-model.md)) and never defined, while [05 §6.5](05-import-export.md) and [02 §5](02-engine.md) each add fields to it. The one document-state struct with no field list | [01](01-data-model.md) |
| U-5 | **P1** | **Crash recovery** ([04 §1.4](04-ui-mode-timeline.md)) is four bullets asserting “zero new subsystems”. Unaddressed: **orphaned `ffmpeg` children** after a hard kill (one per live source, ≤8, plus encoder and PCM sources); in-flight export/proxy jobs and their **partial output files**; sidecar-cache corruption; and [SPEC.md](SPEC.md)'s “at most a few minutes” promise against a **300 s** autosave default | [04](04-ui-mode-timeline.md) |
| U-6 | P2 | **Snap threshold** — [04 §2.5](04-ui-mode-timeline.md) gives “a pixel-distance threshold” with no number and no tie-break rule when two candidates of different priority are both within it | [04](04-ui-mode-timeline.md) |
| U-7 | **P1** | **Mixed frame rates.** [05 §6.2](05-import-export.md) covers VFR input thoroughly and is silent on the common case: a **30 fps clip on a 24 fps sequence**. [02 §4](02-engine.md) covers only `SpeedMap`. Neither conform nor pull-down is specified, and every real project hits this | [05](05-import-export.md) + [02](02-engine.md) |
| U-8 | P2 | **Multi-clip Clip Inspector** — flagged by [13 §12](13-ux-components.md) finding 6, still unowned by [04](04-ui-mode-timeline.md); code confirms it is unsupported | [04](04-ui-mode-timeline.md) |

---

## 6. `MC-*` — never covered anywhere

Ranked by risk. None of these appear in docs 00–13 at all.

### MC-1 · P0 · Security

**No document in the set contains a security model.** Verified exposure:

- **The MCP server has no authentication of any kind.** It binds `127.0.0.1:7842` (`server.rs:154`) — loopback-only, which rules out a remote network attacker and is the single mitigating fact here. It does **not** rule out any other local process, or a web page in the user's browser reaching it via a `localhost` request or DNS-rebinding.
- **`CorsLayer::permissive()`** (`server.rs:166`) — this is what makes the browser vector concrete rather than theoretical: any web page the user visits can issue cross-origin requests to the server and read the responses.
- **No path validation anywhere in the video handlers** — verified: no `canonicalize`, no root containment check, no allowlist. `import_media { paths }`, `relink_media { new_path }`, `export_sequence { out_path }`, `import_captions`/`export_captions`, `apply_lut { lut_path }` and `transcode_media` all take unvalidated filesystem paths. Combined with the above: **arbitrary local file read and write**, with no confirmation step, and [10 §8](10-mcp-tools.md)'s error taxonomy has no `PathNotPermitted` code to refuse with.
- **Every imported file is parsed by an `ffmpeg` subprocess** with attacker-controlled content and no sandbox, seccomp profile, or resource limit beyond the decode read deadline.
- [01 §9](01-data-model.md) resolves asset paths “relative first, then absolute, then relink-by-hash”, so a `.photon` from an untrusted source **silently reads paths outside the project directory**.

Two things are already right. Argument construction is safe — `export/encoder.rs` builds a `Vec<String>` argv and never a shell string, so there is no command-injection surface; and the `.cube` parser bounds `LUT_3D_SIZE` to `2..=256` and rejects mismatched row counts (`lut.rs:22,37-42`), so the obvious allocation bomb is closed. *(An earlier draft claimed otherwise on both counts.)*

**Owner:** none exists. **Fix:** this needs a **new owner doc** (proposed `28-security-model.md`) covering the trust boundary, a path-containment policy with a refusal code, subprocess limits, and parser hardening. No existing doc has a plausible home for it, which is likely why it was never written.

### MC-2 · P1 · Error taxonomy and user-facing error surfaces

[10 §8](10-mcp-tools.md)'s nine MCP `error_code`s are the **only** error catalogue in the set. There is no GUI error model at all: what the user sees when a decode sidecar dies mid-playback, when the encoder fails at frame 40 000, when the disk fills mid-export, when a `.cube` fails to parse, or when the GPU adapter is lost. No severity levels, no stable error identifiers, no routing rule (toast vs badge vs modal vs `EngineStatus.last_error` — the last exists in code and in no document). **Owner:** proposed for [13](13-ux-components.md) (surfaces) + [02](02-engine.md) (taxonomy).

### MC-3 · P1 · GPU device loss and adapter fallback

Zero coverage. [03](03-render-color-pipeline.md) assumes a device forever; [02 §1](02-engine.md) shares one `Arc<GpuContext>` between renderer and engine with **no recovery path**. On device loss, the texture pool, node cache and every `Arc<wgpu::Texture>` in `EngineFrame` become invalid. No adapter-capability floor is stated (f16 storage; compute for [07 §5](07-color-grading.md)'s atomic histograms), and no CPU-fallback policy exists — [11](11-testing-phasing.md) treats “no GPU adapter” purely as a **test-skip** condition rather than a runtime state. **Owner:** [02](02-engine.md) + [03](03-render-color-pipeline.md).

### MC-4 · P2 · Undo bounds exist in code and in no spec doc

> **Withdrawn as a P1.** The first version claimed no cap, no byte budget and no eviction policy. **All three ship.** `history/stacks.rs:112` `set_limits(max_steps, size_bytes)`, `:185` `enforce_steps()`, `:222` `enforce_size()`, with a retention floor and an explicit rule that branches are never auto-trimmed — which also answers the “no statement of how it interacts with the checkpoint/branch machinery” half. Surfaced as user preferences (`preferences.rs:76-82`) and wired at startup.

- **The residual:** no spec document points at any of it. [01 §10](01-data-model.md) states the “deltas, never media” rule and stops. An implementer reading the specs would conclude the bounds still need designing and might build a second mechanism.
- **Owner:** [01](01-data-model.md). **Fix:** one paragraph naming the shipped limits and the preference surface.

### MC-5 · P1 · Large-project scale limits

No stated ceilings for tracks, clips per track, assets, keyframes per track, cues, or graph nodes. [04 §7](04-ui-mode-timeline.md) names “hundreds of clip rects” as a risk with no target. Structurally the sharpest edge: [02 §1](02-engine.md) promises the engine snapshots “the parts it needs… cheap `Clone`”, but `session.rs:678` does `Arc::new(p.clone())` on the **whole `TimelineProject`** on every `doc_generation` bump — an O(project) deep clone per edit, unbounded, on the interactive path. The doc and the code disagree, and the code is the expensive one. **Owner:** [02](02-engine.md) + [25](25-performance.md).

### MC-6 · P1 · Performance regression gating is advisory

[11 §4](11-testing-phasing.md) makes benches “CI-advisory, not blocking… a human reviews the trend line periodically”, and the SS-1 zero-dropped-frames gate and the SS-3 sync test are both `#[ignore]` + `continue-on-error` nightly. **Net effect: no phase can fail on a performance regression**, while [ROADMAP §10](ROADMAP.md#10-definition-of-done) requires budgets “green” as a condition of done. Those two positions are incompatible. **Owner:** [11](11-testing-phasing.md).

### MC-7 · P2 · Accessibility

[13 §0](13-ux-components.md) explicitly declines screen-reader semantics, and [13 §12](13-ux-components.md) records that the three genuinely custom drag controls — colour wheels, curve editors, node marquee — have **no keyboard path** specified in 07, 08 or 09. There is no owning a11y contract: no focus-order rule, no contrast gate, no reduced-motion setting (relevant to [06 §5.2](06-captions-ai.md) caption animations), no minimum hit-target size. **Owner:** [13](13-ux-components.md).

### MC-8 · P2 · Localization

Zero coverage. No string extraction, no locale-aware timecode/number formatting, no font-fallback story for CJK captions against [06 §3.5](06-captions-ai.md)'s `max_chars_per_line = 42` — a character-count heuristic that is meaningless for CJK and wrong for Arabic — and RTL is disposed of in a parenthetical. **Owner:** [06](06-captions-ai.md) for captions; product for the shell.

### MC-9 · P2 · Diagnostics and support bundles

> **Narrowed on 2026-07-20.** The first version claimed no logging policy and no crash reporter. **Both ship** — `tracing_subscriber` + `EnvFilter` (`photonic-app/src/main.rs:22,40`), and a panic hook feeding `CrashReport::capture` (`:69,83`) with a consent preference and a pending-reports UI.

The surviving gap is narrow but real: the ffmpeg **stderr tail is captured** (`decode/sidecar.rs`) and then stops at `DecodeError` — it reaches no user surface, so a decode failure cannot be reported with the one piece of evidence that would explain it. Ties to [MC-2](#mc-2--p1--error-taxonomy-and-user-facing-error-surfaces). **Owner:** [02](02-engine.md).

### MC-10 · P2 · Shortcut conflict management

[04 §5.2](04-ui-mode-timeline.md) resolves three specific collisions by mode-gating, which is sound, but there is no registry-level conflict **detection**, and no policy for a user rebind colliding with a video binding. **Owner:** [04](04-ui-mode-timeline.md).

---

## 7. Doc-set hygiene

| ID | Finding |
|---|---|
| H-1 | **`14-nle-parity.md` and `29-qa-spec.md` both claim number 14.** `29-qa-spec.md` is referenced from **code** — `photonic-core/Cargo.toml:14` and `history/revision_contract.rs:26` both cite its §5 as the rationale for the revision-counter contract — but appears in **no precedence tier** in [ROADMAP §1](ROADMAP.md#1-authority-and-precedence). *(An earlier draft claimed zero inbound references anywhere; that was wrong.)* The precedence hole is wider than one document: `14-nle-parity.md`, `15` and `16` are also unplaced — yet it owns the CAP-001…021 scenario matrix and the AS-1/2/3 acceptance walkthroughs that SS-2 and [ROADMAP §10](ROADMAP.md#10-definition-of-done) depend on. **Load-bearing and unreferenced.** Fix: renumber to `29-qa-spec.md` (28 is reserved for the security model proposed in [MC-1](#mc-1--p0--security)) and insert it into the precedence ladder |
| H-2 | [00 §5](00-overview.md)'s document map omitted docs **13–18** entirely — including [13](13-ux-components.md), which [ROADMAP §1](ROADMAP.md#1-authority-and-precedence) declares **normative**. *(Fixed 2026-07-20 alongside this audit.)* |
| H-4 | **The `S` prefix is overloaded four ways** and this audit walked into it: [ROADMAP §8](ROADMAP.md#8-architecture-decisions-and-defaults) `S1`–`S14` (SPEC amendments), [23](23-legal-open-source-implementation-routes.md) `S1`–`S5` (the same, quoted), [26 §6](26-kdenlive-mlt-parity.md#6-decisions-taken-for-this-document) `K-S1`–`K-S3` (decisions), and SPEC's `SS-1`/`SS-3` success signals — inside which the literal substring `S-3` appears, defeating grep. This document's drift group was renamed `S-*` → **`SD-*`** on 2026-07-20 for exactly this reason. Fix: reserve the `S` prefix for SPEC amendments only, and record that rule here |
| H-3 | Cross-inventory precedence: [ROADMAP §1](ROADMAP.md#1-authority-and-precedence) tier 4 spans 19 **through 27** — so this document places itself in the tier it is criticising — giving two self-declared drafts ([26](26-kdenlive-mlt-parity.md) and this one) the same authority as [23](23-legal-open-source-implementation-routes.md) and [24](24-preview-media-load.md), both *accepted* contracts. Consider a sub-tier for accepted-policy documents |

---

## 8. Resolution order and ownership

**Every finding now has an owner.** The audit's original problem was that several had none — most acutely MC-1, which had no plausible home in the existing set.

| Findings | Owning spec |
|---|---|
| A-1, A-3 — colour operand spaces | **[03 §4.5](03-render-color-pipeline.md#45-operand-spaces-for-blending-and-grading-normative)** (normative) |
| MC-1 — security | **[28](28-security-model.md)** |
| MC-2, U-2 — errors and diagnostics | **[36](36-error-model.md)** |
| MC-3, U-5, MC-5, MC-6 — device loss, crash recovery, scale, perf gating | **[37](37-robustness.md)** |
| U-1, O-2, U-7 — transitions, nesting, conform | **[38](38-sequence-semantics.md)** |
| O-3, O-4, A-4 — undo, forward compat, document identity | **[39](39-document-lifecycle.md)** |
| O-1 — edit semantics (downgraded to P2) | [04 §2.4](04-ui-mode-timeline.md) + `29-qa-spec.md` |
| SD-*, A-2/A-5/A-6/A-9/A-12/A-13 | Applied 2026-07-20 |

### Original ordering

**Before the affected subsystem ships (P0):**
1. [A-1](#a-1--p0--the-live-canvas-composites-in-gamma-headless-composites-in-linear) — the live canvas composites 22 blend modes on gamma-encoded operands while headless composites them in linear. A real canvas-vs-export divergence that the headless-only golden corpus structurally cannot see.
2. [A-3](#a-3--p0--grade-operators-apply-transfer-functions-to-premultiplied-alpha) — grade ops run on premultiplied values; `grade.rs:14-16` notes that every golden fixture is opaque, which is precisely why no test fails. Needs a partial-alpha fixture in [11](11-testing-phasing.md) before it is even observable.
3. [MC-1](#mc-1--p0--security) security — no auth, no path containment, `CorsLayer::permissive()`. Loopback binding narrows the attacker set to local processes and any web page the user visits; it is not a control.

**Before more implementation (P1):** [A-4](#a-4--p1--mcp-binds-one-document-the-gui-has-tabs) MCP/tab binding · [O-2](#o-2--p1--cap-005--nested-sequences)–[O-4](#o-4--p1--cap-020--savereopen-and-backward-compatibility) unowned capabilities · [U-1](#5-u---under-specified-contracts) transition timing (and [08 §2](08-fusion-node-flows.md)'s unsatisfiable "clips overlapping" crossfade rule, which is really a contradiction) · [U-2](#5-u---under-specified-contracts) diagnostic type · [U-5](#5-u---under-specified-contracts) crash recovery · [MC-2](#mc-2--p1--error-taxonomy-and-user-facing-error-surfaces) error taxonomy · [MC-3](#mc-3--p1--gpu-device-loss-and-adapter-fallback) device loss · [MC-5](#mc-5--p1--large-project-scale-limits) scale limits, whose `session.rs:678` whole-project deep clone contradicts [02 §1](02-engine.md).

**Mechanical (P2):** the whole of [§3](#3-sd---spec-versus-code-drift), plus the downgraded [A-2](#a-2--p2--03s-account-of-composite_shader-wiring-is-stale), [A-5](#a-5--p2--10s-set_clip_speed-row-contradicts-the-shipped-code), [A-6](#a-6--p2--05-specifies-a-relinkasset-shape-that-was-never-adopted), [A-9](#a-9--p2--two-docs-assert-an-hdr-non-goal-that-d-09-repealed)–[A-13](#a-13--p2--a-stale-coordination-note-reads-as-an-open-blocker), [O-1](#o-1--p2--cap-003--ripple--roll--slip--slide-semantics-are-under-documented-not-unowned), [MC-4](#mc-4--p2--undo-bounds-exist-in-code-and-in-no-spec-doc), [MC-9](#mc-9--p2--diagnostics-and-support-bundles). Nearly all are one-line edits. [A-10](#a-10--p2--the-mcp-tool-count-is-stated-four-ways-and-matches-nothing) is best fixed by **generating** the catalogue rather than editing a count.

**Definition of done for this document:** every finding is either fixed in its owner doc or explicitly accepted-with-rationale there. A finding closed by rewriting *this* document rather than the owner doc is not closed.
