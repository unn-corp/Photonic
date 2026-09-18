# 34 — Project Interchange: MLT XML Import, OpenTimelineIO, EDL

**Status:** Draft — implementation contract; no code authorization
**Date:** 2026-07-20
**Audience:** interchange implementers, data-model owner, legal reviewer

**Depends on:** [01-data-model.md](01-data-model.md), [05-import-export.md](05-import-export.md), [23-legal-open-source-implementation-routes.md](23-legal-open-source-implementation-routes.md), [26-kdenlive-mlt-parity.md](26-kdenlive-mlt-parity.md), [30-effect-catalogue.md](30-effect-catalogue.md).

**Owns:** [26 X-1](26-kdenlive-mlt-parity.md#x-1--mlt-xml--kdenlive-project-import) (MLT XML / `.kdenlive` import), [26 X-2](26-kdenlive-mlt-parity.md#x-2--opentimelineio-interchange) (OTIO), [26 X-3](26-kdenlive-mlt-parity.md#x-3--edl-aaf-fcpxml) (EDL and beyond). [26 X-4](26-kdenlive-mlt-parity.md#x-4--effect-manifest-as-a-versioned-schema) (the effect-manifest schema) belongs to [30 §2.6](30-effect-catalogue.md).

---

## 1. Positioning

Three formats, three different jobs, and they should not be confused:

| | Purpose | Direction | Fidelity |
|---|---|---|---|
| **MLT XML / `.kdenlive`** | Migration path *into* Photonic from Kdenlive and Shotcut | **Import only** | Structure complete, effects best-effort |
| **OpenTimelineIO** | Neutral interchange with the wider industry | Import **and** export | Structural by design — OTIO does not carry effects |
| **EDL (CMX 3600)** | Conform and colour round-trips | Import and export | Cuts and timecode only |

**Priority: X-2 before X-1 before X-3.** OTIO is Apache-2.0, an ASWF project, the emerging neutral format across Resolve, Premiere, Flame and Baselight — and the reference editors themselves moved to native OTIO support. It is the durable investment. MLT XML is a one-way migration convenience. EDL is cheap and still ubiquitous for conform.

**Clean-room, restated.** These are **file formats** — facts and interfaces, not expression. Implement from published schema documentation only; do not read the reference serializers or deserializers. Test fixtures must be **Photonic-authored**, never scraped from a GPL project's test suite ([23 §3.4](23-legal-open-source-implementation-routes.md#34-clean-room-protocol) item 4).

---

## 2. Shared: the import report

All three importers use one reporting discipline, and `captions/interchange`'s existing `ImportReport` is the precedent — it already surfaces "3 styling directives dropped" rather than silently discarding.

```rust
pub struct InterchangeReport {
    pub imported: ImportCounts,
    pub unsupported: Vec<Unsupported>,
    pub approximated: Vec<Approximation>,
    pub errors: Vec<InterchangeError>,
}
pub struct Unsupported { pub what: String, pub where_: Location, pub consequence: String }
```

**Rule: never drop silently.** Every unmapped effect, transition, or property produces an entry naming what it was, where it was, and what the user will see instead. An import that quietly loses half a project's grades is worse than one that refuses.

**Rule: import is one undo unit.** The whole import is a single `TimelineCmd`, undoable atomically.

---

## 3. X-1 — MLT XML and `.kdenlive`

### 3.1 Structural mapping

The document is a service graph: producers, playlists, tractors, transitions and filters, each a bag of `<property>` elements. It maps onto Photonic's model directly:

| Source | Photonic |
|---|---|
| `<profile>` | `Sequence.frame_rate` + a `SequenceFormat` |
| `<producer>` (avformat) | `MediaAsset` + `ClipSource::Asset` |
| `<playlist>` | `Track` |
| `<entry>` | `Clip { start, duration, source_in }` |
| `<blank length=…>` | a gap (implicit — Photonic has no gap object) |
| `<tractor>` | `Sequence` |
| `<track hide=…>` | `Track.enabled` (video/audio bit) |
| `<transition>` between tracks | track compositing / `Merge` mode |
| same-track transition (two-playlist trick) | `Clip.transition_in` / `transition_out` |
| `<filter>` on a producer | `Clip.effects` |
| `<filter>` on a playlist | `Track.effects` ([26 K-B1](26-kdenlive-mlt-parity.md#k-b1--track-and-master-effect-stacks)) |
| `<filter>` on the tractor | master effects |
| host-private `kdenlive:*` properties | bin structure, clip names, zones, markers, proxies |
| subtitle track (a filter referencing a sidecar `.srt`/`.ass`) | `CaptionTrack` via the existing importers |

**The two-playlists-per-track trick** is the one structural subtlety: each track holds two playlists so a same-track transition can be expressed as a transition between overlapping regions. Photonic models same-track transitions directly on the clip, so the importer must **detect the pattern and collapse it** — treating the two playlists as two tracks would double the track count of every imported project.

### 3.2 Impedance mismatches to specify

| Source | Photonic | Handling |
|---|---|---|
| Inclusive `out`, separate mutable `length` | Half-open `start` + `duration` ([26 PA-7](26-kdenlive-mlt-parity.md#5-photonic-ahead-register-pa---do-not-port-backwards)) | `duration = out − in + 1`. **The off-by-one is the single most likely import bug** — test it explicitly at length 1 |
| Integer frame positions in one profile rate | `Tick` (flicks) + exact rational rate | Exact via the profile rate; no float intermediate |
| Blanks as objects | Gaps as absence | Drop, adjusting following clip starts |
| `hide="video"`/`"audio"` bitmask | `Track.enabled` per kind | Direct |
| Effect identity by service name | `EffectId` | §3.4 mapping table |
| Locale-dependent decimal serialization | Locale-independent | Parse `C` locale; honour the document's declared numeric locale if present |

### 3.3 The animation grammar

Keyframed properties serialize as `position[interpolator]=value` items joined by `;`. This is the fidelity-critical part and is fully documented publicly.

**Parser rules, each a real trap:**

- **Linear is the empty token.** `100=200` is linear; `100~=200` is Catmull-Rom. There is no explicit linear character.
- Both `|` and `!` mean discrete.
- **Unknown interpolator characters fall back to linear** silently. Photonic should instead **record an `Approximation`** — silence is how fidelity loss goes unnoticed.
- **Negative positions are relative to the end**: `-1` is the last frame.
- **`-` is overloaded** — *smooth-tight* as an interpolator, *relative-to-end* as part of a position — disambiguated only positionally. `-1-=220` is "last frame, smooth-tight, 220".
- Beyond linear/discrete/smooth there are **33 easing tokens** covering sinusoidal, quadratic, cubic, quartic, quintic, exponential, circular, back, elastic and bounce, each in in/out/in-out.

**Mapping to `Interp`.** Photonic has `Hold | Linear | Bezier{out_handle, in_handle}`. Discrete → `Hold`; linear → `Linear`; the polynomial, sinusoidal, exponential and circular families → exact or near-exact `Bezier` handles. **Back, elastic and bounce overshoot outside `[0,1]` and are not representable** — they must be recorded as `Approximation`, not silently flattened. This is the same decision [26 K-B12](26-kdenlive-mlt-parity.md#k-b12--named-easing-presets) has to make for authoring, and both should reach the same answer.

**Rect values** serialize as space-separated `x y w h opacity`, but the parser accepts **any non-numeric delimiter**, so `0 0 1920 1080 1`, `0/0:1920x1080:1` and `0%/0%:100%x100%:100%` are the same rect. A `%` suffix **divides by 100** — `100%` is `1.0`, not `100` — and both conventions coexist in real projects, including within one document.

### 3.4 Effects

Effect identity is a service name. Photonic's catalogue ([30 §5](30-effect-catalogue.md)) will map a **minority** of them.

- Maintain an explicit `service name → EffectId` table with per-parameter mapping, including the display-value `factor` ([30 §2.4](30-effect-catalogue.md)) since normalised-0..1 plugin parameters are pervasive.
- An unmapped effect is **preserved inert**: retained in the stack, disabled, flagged, listed in the report — the `GradeOpParams::Unknown` pattern from [07 §1](07-color-grading.md) generalised, and the same behaviour [30 §2.6](30-effect-catalogue.md) requires for unknown manifests.
- Do **not** attempt to approximate an unmapped effect with a different one. A wrong grade is worse than an absent one, because the user cannot see that it is wrong.

### 3.5 Scope

**v1 is read-only.** Photonic does not write MLT XML: doing so would mean maintaining fidelity into a schema owned by another project, for no user benefit that OTIO does not serve better.

---

## 4. X-2 — OpenTimelineIO

### 4.1 Fit

Unusually good. OTIO's **rational time model** matches `Tick` + exact rational `FrameRate` far better than an integer frame count does, so the conversion is lossless in both directions — where MLT import must reconstruct time from a frame index. OTIO markers map onto [26 K-A2](26-kdenlive-mlt-parity.md#k-a2--marker-system-depth)'s ranged, categorised markers, including colour.

Maps cleanly: timeline ↔ `Sequence` · tracks ↔ `Track` · clips ↔ `Clip` · media references ↔ `MediaAsset` (with `content_hash` for relink) · gaps ↔ gaps · markers ↔ markers · transitions ↔ `Transition` (kind approximated) · time warps ↔ `SpeedMap` · stacks ↔ nested sequences.

**OTIO deliberately does not carry effects.** Round-trips are structural. Say so in the UI at export time rather than letting users discover it — the reference's own documentation confirms effects and transitions do not survive.

### 4.2 Implementation route

**Preferred: a Photonic-authored reader/writer for the OTIO JSON schema.** The schema is stable and documented, the subset Photonic needs is small, and this avoids a dependency entirely — consistent with how every other interchange format in this project has been handled (`captions/interchange` is hand-written for SRT, VTT and ASS).

**Fallback:** an OTIO library requires a [23 §3.3](23-legal-open-source-implementation-routes.md#33-required-evidence-record) evidence record first — transitive licences, build scripts, maintenance owner. Apache-2.0 is a preferred licence under [23 §3.2](23-legal-open-source-implementation-routes.md#32-default-license-policy), and specifically the one favoured where patent exposure matters, so intake is plausible; it is simply not automatic.

### 4.3 Downstream

Once X-2 exists, **AAF and FCPXML are reachable through OTIO adapters** rather than as first-party importers. That is the argument for doing X-2 first: it converts two large formats into a configuration problem.

---

## 5. X-3 — EDL

CMX 3600: text, ancient, small, still ubiquitous for conform and colour round-trips. Cuts, timecodes, reel names, simple dissolves, and speed as a percentage. No effects, no multi-layer, no audio detail.

**Import:** an assembly of cuts against source timecode, requiring `MediaAsset`s that carry source timecode — which is [26 K-A12](26-kdenlive-mlt-parity.md#k-a12--timecode-as-a-first-class-concept)'s work. **EDL is therefore blocked on timecode**, and that dependency is the reason it is not the cheap win it first appears.

**Export:** a flat cut list from a single video track, with an explicit report of everything omitted.

**Drop-frame is mandatory here**, not optional: EDL timecode is the canonical drop-frame carrier, and [27 SD-11](27-spec-audit.md#3-sd---spec-versus-code-drift) records that `parse_timecode` currently treats `:` and `;` identically while the MCP docs promise otherwise. Fix that before EDL, or every 29.97 EDL will be silently wrong by ~3.6 s/hour.

---

## 6. Acceptance

| # | Test |
|---|---|
| 1 | **Off-by-one** — a clip of length 1, and a clip at the sequence end, import with exactly correct duration |
| 2 | **Two-playlist collapse** — a project with same-track transitions imports with the original track count |
| 3 | **Animation grammar** — every interpolator token parses; unrepresentable easings are reported, not flattened silently |
| 4 | **Rect dialects** — the three notations in §3.3 produce identical rects; `%` divides by 100 |
| 5 | **Negative positions** resolve against length; `-1-=v` parses as smooth-tight at the last frame |
| 6 | **Unmapped effects** are preserved inert and re-serialize unchanged on save |
| 7 | **OTIO round-trip** — export then import reproduces structure, timing and markers exactly; effects are absent and reported |
| 8 | **Rational time** survives OTIO round-trip at 23.976, 29.97 and 59.94 without drift |
| 9 | **Import is one undo unit** |
| 10 | **Report completeness** — a fixture with a deliberately unsupported construct produces exactly one report entry naming it |
| 11 | **Fixtures are Photonic-authored** — provenance recorded per [23 §12](23-legal-open-source-implementation-routes.md#12-cross-cutting-provenance-manifests) |

Test 1 deserves emphasis: inclusive-`out` versus half-open `duration` is the highest-probability defect in this document, it is invisible in casual testing, and it corrupts every clip in the project by one frame.

---

## 7. Sequencing

| Order | Item | Gate |
|---|---|---|
| 1 | [26 K-A12](26-kdenlive-mlt-parity.md#k-a12--timecode-as-a-first-class-concept) timecode incl. drop-frame | Blocks EDL; needed for source-TC in all three |
| 2 | Shared `InterchangeReport` + one-undo-unit import | Shared by all |
| 3 | **X-2 OTIO**, Photonic-authored reader/writer | Highest durable value |
| 4 | **X-1 MLT XML import** | Migration path; largest parsing surface |
| 5 | **X-3 EDL** | After timecode |
| 6 | AAF / FCPXML via OTIO adapters | Only if demand justifies |

Effect mapping (§3.4) tracks [30](30-effect-catalogue.md)'s catalogue and improves as it grows — the table is expected to start small, and the inert-preservation rule is what makes that acceptable rather than embarrassing.
