# 39 — Document Lifecycle: Undo Contract, Forward Compatibility, Document Identity

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** data-model owner, GUI owner, MCP owner

**Depends on:** [01-data-model.md](01-data-model.md) §9–§10, [04-ui-mode-timeline.md](04-ui-mode-timeline.md), [10-mcp-tools.md](10-mcp-tools.md), [SPEC.md](SPEC.md) (the undo constraint), [36-error-model.md](36-error-model.md).

**Owns:** [27 O-3](27-spec-audit.md#o-3--p1--cap-018--undoredo-of-everything) (CAP-018 undo behaviour), [27 O-4](27-spec-audit.md#o-4--p1--cap-020--savereopen-and-backward-compatibility) (CAP-020 forward compatibility), [27 A-4](27-spec-audit.md#a-4--p1--mcp-binds-one-document-the-gui-has-tabs) (document identity across GUI tabs and MCP).

---

## 1. Undo (CAP-018)

[01 §10](01-data-model.md) provides the command *enum* and a coalescing rule. Nothing owns undo **behaviour**, and CAP-018 appears in no design doc — only in the test docs, which can only assert a contract somebody else wrote.

### 1.1 One verb, one unit

**One user-visible action is one undo step**, including fanned-out edits. A group move that shifts nine clips is one step ([35 §3.5](35-model-decisions.md#35-operation-semantics)); an import that creates forty assets is one step; a marker-category deletion that reassigns two hundred markers is one step.

The corollary matters more: **an operation that cannot be undone atomically must not be committed partially.** Validate every member, then commit — the validate-then-commit discipline already present in `ops.rs`.

### 1.2 Coalescing

Coalescing is **time- and identity-bounded**, never open-ended:

| Rule | Value |
|---|---|
| Same command kind **and** same subject | required |
| Gap between edits | < 500 ms |
| Total coalesced span | ≤ 5 s |
| Broken by | selection change, tool change, panel change, save, any other command kind |

A slider drag is one step; a slider drag, a pause to think, and another drag are two. The span cap exists so a long continuous drag cannot swallow an entire editing session into one undo.

### 1.3 Bounds

[27 MC-4](27-spec-audit.md#mc-4--p2--undo-bounds-exist-in-code-and-in-no-spec-doc) established that the machinery exists — `set_limits(max_steps, size_bytes)`, `enforce_steps()`, `enforce_size()`, user preferences, and a rule that branches are never auto-trimmed. **No spec document points at it**, so an implementer would conclude the bounds still need designing and might build a second mechanism.

Normative, closing that gap:

- History is bounded by **both** a step count and a byte budget; the byte budget dominates, since one `BulkInsertCues` can outweigh a thousand slider steps.
- **Branches are never auto-trimmed** — they are deliberate user artifacts.
- A retention floor guarantees a minimum number of steps regardless of size, so a single enormous command cannot empty the history.
- Trimming is silent — it is a memory policy, not an event.

**Commands must honour the "deltas, not media" rule.** Several arguably do not: `CaptionCmd::BulkInsertCues` carries a whole transcript, `AudioCmd::ApplyDuckingPreset` carries whole FX chains, `SetGrade { old, new }` carries whole grades including 256-point curves, and [08 §4](08-fusion-node-flows.md) requires **deep-cloning a `NodeGraph`** on composition paste. **Recommend:** these are acceptable *because* the byte budget bounds them — but each should report its `mem_estimate` honestly, or the budget is enforced against a fiction.

### 1.4 Undo across a mode switch

Vector mode and video mode share one `CommandHistory`. **Recommend: undo is global and does not respect mode.** Undoing a video edit while in vector mode is allowed, and the shell **switches to the mode the undone command belongs to** so the user sees what changed.

The alternative — per-mode stacks — breaks the single-history property that makes a vector asset a first-class timeline citizen, which is the module's whole premise.

### 1.5 Undo versus in-flight jobs

[10 §6](10-mcp-tools.md) commits job results from a worker thread. Nothing says what happens if the user undoes while a second job is in flight.

**Recommend:**

- A job's result is committed as a **normal undoable command** at completion — it enters the history like any edit.
- A job **captures a document snapshot at submission** and is unaffected by later edits or undos ([26 K-F1](26-kdenlive-mlt-parity.md#k-f1--gui-render-queue)'s "frozen against later edits").
- If the object a job targets **no longer exists** at completion (undone away), the commit is **skipped** with an `Info` diagnostic. It is not resurrected — resurrecting an asset the user just undid is surprising in a way no user would predict.
- Undo **never cancels a running job**. Cancellation is explicit, through the job queue.

### 1.6 What is not undoable

[SPEC.md](SPEC.md) says "every document mutation, without exception, is undoable". Two things currently violate it:

1. **`Track.height_px`** — serialized inside `Document`, changed without a command ([27 A-8](27-spec-audit.md#a-8--p1--track-height-is-not-undoable-against-an-absolute-constraint)).
2. **Session-only state** that is nonetheless persisted (panel sizes, view scroll).

**Recommend: move persisted-UI-preference fields out of `Document` into a sidecar view-state file**, keyed by document, rather than amending the SPEC constraint. Rationale: the constraint is a good one and worth defending; `height_px` is genuinely not a document mutation — it is a view preference that happens to be persisted, and no user expects undo to change a track's height. Weakening an absolute rule to accommodate one field invites the next field.

Sequence tabs, breadcrumbs, selection ([35 §3.4](35-model-decisions.md#34-selection-is-not-a-group)) and preview zones' *rendered* state all belong in the same sidecar.

---

## 2. Forward compatibility (CAP-020)

[01 §9](01-data-model.md) is four bullets on serialization. Nothing owns what happens when a **newer** file carries something this build does not know.

### 2.1 The existing good pattern

`GradeOpParams` has a `#[serde(other)]` variant that loads inert and is preserved ([07 §1](07-color-grading.md)). It is the right answer, and it is applied **once**.

### 2.2 Generalise it

**Every open-ended enum in the persisted model gains an unknown-preserving variant**: `EffectKind`/`EffectId`, `GraphOp`, `AudioFxKind`, `TransitionKind`, `GradeOpKind`, `MarkerAnchor`, `GroupKind`, `ClipSource`.

Rules:

- **Preserve the original serialized form verbatim** and re-emit it unchanged on save. A round-trip through an older build must be lossless.
- **Render inert** — an unknown effect is passthrough, an unknown transition is a cut, an unknown source is a placeholder frame.
- **Diagnose once per document load**, not per frame: `Project::UnknownVariantPreserved` (`Warning`) naming what and where.
- **Never drop, never guess.** Approximating an unknown effect with a similar one is worse than omitting it, because the user cannot see that it is wrong.

This is the same discipline [34 §3.4](34-interchange.md#34-effects) applies to unmapped MLT effects and [30 §2.6](30-effect-catalogue.md#26-versioning-and-migration) to unknown manifests — one rule, three places.

### 2.3 Version policy

| Situation | Behaviour |
|---|---|
| `format_version` **older**, inside `COMPAT_WINDOW` | Migrate forward, save at current |
| **Older**, outside the window | Refuse with `Project::VersionTooOld`, naming the version that can read it |
| **Equal** | Load |
| **Newer**, minor | Load with unknown-preservation (§2.2), **warn that saving may lose newer-only data**, and offer save-as-copy |
| **Newer**, major | Refuse with `Project::VersionTooNew` |

The "newer, minor" row is the one that matters and the one most products get wrong: silently loading and re-saving a newer file is how a user loses work they created on another machine. **Warn before the first save, not after.**

### 2.4 Validation on load

`finalize_load` already flags orphaned `PropertyTrack` paths (retained, not dropped) and **rejects** files with overlapping or unsorted clips. Keep both, and add: a rejection is a `Project::ValidationFailed` diagnostic naming the offending clip, not an opaque failure. A file that cannot be loaded should say which clip is wrong so it can be repaired.

---

## 3. Document identity (A-4)

### 3.1 The contradiction

[10 §2](10-mcp-tools.md): *"Single-document assumption holds… `EngineSession` is therefore a singleton per `AppState`, no `session_id` arg anywhere."*
[04 §1](04-ui-mode-timeline.md): timeline state is *"per-tab, not global… a user may have a vector-only tab and a video-project tab open at once"*.

With two tabs open, every MCP video tool silently targets whichever document `AppState` holds. `play`, `seek` and `render_frame_at` cannot address the other tab, and **CAP-019's "MCP outputs equal GUI outputs" is unverifiable** whenever two projects are open.

### 3.2 Recommendation: bind to the active tab, explicitly

Rather than adding a `document_id` to all 110 tools:

- **MCP binds to the *active* tab**, and this is stated in [10 §2](10-mcp-tools.md) rather than assumed.
- A new read-only tool, **`get_active_document`**, returns the active document's id, name and path. An agent can then check *what it is about to edit* — which is the actual safety requirement.
- A new tool, **`set_active_document { id }`**, switches tabs. Explicit, undo-free, and it makes multi-project automation possible without threading an id through every call.
- Tools accept an **optional** `document_id`; when present it must match the active document, otherwise `DocumentMismatch`. This gives an agent a cheap assertion — "I believe I am editing X" — without making the argument mandatory everywhere.
- **`EngineSession` follows the active tab.** Switching tabs re-binds it; playback stops on the outgoing tab.

Rationale for optional-with-check over mandatory: mandatory ids would touch every tool and every schema for a case most sessions never hit, while an unchecked implicit binding is how an agent edits the wrong project. The assertion form costs one optional field and turns a silent error into a refusal.

### 3.3 CAP-019 verification

The parity tests must **state which document they bind to** and assert `get_active_document` before acting. Without that, a parity test passing with two tabs open proves nothing.

---

## 4. Acceptance

| # | Test |
|---|---|
| 1 | A group move of nine clips is one undo step; an import of forty assets is one step |
| 2 | A failed multi-member operation commits nothing |
| 3 | Two slider drags separated by 600 ms are two steps; a continuous 10 s drag is capped, not merged into one |
| 4 | History respects both step and byte bounds; a branch is never auto-trimmed; the retention floor holds against one huge command |
| 5 | Undoing a video edit from vector mode switches mode and shows the change |
| 6 | A job whose target was undone away skips its commit and diagnoses; undo does not cancel a running job |
| 7 | `height_px` no longer lives in `Document`; changing it does not enter history |
| 8 | A document with an unknown effect, transition and graph op loads, renders inert, warns once, and **re-saves byte-identically** for the unknown parts |
| 9 | A newer-minor document warns before the first save and offers save-as-copy |
| 10 | An out-of-window older document refuses, naming the version that can read it |
| 11 | With two tabs open, an MCP call with a mismatched `document_id` is refused; `get_active_document` reports correctly |
| 12 | CAP-019 parity tests bind explicitly and pass with two documents open |

Test 8 is the one that makes CAP-020 real — round-trip byte-identity for unknown data is the only proof that preservation actually works.

---

## 5. Amendments required — **all applied 2026-07-20**

- **[01 §9](01-data-model.md)** — the §2 forward-compat rules and the version policy table.
- **[01 §10](01-data-model.md)** — the §1 undo contract: one-verb-one-unit, coalescing bounds, bounds policy, job-commit rule.
- **[04 §1.4](04-ui-mode-timeline.md)** — `height_px` and view state move to a sidecar (§1.6).
- **[10 §2](10-mcp-tools.md)** — active-tab binding, the two new tools, the optional `document_id` check (§3.2).
- **[SPEC.md](SPEC.md)** — no change. §1.6 removes the exception rather than weakening the constraint, which is the point.
