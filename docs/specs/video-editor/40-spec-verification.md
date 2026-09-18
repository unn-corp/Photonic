# 40 — Spec Verification: Drift Gating and Acceptance Tracking

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** CI owner, spec authors, engine maintainers

**Depends on:** [11-testing-phasing.md](11-testing-phasing.md), [27-spec-audit.md](27-spec-audit.md) (the audit whose findings this exists to prevent recurring), [ROADMAP.md](ROADMAP.md) §10.

**Owns:** the mechanism that keeps the specification set true — automated drift detection, and an aggregated acceptance index.

---

## 1. The problem this solves

[27](27-spec-audit.md) found **17 spec-versus-code drift findings**. All were fixed on 2026-07-20. **They will recur**, because nothing detects drift except a human reading the spec and the code side by side, and that happened once, by accident, while researching something else.

The failure is systemic, not careless. A spec says `CURRENT_FORMAT_VERSION` goes 2→3; someone bumps it to 4 in the code; nothing connects the two. Every doc in the set decays this way, silently, and the decay is invisible until an implementer trusts a stale line and builds the wrong thing — which is exactly what [27 SD-3](27-spec-audit.md#3-sd---spec-versus-code-drift) and [SD-7](27-spec-audit.md#3-sd---spec-versus-code-drift) would have caused.

**The repository already contains the answer.** `docs/mcp-api.md` is *generated* from `dump_tools` and byte-compared in CI (`ci.yml:121`). It is the one document that cannot drift. This document generalises that pattern to the claims in the design specs that are mechanically checkable.

A second, smaller problem: the specs now carry **roughly 100 acceptance criteria across ten documents**, and nothing aggregates them. They are unreachable as a checklist, so in practice they will not be walked.

---

## 2. What is checkable, and what is not

Not every spec claim can be verified automatically, and pretending otherwise produces a tool nobody trusts.

| Claim class | Checkable? | Example from [27](27-spec-audit.md) |
|---|---|---|
| A named constant's value | **Yes** | SD-3 — `CURRENT_FORMAT_VERSION = 4`, doc said 2→3 |
| An enum's variant set | **Yes** | SD-6 — `EngineCmd` missing five variants |
| A struct's field set | **Yes** | Doc 01's `Clip` missing three fields |
| A named symbol exists | **Yes** | SD-1 — "crate does not exist yet" |
| A dependency is present | **Yes** | SD-5 (`insta`), SD-13 (`rubato`, `subparse`, `egui-snarl`) |
| A file or path exists | **Yes** | SD-16's stale line citations |
| A CI step exists | **Yes** | SD-4 — "CI has no ffmpeg" |
| Which type a cache is keyed on | Partly — checkable as a field set | SD-10 |
| Whether a code path is *reached* | **No** | SD-16's "unreferenced from any live pass" needed a call-graph read |
| Behavioural claims ("blends in linear") | **No** | A-1 — needs a test, not a checker |
| Design rationale | **No** | — |

**Rule: the checker verifies structure, tests verify behaviour, humans verify design.**

Re-counted against the actual [27](27-spec-audit.md) `SD-*` list rather than estimated: **10 or 11 of 17** are structurally checkable (SD-1, 2, 3, 4, 5, 6, 8, 10 partially, 13, 15, 17). **Six are not** — SD-7 (`EngineCmd::Export` exists but is a *stub*), SD-11 (drop-frame parses but ignores the separator), SD-12 (`master_level()` compiles and returns `None`), SD-14 (a stale phase model), SD-16 (needed a call-graph read), and SD-9 (which turned out to be false anyway). Every one of those six is a claim about **what the code does**, not about what exists.

### 2.1 What this tool would NOT have caught

Stated plainly, because the temptation is to oversell it. Four code defects were found by the 2026-07-20 audits, and **the checker as specified catches none of them**:

| Defect | Why the checker misses it |
|---|---|
| Curve-editor arrow-nudge gated on `!keyboard_captured` (dies on focus) | **No spec claim existed** to compare against. Found by reading code against *intent* |
| Node-editor shortcuts gated on `rect_contains_pointer` | Same — zero spec references to the symbol |
| `revision_contract.rs` still feature-gated after its API shipped | The spec's statement ("declared, empty, off by default") is *still true*. The violated commitment — "remove the gate when P1 lands" — is prose |
| Doc 12's `video` kill-switch does not exist | Checkable in principle, but needs an assertion type §3.2 does not have |

The first two are the interesting ones, and they share a signature: **code written correctly and then disabled by its own guard.** No structural checker can see that, because nothing structural is wrong. What finds it is a **lint over banned patterns** — which is a different tool, and cheaper (§3.6).

---

## 3. The drift checker

### 3.1 Anchored code blocks

A fenced block in a spec may declare its source. The checker extracts the declared item from the real code and compares.

````markdown
```rust
// spec-source: crates/photonic-core/src/timeline/sequence.rs::Track
pub struct Track {
    pub id: TrackId,
    ...
}
```
````

- `spec-source: <path>::<item>` — the item is a struct, enum, const, or fn signature.
- Comparison is **structural, not textual**: field and variant *names* and *order*, constant *values*, and for functions the *signature*. Doc comments, formatting, and elided fields marked `...` are ignored.
- `...` means "the doc deliberately abbreviates here" and suppresses the completeness check for that block, but still verifies that everything listed **does** exist. This matters — most spec blocks are abbreviations, and a checker that demands exhaustive listings would make the docs unreadable.

### 3.2 Inline assertions

For claims not naturally expressed as a code block:

```markdown
<!-- spec-assert: const photonic_core::timeline::document::CURRENT_FORMAT_VERSION == 4 -->
<!-- spec-assert: dep-absent insta -->
<!-- spec-assert: dep-present proptest -->
<!-- spec-assert: symbol-exists crates/photonic-video/src/graph/ops.rs::merge_pixel -->
<!-- spec-assert: ci-step-contains ffmpeg -->
<!-- spec-assert: feature-present photonic-app/video -->
<!-- spec-assert: feature-absent photonic-core/video-p1-contract -->
<!-- spec-assert: if symbol-exists ...::CommandHistory::revision then feature-absent photonic-core/video-p1-contract -->
```

`feature-present` / `feature-absent` were added after the 2026-07-20 audit found two defects that needed them and neither existed. **The conditional form is the more important addition:** a spec commitment of the shape *"X is temporary and goes away once Y lands"* is invisible to a flat assertion, because the assertion stays true — it is the *combination* that became wrong. That is exactly the shape of the stranded `video-p1-contract` gate, and it will recur every time scaffolding outlives its trigger.

Each is a one-line, machine-checkable fact. `dep-absent` is as valuable as `dep-present` — it is what would have caught SD-5 and SD-13, where docs asserted dependencies that were never added and the absence hid an unimplemented strategy.

### 3.3 Implementation

`tools/check-spec-drift.py`, run in CI beside the existing MCP doc gate.

Parsing Rust well enough is the only real design question. **Recommend `syn` via a small Rust helper binary** (`tools/spec-extract`) that emits a JSON index of every public struct, enum, const and fn signature in the workspace; the Python checker consumes that index. Rationale: regex over Rust source is a well-known trap — attributes, generics, `cfg`, macros and nested types all break it — and the workspace already builds Rust, so a helper binary costs nothing new. `syn` is already in the dependency graph transitively.

**Rejected:** `rust-analyzer` in batch mode (heavyweight, slow, unstable interface) and doc-comment round-tripping (would require restructuring the specs around the code rather than the reverse).

### 3.4 Failure mode

A drift failure names the document, the line, the expectation and the reality:

```
docs/specs/video-editor/01-data-model.md:366
  spec-assert: const CURRENT_FORMAT_VERSION == 3
  actual:      4  (crates/photonic-core/src/document.rs:110)
```

**Blocking in PR CI**, per [37 §4.2](37-robustness.md#42-recommendation-two-tiers-and-be-honest-about-which-is-which) — it is deterministic and machine-independent, so it belongs in the hard-gate tier. An intentional code change updates the spec in the same commit, which is the entire point: the spec becomes part of the change rather than something to reconcile later.

### 3.5 Adoption

**Do not annotate all 40 documents.** Annotate where drift has actually bitten, then extend:

1. The [27](27-spec-audit.md) `SD-*` sites — all 17, since those are proven drift-prone.
2. Every `struct`/`enum` block in [01-data-model.md](01-data-model.md) — the highest-density factual document in the set.
3. [02-engine.md](02-engine.md)'s `IrOp`, `EngineCmd`, and the cache table.
4. Dependency assertions wherever a doc names a crate.

Everything else stays unannotated and unchecked, which is honest: an unannotated block makes no machine-verifiable claim.

---

### 3.6 The complement: lints and `--all-features`

The checker is one of three mechanisms, and on its own it is the weakest against the defects actually found.

| Mechanism | Catches | Cost |
|---|---|---|
| **Drift checker** (§3) | Structural doc-vs-code divergence — ~10 of 17 `SD-*` | Days |
| **Banned-pattern lints** | Code that is wrong in itself. [41 §8](41-accessibility.md#8-acceptance) item 3 already specifies one: reject `rect_contains_pointer` and `memory().focused().is_none()` inside key-handling blocks. **That single lint catches two of the four defects** the checker cannot | Hours |
| **`cargo test --all-features` in CI** | Test modules stranded behind a feature gate. Catches the third defect, and any future one of its kind, with **one line of CI config** | Minutes |

**Recommendation: do all three, cheapest first.** `--all-features` is one line and would have surfaced 8 silently-excluded tests. The a11y lint is hours and catches two real defects. The checker is days and catches the largest *class* but none of the four most interesting instances.

Stating the order this way is deliberate: the checker is the most interesting thing to build and the least urgent thing to build, and it would be easy to spend a week on it while a one-line CI change sits undone.

## 4. Acceptance index

### 4.1 The gap

Ten documents ([28](28-security-model.md), [30](30-effect-catalogue.md)–[39](39-document-lifecycle.md)) carry an `## Acceptance` section, roughly 100 criteria in total. Nothing collects them. [ROADMAP §10](ROADMAP.md#10-definition-of-done)'s definition of done requires that they pass, with no way to enumerate what "they" are.

### 4.2 Format

Each criterion gets a stable id and a status marker:

```markdown
| # | Test | Id | Status |
|---|---|---|---|
| 1 | CPU/GPU parity across every IR enum variant | `ACC-32-08-01` | `open` |
```

`ACC-<doc>-<section>-<n>`. Status is one of `open` · `covered` (a test exists, named) · `waived` (with a recorded reason). A generated `docs/specs/video-editor/ACCEPTANCE.md` aggregates them, grouped by owning doc and by status.

### 4.3 Linking to tests

A `covered` criterion names its test:

```rust
/// Covers: ACC-32-08-01
#[test]
fn cpu_gpu_blend_mode_equivalence() { ... }
```

The generator scans for `Covers:` annotations and cross-references. **A criterion claiming `covered` with no matching test is a build failure** — otherwise the status field becomes decorative, which is the failure mode of every checklist that is not enforced.

### 4.4 What this is not

Not a replacement for [11](11-testing-phasing.md), which owns test *design* — corpus layout, comparison metrics, harness structure. This owns only the **index**: what has been promised, and whether anything checks it.

---

## 5. Recommendation on scope

**Build §3 first and §4 second.** Drift gating prevents a recurring, demonstrated failure; acceptance tracking prevents a hypothetical one. If only one is built, build the checker.

**Do not extend either into prose.** The temptation will be to check that a doc "mentions" something, or to lint style. That produces false positives, and a checker with false positives gets disabled — at which point the real checks stop running too.

---

## 6. Acceptance

| # | Test | Id | Status |
|---|---|---|---|
| 1 | `spec-extract` emits a complete index of public structs, enums, consts and fn signatures for the workspace | `ACC-40-06-01` | `open` |
| 2 | An anchored block whose field set diverges fails, naming doc, line, expectation and reality | `ACC-40-06-02` | `open` |
| 3 | A block using `...` passes when abbreviating, and still fails on a **listed** field that does not exist | `ACC-40-06-03` | `open` |
| 4 | `dep-present` and `dep-absent` both fail correctly against `Cargo.toml` | `ACC-40-06-04` | `open` |
| 5 | Re-introducing each of the 17 [27](27-spec-audit.md) `SD-*` drifts is caught — a regression suite built from real historical drift | `ACC-40-06-05` | `open` |
| 6 | The checker runs in under 10 s on the full workspace | `ACC-40-06-06` | `open` |
| 7 | `ACCEPTANCE.md` regenerates deterministically; a `covered` criterion with no matching test fails the build | `ACC-40-06-07` | `open` |
| 8 | The checker is blocking in PR CI and reported in the same place as the MCP doc gate | `ACC-40-06-08` | `open` |

Test 5 is the one that proves the tool is worth having: **it is built from drift that actually happened**, so it measures the tool against the real failure rather than an imagined one.

---

## 7. Sequencing

| Order | Item | Cost |
|---|---|---|
| 1 | `tools/spec-extract` (Rust, `syn`) → JSON index | Small |
| 2 | `tools/check-spec-drift.py` — assertions only (§3.2), no block parsing | Small; catches SD-3/4/5/13 immediately |
| 3 | Annotate the 17 `SD-*` sites; build the §6.5 regression suite | Small |
| 4 | Anchored-block comparison (§3.1) | Medium |
| 5 | Annotate [01](01-data-model.md) and [02](02-engine.md) | Medium |
| 6 | Acceptance ids + `ACCEPTANCE.md` generator | Medium |
| 7 | `Covers:` cross-referencing and the enforcement in §4.3 | Small |

**Order the work by cost, not by interest** (§3.6): add `cargo test --all-features` to CI first (one line), then the [41](41-accessibility.md) banned-pattern lint (hours), then steps 1–3 above (about a day). That sequence catches the three known code defects *before* the checker exists, and the checker then catches the larger structural class that recurs.

The argument for doing all of it before writing further specifications is unchanged: every document added without it is another surface that will silently decay.
