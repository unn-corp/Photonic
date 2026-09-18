# 38 — Sequence Semantics: Transitions, Nesting, Frame-Rate Conform

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** data-model owner, engine maintainers, timeline panel owner

**Depends on:** [01-data-model.md](01-data-model.md), [02-engine.md](02-engine.md), [05-import-export.md](05-import-export.md), [08-fusion-node-flows.md](08-fusion-node-flows.md), [32-engine-contracts.md](32-engine-contracts.md), [36-error-model.md](36-error-model.md).

**Owns:** [27 U-1](27-spec-audit.md#5-u---under-specified-contracts) (transition timing), [27 O-2](27-spec-audit.md#o-2--p1--cap-005--nested-sequences) (CAP-005 nested sequences), [27 U-7](27-spec-audit.md#5-u---under-specified-contracts) (mixed frame rates).

---

## 1. Transitions

### 1.1 The apparent contradiction, resolved

[01 §5](01-data-model.md) describes `transition_in` as "overlaps previous clip", while [01 §4](01-data-model.md) makes non-overlap a **validated invariant** enforced by `Sequence::validate()`. Read literally, transitions are impossible.

They are not, because the implemented model does not overlap clips at all. **It borrows media handles.**

> During the first `transition.duration` ticks of clip B, the compositor samples B normally **and** samples the outgoing clip A *past its own out point*, into A's remaining source handle, then mixes the two.

Timeline layout is unchanged; no clip moves; the non-overlap invariant holds; the sequence does not shorten. **"Overlaps previous clip" is wrong and must be corrected in 01** — what overlaps is the *source sampling*, not the clips.

This is a better model than the reference's, which physically overlaps two playlists per track to express the same thing ([34 §3.1](34-interchange.md#31-structural-mapping) has to detect and collapse that on import).

### 1.2 Insufficient handle

A borrows from A's source beyond its out point. That media may not exist — A may already end at its source's last frame.

**Recommend: clamp and diagnose.**

1. Compute available handle: `source_duration - (source_in + duration)`, in source time, through the clip's `SpeedMap`.
2. If it is less than the transition duration, **shorten the transition to the available handle** and emit `Compile::TransitionHandleClipped` (`Info`) naming the clip and both durations.
3. If it is zero, **the transition does not render**; emit a `Warning`.

Rejected alternative: holding A's last frame for the shortfall. It produces a frozen image in the middle of a dissolve, which reads as a bug rather than a limitation — and unlike a shortened transition, the user cannot see why.

**Never silently extend the sequence** or move clips to make room. Timeline layout is the user's.

### 1.3 One transition per cut

`transition_in` on B and `transition_out` on A at the same cut describe the same event twice, and nothing today says which wins.

**Recommend: a transition at a cut is owned by the *incoming* clip's `transition_in`.** `transition_out` remains meaningful **only** where there is no following clip — into a gap or at the sequence end — where it is a fade-out.

This removes the ambiguity structurally rather than by precedence rule. `Sequence::validate()` gains: *a clip's `transition_out` must be `None` when another clip starts exactly at its end.* Migration: where both are set at a cut, keep `transition_in` and drop `transition_out`, with a load-time `Info` diagnostic.

### 1.4 Audio

The audio crossfade window **equals the video transition window** by default, using the same borrowed handles, with the same clamping. It is overridable per clip via the existing `AudioFade`.

Interaction with declick ([31 §4](31-audio-architecture.md#4-boundary-declick)): where a transition is present the cut is not a discontinuity, so **declick does not engage** — the crossfade already provides continuity.

### 1.5 The 08 contradiction

[08 §2](08-fusion-node-flows.md)'s crossfade rule is stated in terms of "clips overlapping", which per §1.1 never happens. It must be restated against the borrowed-handle model, or it is unsatisfiable as written. Recorded here because it is an `A-*`-class contradiction that [27](27-spec-audit.md) filed under `U-1`.

---

## 2. Nested sequences (CAP-005)

[01 §5](01-data-model.md) gives one line plus a cycle-check note; [02 §2](02-engine.md) a parenthetical. Six questions have never been answered.

### 2.1 Audio

**The nested sequence's master-bus output is the clip's audio.** The inner mix — track gains, pans, fades, master processing — is fully applied, and the outer clip's `ClipAudio` (gain, fades, channel map) then applies on top, exactly as for an asset clip.

The inner sequence's tracks are **not** individually visible to the outer mixer. A nest is one stereo source. (Stem-style access to inner tracks would be a genuinely useful future feature; it is out of scope and should not be implied.)

### 2.2 Frame-rate mismatch

The inner sequence has its own `frame_rate`. **Recommend: the inner sequence is evaluated at the tick the outer asks for**, mapped through the same source-time arithmetic as any clip — Photonic's `Tick` is rate-independent, so there is no conversion, only sampling.

Where inner and outer rates differ, sampling selects the inner frame covering that tick — §3's conform rule, applied to a sequence instead of a file. Emit `Info` once per nest on rate mismatch, because it is a legitimate but easily-unintended state.

### 2.3 What of the inner sequence applies

| Inner property | Applies to the nest? | Why |
|---|---|---|
| Tracks, clips, effects, grades | **Yes** | This is the content |
| Master effects / grade ([35 §2](35-model-decisions.md#2-effect-scopes-and-the-adjustment-clip-interaction)) | **Yes** | Part of the sequence's look |
| Caption tracks | **Yes** | Part of the picture |
| `work_range` | **No** | A preview/export range, not content — honouring it would truncate the nest for reasons invisible in the outer timeline |
| `active_format` / `formats` | **No** — the outer format governs | A nest reframes to its host, which is what makes multi-format work ([26 K-F3](26-kdenlive-mlt-parity.md#k-f3--multi-format-render)) compose |
| Markers | **Visible, read-only** | Useful navigation; must not become editable through the nest, or two edit paths exist for one object |
| Project graph | **No** | It is `TimelineProject`-level and applies once, at the top. Applying it per nest would compound it |

### 2.4 Trimming versus inner length

The nest clip carries `start`, `duration` and `source_in` like any clip. If the inner sequence later **shortens** below what the nest references, the tail has no content.

**Recommend: consistent with decision S8** (which fixed this for replace-edit) — **hold the final rendered frame, silent audio**, and emit `Compile::NestedSequenceShortened` (`Warning`) naming the nest. Do not auto-trim the nest clip: silently changing timeline layout because a different sequence was edited is worse than a visible held frame.

### 2.5 Caching

Nested sequences are the **best case** for the content-hash cache: the entire inner composite is one subtree with one hash, so an unedited nest used in ten places evaluates once. No special handling is needed — this falls out — but it should be stated, because it is a strong argument for nesting over duplication and it is invisible today.

Cycle detection already exists (`EditError::SequenceCycle`, `sequence_ancestry`) and is unchanged.

---

## 3. Frame-rate conform

### 3.1 The gap

[05 §6.2](05-import-export.md) covers VFR input thoroughly and is silent on the far more common case: **a 30 fps clip on a 24 fps sequence**. [02 §4](02-engine.md) covers only `SpeedMap`. Every real project hits this.

### 3.2 Recommendation: nearest-source-frame, stated

**v1: nearest source frame.** For sequence tick `t`, map through the clip's trim and speed to a source time, then select the source frame **covering** that time — the same rule playback already uses. No blending, no duplication logic, no rate conversion.

This is what the code does today and what [05 §6.2](05-import-export.md) implies for VFR; the recommendation is to **state it as the conform rule** rather than leave it as an emergent property of the decode path.

Consequences to document honestly: 30 → 24 drops every fifth frame; 24 → 30 repeats; both produce judder on motion. That is the correct trade for v1 — it is exact, cheap, deterministic, and identical between preview and export.

### 3.3 Frame blending, later

Once [32 §1](32-engine-contracts.md#1-source-range--the-one-mechanism-for-temporal-access)'s `source_range` contract exists, an optional blended conform is expressible — declare `[out−1, out+1]` and interpolate. **Do not build it before that contract**, or it becomes the fourth ad-hoc temporal mechanism, which is precisely the accretion [32 §1.1](32-engine-contracts.md#11-the-problem-stated-from-the-references-failure) exists to prevent.

### 3.4 Pulldown is out of scope

3:2 pulldown detection and removal is **not** frame-rate conform — it is a per-field cadence analysis and belongs with interlacing ([32 §6](32-engine-contracts.md#6-interlaced-sources)). It needs an analysis node ([32 §2](32-engine-contracts.md#2-analysis-nodes)) and its own contract. Recorded here only so the two are not conflated: conform maps times, inverse telecine reconstructs frames.

### 3.5 Diagnostics

Conform is silent today. It should emit `Media::FrameRateConformed` (`Info`) once per clip whose source rate differs from the sequence rate, surfaced through [26 K-C7](26-kdenlive-mlt-parity.md#k-c7--import-time-media-triage-report)'s import triage. "Why does this look juddery" is a common and currently unanswerable question.

---

## 4. Acceptance

| # | Test |
|---|---|
| 1 | A transition renders without moving any clip; `Sequence::validate()` passes throughout |
| 2 | A transition on a clip with **no** remaining handle does not render and warns; with partial handle it clamps and reports both durations |
| 3 | `transition_out` set at a cut is migrated away with a diagnostic; validation rejects it thereafter |
| 4 | Audio crossfade matches the video window; declick does not engage across a transition |
| 5 | A nest renders inner master effects and captions; inner `work_range` and `active_format` are ignored |
| 6 | A nest at a different inner frame rate renders correct content and emits one `Info` |
| 7 | Shortening an inner sequence holds the last frame in the outer, silent, with a warning — and does not change outer layout |
| 8 | The same unedited nest used 10 times evaluates its subtree **once** (cache-hit assertion) |
| 9 | Cycle detection still refuses a self-referencing nest |
| 10 | 30 fps on 24 fps produces the documented frame selection, identical in preview and export, with one `Info` |

Test 8 is the one that proves §2.5's claim rather than asserting it.

---

## 5. Amendments required — **all applied 2026-07-20**

- **[01 §5](01-data-model.md)** — replace "overlaps previous clip" with the borrowed-handle model (§1.1); add the `transition_out`-at-a-cut invariant (§1.3); document nest semantics (§2.3).
- **[02 §2](02-engine.md)** — state handle clamping (§1.2) and conform (§3.2) in the compile steps.
- **[05 §6.2](05-import-export.md)** — add the conform rule beside the VFR rule.
- **[08 §2](08-fusion-node-flows.md)** — restate the crossfade rule against borrowed handles (§1.5).
- **[36](36-error-model.md)** — register `TransitionHandleClipped`, `NestedSequenceShortened`, `FrameRateConformed`.
