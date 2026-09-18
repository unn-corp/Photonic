# 12 — Agent Execution Plan: Model Tiers & Parallelism

> **⚠ Superseded for status; retained for sequencing.** This document's P1–P8 wave plan describes a phase model that [ROADMAP.md](ROADMAP.md) §2/§3a replaced — the roadmap now tracks ~60 live `G-`/`D-`/`K-`/`E-`/`X-` items with per-item status, none of which map onto P1–P8. [27 SD-14](27-spec-audit.md#3-sd---spec-versus-code-drift) flagged the phase model as stale across 00, 11 and 12; **00 and 11 were corrected on 2026-07-20 and this document was not**, leaving it the last copy asserting the model straight-faced.
>
> What remains durable and should survive any rewrite: **§3's dependency spine** and **§4's choke-point file list** (`history/mod.rs`, `schema_gen.rs`, `dispatch.rs`, `panels/mod.rs`, `app/mod.rs` — all still real and still contended). What is stale: §5's wave schedule and its gates, several of which reference an acceptance harness that does not exist ([29 QA-1](29-qa-spec.md)).
>
> Three specific corrections, per the 2026-07-20 audit:
> - **§3 spine items 0 and 0b are delivered.** `CommandHistory::revision`/`changes_since`/`affected_nodes` all ship; `graph/` holds a full evaluator, not the "types only, no evaluator body" stub this document plans for.
> - **§6's rollback runbook — RESOLVED (2026-07-21), audit action CLOSED (2026-07-28).** It used to open with "flip the `video` cargo feature off", and no such feature exists — the workspace now ships **zero** cargo features (`photonic-core`'s former `video-p1-contract` gate graduated to always-on, commit `1ccbeea`), and `photonic-gui` depends on `photonic-video` unconditionally. Per the QA-1 decision the kill-switch was **not** implemented (it would buy nothing the revert-range step doesn't) and §6 was rewritten around the revert-range step alone. The QA audit's second recommended code action ("either implement or delete doc 12's `video` kill-switch", [ROADMAP §3c](ROADMAP.md#3c-later-audits-and-cross-cutting-contracts)) is therefore **resolved as *delete***, and the decision is now machine-guarded rather than prose-only — see §6's runbook bullet and its `spec-assert` lines.
> - **§6's coordination primitives name a task registry** (`TaskCreate`/`TaskList`) that this environment does not provide, while §1 declares the document governed by the `claude-subagent-protocol`, which specifies a Session Agent Registry instead. Pick one.


How AI implementation agents build this module: which model tier each role runs on (Fable → Opus → Sonnet), what runs in parallel, what is strictly sequential, and the coordination rules. Governed by the workspace `claude-subagent-protocol` (spawn-once + SendMessage reuse, sonnet-first, permission-before-opus).

---

## 1. Model-tier policy

Tier by **decision density**, not by phase prestige. Escalate only where a wrong decision is expensive to unwind.

| Tier | Use for | Rationale |
|---|---|---|
| **Fable** (main session) | Orchestration; frame-graph IR final design sign-off; cross-doc consistency reviews; phase-gate reviews; anything touching ≥3 crates at once; unblocking stuck agents | Highest judgment per token; keeps global context; never used for bulk code emission |
| **Opus** (permission-gated per protocol) | Architecturally hard, self-contained builds: P1 renderer rework (dirty tracking + persistent buffers + COMPOSITE_SHADER), engine core (graph compile/eval, decode scheduler, A/V clock), audio DSP correctness, color-math kernels | Deep single-domain reasoning; mistakes here are structural |
| **Sonnet** (default) | Well-specified implementation against these docs: timeline data model + edit ops (01 is prescriptive), panels/UI, MCP tool wiring (mechanical 4-touch-point pattern), caption/provider adapters, presets, tests, docs/Features.md updates, golden-corpus authoring | Spec docs carry the design; execution is bounded; cheapest correct tier |

Standing rules:
- **Sonnet-first**: every task starts Sonnet unless this plan marks it Opus/Fable. An agent that detects it is out of depth reports back rather than improvising; orchestrator re-issues at higher tier.
- **Opus requires explicit user permission** at spawn time (protocol requirement). Batch permission per phase, not per agent.
- **Review inversion**: code written at tier N is reviewed at tier ≥ N. Phase-gate reviews (architect + verifier) run at Fable-in-session or Opus.

## 2. Role catalog

| Role | Tier | Cardinality | Responsibility |
|---|---|---|---|
| Orchestrator | Fable | 1 (main session) | Wave dispatch, integration, merge order, conflict arbitration |
| Engine builder | Opus | 1 per engine work-package | photonic-video internals (02) |
| Render builder | Opus | 1 | P1 + video texture path (03) |
| Core-model builder | Sonnet | 1 | core::timeline types, ops, commands, migration (01) |
| UI builders | Sonnet | 2–3 parallel | Timeline panel, monitor/transport, adaptive panels (04); later color page (07 UI), node editor (08 UI), mixer UI (09 UI) |
| MCP builder | Sonnet | 1 | handlers/video.rs + schema/args/dispatch + doc regen (10) |
| Domain builders | Sonnet | 1 per domain | Captions/providers (06), presets/import-export surface (05), grading ops (07 math may escalate Opus), audio DSP (09 DSP is Opus), node catalog (08) |
| QA (arcwright-qa) | Sonnet | 1, reused via SendMessage | Runs test suites, MCP story scripts, perf harness per phase |
| Verifier (arcwright-verifier) | Sonnet | 1, reused | Goal-backward check at each phase gate (L1 exists → L4 real data flows) |
| Architect reviewer (arcwright-architect) | Opus | 1 (2 during P3 and P8) | Design-conformance review of each wave's diff against these docs. P3/P8 run concurrent Opus builder waves — a single reviewer would queue them; spawn a second architect instance for those phases (or serialize the Opus waves if permission for a second is declined) |

## 3. Hard sequencing constraints (cannot parallelize)

Dependency spine — each item blocks everything after it:

0. **CommandHistory::revision extension** (bump on execute/undo/redo + public accessor + `changes_since` introspection, 03 §2.1) → P1 tessellation cache AND P3 engine snapshotting both consume it. Lives in `photonic-core/src/history/` (a §4 choke-point file) but must land in **P1**, before the P2 Core-model builder touches history.
0b. **`ir.rs` signature stub (design-only)** — the `IrOp` enum + `FrameGraph` type signatures from 02 §2, landed as a compile-checked stub before P1's render-builder wave: 03 §3.4's texture pool and Tier-B conversion pass are keyed by IR-shaped contracts, and without the stub the P1 render builder guesses at them. Authored by the orchestrator or architect (design tier), reviewed against 02 verbatim; no evaluator body, types only.
1. **01 data model types + migration** → everything (all crates import these types).
2. **02 frame-graph IR definition** (ir.rs signatures, not full evaluator) → engine eval, 07 GradeOps, 08 catalog lowering, 03 texture pool contract.
3. **P1 renderer rework** → any real-time playback work (P3+ GPU eval). Vector editing golden-corpus must pass BEFORE P1 merges (00 §7 risk 1).
4. **Engine facade (EngineCmd/EngineFrame API, 02 §1)** → timeline UI playback wiring (04), MCP playback/export tools (10).
5. **timeline/ops.rs edit ops** → both GUI interactions (04) and MCP tools (10) — parity by construction requires ops land first.
6. **Mixer core (P3: gain/pan/mute/solo)** → EQ/comp/automation/ducking (P8) — DSP nodes plug into an existing graph.
7. **Per-clip composition splice (02 §2 step 3) proven by tests** → fusion UI (P7/P8) — UI must not drive engine design.
8. **08 authors review 02's IR before P3 engine code starts** (explicit gate, 00 §7 risk 2).

## 4. Safe parallelism (and its boundaries)

Parallel-safe because **file-disjoint** (worktree isolation optional but recommended for same-crate work):

| Wave-mates | Why safe |
|---|---|
| Core-model builder (photonic-core/src/timeline/) ∥ Render builder (photonic-render/) | Different crates; IR types stubbed via 02 signatures |
| Engine decode/media (photonic-video/src/{decode,media}/) ∥ Engine graph (graph/) ∥ Engine audio (audio/) | Sibling modules, interfaces pinned by 02 §1 |
| Timeline panel (gui/src/app/timeline/) ∥ Media pool panel ∥ Monitor/transport | Separate new module files; shared PhotonicApp fields negotiated up front by orchestrator (single small PR adding fields first — the "fields PR" pattern) |
| MCP builder ∥ any GUI builder | Different crates; both call the same core ops |
| Captions (06) ∥ Grading (07) ∥ Audio DSP (09) ∥ Export presets (05) | Disjoint engine submodules + disjoint panels, all downstream of the spine |
| Test/golden-corpus authoring ∥ everything | tests/ + tools/ only |

NOT parallel-safe (serialize or pre-split):
- Two agents touching `history/mod.rs`, `schema_gen.rs`, `dispatch.rs`, `panels/mod.rs` enums, or `app/mod.rs` struct fields — these are **append-choke-point files**. Rule: orchestrator lands a skeleton PR (enum variants, empty match arms, struct fields) first; wave agents then fill disjoint bodies.
- Anything editing `document.rs` / `migration.rs` concurrently.
- Two agents inside the same engine submodule.

## 5. Per-phase wave plan

Notation: `[..]` = one parallel wave; `→` = barrier (previous wave merged + reviewed).

**P1** — decomposed into gate-able stories (the pattern every phase follows; stories map 1:1 onto 11 §6's exit-criteria checkboxes):
- **S1** `CommandHistory::revision` extension (spine 0, Sonnet) — bump on execute/undo/redo + accessor + `changes_since`; existing history tests stay green.
- **S1b** `ir.rs` signature stub (spine 0b, design tier) — types only, reviewed against 02.
- **S2** Vector golden-output baseline capture (Sonnet, ∥ S1) — corpus captured on pre-P1 code, blessed, committed.
- **S3** Dirty tracking + persistent GPU buffers (Opus, after S1) — existing compositor/headless suites pass unchanged; S2 corpus byte-identical.
- **S4** COMPOSITE_SHADER wiring + render-to-texture (Opus, after S3) — S2 corpus re-diffed: byte-identical for untouched paths, PSNR ≥45dB for previously-approximated blend modes (03 §2.6).
- **S5** f16 video texture path + pool (Opus, after S1b+S4) — 03 §3 contract consumed, conversion-pass WGSL in the validation table.
- Gate: architect diff review + QA full-suite + golden diff + CI green.
**P2** `[Core-model (Sonnet, Opus review on commands/migration)]` → fields/skeleton PR (orchestrator) → `[Timeline panel ∥ mode switch+monitor shell ∥ MCP timeline-edit tools]` → gate (AS-1 arrange/cut via MCP script).
**P3** IR review gate (08 authors + architect) → `[Engine graph (Opus) ∥ decode/media (Opus) ∥ audio core (Sonnet) ∥ proxy (Sonnet)]` → **interim architect checkpoint on the engine facade** (EngineCmd/EngineFrame/EngineStatus as built, before any consumer code — a wrong facade decision must surface here, not at phase end) → `[playback wiring in GUI ∥ MCP playback/render_frame_at ∥ media pool panel]` → gate (SS-1 subset on proxy; CAP-022 crash-recovery of a timeline project verified per D-12).
**P4** `[Export loop+encoders (Sonnet, Opus review) ∥ preset system+dialog ∥ aspect/reframe UX ∥ transcode tool]` → gate (AS-1 minus captions).
**P5** `[Provider trait+adapters (Sonnet) ∥ caption track UI ∥ TTS flow ∥ subtitle interchange]` → gate (AS-1 complete, provider mock in CI).
**P6** `[Keyframe curve editor ∥ vector-doc animation binding (Opus — touches core+render) ∥ transitions ∥ effect-param animation]` → gate (AS-3 core).
**P7** `[Grade ops GPU kernels (Opus) ∥ color page UI ∥ scopes (Sonnet) ∥ LUT/CDL parsers (Sonnet)]` → gate (AS-2 grade pass).
**P8** `[Node editor UI ∥ node catalog lowering ∥ DSP fx (Opus) ∥ automation lanes UI ∥ ducking preset]` → final gate (AS-2, AS-3 complete; SS-1..3 full).

Every gate = QA suite green + verifier goal-backward pass + architect diff review + CI gates + Features.md/mcp-api.md updated. Fail → findings loop back to the owning builder (SendMessage reuse, no respawn).

## 6. Communication & state rules

- Task registry (TaskCreate/TaskList) is the single work ledger; one task per work-package, `blockedBy` encodes §3 spine.
- Builders report via Agent return values; long-lived roles (QA, verifier, architect) are spawned once and re-engaged via SendMessage.
- Worktree isolation for any wave with ≥2 builders in one crate.
- No agent edits another agent's in-flight files; conflicts escalate to orchestrator, never resolved by force-push.
- Context economy: builders receive the relevant spec doc numbers, not the whole set; 01/02 always included.
- **Story template** (every task-registry entry uses it): goal (one sentence), spec sections (doc §), files owned, DoD (tests that must pass + exit-criteria checkbox it closes), tier, blockers. Prevents scope drift between parallel builders in one wave.
- **Phase-kickoff checklist** (orchestrator, before dispatching a wave): spine dependencies merged; CI green on branch tip; the phase's 11 §6 exit criteria re-read; choke-point skeleton PR landed if the wave needs one; reviewer capacity confirmed (§2 architect note).
- **Batched reviews (process amendment, locked with user 2026-07-07):** reviews are NOT per-story. The orchestrator verifies each story's DoD (tests/golden/clippy) at commit time; architect + QA review happens ONCE per phase gate over the whole phase diff. The single exception is the P3 engine-facade interim checkpoint (§5 P3), which stays because consumer code builds on the facade mid-phase. Story-level review pings are retired.
- **Rollback runbook:** every phase merge is revertable as one commit range. Incident procedure: **revert range on a fix branch → re-land** once fixed. Recorded here so mid-incident nobody designs the procedure from scratch. **The revert range is the kill-switch** — see the decision record below.

### 6.1 Decision record — the `video` kill-switch was deleted, not built

An earlier draft of §6's runbook opened with "flip the `video` cargo feature off". No such feature was ever built, and the 2026-07-20 QA audit recorded it as one of two recommended code actions ([ROADMAP §3c](ROADMAP.md#3c-later-audits-and-cross-cutting-contracts)): *implement or delete*. **Resolved as delete, 2026-07-28.** Reasoning, recorded so it is not re-litigated:

1. **It was never a runtime disable.** As specified in [11 §7](11-testing-phasing.md#7-rollout-guards) it is a *cargo* feature on `photonic-app` gating `photonic-video` as an optional dependency. Flipping it during an incident means a rebuild and a re-ship — the same cost as the revert-range step, minus the revert's guarantee that you land on a tree that was green.
2. **Its rationale expired with the phase model.** 11 §7 positions the feature as *default-on from P3*, i.e. it exists only to keep pre-P3 half-built code out of the default build. That window closed: this document's own banner records the P1–P8 phase model as superseded by [ROADMAP](ROADMAP.md) §2/§3a, and `photonic-gui` now depends on `photonic-video` unconditionally (`crates/photonic-gui/Cargo.toml`). "No-video" is not a configuration anyone ships.
3. **It is not implementable as 11 §7 writes it.** That bullet puts the feature on `photonic-app` "gating `photonic-video` as an optional workspace dependency" — but `photonic-app` has no `photonic-video` dependency to make optional (`crates/photonic-app/Cargo.toml` lists core/render/gui/mcp only); it reaches the video crate transitively through `photonic-gui`. Building it means a feature-forwarding chain `photonic-app` → `photonic-gui` → `photonic-video`, which 11 §7 never specified and nobody has costed.
4. **And doing it now is strictly a cost.** It would add `optional = true` plus `#[cfg(feature = "video")]` at every call site across `photonic-gui`/`photonic-app`, and a second `video`/`no-video` CI axis on top of the existing 3-OS matrix — a build configuration nobody runs, which therefore rots. 11 §7 makes this argument itself ("the repo does not carry long-lived feature-flagged forks") and then specifies the feature anyway; the argument wins.
5. **Nothing depends on it.** No code references it (`grep -ri kill.switch crates/` is empty), no CI job selects it, and no other spec builds on it. The only remaining references are descriptive, not load-bearing.

Guarded mechanically per [40 §3.2](40-spec-verification.md#32-inline-assertions) so this cannot silently reverse — if someone re-adds the feature, the drift gate fails and this decision record is what they are sent to:

<!-- spec-assert: feature-absent photonic-app/video -->
<!-- spec-assert: feature-absent photonic-gui/video -->

**Still stale elsewhere (follow-up, not owned by this doc):** [11 §7](11-testing-phasing.md#7-rollout-guards)'s "Feature-gating strategy" bullet still specifies the compile-time `video` feature as live, and [40 §2.1](40-spec-verification.md#21-what-this-tool-would-not-have-caught)'s table still lists this as an open defect needing "an assertion type §3.2 does not have" — §3.2 has had `feature-absent` since it was added, and the two asserts above are it.
