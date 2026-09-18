# 209 — Large-Radius Multi-Pass Blur Quality

> **Status: Accepted and Implemented (wave-1, 2026-08-03).**  
> `blur_plan` multi-iteration (plus CPU `step`) and the GPU multi-iteration twin
> are shipped with tests green; see ROADMAP §0's OpenCut harvest table.  
> Harvested as a *quality recipe* common to CapCut-class GPU effect stacks
> (OpenCut classic documents the same failure mode). Photonic-native under
> [207](207-opencut-harvest-index.md) §2.

**Owner refs:**  
- [30](../specs/video-editor/30-effect-catalogue.md) Blur / Glow / surface effects  
- Shipped dual-pass Gaussian (`ops::blur`, GPU `textureLoad` path)  
- [208](208-sdf-jfa-mask-feather.md) — consumers that still use Gaussian feather  
- [25](../specs/video-editor/25-performance.md) preview budgets  

**Territory:** `photonic-video` graph eval (CPU + GPU). **Effort:** S.  
**Format impact:** none (runtime only; may add optional effect params later).

---

## 1. Problem and user outcome

**Failure mode.** A fixed-width Gaussian sample kernel (e.g. ±N taps at step 1)
covers only ±N texels. When target σ grows past ~kernel_radius/3, the discrete
kernel no longer approximates the Gaussian — results look like a **box blur**
with ringing/banding.

**After 209.** Photonic blur (and multi-pass consumers: Glow, large Gaussian
feather fallbacks) remain Gaussian-like at large radii by combining:

1. **Step scaling** (`u_step`) — space taps farther apart with bilinear
   filtering filling gaps, and  
2. **Iteration stacking** — multiple H+V pairs so effective
   `σ_eff ≈ σ_pass * √iterations`.

Users see soft, round bokeh-like blur instead of a milky box at high radius.

---

## 2. Current state in Photonic

| Surface | State |
|---|---|
| Separable dual-pass Gaussian CPU+GPU | **Shipped** (K-0.2) |
| Glow / multi-pass effects | Present; quality at extreme radius **unspecified** |
| Documented step/iteration policy | **Missing** |
| CPU/GPU parity suite | Exists for nominal radii; need extreme-σ rows |

---

## 3. Normative quality contract

### 3.1 Parameters

For a logical blur radius `R` (pixels at the **logical** frame size, not the
pool bucket):

| Symbol | Meaning |
|---|---|
| `σ` | Gaussian sigma derived from `R` (pin formula in impl; e.g. `σ = R / 2` or catalogue-defined) |
| `K` | Half-width of the tap kernel in samples (implementation constant, e.g. 16–32) |
| `step` | Texel spacing between taps |
| `iters` | Number of full H+V pairs |

### 3.2 Selection policy (normative intent)

```text
// Pseudocode — concrete thresholds chosen at impl time and pinned by tests.
step  = clamp(ceil(σ / σ_per_step_budget), 1, STEP_MAX)   // STEP_MAX ≤ 4
iters = max(1, ceil((σ / step) / σ_per_iter_budget))
// Prefer raising iters before raising step above STEP_MAX.
```

**Hard rules:**

1. **`STEP_MAX ≤ 4`** for production paths. Larger steps create visible banding
   even with bilinear filtering.  
2. Prefer **more iterations** over larger step when quality and budget conflict.  
3. All intermediate targets are **logical** resolution for parity; Draft scale
   must still pass [32 §7](../specs/video-editor/32-engine-contracts.md)
   scale-invariance within existing tolerances.  
4. CPU and GPU must share the **same** step/iter plan for a given `(σ, canvas)`
   so parity tests remain meaningful.

### 3.3 Multi-pass effects (`buildPasses` idea)

CapCut-class stacks resolve a static pass list **or** a dynamic plan from
parameters. Photonic equivalent:

- Effect manifests / IR lowering may expand one logical `Blur` into N H+V
  pairs.  
- Content hash must include the **expanded** plan (or the inputs that uniquely
  determine it: σ, canvas size), never a stale single-pass identity.

No TypeScript-style `buildPasses` API is required — this is an **IR lowering**
concern.

---

## 4. Non-goals

- Replacing blur with SDF for mask feather (that is 208).  
- Lens blur / hex bokeh (Tier-2 catalogue).  
- Changing user-facing param names or ranges in the inspector without a
  catalogue bump.  
- Copying third-party WGSL.

---

## 5. Tests and acceptance

| ID | Case | Pass |
|---|---|---|
| T1 | σ = 2, small — matches pre-209 reference within existing parity ε | regression |
| T2 | σ = 24, step/iter plan uses step ≤ 4 and iters ≥ 2 | unit on planner |
| T3 | CPU vs GPU parity at σ ∈ {2, 8, 24} on 64×64 and 256×256 fixtures | parity suite |
| T4 | Extreme σ does not introduce periodic banding (max horizontal FFT peak below threshold **or** visual golden + human note in fixture README) | golden / advisory |
| T5 | Content hash differs when σ changes enough to change iter count | unit |

---

## 6. Provenance

| Idea | Origin |
|---|---|
| Separable Gaussian | Classical signal processing; already in Photonic |
| Step sampling + multi-iteration wide blur | Common real-time graphics practice (many public engine posts); also documented in OpenCut classic `effects-renderer.md` as a quality note |

Reimplement from this contract. Do not port OpenCut effect pipeline code.

---

## 7. Delivery

- **Single PR preferred:** planner + CPU/GPU path + parity rows.  
- No ROADMAP K-id required — quality fix under existing Blur/Glow surface.  
- Blocks nothing; **unblocks** confidence for 208 Gaussian fallbacks.
