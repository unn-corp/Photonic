# 36 — Error Model: Taxonomy, Diagnostics, and Surfaces

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** engine maintainers, GUI owner, MCP owner

**Depends on:** [02-engine.md](02-engine.md), [08-fusion-node-flows.md](08-fusion-node-flows.md) (which places a requirement on the diagnostic type), [10-mcp-tools.md](10-mcp-tools.md) §8, [13-ux-components.md](13-ux-components.md), [28-security-model.md](28-security-model.md).

**Owns:** [27 MC-2](27-spec-audit.md#mc-2--p1--error-taxonomy-and-user-facing-error-surfaces) (error taxonomy and user-facing surfaces) and [27 U-2](27-spec-audit.md#5-u---under-specified-contracts) (the compile-diagnostic type).

---

## 1. The gap

[10 §8](10-mcp-tools.md)'s nine MCP `error_code`s are the **only** error catalogue in the spec set. There is no GUI error model: nothing says what the user sees when a decode sidecar dies mid-playback, when the encoder fails at frame 40 000, when the disk fills mid-export, when a `.cube` fails to parse, or when the GPU adapter is lost.

Separately, [02 §2](02-engine.md) promises the compiler will "surface a diagnostic (never black-frame silently)" and [08 §5](08-fusion-node-flows.md) states as a hard requirement that "02's diagnostic type must carry `GraphNodeId`" — but **02 defines no diagnostic type**. The code has a `CompileDiagnostic`; the contract does not.

These are one problem: **errors exist and have nowhere to go**.

---

## 2. Principles

1. **Every error has a stable identifier.** Not a string — a variant. Strings are for humans; identifiers are for tests, docs, MCP clients and support.
2. **Every error names a consequence.** "Decode failed" is useless; "this clip will show a placeholder until the file is available" is actionable.
3. **Severity determines the surface, not the subsystem.** A decode failure and a parse failure at the same severity look the same to the user.
4. **Never fail silently, never black-frame silently.** [02](02-engine.md) already commits to this; this document makes it checkable.
5. **Degrade rather than stop, where the degradation is visible.** A missing effect renders passthrough *and says so*. A missing asset renders a placeholder *and says so*. Silence is what makes a degradation a defect.

---

## 3. Taxonomy

```rust
pub struct Diagnostic {
    pub code: DiagCode,
    pub severity: Severity,
    pub subject: Subject,            // what it is about
    pub message: String,             // human, localizable
    pub consequence: String,         // what the user will see
    pub remedy: Option<Remedy>,      // what they can do
}

pub enum Severity { Info, Warning, Error, Fatal }

pub enum Subject {
    Asset(AssetId), Clip(ClipId), Track(TrackId), Sequence(SequenceId),
    GraphNode { graph: GraphId, node: GraphNodeId },     // 08's requirement, discharged
    Effect { clip: ClipId, index: usize },
    Export(JobId), Project, Engine,
}

pub enum Remedy { Relink(AssetId), OpenSettings(SettingsPage), Retry, ConvertMedia(AssetId), None }
```

`Subject` is what makes a diagnostic **navigable** — the panel can offer "show me", and the MCP client can address the same object. A diagnostic that cannot say what it is about cannot be acted on.

### 3.1 Severity

| Severity | Meaning | Surface (§4) |
|---|---|---|
| `Info` | Something was adjusted and the user should know | Status line, diagnostics panel |
| `Warning` | Output is degraded but usable | Badge on the subject + panel |
| `Error` | This operation failed; the rest continues | Toast + panel + badge |
| `Fatal` | The session cannot continue safely | Modal, with a save path |

**`Fatal` must be rare and must always offer to save.** Losing work to an error dialog is worse than the error.

### 3.2 Code families

| Family | Examples |
|---|---|
| `Media` | `NotFound`, `Unreadable`, `UnsupportedCodec`, `ProbeFailed`, `VariableFrameRate`, `Interlaced`, `NonSeekable` |
| `Decode` | `SidecarCrashed`, `SidecarTimeout`, `SeekFailed`, `FrameDropped` |
| `Compile` | `PortTypeMismatch`, `GraphCycle`, `UnknownEffect`, `EffectUnavailableAtScope`, `ParamOutOfRange`, `TimeOffsetBudgetExceeded`, `UnsupportedBlendMode` |
| `Render` | `DeviceLost`, `OutOfMemory`, `TextureTooLarge`, `AdapterCapabilityMissing` |
| `Export` | `EncoderUnavailable`, `EncoderFailed`, `DiskFull`, `PresetInvalid`, `LoudnessCeilingBreached` |
| `Audio` | `DeviceUnavailable`, `Xrun`, `SampleRateMismatch`, `LatencyBudgetExceeded` |
| `Project` | `VersionTooNew`, `MigrationFailed`, `ValidationFailed`, `UnknownVariantPreserved` |
| `Security` | `PathNotPermitted`, `Unauthenticated` — [28](28-security-model.md) |
| `Interchange` | `Unsupported`, `Approximated`, `MalformedInput` — [34](34-interchange.md) |
| `Caption` | `NoLineBreakOpportunities` (Thai/Lao/Khmer/Burmese have no UAX #14 breaks — refuse to wrap rather than overflow silently), `KaraokeModeDegraded` — [42](42-localization.md) |

Registered by later specs, listed here so the enum stays the single source of truth: `Compile::TransitionHandleClipped`, `Compile::NestedSequenceShortened`, `Media::FrameRateConformed` ([38](38-sequence-semantics.md)) · `Render::FontSubstituted`, `Render::MissingGlyph` ([42 §7.2](42-localization.md#72-three-fallback-defects-increasing-in-severity)) · `Project::UnknownVariantPreserved`, `Project::VersionTooNew`, `Project::VersionTooOld`, `DocumentMismatch` ([39](39-document-lifecycle.md)).

**`Render::MissingGlyph` is an error, not a warning**, and **export refuses to complete silently on it** — a glyph id of 0 in a caption run means the exported video contains a literal box, and an export reporting success while producing that is the same failure class as one that stops at frame 40,000 and reports success ([37 §1.3](37-robustness.md#13-recovery-protocol)).

`Media::VariableFrameRate`, `Media::Interlaced` and `Media::NonSeekable` are `Info`/`Warning` diagnostics, not failures — they are how [26 K-C7](26-kdenlive-mlt-parity.md#k-c7--import-time-media-triage-report)'s import triage reports itself, reusing this machinery rather than inventing a parallel one.

---

## 4. Surfaces

| Surface | Carries | Rule |
|---|---|---|
| **Badge on the subject** | Warning/Error scoped to a clip, track, asset or node | Always present while the condition holds; clicking opens the panel filtered to it |
| **Diagnostics panel** | Everything, searchable, groupable by subject | The one place that shows the whole picture |
| **Toast** | New `Error` | Transient, non-blocking, click-through to the panel. **Coalesced** — 400 failing frames produce one toast |
| **Modal** | `Fatal` only | Offers save |
| **`EngineStatus.last_error`** | Most recent engine-thread error | Already exists; becomes a `Diagnostic` |
| **MCP result** | The diagnostic for the failing call | §5 |
| **Log** | Everything, with the technical detail the user surfaces omit | Includes the ffmpeg stderr tail |

### 4.1 Coalescing

Errors in the render loop repeat per frame. Every surface **must** coalesce on `(code, subject)` and carry a count — an uncoalesced diagnostic stream is indistinguishable from a hang.

### 4.2 The ffmpeg stderr tail

`decode/sidecar.rs` captures a stderr tail and it currently reaches no user surface ([27 MC-9](27-spec-audit.md#mc-9--p2--diagnostics-and-support-bundles)). It is attached to `Decode::*` diagnostics as **technical detail** — shown on demand, included in a support bundle, never in the primary message. It is usually the only evidence of *why* a decode failed.

---

## 5. MCP mapping

[10 §8](10-mcp-tools.md)'s nine codes remain the wire vocabulary and gain `PathNotPermitted` and `Unauthenticated` ([28](28-security-model.md)). Every MCP error result carries the full `Diagnostic` in its data payload, so an agent gets `code`, `subject` and `consequence` rather than prose.

**Generated, not hand-maintained** — the code table in 10 §8 derives from the `DiagCode` enum under the existing doc-drift gate, the same fix [27 A-10](27-spec-audit.md#a-10--p2--the-mcp-tool-count-is-stated-four-ways-and-matches-nothing) asks for on the tool catalogue.

---

## 6. Compile diagnostics

Discharges [08 §5](08-fusion-node-flows.md)'s requirement and [27 U-2](27-spec-audit.md#5-u---under-specified-contracts).

`graph::compile` returns `(FrameGraph, Vec<Diagnostic>)`. Rules:

- **Compilation never fails outright.** A bad node falls back to a defined behaviour — pass through, or a diagnostic placeholder frame — and emits a diagnostic. A timeline that will not compile is unusable; one that compiles with a warning is editable.
- **Diagnostics carry `Subject::GraphNode`** where a node is at fault. This is 08's stated hard requirement.
- **Deterministic** — same document, same tick, same diagnostics in the same order. They participate in nothing that varies at runtime, so they are safe to assert in goldens.
- **Cheap** — compile budget is < 0.5 ms ([02 §8](02-engine.md#8-perf-budgets-verified-in-11)); diagnostics must not allocate on the success path.

Known emitters today, all currently silent or ad hoc: non-`Normal` blend on the GPU path ([32 §8](32-engine-contracts.md#8-cpugpu-equivalence)), `Wipe`/`Push` fallback, inert `Lut3d`, `TimeOffset` budget, port type mismatch, effect at a scope it does not declare ([30 §2.3](30-effect-catalogue.md#23-capability-and-applicability)).

---

## 7. Acceptance

1. Every `DiagCode` has a message, a consequence, and a test that produces it.
2. A decode failure mid-playback produces exactly **one** coalesced toast, a clip badge, and a panel entry with the stderr tail attached.
3. An export failing at frame N reports the frame, the cause, and leaves no partial file registered.
4. A compile diagnostic carries `Subject::GraphNode` for a node-caused fault; goldens assert the exact diagnostic set.
5. Compilation of a document with an unknown effect **succeeds**, renders passthrough, and diagnoses it.
6. No error path panics. A malformed file, a lost device and a full disk each produce a `Diagnostic`.
7. `Fatal` offers a save path.
8. The MCP code table matches the `DiagCode` enum under the drift gate.
9. A support bundle contains the log and recent diagnostics and **no media content** — consistent with [ROADMAP §7](ROADMAP.md#7-legal-content-and-product-gates)'s no-content-logging rule.

---

## 8. Sequencing

| Order | Item |
|---|---|
| 1 | `Diagnostic` type + `DiagCode` enum + `Subject`; wire `CompileDiagnostic` onto it (§6) |
| 2 | `EngineStatus.last_error` becomes a `Diagnostic`; log integration + stderr tail |
| 3 | Diagnostics panel + badges + coalescing |
| 4 | MCP payload + generated code table |
| 5 | Toasts and the `Fatal` modal |
| 6 | Backfill emitters — the silent fallbacks in §6 become diagnosed ones |

Step 6 is where the value lands: several currently-silent degradations ([26 §4.2](26-kdenlive-mlt-parity.md#42-phase-gated-seams-that-k--items-depend-on)'s passthrough effects, inert LUTs, blend-mode fallback) become visible the moment there is somewhere to report them. That is worth more than any single fix on the list.
