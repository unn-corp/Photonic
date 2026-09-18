# 199 — X-1 MLT XML / `.kdenlive` project import (read-only)

> Status: **proposed mini-spec — not accepted, no code authorization.** Written to
> satisfy the [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands)
> K-Band 5 exit condition ("an accepted mini-spec exists *before* code, naming its
> data-model change, migration, undo unit, MCP surface and acceptance fixtures").
> Owner docs: [34 §3](../specs/video-editor/34-interchange.md#3-x-1--mlt-xml-and-kdenlive)
> and [26 §18](../specs/video-editor/26-kdenlive-mlt-parity.md#18-x----interop-and-format).
> Nothing here authorizes edits to product crates; [23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)'s
> agent-proof boundary applies until X-1 is separately scheduled.

**Owner ref:** 34 §3, 26 §18 X-1 · **Territory:** `photonic-video-engine` · **Effort:** L
**Clean-room posture:** this is the item where the [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
fence is load-bearing rather than inherited. Kdenlive is `GPL-3.0` and MLT is
`LGPL-2.1+`, both **`REJECT`**. §9 is the centre of this document, not an
appendix — it specifies how a fixture is authored from nothing and how a
reviewer confirms that after the fact.

---

## 1. Problem and user outcome

A prospective Photonic user with a Kdenlive or Shotcut project has exactly one
migration route today: re-cut it by hand. There is no importer of any project
format in the tree — the only interchange code is subtitle-shaped
(`crates/photonic-video/src/captions/interchange/`), and the only project format
Photonic reads is its own `.photon`.

After X-1, a user can:

1. Point Photonic at a `.mlt` or `.kdenlive` file and get a real
   `TimelineProject`: a media pool with the project's clips, one `Sequence` per
   tractor, tracks in the right order, clips at the right frames with the right
   trims, gaps preserved, same-track transitions on the right clips, markers,
   bin folders, and the subtitle track handed to the existing caption importer.
2. Be told **before the import commits**, in a form they can read, exactly what
   did not survive — per item, per reason, with a count. An MLT project can
   carry hundreds of filters with no Photonic equivalent; a silent partial
   import is the failure mode this whole document is designed against.
3. Undo the entire import with one Ctrl+Z.
4. Drive the same thing from an agent over MCP, with a `dry_run` that produces
   the identical report without touching the document.

**Non-goals, stated up front so they are not rediscovered later.**

- **There is no MLT export, ever.** 34 §3.5 already settles this and this
  document does not reopen it: writing MLT XML means maintaining fidelity into a
  schema owned by a `REJECT`-licensed project, for a user benefit
  [X-2](196-x-2-opentimelineio-interchange.md) serves better.
- **X-1 is not a Kdenlive compatibility layer.** It is a one-way door. A project
  imported into Photonic and edited is a `.photon`; it does not go back.
- **Structure is the promise; effects are best-effort.** 34 §3.4 says so and §3.8
  below makes the size of "best-effort" concrete: 44 shipped effect manifests
  against a plugin surface an order of magnitude larger.

---

## 2. Current state in code

Exact, as of `feat/video-editor-module` @ `8a33f32`. Read this before disagreeing
with §3 or §4.

### 2.1 What exists and is directly usable

| Thing | Where | Note |
|---|---|---|
| `Tick(i64)` flicks, `TICKS_PER_SECOND = 705_600_000` | `crates/photonic-core/src/timeline/time.rs:13,23` | PA-8 |
| `FrameRate { num: u32, den: u32 }`, `ticks_per_frame()`, `frame_at()`, `snap()`, `frame_start()`, `is_exact()`, `is_drop_frame_rate()` | `time.rs:72,108,119,126,133,141,157` | `frame_start(n)` is the exact integer-frame → `Tick` conversion X-1 needs |
| `Timecode { .., drop_frame }`, `parse_to_tick`, `from_frame_index` | `time.rs:169,244,192` | K-A12 landed; drop-frame `;` vs `:` honoured and exhaustively tested |
| `Sequence { frame_rate, formats, active_format, video_tracks, audio_tracks, caption_tracks, markers, groups, audio_master, master_effects, master_grade, work_range, start_timecode }` | `sequence.rs:126` | |
| `Track { id, name, kind, clips, enabled, locked, sync_lock, audio, height_px, effects, grade, blend, opacity }` | `sequence.rs:628` | `effects` is K-B1's track scope — the home for a `<filter>` on a playlist |
| `Clip { start, duration, source, source_in, speed, transform, reframe, effects, grade, composition, transition_in, transition_out, audio, enabled, color_label, markers, group, link_group, multicam }` | `clip.rs:27` | half-open `start` + `duration` (PA-7) |
| `ClipSource::{Asset, Vector, NestedSequence, SolidColor, Adjustment, Text, Unknown}` | `clip.rs:165` | `NestedSequence` is the nested-tractor analogue; `insert_clip` cycle-checks it (`ops.rs:508,522`) |
| `Transition { kind, duration, params }`, `TransitionKind::{CrossDissolve, DipToBlack, DipToColor, Wipe, Push, Unknown}` | `clip.rs:698,719` | |
| `SpeedMap::{Constant(Ratio), Keyframed{keys}}`, exact `i128` rational arithmetic | `clip.rs:375,407` | the `timewarp` target |
| `Marker { at, duration, name, note, category, color, anchor }`, `MarkerCategory` on the project | `sequence.rs:833,732` | K-A2 landed; ranged markers exist |
| `MediaAsset { kind, source, probe, proxy, content_hash, bin, effects, grade, rating, tags }`, `AssetSource::File { path, rel_path }` | `media.rs:42,121` | `rel_path` is the project-relative fallback an MLT `resource` often is |
| `MediaBin { id, name, parent }` | `media.rs:293` | the `kdenlive:folderid` target |
| `ClipEffect { kind, id: EffectId, version, enabled, inert, params }` | `clip.rs:620` | **`inert` already exists** — 30 §2.6's preserve-but-do-not-render flag |
| 44 effect manifests in one static table | `effect_manifest.rs:535` | the right-hand side of §3.8's mapping table |
| `EffectParams` = ordered `Vec<(PropPath, PropValue)>`; `PropValue::{Float, Vec2, Color, Bool, Enum}` | `effect_kind.rs:81`, `anim.rs:52` | **no string variant** — see §4.2, this is the sharpest constraint in the document |
| `Interp::{Hold, Linear, Bezier{out_handle, in_handle}}`, `Keyframe { at, value, interp }` | `anim.rs:76,92` | the animation-grammar target |
| `Command::Batch(Vec<Command>)`; inverse is the reversed batch of inverses | `crates/photonic-core/src/history/mod.rs:3172-3178` | the whole undo story (§6) |
| `history.execute_discrete(Command::Batch(cmds), &mut doc)` for a multi-asset import | `crates/photonic-mcp/src/handlers/video.rs:2622` | exact precedent for one-import-one-undo |
| Pure `ops::` constructors: `create_project` `:95`, `add_asset` `:101`, `add_sequence` `:325`, `set_active_sequence` `:341`, `create_bin` `:1942` | `crates/photonic-core/src/timeline/ops.rs` | GUI and MCP both call these |
| `guess_asset_kind` extension table | `handlers/video.rs:2516` | reused for `resource` kind inference |
| `roxmltree` 0.20 is **already a workspace dependency** | root `Cargo.toml:77`, `crates/photonic-core/Cargo.toml:18`, used at `import.rs:27` (SVG) and `grade.rs:333` (CDL XML) | §4.3: X-1 adds **no new dependency** |
| `DiagFamily::Interchange` | `crates/photonic-core/src/diag.rs:160` | the family exists and has **zero** codes |
| `ImportSummary` / `ExportSummary` / `InterchangeError` for captions | `captions/interchange/mod.rs:33,44,23` | 34 §2's reporting precedent |
| GUI "Import media" route with an `rfd` picker | `crates/photonic-gui/src/app/panel_actions.rs:42-56` | the pattern §7.3 extends — note it commits **one `execute_discrete` per asset** (`panel_actions.rs:65`), which X-1 must *not* copy |

### 2.2 What does not exist yet — stated plainly

- **No `interchange/` module in `photonic-video`.** The only `interchange` path
  in the tree is `captions/interchange/`. `grep -rn 'mlt_service\|<tractor\|
  InterchangeReport' crates/` returns clean (verified 2026-07-28).
- **No `InterchangeReport` type.** 34 §2 sketches one; nothing implements it.
  [196 §4.1](196-x-2-opentimelineio-interchange.md) proposes the concrete shape.
- **No `DiagCode` in the `Interchange` family.** `diag_catalogue.rs:27`'s
  `EXPECTED_WIRE_CODES` is a deliberately frozen list and the family is empty in
  it; 196 §8 proposes the first five codes.
- **`SequenceFormat` is `{ name, width, height }`** (`sequence.rs:609`) — no
  frame rate, **no pixel aspect ratio**. `pixel_aspect` exists only per-asset on
  `MediaProbe.video` (`media.rs:189`). An MLT `<profile>` with non-square pixels
  therefore has nowhere to land (§3.9).
- **Photonic has no gap object.** Gaps are the absence of a clip; `Sequence::validate`
  (`sequence.rs:378`) enforces only positive duration, sorted order and
  non-overlap.
- **`finalize_effect_ids` only walks clip effects.** `load.rs:485-521` iterates
  `video_tracks ∪ audio_tracks → track.clips → clip.effects`. It never touches
  `Track::effects`, `Sequence::master_effects` or `MediaAsset::effects`. An
  inert effect placed in one of those three scopes is **not re-marked inert on
  the next load** — it keeps whatever `inert` bit was serialized, and an
  effect whose id gains a manifest later never un-inerts there. X-1 puts
  effects in all four scopes (§3.8), so this is a live defect on X-1's path,
  not a hypothetical. §15 follow-up 3.
- **No `PathPolicy`.** [28 §3.1](../specs/video-editor/28-security-model.md#31-the-rule)
  specifies it; `grep -rn 'PathPolicy\|path_policy' crates/` returns clean
  (recorded independently at [195 §2](195-k-c1-clip-jobs-framework.md)). This
  matters because an MLT `resource` is an arbitrary path from an untrusted file
  (§3.6, §13).
- **`Command::Batch` applies members one at a time** (`history/mod.rs:2824-2828`)
  and **`TimelineCmd::apply` debug-asserts `Sequence::validate()` after every
  command** (`commands.rs:1749-1757`). A plural edit expressed as per-clip
  commands can transiently violate an invariant and panic in debug. §6 is written
  around this.

---

## 3. The mapping — where the impedance actually is

This is the design. §4–§8 follow from it. Every claim about MLT below is a claim
about the **published format description** (§9.1 names the sources); anything
this document could not confirm from a published source is marked
**(confirm-before-code)** and is a gate on implementation, not a licence to guess.

### 3.1 Time — MLT is exact, and X-2's rate-recovery ladder is not needed

This is the one place X-1 is *easier* than [X-2](196-x-2-opentimelineio-interchange.md),
and saying so prevents an implementer copying machinery they do not need.

`<profile>` declares `frame_rate_num` and `frame_rate_den` as **integers**. That
is Photonic's `FrameRate { num, den }` directly. There is no `f64` anywhere in
the transaction and therefore **none of 196 §3.1's three-step rate ladder
applies** — no canonical-rate table, no `1e-6` tolerance, no continued-fraction
approximation, no `InterchangeRateApproximated`. X-1 reads two integers.

Positions are integer frame counts in that profile's timebase, so:

```
tick = FrameRate::frame_start(frame)      // time.rs:133 — exact i64, no float
```

Two consequences to specify rather than discover:

- **`FrameRate::is_exact()` is the only rate warning X-1 emits.** A profile whose
  `TICKS_PER_SECOND * den / num` does not divide evenly (`time.rs:141`) means
  every frame boundary carries sub-tick rounding. That is one `Approximation`
  per import naming the rate, not per clip.
- **Every imported position is frame-aligned by construction.** Photonic's model
  permits sub-frame positions (`ops.rs` never calls `FrameRate::snap`), but an
  MLT import can never produce one. Test B pins that as a property.

**Clock-form times.** MLT property time values may be written either as a frame
integer or as a clock string (`HH:MM:SS.mmm`), and the two forms can coexist in
one document. The parser must accept both and convert the clock form through the
profile rate, rounding to the nearest frame and emitting one `Approximation` when
the clock value does not land on a frame boundary. **(confirm-before-code:** the
exact accepted grammar, including whether a trailing frame field is legal, must
be read off the published property-time documentation before the parser is
written. Getting this wrong is a silent one-frame error, which is §3.3's failure
mode by another route.**)**

### 3.2 Structure — the document is a service graph

| MLT | Photonic | Notes |
|---|---|---|
| `<profile>` | `Sequence.frame_rate` + `SequenceFormat { name, width, height }` | `display_aspect_*` / `sample_aspect_*` have no home — §3.9 |
| `<producer>` / `<chain>` with `mlt_service=avformat*` | `MediaAsset` + `ClipSource::Asset` | MLT 7 uses `<chain>` with `<link>` children where 6 used `<producer>`; both must parse **(confirm-before-code)** |
| `mlt_service=qimage`/`pixbuf` | `MediaAsset { kind: Image }` | via `guess_asset_kind` on `resource` |
| `mlt_service=color`/`colour` | `ClipSource::SolidColor` | `resource` is `#AARRGGBB` or a colour name; unparseable → black + `Approximation` |
| `mlt_service=timewarp` | `SpeedMap::Constant(Ratio)` + the underlying asset | `resource` is `<speed>:<path>`; §3.5 |
| `mlt_service=tractor` (a nested tractor in the same document) | `ClipSource::NestedSequence` + a new `Sequence` | recurse; `ops::insert_clip` (`ops.rs:522`) cycle-checks |
| `mlt_service=xml` (a *reference to another file*) | — | **refused**, §3.6 |
| any other `mlt_service` | — | `Unsupported`; the entry becomes a gap of its own length |
| `<playlist>` | `Track` | §3.3 |
| `<entry producer= in= out=/>` | `Clip { start, duration, source_in }` | §3.4 |
| `<blank length=…/>` | a gap (absence) | advances the next clip's `start` |
| `<tractor>` | `Sequence` | one per tractor; the outermost becomes active |
| `<track hide="video|audio|both"/>` | `Track.enabled` per kind | direct |
| `<transition>` between two track indices | track compositing (`Track.blend`, `Track.opacity`) or a clip `transition_in` | §3.3 |
| `<filter>` on a producer/chain | `Clip.effects` or `MediaAsset.effects` | §3.8 |
| `<filter>` on a playlist | `Track.effects` (K-B1) | §3.8 |
| `<filter>` on the tractor | `Sequence.master_effects` | §3.8 |
| a subtitle filter referencing a sidecar `.srt`/`.ass` | `CaptionTrack` via `captions::interchange` | §3.7 |
| `kdenlive:*` / `shotcut:*` properties | bin structure, names, markers, zones | §3.7 |

**Import always creates, never merges.** One import produces one new bin, N new
assets and M new sequences. Assets are matched against the existing pool by
`content_hash` first, then absolute path, then filename — the ladder `media.rs`'s
module doc already describes for relink — and only created when no match is
found. No existing sequence, track or clip is touched. Merging into an existing
sequence needs a conflict model nobody has specified (§13, Q1).

### 3.3 The two-playlist collapse, and the transition it encodes

34 §3.1 records the structural subtlety: a track is represented by **two**
playlists so that a same-track transition can be expressed as an overlap between
them, with a `<transition>` mixing the two. Photonic models a same-track
transition directly on the incoming clip (`Clip.transition_in`), so:

> **Rule.** The importer detects the two-playlists-per-track pattern and collapses
> each pair into **one** `Track`. Treating the two playlists as two tracks would
> double the track count of every imported project and turn every dissolve into a
> permanent two-layer composite.

Collapse algorithm, written out because "detect the pattern" is where this goes
wrong:

1. Group the tractor's `<track>` entries by the compositing `<transition>`s that
   join them. A pair (A, B) whose only joining transition is a same-track
   mix/luma between exactly those two indices, where the union of A and B has no
   overlapping non-transition regions, is a **track pair**.
2. Merge A and B into one clip list ordered by `start`.
3. For each overlapping region of duration `D` where a clip from A ends after a
   clip from B begins: the **later-starting** clip gets
   `transition_in = Transition { kind, duration: D, params }`, and the earlier
   clip's `duration` is shortened so the two abut (`earlier.end() == later.start`).
   This is the model Photonic's compiler implements: `active_transition`
   (`crates/photonic-video/src/graph/compile.rs:744`) fires only for a
   `transition_in` on the incoming clip and borrows the outgoing clip's remaining
   source handle backwards. Nothing else is representable.
4. If a pair does **not** match — three playlists, an overlap with no joining
   transition, or an overlap of more than two clips — do not collapse. Import the
   playlists as separate tracks, and emit one `Unsupported` naming the track.
   Guessing here silently re-times footage.

**`transition_out` is not produced by this path, ever.** In Photonic a
`transition_out` is a fade to transparent into a gap or the sequence end and is
*invalid at a cut* — `Sequence::validate_transitions` (`sequence.rs:414`)
rejects a `transition_out` whose next clip is adjacent, and `finalize_load`
drops one at load with a `LoadNoticeCode::TransitionOutAtCutDropped`
(`load.rs:593-597`, emitted at `load.rs:297`). An importer that maps an MLT fade-out onto `transition_out`
without checking adjacency produces documents that fail their own load. A fade
to transparent at the *end of a track or into a gap* is the only legal case and
is the only one X-1 emits.

**Kind mapping.** A same-track mix maps to `TransitionKind::CrossDissolve`; a
luma-wipe transition maps to `Wipe` with `TransitionParams.direction` when the
direction is declared and to `CrossDissolve` + `Approximation` when it is not.
Any other transition service → `CrossDissolve` + `Approximation` naming the
original service. **Never `TransitionKind::Unknown`** — see §4.2.

**Track-level `<transition>`s** (a composite between two different tracks, not a
collapsed pair) map to `Track.blend` / `Track.opacity` (K-A9 landed, `sequence.rs:664,668`).
A blend mode with no `BlendMode` counterpart → `BlendMode::Normal` +
`Unsupported`.

### 3.4 The inclusive-`out` off-by-one — the highest-probability defect

34 §6 test 1 already says this is the single most likely bug in the document and
it is right. MLT carries an **inclusive `out`** alongside a separately mutable
`length`; Photonic carries a **half-open** `start` + `duration` (PA-7, a
protected surface per ROADMAP §9).

```
source_in = FrameRate::frame_start(entry.in)
duration  = FrameRate::frame_start(entry.out - entry.in + 1)
```

Three traps around it:

- **A one-frame clip has `in == out`.** `out - in + 1 == 1`, not `0`. A
  `duration` of zero fails `Sequence::validate` (`sequence.rs:386`) and the
  import aborts under §6's validate-then-commit — loudly, which is the correct
  failure, but only if the fixture exists to catch it. Test 1 is mandatory.
- **A producer's own `in`/`out` are absolute producer positions**, not offsets
  relative to a parent's trim. An entry's `in` is *not* added to the producer's
  `in`. **(confirm-before-code)**
- **`length` is not `out - in + 1`.** It is the producer's declared extent and
  may exceed the trimmed range. It feeds `MediaProbe.duration` at most, never
  `Clip.duration`.

Because `frame_start` is exact and every position is an integer frame, there is
no rounding anywhere in this arithmetic. The defect, when it happens, is
arithmetic, not numeric — which is exactly why it survives casual testing.

### 3.5 Speed

- A `timewarp` producer's `resource` is `<speed>:<path>`. The speed is a decimal
  string; parse it as an exact rational when it is a terminating decimal
  (`2.5` → `5/2`) and as the nearest `Ratio` with denominator ≤ 1000 otherwise,
  emitting an `Approximation` only in the second case. `SpeedMap::Constant` is
  exact `i128` rational arithmetic (`clip.rs:397,405-411`); feeding it a float-rounded
  ratio would throw away PA-8's whole point.
- A negative speed is **reverse playback**. Photonic's `SpeedMap` is a `Ratio`
  and `source_delta` handles negative `dt` by symmetry (`clip.rs:393,397`), but no
  shipped edit op produces a negative `Ratio` and the compiler's behaviour is
  unpinned. **Decision: v1 refuses a negative speed** — the clip imports at 1×
  with an `Unsupported` naming it. Importing a construct the engine has never
  been tested against is worse than declining it.
- MLT 7's keyframed time-remap link maps to `SpeedMap::Keyframed { keys }`
  **(confirm-before-code** on the property spelling and the key grammar, which is
  §3.6's grammar again**)**. If the grammar cannot be confirmed from a published
  source, the fallback is the clip's average ratio as a `Constant` plus an
  `Approximation` naming the key count — the same trade 196 §3.5 makes for OTIO,
  for the same reason: a wrong source-side length is visible, a missing warp is
  not.

### 3.6 The animation grammar — fidelity-critical, and fully public

Keyframed property values serialize as `position[interpolator]=value` items
joined by `;`. 34 §3.3 and 26 §18 both record the grammar; this section says what
the parser must *do*, and what happens to what it cannot represent.

**Parser rules, each a real trap:**

| Rule | Consequence if missed |
|---|---|
| **Linear is the empty token** — `100=200` is linear, `100~=200` is Catmull-Rom | Every un-suffixed key silently becomes whatever the default arm is |
| Both `\|` and `!` mean discrete | Half of all hold keys become linear |
| **A negative position is relative to the end**; `-1` is the last frame | Keys land at negative ticks and fail validation |
| **`-` is overloaded** — *smooth-tight* as an interpolator, *relative-to-end* as part of a position — disambiguated only positionally. `-1-=220` is "last frame, smooth-tight, 220" | The single hardest token in the grammar |
| Beyond linear/discrete/smooth there are **33 easing tokens**, in/out/in-out across sinusoidal, quadratic, cubic, quartic, quintic, exponential, circular, back, elastic and bounce | An unknown character falls back to linear *in the reference*; Photonic must not |

**Mapping onto `Interp`** (`anim.rs:76`), which is `Hold | Linear | Bezier{out_handle, in_handle}`:

| Source family | `Interp` | Report |
|---|---|---|
| discrete | `Hold` | none |
| linear | `Linear` | none |
| sinusoidal, quadratic, cubic, quartic, quintic, exponential, circular (in/out/in-out) | `Bezier` with the standard CSS-equivalent handles | none — these are exact or visually indistinguishable |
| smooth / Catmull-Rom | `Bezier` with tangent-derived handles | one `Approximation` per property lane, not per key |
| **back, elastic, bounce** | `Bezier` cannot express them | **`Unsupported` per property lane** |

**The overshoot families are the honest hard case.** `Interp::Bezier`'s handles
are documented as "control handles in [0,1]² over the segment's unit box"
(`anim.rs:81-86`). Back, elastic and bounce leave `[0,1]`. The choices are: flatten
them to the nearest ease (silent fidelity loss), widen `Interp`'s contract
(a model change with a rendering-behaviour blast radius), or report them.

> **Decision: report them, and import the segment as `Linear`.** The keyframe
> *values* and *times* are preserved exactly — only the shape between them is
> lost — and the report names the property and the key count. Flattening to a
> similar ease is the one option that is actively worse, because the user cannot
> see that it is wrong. This is the same answer [K-B12](../specs/video-editor/26-kdenlive-mlt-parity.md#k-b12--named-easing-presets)
> has to reach for authoring, and 34 §3.3 already asks that both reach the same
> one; if K-B12 later admits overshoot into `Interp`, X-1's table gains three
> rows and nothing else changes.

**Unknown interpolator characters do not fall back silently.** The reference
behaviour is a silent fallback to linear; Photonic imports the segment as
`Linear` and emits one `Approximation` naming the character. 34 §3.3 states this
explicitly and it is the difference between a report that is trusted and one that
is not.

**Rect values.** A rect serializes as `x y w h opacity`, but the parser accepts
**any non-numeric delimiter**, so `0 0 1920 1080 1`, `0/0:1920x1080:1` and
`0%/0%:100%x100%:100%` are the same rect. A `%` suffix **divides by 100** —
`100%` is `1.0`, not `100` — and both conventions coexist inside a single
document. The parser is therefore: split on any maximal run of non-numeric,
non-sign, non-decimal-point characters; parse each field; apply the `%` divisor
per field, not per rect. Test 4 pins all three notations to one result.

**Locale.** MLT stores the writer's numeric locale in the document because
doubles were serialized locale-dependently — 26 §17 records locale-independent
serialization as one of Photonic's protected E-8 properties. The parser reads
values in the `C` locale and honours the document's declared numeric locale when
one is present, converting a comma decimal separator on the way in. A value that
parses differently under the two readings and has no declared locale → parse in
`C` and emit one `Approximation` for the document.

### 3.7 Host-private namespaces — and the subtitle sidecar

`kdenlive:*` and `shotcut:*` properties are not in the MLT DTD. They are the
host's private data and they carry the things a user will most notice missing:
clip names, bin folders, markers, zones, and proxy paths.

> **Rule: the importer consumes a declared, closed allowlist of host-private
> keys and ignores every other key in those namespaces without reporting it.**

Two halves, both deliberate:

- **Closed allowlist.** Each consumed key is named in one table in the source,
  with the published-or-observed source that establishes its meaning recorded
  beside it (§9.2). A key not on the list is never guessed at.
- **Ignored without reporting.** Host UI state — panel geometry, zoom level,
  last-selected clip, version stamps — is not lost fidelity and reporting it
  would bury the entries that matter under noise. This is the one deliberate
  exception to 34 §2's never-drop-silently rule, and it is narrow: it applies
  only to keys inside a recognised host namespace, never to an MLT element, an
  `mlt_service`, or a filter property.

The keys X-1 consumes are: the clip display name, the bin/folder id and the
folder table, the clip zone, the marker list, the proxy path, and the original
(pre-proxy) resource. Each of those is **(confirm-before-code)** as to its exact
spelling — see §9.2 for how that confirmation is obtained without reading source.

**Proxies are not imported as proxies.** A `kdenlive:proxy` path points at a file
generated by another application with its own scaling and codec conventions.
X-1 imports the **original** resource and leaves `MediaAsset.proxy` as `None`;
Photonic generates its own proxy on demand. Adopting a foreign proxy would make
`content_hash` (`media.rs:55`), which is the relink identity, describe a file
Photonic did not make and cannot regenerate.

**Subtitles.** A subtitle track appears as a filter referencing a sidecar
`.srt`/`.ass` file. The importer resolves the sidecar path relative to the
project file, hands the bytes to the existing `captions::interchange` parsers,
and inserts the result as a `CaptionTrack` (`captions.rs:16`). A missing or
unparseable sidecar → `Unsupported`, no caption track, and the `ImportSummary`
notes from the caption parser (`captions/interchange/mod.rs:33`) are folded into the
`InterchangeReport` rather than discarded. Sidecars are read through the same
containment rule as `resource` paths (§3.9).

### 3.8 Effects — the service-name table, and what "preserved inert" can actually mean

Effect identity in MLT is a service name string. Photonic ships **44 effect
manifests** (`effect_manifest.rs:535`), 8 grade ops (`grade.rs:80`) and 4 audio
fx kinds (`audio.rs:245`). The plugin surface on the other side is an order of
magnitude larger, so the table maps a minority and always will.

**Structure of the mapping table.** One entry per mapped service:

```rust
struct ServiceMap {
    service:  &'static str,          // e.g. "brightness"
    target:   MapTarget,             // Effect(EffectId) | Grade(GradeOpKind) | AudioFx(AudioFxKind)
    params:   &'static [ParamMap],   // per-property
}
struct ParamMap {
    from:   &'static str,            // MLT property name
    to:     &'static str,            // PropPath
    factor: f64,                     // display-value scale (30 §2.4)
    clamp:  Option<(f64, f64)>,
}
```

`factor` is not optional decoration: normalised-`0..1` plugin parameters are
pervasive on the source side while Photonic's manifests declare real ranges, and
30 §2.4 already owns the display-value `factor` concept. A missing `factor` is a
100× error that looks like a plausible grade.

**Scope routing.** A `<filter>` lands in the scope its owner implies — producer →
`Clip.effects` (or `MediaAsset.effects` when the producer is the bin entry
rather than a timeline entry), playlist → `Track.effects`, tractor →
`Sequence.master_effects`. All four `VfxOwner` scopes exist and are compiled in
normative order (35 §2.4).

**Unmapped services are preserved inert.** 34 §3.4 requires it and
[196 §4.2](196-x-2-opentimelineio-interchange.md) explicitly carves MLT out of
its no-preservation rule, on the grounds that an MLT filter and a Photonic effect
are the same *kind of thing* with a mapping table behind them. This document
agrees and makes it concrete:

```rust
ClipEffect {
    id: EffectId::new(format!("mlt.{service}")),   // reserved namespace
    kind: EffectKind::Unknown(UnknownTag::intern(&id)),  // the only value the type admits
    version: 0,
    enabled: false,
    inert: true,
    params: /* the representable subset — see below */,
}
```

Three points, each load-bearing:

1. **The `mlt.` prefix is a reserved namespace.** Photonic must never ship an
   `EffectId` beginning `mlt.`; §10 test 12 is a source lint over `MANIFESTS`
   asserting it. That prefix is what defuses 196 §4.2's collision objection: a
   foreign service name can never collide with a real Photonic effect id.
2. **`EffectKind::Unknown` is used because the type admits nothing else.**
   `ClipEffect.kind` is not optional, and `ClipEffect::from_manifest`
   (`clip.rs:672`) already reaches for `EffectKind::Unknown(UnknownTag::intern(id))`
   for any manifest-less id. `kind` is documented as "removed after one format
   version" (`clip.rs:615-617`), so this coupling is transitional. The residual cost
   is real and named: an MLT-imported project will appear in
   `LoadReport::unknown_variants` (`load.rs:614-615`) on every load, which is 39
   §2.2's "a newer build wrote this" channel and would be a false statement.
   That channel is **currently consumed only by tests** (`from_value` discards
   the report, `document.rs:1706`), so nothing user-visible is wrong today — but
   §15 follow-up 2 requires that when 36 §3 wires it up, a tag in a reserved
   foreign namespace reports as "imported from another application" and not as a
   forward-compat variant. `ClipEffect.inert` already distinguishes the two in
   the data.
3. **Do not approximate an unmapped effect with a different one.** 34 §3.4 is
   explicit and this document does not soften it. A wrong grade is worse than an
   absent one because the user cannot see that it is wrong.

**What "preserved" can carry, and what it cannot.** This is the sharpest
constraint in the document and it is a property of the shipped model, not a
choice: `EffectParams` is `Vec<(PropPath, PropValue)>` (`effect_kind.rs:81`) and
`PropValue` is `Float | Vec2 | Color | Bool | Enum` (`anim.rs:52`). **There is no
string variant, and `effect_manifest.rs:21-27` records that adding one was
already considered and rejected** because it "breaks the crate-wide `Copy` derive
on `PropValue` (and every exhaustive `match` on `PropValueKind` in the GUI)".
An MLT property bag is stringly-typed end to end (PA-9 records this as one of the
things Photonic is ahead on).

> **Decision.** An inert imported effect carries the **numerically representable**
> subset of its properties — anything that parses as a number, a rect (→ `Vec2`
> pairs), a colour, or a boolean, keyed by the MLT property name verbatim as its
> `PropPath`. Every property that does not is **not** stored in the document and
> instead appears in the report and in the optional sidecar report file (§8.2),
> verbatim, one entry each.

Justification, because the alternative is tempting and would be wrong:

- **X-1 is read-only.** There is no MLT writer, so nothing round-trips back out.
  The only round trip that must hold is `.photon` save → reload, and what *is*
  stored round-trips byte-identically because inert effects' params are left
  untouched by load (`load.rs:114`, `load.rs:479`). 34 §6 test 6 is satisfied.
- **Adding `PropValue::Str(String)` is not additive.** `PropValue` is
  `#[serde(tag = "t", content = "v")]` with no untagged fallback (`anim.rs:51`).
  A document containing `{"t":"str",...}` fails to deserialize in *any* build
  that predates the variant — the whole document, not the one field, because an
  unknown enum variant is not an unknown field and `COMPAT_WINDOW`'s lenient path
  (`migration.rs:16`) only drops unknown *fields*. That is a backward-compat
  break dressed as an additive change, and it would be introduced to serve a
  read-only importer's disabled effects. Emphatically not worth it.
- **The inert effect is never evaluated.** `compile.rs:1214` skips it. The
  preserved params exist so the user can see *what was there*, and the report
  plus the sidecar file carry that better than a param bag the inspector cannot
  render anyway.

**Audio filters** route to `Track.audio.fx` / `MasterBus` where they map to one
of the four `AudioFxKind`s, and to the same inert treatment otherwise. **Grade-shaped
filters** map to `GradeOpKind` where one exists; there is no inert channel on
`Grade` equivalent to `ClipEffect.inert`, so an unmapped grade-shaped filter
becomes an inert `ClipEffect` in the same scope rather than a `GradeOp` —
`GradeOpParams::Unknown` (07 §1) preserves *params* for a known-op-unknown-shape
case, which is a different problem.

### 3.9 The lossiness register — what import cannot carry

Every row produces exactly one report entry naming what it was, where it was, and
what the user will see instead. This table **is** the acceptance criterion for
§10 test 10.

| Source construct | Where | Why there is no Photonic form | Result |
|---|---|---|---|
| `<profile>` `sample_aspect_*` / `display_aspect_*` ≠ 1:1 | sequence | `SequenceFormat` is `{name, width, height}` (`sequence.rs:609`); no PAR anywhere on the sequence | `Unsupported`; imports as square-pixel at the declared `width`×`height`. **Anamorphic SD projects will look horizontally wrong** and the report must say so in those words |
| `<profile>` `progressive="0"` | sequence | interlacing is per-asset (`MediaProbe.video.scan`, `media.rs:198`), not per-sequence; K-G6 is not scheduled | `Unsupported`, one entry |
| `<profile>` `colorspace` | sequence | Photonic's working space is fixed linear Rec.709 (PA-2/PA-14) | `Unsupported` only when it is not 709; a 709 profile is silent |
| an unmapped `<filter>` service | all four scopes | §3.8 | preserved **inert**, one `Unsupported` per distinct service (coalesced with a count, not per instance) |
| a non-numeric filter property | effect | `PropValue` has no string variant (§3.8) | dropped from the document, one entry per property, verbatim in the sidecar report |
| back / elastic / bounce easing | keyframe lane | `Interp::Bezier` handles are in `[0,1]²` (`anim.rs:81`) | segment imports `Linear`, one entry per lane |
| an unmapped `<transition>` service | track or clip | five `TransitionKind`s | `CrossDissolve` + `Approximation` |
| a blend mode with no `BlendMode` counterpart | track | 26 modes ship (K-0.3) but not all of MLT's | `Normal` + `Unsupported` |
| a `mlt_service=xml` producer (external document reference) | producer | §3.6 refusal | `Unsupported`; entries become gaps |
| a non-collapsible playlist pair | track | §3.3 step 4 | imports as separate tracks + `Unsupported` |
| a negative `timewarp` speed | clip | §3.5 | 1× + `Unsupported` |
| `<producer>` `length` beyond the trimmed range | producer | Photonic has no separate mutable length (PA-7) | silent — this is a PA-7 win, not a loss |
| a `resource` outside the project directory that cannot be resolved | asset | offline is a first-class state | asset created **offline**, `InterchangeMediaUnresolved`, `Warning` — not an error |
| host UI state in `kdenlive:*` / `shotcut:*` | project | §3.7 | ignored, deliberately unreported |

**Path handling for `resource`.** An MLT `resource` is an arbitrary path from an
untrusted file — 28 §6's exact scenario. X-1 resolves relative resources against
the project file's directory (writing both `path` and `rel_path`, `media.rs:121`)
and **never reads a resource's bytes during import**: no probe, no hash of a file
outside the project directory, no sidecar read outside it. Probing is
`SetAssetMeta`'s job (`ops.rs:295`) on the L1/L2 ladder in
[24](../specs/video-editor/24-preview-media-load.md), and deferring it means an
import cannot be used to make Photonic stat or read an attacker-named path. A
resource resolving outside the project directory is imported as an offline asset
with its path preserved verbatim and one report entry, per 28 §6's
report-and-confirm recommendation. When `PathPolicy` (28 §3.1) lands, this
becomes a call into it; until then the rule is "resolve, do not read".

---

## 4. Data-model change

### 4.1 None in `photonic-core`

No new field, no new variant, no new type in the persisted model. Import produces
ordinary `MediaAsset`, `MediaBin`, `Sequence`, `Track`, `Clip`, `ClipEffect`,
`Transition`, `Marker`, `SpeedMap` and `CaptionTrack` values that the v5 model
already expresses. Everything MLT-specific lives in the new module.

New types, all in `crates/photonic-video/src/interchange/`, none persisted:

```
interchange/
  mod.rs        InterchangeReport, Unsupported, Approximation, Location  (from 196 §4.1)
  mlt/
    mod.rs      the importer entry point
    parse.rs    roxmltree → a typed MltDoc { profile, producers, playlists, tractors, .. }
    anim.rs     the §3.6 keyframe / rect grammar — a pure str → Vec<Keyframe> function
    services.rs the §3.8 ServiceMap table
    build.rs    MltDoc → Vec<TimelineCmd>
```

`parse.rs` and `anim.rs` are pure byte-in/structure-out functions with no
`photonic-core` dependency beyond `Tick`/`Interp`, which is what makes them the
fuzz targets 28 §5.3 asks for.

### 4.2 What X-1 must *not* do, and why the house pattern does not fit

The 39 §2.2 unknown-preserving machinery (`ClipSource::Unknown`,
`TransitionKind::Unknown`, `GradeOpKind::Unknown`, `unknown.rs`) exists so a
document written by a **newer Photonic build** round-trips through an older one.
Its correctness argument is that the preserved tag is in *Photonic's own
namespace* and some Photonic build understands it.

- **`ClipSource::Unknown` is never synthesised.** An unrecognised `mlt_service`
  becomes a **gap** (preserving downstream timing) and a report entry. Storing
  `"frei0r.cairoblend"` in a `ClipSource::Unknown` map would re-emit a foreign
  identifier into `.photon` as though it were a Photonic source kind.
- **`TransitionKind::Unknown` is never synthesised.** `forward_compat_inert.rs:13`
  pins an unknown `TransitionKind` to render as a **hard cut**; importing a real
  MLT dissolve as something that renders as a cut is a worse outcome than
  `CrossDissolve` + a report entry.
- **`EffectKind::Unknown` *is* used**, under the reserved `mlt.` namespace, with
  the residual cost named and a follow-up filed (§3.8 point 2). This is the one
  place X-1 and 196 §4.2 diverge, and 196 anticipated the divergence in writing.

### 4.3 Dependency: none new

X-1 needs an XML parser. **`roxmltree` 0.20 is already a workspace dependency**
(root `Cargo.toml:77`), already used by `photonic-core` for SVG import
(`import.rs:27`) and ASC CDL parsing (`grade.rs:333`), already in `Cargo.lock`,
already cargo-deny clean. X-1 adds one line —
`roxmltree = { workspace = true }` — to `crates/photonic-video/Cargo.toml`.

**No [23 §3.3](../specs/video-editor/23-legal-open-source-implementation-routes.md#33-required-evidence-record)
evidence record is required.** That gate is for *intake*, and this is not an
intake: the crate is in the build, its transitive footprint is already accounted
for, and no new licence enters the tree. 26 §2 item 4 names X-2 and K-E1 as the
two items that contemplate a dependency; X-1 is not among them and does not
become one.

Three parser settings are normative, not defaults, and each is verified against
the vendored crate source:

1. **`ParsingOptions::allow_dtd` must be set `true`.** It defaults to `false`
   (`roxmltree-0.20.0/src/parse.rs:345`) and an MLT document declaring
   `<!DOCTYPE mlt SYSTEM "mlt-xml.dtd">` fails with `Error::DtdDetected`
   otherwise. This is the single setting an implementer will hit first and
   "fix" without thinking about what it turns on.
2. **`ParsingOptions::nodes_limit` must be set.** It defaults to `u32::MAX`
   (`parse.rs:346`). Set it to a documented bound and surface
   `Error::NodesLimitReached` (`parse.rs:97,516`) as a parse failure.
3. **Total input size is bounded before the file is read into a `String`**, per
   28 §5.3.

What is *not* a risk, recorded so it is not re-litigated: roxmltree parses a
`&str` and performs no I/O, so a `SYSTEM` external id is never fetched — there is
no XXE file-read or SSRF surface. Entity-reference loops are caught
(`Error::EntityReferenceLoop`, `parse.rs:468,491`), so "billion laughs" is handled
by the crate. The residual XML risks are node count and input size, and both are
closed by rules 2 and 3.

---

## 5. Migration and format-version impact

**`CURRENT_FORMAT_VERSION` stays 5** (`crates/photonic-core/src/document.rs:117`).
X-1 lands additively inside v5 and needs no v6.

Reasoning, point by point:

- The migration chain (`migration.rs:58`) exists to *reinterpret persisted data*
  on the way from N to N+1. X-1 reinterprets nothing and adds no field (§4.1), so
  there is no v5→v6 step to write and nothing for `run_migrations` to do.
- A document produced by an MLT import is indistinguishable from one produced by
  hand: same `Sequence`, `Track`, `Clip`, `ClipEffect` shapes, same serde. It
  saves at v5 and opens in any build that reads v5.
- The precedent cuts both ways. `V1ToV2` and `V2ToV3` (`migration.rs:70,87`) are
  no-op steps that exist only to stamp a number for purely additive changes.
  Adding a v6 that stamps a number for a change touching no field would cost
  every user a compat-window step (`COMPAT_WINDOW = 1`, `migration.rs:16`) for
  nothing, and would make `V5ToV6` a lie about what changed. **Bump only when
  data must be reinterpreted.**
- The one change that *would* force a v6 is the rejected one: `PropValue::Str`
  (§3.8). §4's decision is therefore also a format-version decision, and it
  should be settled here rather than discovered when a v6 appears in a diff.

ROADMAP §10 point 5 ("additive serde/migration round-trip passes when model
changes") is satisfied because the model does not change. Test 11 pins
`format_version == 5` for a document containing an MLT-imported sequence with
inert `mlt.*` effects, and pins that those effects round-trip byte-identically.

---

## 6. Undo unit and its exact inverse

### 6.1 One verb, one `Command::Batch`

The user verb is **"Import Kdenlive / MLT project…"** and it produces exactly one
undo entry, via the shape `import_media` already uses (`handlers/video.rs:2622`):

```rust
history.execute_discrete(Command::Batch(cmds), &mut doc);
```

`cmds`, in this order:

1. `Command::Timeline(ops::create_project())` — only when `doc.timeline.is_none()`.
2. `Command::Timeline(ops::create_bin(file_stem, None))`, then one
   `ops::create_bin(name, parent)` per imported folder — so the source project's
   bin tree survives and the whole import is visibly grouped.
3. `Command::Timeline(ops::add_asset(a))` × N, each with `a.bin` set **at
   construction** (the same trick `import_media` uses at `handlers/video.rs:2614`
   to dodge the `AssignAssetBin`-before-`AddAsset` ordering problem).
4. `Command::Timeline(ops::add_sequence(s))` × M — **innermost nested tractor
   first**, so a `ClipSource::NestedSequence` clip never references a sequence
   that is not yet present.
5. `Command::Timeline(ops::set_active_sequence(p, Some(outermost)))`.

**Each `AddSequence` carries its fully-built `Sequence`, tracks and clips inline**
(`commands.rs:451`). There are **no per-clip commands**, and this is not an
optimisation — it is a correctness requirement. `Command::Batch::apply` applies
members one at a time (`history/mod.rs:2824-2828`) and `TimelineCmd::apply`
debug-asserts `Sequence::validate()` after **every** member
(`commands.rs:1749-1757`). An import expressed as N `InsertClip` commands would
transiently hold a partially-populated sequence — and, during the §3.3 collapse,
transiently overlapping clips — and would panic in debug. A 900-clip project is
`1 + B + N + M` commands, not 900.

### 6.2 The exact inverse

Mechanical, not hand-written. `Command::Batch` inverts as "the reversed batch of
inverses" (`history/mod.rs:3172-3178`), so the inverse of the above is, in order:

| Forward | Inverse | Where |
|---|---|---|
| `SetActiveSequence { old, new }` | `SetActiveSequence { old: new, new: old }` | `commands.rs:2226` |
| `AddSequence { sequence }` × M | `RemoveSequence { sequence, order_index, was_active }` × M, reverse creation order | `commands.rs:2206` |
| `AddAsset { asset }` × N | `RemoveAsset { asset }` × N, reverse order | `commands.rs:2157` |
| `AddBin { bin }` × B | `RemoveBin { bin }` × B, reverse order (children before parents) | `commands.rs` |
| `CreateProject { project }` | `RemoveProject { project }` | `commands.rs` |

Every member already has a tested inverse. Redo re-applies the forward batch.
One undo returns the document to a byte-identical `to_json` — test 9.

**Validate-then-commit** (39 §1.1). The whole file is parsed, every `Sequence` is
run through `Sequence::validate` (`sequence.rs:378`) *including*
`validate_transitions` (`sequence.rs:414`) and `validate_groups`
(`sequence.rs:433`), **before the first command is constructed**. A parse or
validation failure yields `Err(InterchangeError)` and mutates nothing. A
partially imported timeline is not an acceptable outcome and is not reachable.

**`mem_estimate` must be honest.** This batch carries whole `Sequence` values
(`commands.rs:1638` measures them by JSON length) and is legitimately large. It
is bounded by the history byte budget like any other large command, and the
retention floor guarantees it cannot empty the history by itself.

---

## 7. MCP surface and GUI parity

One direction, one tool, one GUI route. CAP-019 parity is not optional and PA-11
records that the MCP trail is already the weak side.

### 7.1 `import_mlt_project`

| Arg | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | — | `.mlt` or `.kdenlive` file to read |
| `bin` | string? | file stem | top-level media-pool bin for created assets |
| `relink_by_hash` | bool | `true` | reuse an existing pool asset when `content_hash` matches |
| `activate` | bool | `true` | make the imported outermost sequence active |
| `dry_run` | bool | `false` | parse and report; execute no command, mutate nothing |

Returns the report as structured data:

```json
{ "sequences_created": 3, "tracks": 7, "clips": 214, "markers": 9,
  "assets_created": 22, "assets_reused": 2, "caption_tracks": 1,
  "unsupported":  [{ "what": "filter frei0r.glow", "where": "clip 12 on track V2",
                     "consequence": "kept in the stack, disabled; it will not render",
                     "count": 37 }],
  "approximated": [ … ] }
```

`dry_run` carries more weight here than in 196: an MLT project's report is
routinely long, and the pre-import sheet (§7.3) is driven by the identical
`dry_run` pass so the GUI and MCP show byte-identical text.

**No `export_mlt` tool exists and none is proposed.** 34 §3.5 settles it; adding
one later would be a new item with a new mini-spec.

### 7.2 Why one tool, not two

There is a real argument for a separate `inspect_mlt_project` that never touches
the document. It is rejected: `dry_run: true` on one tool is the same capability
with one fewer name to keep in sync, and 26 §16's K-H trail is already long
enough. An agent that wants to look before it leaps calls the tool twice.

Both the tool and its schema go through `crates/photonic-mcp/src/dispatch.rs`,
the tool-name list at `handlers/video.rs:8330+`, and `schema_gen.rs`;
`docs/mcp-api.md` is regenerated by `cargo run -p photonic-mcp --bin dump_tools |
python3 tools/gen-mcp-docs.py` and the CI drift gate
(`.github/workflows/ci.yml:162-167`) enforces the diff.

Every failing tool result carries the full `Diagnostic` in its data payload per
36 §5, not a prose string.

### 7.3 GUI route

`FILE_OPTIONS` is `&["Document", "Save", "Export"]`
(`crates/photonic-gui/src/app/mod.rs:290`), rendered by the File drawer
(`crates/photonic-gui/src/app/menu_drawer.rs:31`). [196 §7.3](196-x-2-opentimelineio-interchange.md)
proposes adding a fourth column, **"Interchange"**. X-1 adds one entry to that
column — **"Import Kdenlive / MLT project…"** — with an `rfd` picker filtered to
`mlt`, `kdenlive`, using the same `run_file_dialog` pattern the Open/Save-As
buttons already use (`menu_drawer.rs:54,138`).

If X-1 lands **before** X-2, X-1 creates the column; if after, it joins it. Either
way there is exactly one Interchange column and neither item ships a second
surface.

The command opens a **modal report sheet before committing**, driven by the
`dry_run` pass, listing unsupported and approximated entries **grouped by reason
with counts** and enumerating only on expansion. A 500-clip project with one
unmapped grade per clip must read as "500 clips: `frei0r.coloradj_RGB` kept
inert" and not as 500 rows — report fatigue defeats §1's whole point (§13 risk 4).
The sheet has "Copy report" and "Save report…" buttons (§8.2).

---

## 8. The report and diagnostics

### 8.1 One source of truth, two surfaces

X-1 **reuses 196 §4.1's types verbatim** — `InterchangeReport`, `Unsupported`,
`Approximation`, `Location` — living in `interchange/mod.rs`, not in
`interchange/mlt/`. 34 §2 requires exactly one reporting discipline across all
three importers and this document adds nothing to it. Two consumers:

- **The sheet** (GUI) and the tool result (MCP) — full detail, per item,
  grouped by reason with counts.
- **The diagnostic log** — one coalesced entry per `(code, subject)` per the
  existing `DiagnosticLog` behaviour (`diag.rs`), so a 400-clip file with 400
  dropped filters fires one toast, not 400.

**Location needs one addition for MLT.** 196 §4.1's `Location` enum is
`Timeline | Track | Item | Asset`. An MLT `<filter>` on a tractor or a playlist
belongs to no item and no asset. **X-1 adds one variant,
`Location::Scope { owner: VfxOwner }`**, so a master-scope entry can say where it
was. That is an addition to a non-persisted engine type in `photonic-video`, not a
model change, and X-3 inherits it.

### 8.2 The sidecar report file — where the dropped strings go

§3.8 drops non-numeric filter properties from the document. They are not lost:
the import sheet's **"Save report…"** button, and the MCP tool's report payload,
carry every dropped property verbatim — service name, property name, property
value — and the sheet writes `<imported-name>.import-report.json` to a
user-chosen location on request.

This is a user artefact, not document state and not a cache sidecar. It is what
makes "never drop silently" true in the strong sense: a user who needs to know
exactly what a filter was set to can read it, and an implementer extending the
§3.8 service table can diff two reports to see which services actually occur in
real projects.

### 8.3 Diagnostic codes

X-1 **reuses four of 196 §8's five proposed codes** and adds **one**:

| Code | Owner | Default severity | Consequence line |
|---|---|---|---|
| `InterchangeParseFailed` | 196 §8 | `Error` | "The file could not be read; nothing was imported." |
| `InterchangeUnsupportedConstruct` | 196 §8 | `Warning` | "Part of the file has no Photonic equivalent and was left out." |
| `InterchangeMediaUnresolved` | 196 §8 | `Warning` | "Media could not be located; the clip is offline." |
| `InterchangeRateApproximated` | 196 §8 | `Warning` | unused by X-1 (§3.1) — listed so it is clear it was considered, not forgotten |
| `InterchangeLossyExport` | 196 §8 | `Warning` | unused by X-1 — there is no MLT export |
| **`InterchangeEffectKeptInert`** | **X-1** | `Warning` | "An effect from the imported project has no Photonic equivalent. It is kept in the clip's stack, disabled, and will not render." |

The new code earns its place on the coalescing key. `DiagnosticLog` coalesces on
`(code, subject)`, so folding inert-preserved effects into
`InterchangeUnsupportedConstruct` would give a project that has both a dropped
transition *and* 200 inert filters on the same clip **one** toast for two
materially different outcomes — one where something vanished, one where something
is sitting visibly in the stack, disabled. Those need different remedies and
different words.

If X-2 has not landed when X-1 does, X-1 registers all five of 196 §8's codes
plus its own; if it has, X-1 registers one. Adding any of them requires updating
`DiagCode::family()`, `default_severity()` and `consequence()` in lockstep (the
macro at `diag.rs:170` generates `ALL`, `as_str` and `FromStr` for free) plus
`EXPECTED_WIRE_CODES` in `crates/photonic-core/tests/diag_catalogue.rs:27` — a
deliberately frozen list, so forgetting it trips the gate, which is the gate
working as designed. `families_partition_all_codes`
(`diag_taxonomy.rs:102-120`) already enumerates `Interchange` and keeps passing.

---

## 9. Clean room: authoring a fixture from nothing, and confirming it afterwards

26 §7 states X-1's constraint in one sentence: *"implement from the published DTD
only; fixtures must be Photonic-authored, never scraped from a GPL project test
suite."* Everything below is that sentence made operational, because a rule
nobody can execute or audit is not a rule.

### 9.1 The two rooms

| | Requirements room | Implementation room |
|---|---|---|
| **Person** | requirements author | implementer |
| **May read** | the published MLT XML DTD and online format documentation (26 §2 row 8: *readable as a format specification*); Kdenlive user documentation at `docs.kdenlive.org`, `CC-BY-SA-4.0` (26 §2 row 7: *readable as a requirements source; cite, never paste*); standards; the observation notes of §9.2 | **only** this document and the requirements author's notes |
| **May never read** | — | the MLT source tree, the Kdenlive source tree, frei0r, `mlt++`, any GPL/LGPL derivative, or any of their test data |
| **Produces** | this spec's §3 tables, the §3.8 service table's *semantics*, the §9.2 observation notes, the fixtures | the parser and builder |

23 §3.4 item 6 is the fallback if this is ever violated: *"if rejected source has
already been inspected for a specific subsystem, assign that subsystem to an
independent implementer."* An implementer who has read MLT source at any point,
for any reason, is disqualified from this item and says so before starting rather
than after.

### 9.2 The host-private namespace problem, solved without reading source

The MLT DTD does not describe `kdenlive:*` or `shotcut:*`. Their meanings are
not in any published schema. There are exactly three ways to learn them, and only
two are permitted:

1. **Read Kdenlive's source.** `REJECT`. Not available, at any point, to anyone
   on this item.
2. **Read Kdenlive's user documentation.** Permitted (26 §2 row 7). It establishes
   which *concepts* exist — folders, zones, markers, proxies — but not the exact
   key spellings.
3. **Black-box observation of a self-produced file.** Permitted under 23 §3.4's
   written-observation rule, and it is how the spellings are obtained.

**The observation protocol, normatively:**

- The observer is the **requirements author**, never the implementer, and works
  on a machine with no Photonic checkout.
- Inputs are **owned**: the observer's own media, or the existing Photonic
  synthetic corpus (`color_bars.mp4`, `counter.mp4`), placed in a project built
  by hand in the reference application.
- The observer records a dated **written observation note** — a plain-text file
  listing, per observed key, the key name, the UI action that produced it, the
  observed value, and the inferred meaning. **The note enters the repo. The
  observed file does not.**
- The observation note carries no XML fragments longer than a single
  `name="value"` pair, because a key name is a fact and a document is expression.
- Legal approval is recorded before the protocol runs, per 23 §3.4's closing
  paragraph.

This is what closes the gap that would otherwise force either a guess or a
licence violation, and it is why every host-private key in §3.7 is marked
**(confirm-before-code)** rather than asserted.

### 9.3 How a fixture is authored from nothing

**Every fixture is a hand-written `.mlt` file in this repository.** Not exported
from Kdenlive, not exported from Shotcut, not adapted from anyone's test suite,
not "cleaned up" from a real project. The authoring procedure:

1. Start from an empty file. Write the `<mlt>` root, then the `<profile>`,
   element by element, **with the DTD clause each element satisfies cited in an
   XML comment beside it.**
2. Add only what the fixture's one stated purpose requires. `offbyone.mlt` has
   two producers and three entries; it does not have a filter.
3. Point every `resource` at either a file in the existing Photonic corpus
   (`crates/photonic-video/tests/fixtures/`, `README.md` records its provenance)
   or at a path that deliberately does not exist.
4. Use only Photonic-generated UUIDs and Photonic-authored names. No `id="4"`
   that came from somewhere; no `title` string; no version stamp.
5. Write the provenance header (§9.4) at the top of the file.
6. Add a row to `crates/photonic-video/tests/fixtures/mlt/README.md` recording
   the fixture, its purpose, the published clause it exercises, the author, and
   the date — the same shape the existing corpus README uses and what
   [23 §12](../specs/video-editor/23-legal-open-source-implementation-routes.md#12-cross-cutting-provenance-manifests)'s
   `FixtureRightsManifest` asks for, with `source_method = Synthetic`.

**The fixtures do not need to be files Kdenlive would produce.** They need to be
files that exercise the parser against the published format. That distinction is
the entire point: chasing byte-compatibility with a specific Kdenlive build's
output is how a fixture corpus becomes a transcription of someone else's
behaviour.

### 9.4 How a reviewer confirms provenance after the fact

Provenance that only the author can vouch for is not provenance. Five checks, all
mechanical, all runnable by a reviewer who was not present:

1. **Header lint.** Every `tests/fixtures/mlt/*.mlt` begins with:
   ```xml
   <!-- Photonic-authored fixture. Not derived from any MLT, Kdenlive or Shotcut
        source tree or test suite. Written by <author>, <date>, against the
        published MLT XML DTD clauses cited inline. Purpose: <one line>. -->
   ```
   A test in `crates/photonic-video/tests/mlt_fixture_provenance.rs` asserts the
   header is present and non-empty on every fixture, patterned on the existing
   `photonic-gui/tests/keyboard_gate_lint.rs` source lint. A fixture added
   without it fails CI.
2. **Foreign-artefact scan.** The same test asserts no fixture contains a
   reference-application version stamp, an absolute path outside the repository,
   a home-directory path, an e-mail address, or a UUID not present in the
   fixtures README's manifest. These are the fingerprints a copied file carries
   and an authored one does not.
3. **Git history.** `git log --follow --diff-filter=A -- <fixture>` shows a
   single Photonic commit adding a single file. A bulk import of a directory of
   `.mlt` files in one commit is the signature this check exists to catch, and it
   is visible forever.
4. **Size and shape.** Every fixture is small enough to read in full — the budget
   below is 8 KB each. A reviewer can therefore actually read them, which is the
   only check that catches a *paraphrased* copy. This is why the corpus is many
   tiny single-purpose files rather than a few realistic projects.
5. **Attestation.** The implementer records the 23 §3.4 attestation for this
   subsystem — that they did not inspect the MLT or Kdenlive source trees — and
   a second reviewer checks identifiers, comments, constants, control flow and
   test provenance before merge (23 §3.4 items 3 and 5, and 26 §2 item 2's
   blanket rule).

**Naming discipline** (26 §2, closing): describe the capability as *"imports
Kdenlive and Shotcut projects"*, never as endorsement, certification, or an
official relationship with the KDE project or the MLT project.

---

## 10. Acceptance fixtures and tests

### 10.1 Fixtures — Photonic-authored, and X-1 is *not* a gated item

All fixtures are hand-written `.mlt` XML under a new
`crates/photonic-video/tests/fixtures/mlt/` with a `README.md` manifest, authored
per §9.3 and audited per §9.4.

Media references point either at the existing Photonic-generated corpus
(`color_bars.mp4`, `counter.mp4`, `beep_flash.mp4` — provenance already recorded
in `crates/photonic-video/tests/fixtures/README.md`) or at paths that
deliberately do not exist. **No third-party or rights-encumbered content is
required, so X-1 is not `legal-or-fixture-blocked`.** Recording that explicitly
matters: it is the difference between a schedulable item and a blocked one, and
it is a different answer from the one the G-20 / K-D1 rows get.

Added weight is text XML at ≤ 8 KB per fixture, on the order of 100 KB total —
negligible against the corpus's ~2.5 MiB current size and
[11 §1.5](../specs/video-editor/11-testing-phasing.md)'s 5 MB budget.

| Fixture | Exercises |
|---|---|
| `minimal.mlt` | `<profile>` + one producer + one playlist + one tractor; the smallest legal document |
| `offbyone.mlt` | a 1-frame entry (`in == out`), an entry at the sequence end, three back-to-back entries — §3.4 |
| `blanks.mlt` | `<blank>` between entries, at the head, and two adjacent — §3.2 gap arithmetic |
| `two_playlist_pair.mlt` | the §3.3 collapse: two playlists, one overlap, one joining transition |
| `two_playlist_unmatched.mlt` | three playlists on one track — the §3.3 step-4 refusal |
| `anim_grammar.mlt` | every interpolator token: empty (linear), `~`, `\|`, `!`, `-`, and one from each of the 33 easing families |
| `anim_negative_pos.mlt` | `-1=v`, `-1-=v` (the overloaded `-`), and a negative position on a lane whose length changes |
| `rect_dialects.mlt` | `0 0 1920 1080 1`, `0/0:1920x1080:1`, `0%/0%:100%x100%:100%` on one property |
| `locale_comma.mlt` | a declared comma-decimal locale and a `C`-locale sibling value |
| `filters_mapped.mlt` | three services that map, one with a `factor`-scaled normalised param |
| `filters_unmapped.mlt` | one unmapped service with numeric params, one with a string-valued property, at clip / playlist / tractor scope |
| `nested_tractor.mlt` | a tractor referenced as a producer, two levels deep |
| `xml_producer.mlt` | an `mlt_service=xml` producer — the §3.6 refusal |
| `timewarp.mlt` | `0.5:`, `2.0:`, and a negative speed |
| `profile_anamorphic.mlt` | `sample_aspect_num/den = 40/33` — the §3.9 PAR loss |
| `profile_ntsc.mlt` | `frame_rate_num/den = 30000/1001` |
| `missing_media.mlt` | a `resource` that does not exist, and one outside the project directory |
| `subtitle_sidecar.mlt` | a subtitle filter pointing at a Photonic-authored `.srt` |
| `hostile.mlt` | 100 MB declared length on a 2 KB file, 10⁵ nested elements, an entity-expansion attempt, a `resource` of `../../../etc/passwd` |

### 10.2 Tests

Numbered rows are the ones 34 §6 already owns; lettered rows are this document's.

| # | Test | Owner |
|---|---|---|
| 1 | **Off-by-one** — a 1-frame clip and a clip at the sequence end import with exactly correct `duration`; `duration == out - in + 1` frames | 34 §6 |
| 2 | **Two-playlist collapse** — `two_playlist_pair.mlt` imports with the original track count and the transition on the incoming clip | 34 §6 |
| 3 | **Animation grammar** — every interpolator token in `anim_grammar.mlt` parses; back/elastic/bounce produce `Unsupported`, never a silently flattened ease | 34 §6 |
| 4 | **Rect dialects** — the three notations produce identical rects; `%` divides by 100 | 34 §6 |
| 5 | **Negative positions** resolve against length; `-1-=v` parses as smooth-tight at the last frame | 34 §6 |
| 6 | **Unmapped effects** are preserved inert and re-serialize unchanged on `.photon` save/reload | 34 §6 |
| 9 | **Import is one undo unit** — one `execute_discrete`; one undo restores a byte-identical `to_json` | 34 §6 |
| 10 | **Report completeness** — every row of §3.9 that a fixture exercises produces exactly one entry naming it; `filters_unmapped.mlt` produces exactly three | 34 §6 |
| 11 | **Fixture provenance** recorded in the fixtures README | 34 §6 |
| A | **Frame alignment** — every imported `start`, `duration` and `source_in` satisfies `t == rate.snap(t)`; there is no sub-frame position anywhere in an imported document | §3.1 |
| B | **Exact rational rate** — `profile_ntsc.mlt` yields `FrameRate { num: 30000, den: 1001 }`, not 29.97, and no `InterchangeRateApproximated` is emitted | §3.1 |
| C | **Gap arithmetic** — `blanks.mlt`: removing each `<blank>` shifts the following clip's `start` by exactly its length; `end() == next.start` where blanks are absent | §3.2 |
| D | **Track order** — a two-video-track tractor composites bottom-up in the same order after import (`video_tracks[0]` is the bottom, `sequence.rs:134`) | §3.2 |
| E | **No `transition_out` at a cut** — no imported document is rejected by `Sequence::validate_transitions`, and no import produces a `LoadNoticeCode::TransitionOutAtCutDropped` on reload | §3.3 |
| F | **Collapse refusal** — `two_playlist_unmatched.mlt` imports as three tracks with exactly one `Unsupported`, and no clip is re-timed | §3.3 |
| G | **Inert namespace** — after importing `filters_unmapped.mlt`, every unmapped effect has `inert == true`, `enabled == false`, and an `id` starting `mlt.`, in all three scopes it appears in | §3.8 |
| H | **No forward-compat leakage** — after importing `filters_unmapped.mlt` and `xml_producer.mlt`, the document contains **no** `ClipSource::Unknown` and **no** `TransitionKind::Unknown`; the only `EffectKind::Unknown` tags present all start `mlt.` | §4.2 |
| I | **Reserved-prefix lint** — no `EffectId` in `MANIFESTS` (`effect_manifest.rs:535`) starts with `mlt.` | §3.8 |
| J | **Validate-then-commit** — a fixture with an overlapping entry that cannot be collapsed fails with `Err`, no document mutation, and no history entry | §6.2 |
| K | **`dry_run` purity** — `dry_run: true` produces a byte-identical report to the real import and leaves `to_json` and the history depth unchanged | §7.1 |
| L | **GUI/MCP parity** — the GUI sheet text and the MCP `unsupported` array come from one `InterchangeReport` and agree | ROADMAP §10.10 |
| M | **Format version unchanged** — a document containing an MLT-imported sequence with inert `mlt.*` effects saves at `format_version == 5` and reloads with those effects byte-identical | §5 |
| N | **Bounded parse** — `hostile.mlt`: the declared-length lie is refused, the node limit trips `InterchangeParseFailed`, the entity expansion does not allocate unboundedly, and the `../../../etc/passwd` resource becomes an offline asset **with no filesystem read attempted** | §4.3, 28 §5.3 |
| O | **Fuzz** — `parse.rs` and `anim.rs` run under a fuzz target; no panic on any input, per 28 §5.3's explicit ask | 28 §5.3 |
| P | **Provenance lint** — §9.4 checks 1 and 2 as a test over `tests/fixtures/mlt/` | §9.4 |
| Q | **Caption handoff** — `subtitle_sidecar.mlt` produces a `CaptionTrack` whose cues match the sidecar, and the caption `ImportSummary` notes appear in the `InterchangeReport` | §3.7 |

Test 1 deserves the emphasis 34 §6 gives it: inclusive-`out` versus half-open
`duration` is invisible in casual testing and corrupts every clip in the project
by one frame. Test A is its structural companion — an import that is off by one
*frame* will usually still be frame-aligned, so A catches a different class of
arithmetic error than 1 does.

---

## 11. What X-1 reuses from X-2, and where MLT forces a divergence

34 §7 sequences **X-2 before X-1**, and this document assumes that order. What
X-1 inherits, and what it must build if the order is inverted:

| From [196](196-x-2-opentimelineio-interchange.md) | X-1 uses it | Divergence |
|---|---|---|
| `InterchangeReport` / `Unsupported` / `Approximation` / `Location` (§4.1) and the never-drop-silently rule | verbatim, from `interchange/mod.rs` | X-1 **adds** `Location::Scope { owner: VfxOwner }` (§8.1) — MLT filters attach to scopes that own no item |
| The five `DiagFamily::Interchange` codes (§8) | four of five | X-1 **adds** `InterchangeEffectKeptInert` (§8.3); `InterchangeRateApproximated` and `InterchangeLossyExport` are unused |
| The rate-recovery ladder (§3.1) | **not used at all** | MLT declares an exact `num`/`den`. This is the largest divergence and the one an implementer is most likely to copy by reflex |
| The source-timecode rebase convention (§3.6) | **not needed** | MLT entry `in`/`out` are producer-relative frame indices, not source timecode. X-2's highest-probability defect has no analogue here; X-1's is the inclusive-`out` off-by-one instead |
| The one-batch-one-undo commit shape (§6.1) and the `ops::` constructor discipline | verbatim | X-1's batch additionally carries a bin *tree*, so `AddBin` ordering is parent-before-child |
| The GUI "Interchange" File-drawer column and the report sheet (§7.3) | joins it | X-1 has one entry, not two — there is no export |
| The `photonic` metadata namespace (§3.7) | **not applicable** | that is a property of an OTIO *writer*; X-1 writes nothing |
| The fixture-provenance discipline (§9.1) | extends it | X-2's fixtures need only be Photonic-authored JSON; X-1's need §9's two-room protocol, because the format's host-private half is not published |
| Validate-then-commit (§6.1) | verbatim | |

**If X-1 lands first**, it must additionally build `InterchangeReport` and
register the `Interchange` diag codes it uses. Both are small and both live in
shared locations, so the order affects sequencing but not design. What must not
happen is two report types.

---

## 12. Definition of done (ROADMAP §10), made answerable

| # | ROADMAP §10 requirement | How X-1 answers it |
|---|---|---|
| 1 | Core op/engine service with unit tests | `photonic-video/src/interchange/mlt/{parse,anim,services,build}.rs`; tests §10.2 |
| 2 | GUI route, or a recorded exception | File drawer → **Interchange** → "Import Kdenlive / MLT project…" (§7.3). **No exception is sought** |
| 3 | MCP tool/schema/generated docs | `import_mlt_project` (§7.1); `docs/mcp-api.md` regenerated, CI drift gate (`ci.yml:162-167`) green |
| 4 | One user verb, one undo unit; undo/redo identity | §6; tests 9 and J. The inverse is tabulated per member at §6.2 |
| 5 | Additive serde/migration round-trip when the model changes | **The model does not change** (§4.1/§5). Test M pins `format_version == 5` and inert-effect byte identity |
| 6 | Pixel/audio IR/eval/golden/sync coverage | **N/A — X-1 adds no pixel or audio path.** It produces ordinary model values that existing compile paths already cover. Stated rather than invented; 196 §11 hit the same clause first |
| 7 | Hard gates green; trend metrics not regressed | Parsing is off the hot path. One added hard gate, because it is deterministic: a 1000-clip fixture parses and builds in < 1 s on the CI runner |
| 8 | Offline, privacy, licensing, content, product gates | **Offline:** parsing is local; import performs **no filesystem read of any `resource`** (§3.9) and no network access. **Licensing:** §14; no new dependency (§4.3). **Content:** §10.1 — not gated. **Privacy:** the report and the sidecar report file contain paths and service names, never media content |
| 9 | Protected surfaces not regressed | PA-7 (half-open) is what §3.4 defends and tests 1/C anchor; PA-8 (flicks + exact rational) is what §3.1 defends and tests A/B anchor; PA-9 (typed model) is what §4.2 and §3.8 defend and tests G/H/I anchor. E-8's locale-independent serialization is §3.6's last paragraph |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | L1 module exists → L2 real parser over the §10.1 corpus → L3 wired into the File drawer and `dispatch.rs` → L4 a **self-authored `.kdenlive` project produced under §9.2's observation protocol** imports, plays back, and exports a frame. Test L pins parity |

Point 10's L4 is worth spelling out: the acceptance evidence is a real
reference-application project, produced by the requirements author under the
observation protocol, imported and played. That file is *evidence*, recorded in
the observation note; it is **not** committed as a fixture (§9.3).

---

## 13. Risks, open questions, deliberate exclusions

### 13.1 Risks

1. **The inclusive-`out` off-by-one.** Highest probability, invisible in casual
   testing, corrupts every clip by one frame. Mitigation: test 1 is mandatory and
   `offbyone.mlt` exists solely for it; the arithmetic appears in exactly one
   function so there is one place to get it right.
2. **The two-playlist collapse mis-detecting.** A false positive silently merges
   two genuinely separate tracks and destroys a composite; a false negative
   doubles the track count, which is ugly but visible and reversible. The
   asymmetry justifies §3.3 step 4's bias: **when in doubt, do not collapse.**
   Test F pins the refusal.
3. **The `mlt.` namespace leaking into the forward-compat channel.** §3.8 point 2
   names the residual cost. It is currently harmless because `LoadReport` has no
   consumer, and §15 follow-up 2 must land with or before 36 §3's diagnostic
   wiring or the message becomes wrong.
4. **Report fatigue.** A real project produces hundreds of entries. Grouping by
   reason with counts (§7.3) is the defence and it must be built with the sheet,
   not retrofitted. An unreadable report is the same product outcome as no report.
5. **Service-table rot.** §3.8's table starts small and every added entry is a
   fidelity claim someone must be able to defend. Mitigation: the sidecar report
   file (§8.2) turns "which services actually occur" into data, so the table
   grows against evidence rather than against a wish list.
6. **`(confirm-before-code)` items becoming guesses.** Six things in §3 are
   marked as needing confirmation against a published source. If the source
   cannot be found, the correct outcome is an `Unsupported` row, not an
   inference. A reviewer should treat a resolved marker with no cited source as
   a blocker.

### 13.2 Open questions needing a product call

- **Q1 — Import destination.** This spec always creates new sequences and a new
  bin, and never merges. *Recommendation: ship create-only.* Merge needs a
  conflict model (track matching, id collision, marker merge) that is a mini-spec
  of its own, and create-only is strictly forward-compatible with adding merge
  later. Same recommendation and same reasoning as 196 §12.2 Q1, deliberately.
- **Q2 — Should an anamorphic project refuse to import?** §3.9 imports it as
  square-pixel with a loud report, and the result is geometrically wrong. The
  alternative is to refuse. *Recommendation: import with the report.* A user
  migrating a 4:3 anamorphic archive still wants the edit structure, and the
  correct fix is a `pixel_aspect` on `SequenceFormat`, which is a real feature
  with its own owner — not a reason to withhold the import. If it lands, this row
  leaves the lossiness register.
- **Q3 — Should the sidecar report file be written automatically?** §8.2 makes it
  on-request. *Recommendation: on-request.* An automatic file next to the
  imported project writes into a directory the user did not choose, and 39 §1.6's
  view-state discipline says a path preference is not document state. One button
  is enough.
- **Q4 — Does Shotcut get its own fixtures?** One importer covers both, since
  `.kdenlive` and Shotcut's `.mlt` are the same format with different host
  namespaces (26 §18). *Recommendation: one importer, one fixture corpus, and a
  `shotcut:` allowlist added only when §9.2's observation protocol has actually
  been run for it.* Claiming Shotcut support without having observed a Shotcut
  document would be an unearned compatibility claim.

### 13.3 Deliberately out of scope for v1

- **MLT XML export.** 34 §3.5; never.
- **External document references** (`mlt_service=xml`). §3.6; refused, reported.
- **Proxy adoption.** §3.7; Photonic generates its own.
- **Melt-style consumers, `<consumer>` elements, and render profiles.** They
  describe an output pipeline Photonic does not share; ignored without report,
  the same treatment as host UI state.
- **Negative-speed clips.** §3.5; refused pending a compiler contract.
- **Interlaced sources.** K-G6 is unscheduled; §3.9 reports and moves on.
- **Round-tripping unmapped filter *string* parameters into the document.**
  §3.8; they go to the report and the sidecar file.
- **`PropValue::Str`.** §3.8, §5; rejected with reasons, and the rejection is
  itself the format-version argument.
- **Automatic probing of imported assets.** §3.9; the L1/L2 ladder in
  [24](../specs/video-editor/24-preview-media-load.md) owns it and doing it during
  import would make the importer a filesystem-read primitive.

---

## 14. Clean-room provenance

Required by [26 §7](../specs/video-editor/26-kdenlive-mlt-parity.md#7-how-to-read-the-item-tables);
this item's provenance risk is the highest of any in 26, so the note is explicit
and §9 is its operational half.

- **Design source.** The **published MLT XML DTD** and the MLT online format
  documentation — 26 §2 row 8 classifies these as *readable as a format
  specification*. Formats are facts and interfaces, not expression (34 §1). Plus
  **Kdenlive's user documentation** (`docs.kdenlive.org`, `CC-BY-SA-4.0`), 26 §2
  row 7, *readable as a requirements source: cite, never paste* — used only to
  establish which concepts exist, never for a key spelling or a value. Plus the
  §9.2 written observation notes, produced under 23 §3.4's black-box protocol
  with owned inputs, by a person who is not the implementer.
- **Not derived from.** The **MLT source tree** (`LGPL-2.1+`, `REJECT` for this
  document per 26 §2), the **Kdenlive source tree** (`GPL-3.0`, `REJECT`, already
  named in 23 §5), `mlt++`, the `plusgpl`/`qt`/`melt` modules, frei0r, LADSPA, or
  any GPL/LGPL derivative. Specifically **not** MLT's serializer or
  deserializer, and **not** any of their test data — 34 §3 and 26 §7 both say
  this and §9.1's two-room split is how it is enforced rather than asserted.
- **Fixtures.** Photonic-authored `.mlt` XML, §9.3, audited by §9.4's five
  mechanical checks. **No file is copied or adapted from any other project's test
  suite.** No third-party or rights-encumbered media is required, so 23 §7.2's
  `AssetRightsManifest` gate is not engaged and **X-1 is not a legal- or
  fixture-gated item.**
- **No new dependency.** `roxmltree` is already in the build (§4.3), so nothing
  in 26 §2's reject list enters the tree directly or transitively, and no
  [23 §3.3](../specs/video-editor/23-legal-open-source-implementation-routes.md#33-required-evidence-record)
  evidence record is required. 26 §2 item 4 names X-2 and K-E1 as the only two
  items contemplating a dependency; X-1 must not become a third.
- **Attestation.** The implementer records the 23 §3.4 attestation for this
  subsystem before merge; an independent reviewer checks identifiers, comments,
  constants, control flow and test provenance (23 §3.4 items 3 and 5). An
  implementer who has previously inspected MLT or Kdenlive source is reassigned
  under 23 §3.4 item 6 rather than asked to forget.
- **Naming discipline.** *"Imports Kdenlive and Shotcut projects."* Never
  endorsement, certification, compatibility guarantee, or an official
  relationship with the KDE project or the MLT project.

---

## 15. Follow-ups

Recorded here rather than edited into the owning documents, per this proposal's
one-file scope.

1. **[34 §3](../specs/video-editor/34-interchange.md#3-x-1--mlt-xml-and-kdenlive)**
   should absorb four things this document decides that it does not currently
   state: the §3.3 collapse-refusal rule (when in doubt, do not collapse), the
   §3.5 negative-speed refusal, the §3.7 host-namespace allowlist rule with its
   deliberate unreported-ignore exception, and the §3.8 conclusion that an inert
   effect **cannot** carry string parameters. Its §3.4 currently reads as though
   full parameter preservation is available; it is not, and that is a property of
   the model rather than a choice.
2. **[39 §2.2](../specs/video-editor/39-document-lifecycle.md)** and
   [36 §3](../specs/video-editor/36-error-model.md)'s eventual wiring of
   `LoadReport::unknown_variants` must distinguish a tag in a reserved foreign
   namespace (`mlt.`) from a genuine forward-compat tag and must not report the
   former as "a newer build wrote this". `ClipEffect.inert` already carries the
   distinction; only the message needs it. This must land **with or before** that
   wiring, not after (§13 risk 3).
3. **`finalize_effect_ids` (`crates/photonic-core/src/timeline/load.rs:485-521`)
   only walks clip effects** and skips `Track::effects`,
   `Sequence::master_effects` and `MediaAsset::effects`. This is a pre-existing
   defect, independent of X-1 — 35 §2's four scopes all exist and only one is
   reconciled on load — but X-1 puts effects in all four and is the first item to
   depend on it. It should be fixed as its own small change, with a test per
   scope, rather than inside X-1.
4. **[34 §2](../specs/video-editor/34-interchange.md)** sketches `Unsupported`
   with a `where_` field; [196 §4.1](196-x-2-opentimelineio-interchange.md)
   proposes `location: Location`, and §8.1 here adds `Location::Scope`. One of
   the three should become the single definition before either importer is built.
5. **[36 §3.2](../specs/video-editor/36-error-model.md)** should register the
   `Interchange` codes as owned — 196 §8's five plus §8.3's
   `InterchangeEffectKeptInert` — since the family currently has none, and
   `diag.rs:140`'s "the ten error families" comment stays accurate either way.
6. **[ROADMAP.md](../specs/video-editor/ROADMAP.md) §2** X-1 row (line 198) should
   link this proposal once accepted, and §2's mini-spec table (line 80) should
   gain an X-1 row reading *"additive in v5 / none — `roxmltree` already in the
   workspace, so no 23 §3.3 evidence record"*.
7. **[28 §5.3](../specs/video-editor/28-security-model.md#53-parsers)** names MLT
   XML as "the one format here where a document can reference other documents".
   §3.6's refusal answers that; 28 should record the refusal as the accepted
   answer, so a later implementer does not read the sentence as an instruction to
   build safe recursion.
8. **`SequenceFormat` has no pixel aspect ratio** (`sequence.rs:609`), while
   `MediaProbe.video.pixel_aspect` does (`media.rs:189`). §13 Q2 is one
   consequence; anamorphic media on a square-pixel sequence is another and it
   already exists without X-1. Worth its own item.
9. **When `PathPolicy` ([28 §3.1](../specs/video-editor/28-security-model.md#31-the-rule),
   specified but unimplemented — [195 §2](195-k-c1-clip-jobs-framework.md))
   lands**, §3.9's "resolve, do not read" rule should become a call into it, and
   the sidecar-subtitle read in §3.7 should be routed through it too.
