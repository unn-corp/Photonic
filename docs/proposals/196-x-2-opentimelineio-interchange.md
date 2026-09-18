# 196 — X-2 OpenTimelineIO interchange (import and export)

> Status: **proposed mini-spec — not accepted, no code authorization.** Written to
> satisfy the [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands)
> K-Band 5 exit condition ("an accepted mini-spec exists *before* code, naming its
> data-model change, migration, undo unit, MCP surface and acceptance fixtures").
> Owner docs: [34 §4](../specs/video-editor/34-interchange.md#4-x-2--opentimelineio)
> and [26 §18](../specs/video-editor/26-kdenlive-mlt-parity.md#18-x----interop-and-format).
> Nothing here authorizes edits to product crates; [23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)'s
> agent-proof boundary still applies until X-2 is separately scheduled.

## 1. Problem and user outcome

Photonic today can only exchange a **timeline** with another application by
exporting rendered pixels. There is no structural interchange path at all: the
only interchange code in the tree is subtitle-shaped
(`crates/photonic-video/src/captions/interchange/`), and the only project format
Photonic reads or writes is its own `.photon`.

After X-2:

- A user can **open an OTIO timeline authored elsewhere** (Resolve, Premiere,
  Flame, Baselight, Kdenlive 25.04+, or any adapter output) and get real Photonic
  sequences, tracks, clips, gaps, markers, transitions, nested stacks and speed
  changes, with the media pool populated and offline clips clearly offline.
- A user can **export a Photonic sequence as `.otio`** and hand it to a colourist,
  an online editor, or a conform house.
- In **both** directions the user is told, before the operation completes and in a
  form they can read, exactly what did not survive — per item, per reason.

The last bullet is the actual product requirement. OTIO is structurally faithful
and creatively empty: no effects, no grades, no node graphs, no per-sequence
formats, no clip transforms. A lossy export that does not say it was lossy is the
failure mode this document is designed against, and it is why the "unsupported
report" is specified before the parser.

**Non-goal:** OTIO is not a Photonic project format. A user who wants a lossless
Photonic round trip already has `.photon`. §4 turns that into a hard design rule.

## 2. Current state in code

Exact, as of `feat/video-editor-module` @ `19f9fd5`. Read this section before
disagreeing with §3.

### 2.1 What exists and is directly usable

| Thing | Where | Note |
|---|---|---|
| `Tick` = `i64` flicks, `TICKS_PER_SECOND = 705_600_000` | `crates/photonic-core/src/timeline/time.rs:13,23` | PA-8 |
| `FrameRate { num: u32, den: u32 }` exact rational, `ticks_per_frame()`, `is_exact()`, `frame_at()` | `time.rs:72,108,141,119` | `is_exact()` is already the "this rate does not divide the flick" flag |
| `Timecode::parse_to_tick` / `from_frame_index`, drop-frame correct and exhaustively tested | `time.rs:169,244` and `time.rs` tests | K-A12 landed; drop-frame `;` vs `:` is honoured (the [27 SD-11](../specs/video-editor/27-spec-audit.md) defect is fixed) |
| `Sequence { frame_rate, formats, active_format, video_tracks, audio_tracks, caption_tracks, markers, groups, master_effects, master_grade, work_range, start_timecode }` | `sequence.rs:126` | `start_timecode: Tick` maps straight to OTIO `global_start_time` |
| `Track { kind, clips, enabled, locked, effects, grade, blend, opacity }` | `sequence.rs:628` | |
| `Clip { start, duration, source, source_in, speed, transform, reframe, effects, grade, composition, transition_in, transition_out, audio, enabled, color_label, markers, group, link_group, multicam }` | `clip.rs:27` | half-open `start`+`duration` (PA-7) |
| `ClipSource::{Asset, Vector, NestedSequence, SolidColor, Adjustment, Text, Unknown}` | `clip.rs:165` | `NestedSequence` is the Stack analogue |
| `Marker { at, duration, name, note, category, color, anchor }`, ranged and categorised | `sequence.rs:833` | K-A2 landed |
| `MediaAsset { source, probe, proxy, content_hash, bin, effects, grade, rating, tags }` | `media.rs:42` | `content_hash` is the relink identity |
| `AssetSource::File { path, rel_path }` | `media.rs:121` | `rel_path` first, then `path`, then hash |
| `Command::Batch(Vec<Command>)`; inverse is the reversed batch of inverses | `crates/photonic-core/src/history/mod.rs:3172` | this is the whole undo story (§6) |
| `history.execute_discrete(Command::Batch(cmds), &mut doc)` used for a multi-asset import today | `crates/photonic-mcp/src/handlers/video.rs:2622` | exact precedent for "import is one undo unit" |
| Pure `ops::` command constructors (`create_project` `ops.rs:95`, `add_asset` `:101`, `add_sequence` `:325`) | `crates/photonic-core/src/timeline/ops.rs` | GUI and MCP both call these — CAP-019 parity |
| `DiagFamily::Interchange` | `crates/photonic-core/src/diag.rs:160` | **the family exists and has zero codes**; the partition test at `crates/photonic-core/tests/diag_taxonomy.rs:113` already enumerates it, so adding codes costs nothing structurally |
| `ImportSummary` / `ExportSummary` / `InterchangeError` for captions | `crates/photonic-video/src/captions/interchange/mod.rs:23,33,44` | the reporting precedent 34 §2 points at |
| `serde_json` is already a dependency of `photonic-video` | `crates/photonic-video/Cargo.toml:17` | a Photonic-authored OTIO reader/writer needs **no new crate** |

### 2.2 What does not exist yet — say it plainly

- **No `interchange/` module in `photonic-video`.** The only `interchange` path in
  the tree is `captions/interchange/`.
- **No `InterchangeReport` type.** 34 §2 sketches one; nothing implements it.
- **No `DiagCode` in the `Interchange` family.** X-2 registers the first ones.
- **No source timecode on media.** `MediaAsset` and `MediaProbe` (`media.rs:42,153`)
  carry no start-timecode field — grep for `timecode` in `media.rs` returns nothing.
  This is the single most dangerous fact in this document; §3.6 handles it.
- **No GUI route for caption import/export.** `import_captions` / `export_captions`
  are MCP-only (`handlers/video.rs:5362,5411`); grepping `photonic-gui` and
  `photonic-app` for `parse_srt|write_srt|write_vtt|parse_ass|interchange` returns
  only an unrelated keybinding comment. X-2 must not repeat that — ROADMAP §10
  point 2 requires a GUI route or a recorded exception.
- **Clip positions are not frame-snapped.** `Sequence::validate` (`sequence.rs:378`)
  enforces only positive duration, sorted order and non-overlap; `ops.rs` never
  calls `FrameRate::snap`. Sub-frame positions are legal in the model.
- **`SequenceFormat` is `{ name, width, height }`** (`sequence.rs:609`) — no frame
  rate, no pixel aspect. Rate lives on the `Sequence`.

## 3. The mapping — where the impedance actually is

This is the design. §4–§9 follow from it.

### 3.1 Time — exact in both directions, with a stated failure mode

OTIO's time model is `RationalTime { value: f64, rate: f64 }` and
`TimeRange { start_time, duration }`, and OTIO's `TimeRange` is **half-open** —
which is why 34 §4.1 calls the fit "unusually good" and why none of X-1's
inclusive-`out` off-by-one hazard applies here. The mismatch is not the interval
convention; it is that **OTIO's `rate` is an `f64`** and Photonic's is an exact
`u32/u32`.

**Export (Photonic → OTIO).**

```
rate_f64 = num as f64 / den as f64          // 30000/1001 → 29.97002997002997
value    = tick / rate.ticks_per_frame()    // exact integer when frame-aligned
```

- Every emitted `RationalTime` uses the owning sequence's `rate_f64`.
- When `tick % ticks_per_frame != 0` (legal — see §2.2), emit
  `value = tick as f64 / ticks_per_frame as f64`. Sub-frame values are valid OTIO.
- **Always** write the exact rational and the exact tick into the Photonic
  metadata namespace (§3.7): `{"rate":{"num":30000,"den":1001},"start_ticks":…}`.
  This is what makes Photonic → OTIO → Photonic bit-exact regardless of `f64`.

**Import (OTIO → Photonic).** Rate recovery is a three-step ladder, in order:

1. `metadata.photonic.rate = {num, den}` present and `num > 0` → use it. Exact.
2. Otherwise match `rate` against the canonical table
   (24, 25, 30, 48, 50, 60, 90, 100, 120 over `/1` and over `×1000/1001`;
   `FrameRate` already names `FPS_23_976`, `FPS_29_97`, `FPS_59_94` at `time.rs:78-96`)
   with absolute tolerance `1e-6`. Exact.
3. Otherwise best rational approximation by continued fractions with denominator
   ≤ 1001, and emit **`Approximation`** + `InterchangeRateApproximated` naming the
   requested and chosen rates and the drift in ms/hour. If the resulting
   `FrameRate::is_exact()` is false, say so in the same entry — the user is being
   told their timeline will not land on tick boundaries.

Then, using the *recovered exact* rate, never the `f64`:

```
tick = round_half_away_from_zero(value * TICKS_PER_SECOND * den / num)   // i128
```

computed in `i128` when `value` is integral (the common case → exact), and in
`f64` then rounded to the nearest tick otherwise. A flick is ≈1.42 ns, so
non-integral rounding error is ≤ 0.71 ns — below one audio sample at 192 kHz by
four orders of magnitude. **Do not report it.** Reporting sub-nanosecond rounding
trains users to ignore the report, which is the one thing the report cannot afford.

**Rule: no `f32`/`f64` time enters the Photonic model.** Conversion happens in the
reader and nowhere else, the same discipline `time.rs`'s module doc states.

### 3.2 Structure

| OTIO | Photonic | Notes |
|---|---|---|
| `Timeline` | `Sequence` | one `.otio` file holds one `Timeline` |
| `Timeline.global_start_time` | `Sequence.start_timecode` | `sequence.rs:166` |
| `Timeline.tracks` (a `Stack`) | `Sequence.video_tracks` + `audio_tracks` | split by `Track.kind` |
| `Track.kind == "Video"` / `"Audio"` | `TrackKind::Video` / `Audio` | any other `kind` → `Unsupported`, track dropped whole |
| `Track` order (bottom-up in the Stack) | `video_tracks` index order | document the direction in the reader; getting it backwards silently inverts every composite |
| `Clip` | `Clip` | |
| `Gap` | absence | Photonic has no gap object; a `Gap` advances the next clip's `start` |
| `Stack` nested in a `Track` | `ClipSource::NestedSequence` + a new `Sequence` | recurse; `ops::insert_clip` (`ops.rs:508`) already cycle-checks nesting |
| `Transition` | `Clip.transition_in` | §3.4 |
| `Marker` | `Marker` | §3.3 |
| `LinearTimeWarp` / `FreezeFrame` | `SpeedMap` | §3.5 |
| `ExternalReference` / `MissingReference` / `GeneratorReference` | `MediaAsset` / offline / `ClipSource::SolidColor` | §3.6 |
| `ImageSequenceReference` | — | no Photonic analogue → `Unsupported`, clip becomes a Gap |
| any unrecognised `OTIO_SCHEMA` in a track | — | `Unsupported`, becomes a Gap of its `source_range.duration` |

**Multiple timelines.** A `.otio` file is one `Timeline`; a `.otiod`/`.otioz`
bundle is out of scope for v1 (§12). Import creates one new `Sequence` (plus one
per nested `Stack`) and never merges into an existing one — merging would need a
conflict model nobody has specified.

**Import always creates, never mutates.** Assets are matched against the existing
pool by `content_hash` first, then absolute path, then filename — the same ladder
`media.rs`'s module doc already describes for relink — and only created when no
match is found. No existing sequence, track or clip is touched.

### 3.3 Markers

OTIO `Marker { marked_range: TimeRange, name, color, metadata }`, where `color` is
a string from a small named set (`RED`, `GREEN`, `BLUE`, `CYAN`, `MAGENTA`,
`YELLOW`, `ORANGE`, `PINK`, `PURPLE`, `BLACK`, `WHITE`) with free strings tolerated.

- `marked_range.start_time` → `Marker.at`; `marked_range.duration` → `Marker.duration`.
  Ranged markers survive both ways — this is K-A2's payoff.
- Markers on the `Timeline`'s stack → `Sequence.markers`, `anchor = Timecode`.
  Markers on a `Clip` → `Clip.markers`, `anchor = Content`, and **`at` is relative
  to the clip start**, because `Clip::marker_sequence_tick` is `self.start + m.at`
  (`clip.rs:128`). OTIO clip markers are stated in the clip's trimmed source time,
  so the reader must subtract `source_range.start_time` — not `available_range`'s.
  Getting this wrong displaces every clip marker by the clip's source-in.
- **Colour, export:** effective colour = `marker.color` ?? its category's colour
  (`MarkerCategory.color`, `sequence.rs:735`) ?? the neutral default; snap to the
  nearest named OTIO colour by Euclidean distance in sRGB (Photonic `Color` is
  gamma-encoded sRGB — `crates/photonic-core/src/color.rs:10`), and write the exact
  `Color` plus `category` id and name into metadata.
- **Colour, import:** metadata first (restore `category` only if that
  `MarkerCategoryId` already exists in the target project); otherwise the named
  colour becomes `Marker.color` and `category` stays `None`. **Import never invents
  `MarkerCategory` rows.** Five new categories per imported file would pollute the
  project's category list, which is a user-curated surface (`default_seed`,
  `sequence.rs:756`).
- `MarkerAnchor::Unknown` (`sequence.rs:820`) exports as `Timecode`, matching how
  every consumer already treats it.

### 3.4 Transitions — the geometry is asymmetric, and that is a real approximation

Photonic's transition semantics, read out of the compiler rather than guessed:
`active_transition` (`crates/photonic-video/src/graph/compile.rs:744`) fires only
for a **`transition_in`** on the incoming clip, over the window
`[clip.start, clip.start + duration)` — **entirely after the cut** — borrowing the
outgoing clip past its own end from its remaining source handle.
`transition_out` is a fade to transparent into a gap or the sequence end and is
invalid at a cut (`Sequence::validate_transitions`, `sequence.rs:414`).

OTIO's `Transition` sits between two items and carries `in_offset` (extent before
the cut) and `out_offset` (extent after), and consumes no track time.

- **Export:** a `transition_in` of duration `D` on clip B →
  `Transition { transition_type: "SMPTE_Dissolve", in_offset: 0, out_offset: D }`
  inserted before B. Write the **authored** `D`, not the handle-clamped effective
  duration — the model stores the authored value and clamping is a render-time
  decision. Kind and `TransitionParams` (curve, colour, direction, softness) go
  into metadata; every kind other than `CrossDissolve` also produces an
  `Approximation` entry, because `SMPTE_Dissolve` is what the receiving tool will
  render. A `transition_out` (a fade to transparent) has no OTIO analogue at all →
  `Unsupported`.
- **Import:** `Transition` → the following clip's `transition_in` with
  `duration = in_offset + out_offset`, preserving the cut position. When
  `in_offset != 0` this shifts the transition's centre later by `in_offset`, so emit
  an `Approximation` naming the shift in frames. Moving the cut instead would be
  worse: it would re-time every clip after it. `transition_type` other than
  `SMPTE_Dissolve` → `CrossDissolve` + `Approximation`; the original string goes in
  metadata. **Never** synthesise a `TransitionKind::Unknown` (see §4.2).
- A `Transition` at the head of a track, or between two `Gap`s, is dropped with an
  `Unsupported` entry — Photonic cannot anchor it.

### 3.5 Speed

- `SpeedMap::Constant(Ratio { num, den })` ↔ `LinearTimeWarp { time_scalar }`,
  with the exact `Ratio` in metadata so the round trip stays rational.
  `time_scalar == 0.0` ↔ `FreezeFrame`.
- `SpeedMap::Keyframed { keys }` (`clip.rs:375`) has **no OTIO representation**.
  Export writes a `LinearTimeWarp` whose `time_scalar` is the clip's overall
  average ratio (`speed.source_delta(duration) / duration`) and emits an
  `Approximation` naming the key count. Rationale: dropping the warp entirely would
  make the clip's source-side length visibly wrong in the receiving tool, and the
  average at least conforms. The exact key list goes into metadata, so a Photonic
  round trip is lossless.
- Any other OTIO `Effect` subclass on a clip → `Unsupported`. It is not preserved
  as an inert `ClipEffect`; see §4.2.

### 3.6 Media references — and the source-timecode trap

**This is X-2's off-by-one equivalent: the highest-probability defect, invisible
in casual testing, and it corrupts every clip in the project.**

`MediaAsset`/`MediaProbe` carry **no source timecode** (§2.2). OTIO's
`available_range.start_time` normally *is* the media's source timecode, and
real-world exports from Resolve and Avid routinely carry an hour-1 start
(`01:00:00:00`). A reader that sets `source_in = source_range.start_time` will
give every clip a `source_in` of one hour and read a hundred hours past the end
of a ten-second file.

**Rule:** `source_in = source_range.start_time − available_range.start_time`,
clamped at zero, and the discarded `available_range.start_time` is written into
the asset's Photonic metadata so export can restore it. When `available_range` is
absent, treat `start_time` as zero and emit an `Approximation` naming the asset —
that is a genuine guess.

Export writes `available_range = { start_time: 0 @ rate, duration: probe.duration }`
when a probe exists, and omits `available_range` when it does not.

| OTIO | Photonic | Handling |
|---|---|---|
| `ExternalReference.target_url` `file://` absolute | `AssetSource::File { path }` | percent-decode; on Windows handle the `file:///C:/…` form |
| relative `target_url` | `AssetSource::File { rel_path }` + absolute `path` resolved against the `.otio` file's directory | export writes `rel_path` as the relative URL when set |
| non-`file` scheme | offline asset, path preserved verbatim | `InterchangeMediaUnresolved`, `Warning` |
| `MissingReference` | asset with an unresolvable path | offline is a first-class state (`media.rs` module doc) — not an error |
| `GeneratorReference` kind `SolidColor` | `ClipSource::SolidColor` | parse the colour from `parameters`; unparseable → black + `Approximation` |
| other `GeneratorReference` kinds | Gap + metadata | `Unsupported` |
| — | `content_hash` | export writes it into metadata; import uses it as the first relink key |

Asset kind is inferred from the extension via the existing `guess_asset_kind`
path (`handlers/video.rs`, used by `import_media`); an unrecognised extension →
`Unsupported`, clip becomes a Gap. **Import never probes** — probing is
`SetAssetMeta`'s job (`commands.rs:426`) and belongs to the L1/L2 import ladder in
[24](../specs/video-editor/24-preview-media-load.md), not to the parser.

### 3.7 The Photonic metadata namespace

All Photonic-private data lives under one key inside OTIO `metadata` dicts:

```json
"metadata": {
  "photonic": {
    "v": 1,
    "clip_id": "4f1c…",
    "start_ticks": 705600000,
    "duration_ticks": 1411200000,
    "source_in_ticks": 0,
    "rate": { "num": 30000, "den": 1001 },
    "dropped": ["effects:2", "grade", "transform_keyframes:5"]
  }
}
```

**Hard rule, and the most contested decision in this document: the namespace
carries identity and time-exactness only, never appearance.** Ids, exact ticks,
exact rationals, `content_hash`, the discarded `available_range.start_time`, the
original transition kind/params, the original speed keys — yes. Grades, effect
stacks, node graphs, reframe transforms, per-sequence formats, audio parameters —
**no**. They are dropped and reported.

Justification, because the alternative is tempting:

1. An `.otio` that secretly carries a complete private copy of the project is a
   `.photon` with extra steps and a second, independently-versioned serialization
   of the whole timeline model to keep in sync forever.
2. It makes "OTIO is structural" a lie that no third-party tool can see through —
   the file round-trips perfectly in Photonic and loses everything anywhere else,
   which is precisely the surprise §1 exists to prevent.
3. `dropped` is a machine-readable list *inside the file*, so a third-party tool,
   a diff, or a future Photonic build can see what was omitted without the
   transient report. The lossiness is self-documenting.

`photonic.v` is versioned independently of `format_version`. A reader seeing
`v > 1` **ignores the whole namespace** and falls back to the OTIO-native fields,
emitting one `Info`. That is the compat-window analogue and it fails safe: worst
case the import is merely less exact, never wrong.

### 3.8 The lossiness register — what export drops

Every row produces an `Unsupported` entry naming what, where, and what the user
will see instead. This table *is* the acceptance criterion for §9 test 10.

| Photonic | Where | Why there is no OTIO form |
|---|---|---|
| `Clip.effects`, `Track.effects`, `Sequence.master_effects`, `MediaAsset.effects` (all four `VfxOwner` scopes, `commands.rs:150`) | clip/track/master/asset | OTIO carries no effects |
| `Clip.grade`, `Track.grade`, `Sequence.master_grade`, `MediaAsset.grade` | same four scopes | ditto |
| `Clip.composition: Option<GraphId>` node graph | clip | ditto |
| `Clip.transform: AnimProps<ClipTransform>` | clip | OTIO has no transform, animated or static. **A scaled/repositioned clip exports as if untouched** — call this out in the UI, it is the most visually surprising loss |
| `Clip.reframe: HashMap<usize, ClipTransform>` | clip | per-format transforms; PA-6 has no analogue |
| `Sequence.formats` beyond `active_format` (`sequence.rs:131`) | sequence | OTIO has no resolution field at all; even the active format goes to metadata only |
| `Sequence.caption_tracks` | sequence | no subtitle schema; the report names `export_captions` as the route |
| `Clip.audio`, `Track.audio`, `Sequence.audio_master` | all | no mixing model |
| `Track.blend`, `Track.opacity` | track | no compositing model |
| `Clip.group` / `Sequence.groups` / `Clip.link_group` | clip | no grouping; restorable from metadata on a Photonic round trip |
| `MulticamGroup` inactive angles (`clip.rs:276`) | clip | only the active angle is expressed |
| `Clip.transition_out` | clip | §3.4 |
| `Sequence.work_range`, `MediaAsset.rating`/`tags`, `MediaBin` hierarchy | project | no analogue |
| `ClipSource::{Adjustment, Text, Vector, Unknown}` | clip | become Gaps carrying `photonic.was` so a Photonic round trip restores them |

## 4. Data-model change

### 4.1 None

No new field, no new variant, no new type in `photonic-core`. Import produces
ordinary `MediaAsset`, `Sequence`, `Track`, `Clip`, `Marker`, `Transition` and
`SpeedMap` values that the v5 model already expresses; export reads them. Every
OTIO-specific concept lives either in the new `photonic-video` interchange module
or in the exported file's metadata.

New types, all in `crates/photonic-video/src/interchange/` (new module, modelled
on `captions/interchange/`), none persisted in `Document`:

```rust
pub struct InterchangeReport {
    pub imported: ImportCounts,
    pub unsupported: Vec<Unsupported>,
    pub approximated: Vec<Approximation>,
    pub errors: Vec<InterchangeError>,
}
pub struct Unsupported   { pub what: String, pub location: Location, pub consequence: String }
pub struct Approximation { pub what: String, pub location: Location, pub detail: String }
pub enum   Location      { Timeline, Track { index: usize, name: String },
                           Item { track: usize, index: usize, name: String },
                           Asset { url: String } }
```

`InterchangeReport` is shared by X-1 and X-3 (34 §2 requires exactly this) and so
belongs in `interchange/mod.rs`, not in the `otio` submodule.

One small **non-model** addition: register the first `DiagFamily::Interchange`
codes in `crates/photonic-core/src/diag.rs` (§8). `DiagCode` is a wire vocabulary,
not persisted document state, so this is not a format change.

### 4.2 Why import must *not* use the 39 §2.2 unknown-preserving variants

The house pattern (`ClipSource::Unknown`, `EffectKind::Unknown(UnknownTag)`,
`TransitionKind::Unknown`, `crates/photonic-core/src/timeline/unknown.rs`) exists
so that a document written by a **newer Photonic build** round-trips through an
older one losslessly. Its correctness argument is that the preserved tag is in
*Photonic's own namespace* and some Photonic build does understand it.

An unrecognised `OTIO_SCHEMA` string, or a foreign `Effect` subclass, is in a
**different vocabulary**. Storing `"SchemaDef.1"` in a `ClipSource::Unknown` map,
or an OTIO effect name in an `EffectKind::Unknown(UnknownTag)`, would:

- re-emit foreign identifiers into `.photon` as though they were Photonic source
  and effect kinds;
- collide the moment Photonic ships a real effect with a colliding name — and
  `UnknownTag` interns into a process-wide leaked table (`unknown.rs:61`) shared
  with genuine forward-compat tags;
- silently defeat 39 §2.2's diagnostic, which tells the user "a newer build wrote
  this" when in fact an unrelated application did.

**Decision: unknown OTIO constructs become Gaps (preserving downstream timing) or
are dropped, and are always reported.** The forward-compat machinery is reserved
for Photonic's own namespace, and X-1's inert-preservation rule
([34 §3.4](../specs/video-editor/34-interchange.md#34-effects)) does not
generalise here — X-1 preserves MLT effects inert because an MLT filter and a
Photonic effect are the same *kind of thing* with a service-name mapping table
behind them, whereas OTIO effects are declaredly non-portable and have no
catalogue to map against.

## 5. Migration

**`CURRENT_FORMAT_VERSION` stays 5** (`crates/photonic-core/src/document.rs:117`).
X-2 needs no v6 and lands additively inside v5.

Reasoning, point by point:

- The migration chain (`crates/photonic-core/src/migration.rs:58`) upgrades
  documents when the **persisted model** grows. X-2 does not grow it (§4.1), so
  there is no v5→v6 step to write and nothing for `run_migrations` to do.
- A document produced by an OTIO import is indistinguishable from one produced by
  hand: same `Sequence`, `Track`, `Clip` shapes, same serde. It saves at v5 and
  opens in any build that reads v5.
- The precedent cuts the other way too: `V1ToV2` and `V2ToV3`
  (`migration.rs:70,87`) exist only to stamp a version number for purely additive
  changes. Adding a v6 that stamps a number for a change that touches no field
  would add a compat-window step for nothing and shrink the effective window
  (`COMPAT_WINDOW = 1`, `migration.rs:16`) for every user.
- ROADMAP §10 point 5 ("additive serde/migration round-trip passes when model
  changes") is satisfied because the model does not change. The round trip that
  actually needs a test is OTIO ↔ Photonic (§9), not v5 ↔ v6.

**The versioned surface X-2 does introduce** is the `photonic` metadata namespace
(`photonic.v`, starting at 1), versioned independently, with the fail-safe read
policy in §3.7. It is a file-format surface of the `.otio` writer, not of
`.photon`, and it must never be conflated with `format_version`.

**If a later reviewer disagrees** and wants foreign-metadata retention in the
core model (§3.7's rejected alternative), that *would* need v6 — an `Option<Value>`
sidecar on `Clip`/`Track`/`Sequence` is a persisted field. That is the strongest
practical argument for §3.7's rule, and it should be decided here rather than
discovered mid-implementation.

## 6. Undo unit

### 6.1 Import: one verb, one `Command::Batch`

The user verb is **"Import OTIO…"** and it produces exactly one undo entry, via
the shape `import_media` already uses (`handlers/video.rs:2622`):

```rust
history.execute_discrete(Command::Batch(cmds), &mut doc);
```

`cmds`, in order:

1. `Command::Timeline(ops::create_project())` — only when `doc.timeline.is_none()`.
2. `Command::Timeline(ops::create_bin(file_stem, None))` — one bin named after the
   imported file, so an import is undoable *and* visibly grouped.
3. `Command::Timeline(ops::add_asset(a))` × N, each `a.bin` set at construction
   (same trick `import_media` uses to dodge the AssignAssetBin ordering problem).
4. `Command::Timeline(ops::add_sequence(s))` × M — innermost nested `Stack` first,
   so a `NestedSequence` clip never references a sequence that is not yet present.
   **Each `AddSequence` carries its fully-built `Sequence` with tracks and clips
   inline** (`commands.rs:451`), so there are no per-clip commands: a 900-clip
   import is 1 + 1 + N + M commands, not 900.
5. `Command::Timeline(ops::set_active_sequence(p, Some(top_level)))`.

**Exact inverse**, and it is mechanical rather than hand-written: `Command::Batch`
inverts as "the reversed batch of inverses" (`history/mod.rs:3172-3176`), so the
inverse is `SetActiveSequence{old}` → `RemoveSequence` × M in reverse creation
order → `RemoveAsset` × N → `RemoveBin` → `RemoveProject`. Every member already
has a tested inverse (`commands.rs:2206,2211`). Redo re-applies the forward batch.

**Validate-then-commit** (39 §1.1): the whole file is parsed, every `Sequence` is
run through `Sequence::validate` (`sequence.rs:378`) and every clip through the
`ops::insert_clip` preconditions **before** the first command is constructed. A
parse or validation failure yields `Err(InterchangeError)` and mutates nothing —
a partially imported timeline is not an acceptable outcome and is not reachable.

`mem_estimate` (39 §1.3) must be honest: this batch carries whole `Sequence`
values and is legitimately large. It is bounded by the history byte budget like
`CaptionCmd::BulkInsertCues`, and the retention floor guarantees it cannot empty
the history by itself.

### 6.2 Export: no undo entry

Export mutates no document state and therefore records nothing (39 §1.6). If a
future revision adds "remember the last export path", that is view state and
belongs in the sidecar 39 §1.6 already specifies, not in `Document`.

## 7. MCP surface and GUI parity

Both directions get an MCP tool and a GUI route. CAP-019 parity is not optional
here, and PA-11 records that the MCP trail is already the weak side — X-2 must not
add to that debt, nor repeat the caption gap (§2.2).

### 7.1 `import_otio`

| Arg | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | — | `.otio` file to read |
| `bin` | string? | file stem | media-pool bin for created assets |
| `relink_by_hash` | bool | `true` | reuse an existing asset when `content_hash` matches |
| `activate` | bool | `true` | make the imported top-level sequence active |
| `dry_run` | bool | `false` | parse and report; execute no command |

Returns the report as structured data:

```json
{ "sequences_created": 2, "tracks": 5, "clips": 143, "markers": 12,
  "assets_created": 9, "assets_reused": 3,
  "unsupported": [{ "what": "…", "where": "…", "consequence": "…" }],
  "approximated": [ … ] }
```

`dry_run` matters more than it looks: it lets an agent (and the GUI's pre-import
sheet) show the loss report **before** the user commits, which is §1's requirement.

### 7.2 `export_otio`

| Arg | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | — | destination `.otio` |
| `sequence_id` | uuid? | active sequence | which sequence |
| `include_metadata` | bool | `true` | write the `photonic` namespace; `false` yields a clean-room file for third-party tools |
| `flatten_nested` | bool | `false` | inline nested sequences instead of writing `Stack`s, for consumers with poor nesting support |

Returns `{"path":…, "clips_written":…, "unsupported":[…], "approximated":[…]}`.
It **succeeds with warnings**; a lossy export is not an error, but it is never
silent — `is_error` stays false and `unsupported` is non-empty.

Both tools go through `crates/photonic-mcp/src/dispatch.rs`, the tool-name list at
`handlers/video.rs:8330+`, and `schema_gen.rs`; `docs/mcp-api.md` is regenerated
and the CI drift gate (`.github/workflows/ci.yml:162-167`) enforces it.

### 7.3 GUI route

`FILE_OPTIONS` is `&["Document", "Save", "Export"]`
(`crates/photonic-gui/src/app/mod.rs:290`), rendered by the File drawer
(`crates/photonic-gui/src/app/menu_drawer.rs:31`). Add a fourth column,
**"Interchange"**, holding "Import OTIO…" and "Export OTIO…", each an `rfd`
picker filtered to `otio` — the same `run_file_dialog` pattern the Open/Save-As
buttons already use (`menu_drawer.rs:54,138`).

Both commands open a **modal report sheet** before committing (import) or after
writing (export), listing unsupported and approximated entries grouped by reason
with counts, and a "Copy report" button. Import's sheet is driven by the same
`dry_run` pass the MCP tool exposes, so GUI and MCP show byte-identical text.

## 8. The report and diagnostics

Two surfaces, one source of truth (`InterchangeReport`):

- **The sheet** (GUI) and the tool result (MCP) — full detail, per item.
- **The diagnostic log** — one coalesced entry per code per subject, per the
  existing `DiagnosticLog` behaviour (`diag.rs`), so a 400-clip file with 400
  dropped effects fires one toast, not 400.

New `DiagCode` variants, the first members of the already-declared
`DiagFamily::Interchange` (`diag.rs:160`):

| Code | Default severity | Consequence line |
|---|---|---|
| `InterchangeParseFailed` | `Error` | "The file could not be read; nothing was imported." |
| `InterchangeUnsupportedConstruct` | `Warning` | "Part of the file has no Photonic equivalent and was left out." |
| `InterchangeRateApproximated` | `Warning` | "The frame rate was rounded; timing may drift." |
| `InterchangeMediaUnresolved` | `Warning` | "Media could not be located; the clip is offline." |
| `InterchangeLossyExport` | `Warning` | "The exported file does not carry every part of this sequence." |

Adding these requires updating `DiagCode::family()`, `default_severity()` and
`consequence()` in lockstep (the macro at `diag.rs:170` generates `ALL`,
`as_str` and `FromStr`, so those three stay consistent for free) plus the
catalogue tests `crates/photonic-core/tests/diag_catalogue.rs` and
`diag_taxonomy.rs`. `families_partition_all_codes`
(`diag_taxonomy.rs:102`) already enumerates `Interchange`, so it keeps passing.

**Rule, restated from 34 §2 because it is the whole point: never drop silently.**
Every unmapped construct produces exactly one entry naming what it was, where it
was, and what the user will see instead.

## 9. Acceptance fixtures and tests

### 9.1 Fixtures — Photonic-authored, and X-2 is *not* a gated item

All fixtures are **hand-written JSON**, authored in this repo against the published
OTIO schema, committed under a new
`crates/photonic-video/tests/fixtures/otio/` with a `README.md` recording
provenance per [23 §12](../specs/video-editor/23-legal-open-source-implementation-routes.md#12-cross-cutting-provenance-manifests),
following the pattern of the existing corpus README
(`crates/photonic-video/tests/fixtures/README.md`). **No file is copied or adapted
from OpenTimelineIO's, Kdenlive's, or any other project's test suite** — 26 §7 and
[23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol)
item 4 say exactly this for X-1 and the same reasoning applies verbatim.

Media references point either at the existing Photonic-generated corpus
(`color_bars.mp4`, `counter.mp4`, `beep_flash.mp4`) or at paths that deliberately
do not exist. **No third-party or rights-encumbered content is required, so X-2 is
not `legal-or-fixture-blocked`** — unlike G-20 / K-D1. Recording that explicitly
matters: it is the difference between a schedulable item and a blocked one.

Total added fixture weight is text JSON, on the order of 30 KB — negligible
against [11 §1.5](../specs/video-editor/11-testing-phasing.md)'s corpus budget.

| Fixture | Exercises |
|---|---|
| `roundtrip_basic.otio` | 2 video + 1 audio track, 5 clips, 2 gaps, 25 fps |
| `ntsc_rates.otio` (×3) | 24000/1001, 30000/1001, 60000/1001 |
| `source_tc_hour1.otio` | `available_range.start_time = 01:00:00:00` — §3.6's trap |
| `markers.otio` | point + ranged, sequence- and clip-scoped, every named colour |
| `transitions.otio` | `in_offset = 0`, and `in_offset > 0` (the shift case) |
| `nested_stack.otio` | a `Stack` inside a `Track`, two levels deep |
| `timewarp.otio` | `LinearTimeWarp` at 0.5×/2×, and `FreezeFrame` |
| `unsupported.otio` | one unknown `OTIO_SCHEMA` item + one non-mappable `Effect` |
| `missing_media.otio` | `MissingReference` and a non-`file` scheme URL |
| `sub_frame.otio` | a `source_range` not on a frame boundary |
| `exotic_rate.otio` | `rate = 47.952` — forces the approximation ladder's step 3 |

### 9.2 Tests

Numbered rows are the ones 34 §6 already owns; the rest are this document's.

| # | Test | Owner |
|---|---|---|
| 7 | **Round trip** — Photonic → `.otio` → Photonic reproduces structure, timing and markers exactly; effects are absent and reported | 34 §6 |
| 8 | **Rational time survives** at 23.976, 29.97 and 59.94 with **zero** tick drift over a 1-hour sequence | 34 §6 |
| 9 | **Import is one undo unit** — one `execute_discrete`; one undo restores the document to a byte-identical `to_json` | 34 §6 |
| 10 | **Report completeness** — `unsupported.otio` produces exactly two entries, one naming each construct | 34 §6 |
| 11 | **Fixture provenance** recorded in the fixtures README | 34 §6 |
| A | **Source-TC rebase** — `source_tc_hour1.otio` yields `source_in == 0`, not one hour | §3.6 |
| B | **Half-open boundary** — a clip abutting the next has `end() == next.start`; a 1-frame clip survives both directions | PA-7 |
| C | **Gap arithmetic** — removing a `Gap` shifts the following clip start by exactly the gap duration |§3.2 |
| D | **Track order** — a two-video-track file composites in the same order after round trip | §3.2 |
| E | **Transition asymmetry** — `in_offset > 0` produces one `Approximation` naming the frame shift; `in_offset == 0` produces none | §3.4 |
| F | **Rate ladder** — metadata beats the table beats the approximation; `exotic_rate.otio` emits exactly one `InterchangeRateApproximated` | §3.1 |
| G | **Clip-marker frame** — a clip marker at source-time *t* on a clip with `source_in > 0` lands at the right sequence tick | §3.3 |
| H | **No forward-compat leakage** — after importing `unsupported.otio`, no `ClipSource::Unknown`, `EffectKind::Unknown` or `TransitionKind::Unknown` exists in the document | §4.2 |
| I | **Metadata is optional** — `include_metadata: false` still round-trips structure and timing (via the rate table), losing only exactness | §3.7 |
| J | **`photonic.v = 2` is ignored**, import falls back to OTIO-native fields and emits one `Info` | §3.7 |
| K | **Validate-then-commit** — a file with an overlapping clip fails with no document mutation and no history entry | §6.1 |
| L | **GUI/MCP parity** — the GUI sheet text and the MCP `unsupported` array come from one `InterchangeReport` and agree | ROADMAP §10.10 |
| M | **Format version unchanged** — a document containing an OTIO-imported sequence saves at `format_version == 5` and reloads unchanged | §5 |

Test A deserves the same emphasis 34 §6 gives its test 1: it is invisible in
casual testing (a file whose media happens to start at zero passes either way),
and when it is wrong it is wrong for every clip in the project.

## 10. What X-3 reuses

X-2 gates X-3, so these are commitments, not conveniences:

- **`InterchangeReport` / `Unsupported` / `Approximation` / `Location`** and the
  never-drop-silently rule — shared verbatim; they live in `interchange/mod.rs`,
  not in `interchange/otio/`.
- **The `Tick` ↔ rational-time conversion and the rate-recovery ladder** (§3.1).
  EDL's timecode side already has `Timecode::parse_to_tick` (`time.rs:244`); the
  rate ladder is what turns an EDL's declared FPS into a `FrameRate`.
- **The source-timecode rebase rule** (§3.6). EDL is *entirely* source-timecode
  based, which is why 34 §5 blocks it on K-A12. X-2 establishes the rebase
  convention and the metadata slot; when K-A12 gives `MediaAsset` a real source-TC
  field, X-3 reads it and X-2's metadata fallback becomes a compatibility shim.
- **The one-batch-one-undo import commit shape** (§6.1) and the `ops::`
  constructor discipline.
- **The `Interchange` diag codes** (§8) — X-3 adds no new ones.
- **The GUI "Interchange" File-drawer column and the report sheet** — EDL and
  FCPXML become entries in the same column, not new surfaces.
- **The fixture provenance discipline** (§9.1).

**AAF and FCPXML.** 34 §4.3 and 26 §18 both say these are reachable "through OTIO
adapters". Be precise about what that means here: the OTIO adapter ecosystem is
Python, and bundling it would be a dependency intake requiring a
[23 §3.3](../specs/video-editor/23-legal-open-source-implementation-routes.md#33-required-evidence-record)
evidence record. **The recommendation is that X-3's AAF/FCPXML route is
user-side conversion** — the user runs the adapter, Photonic reads the `.otio` —
and that Photonic ships no adapter runtime. That keeps the promise 34 §4.3 makes
("converts two large formats into a configuration problem") honest.

## 11. Definition of done (ROADMAP §10), made answerable

| # | ROADMAP §10 requirement | How X-2 answers it |
|---|---|---|
| 1 | Core op/engine service with unit tests | `photonic-video/src/interchange/{mod,otio/{read,write,time,report}}.rs`; tests §9.2 |
| 2 | GUI route, or a recorded exception | File drawer → **Interchange** column (§7.3). No exception is sought |
| 3 | MCP tool/schema/generated docs | `import_otio`, `export_otio`; `docs/mcp-api.md` regenerated, CI drift gate green |
| 4 | One user verb, one undo unit; undo/redo identity | §6; test 9 and test K |
| 5 | Additive serde/migration round-trip when the model changes | The model does not change (§4.1/§5). Test M pins `format_version == 5` |
| 6 | Pixel/audio IR/eval/golden/sync coverage | **N/A — X-2 touches no pixel or audio path.** No new goldens; state this rather than inventing coverage |
| 7 | Hard gates green; trend metrics not regressed | Parsing is off the hot path; no budget interaction. Add a bound: a 1000-clip file imports in < 1 s on the CI runner, asserted as a hard gate because it is deterministic |
| 8 | Offline, privacy, licensing, content, product gates | Offline: parsing is local, no network, no telemetry. Licensing: §13. Content: §9.1 — **not gated** |
| 9 | Protected surfaces not regressed | PA-7 (half-open), PA-8 (flicks + exact rational) and PA-9 (typed model) are exactly what §3.1/§4.2 defend; tests B, F and H are their regression anchors |
| 10 | Goal-backward L1–L4, including GUI/MCP parity | L1 module exists → L2 real parser → L3 wired into the File drawer and dispatch → L4 a real `.otio` from a real tool imports and re-exports; test L pins parity |

## 12. Risks, open questions, and what is out of scope

### 12.1 Risks

1. **Source-timecode rebase (§3.6).** Highest probability, highest blast radius,
   lowest visibility. Test A is mandatory and must use a fixture whose media
   *does not* start at zero.
2. **Track ordering direction (§3.2).** Reversing the stack order silently
   inverts every composite. Test D exists solely for this.
3. **Metadata scope creep.** Every future feature will want "just one more field"
   in the `photonic` namespace. §3.7's identity-only rule is the defence and
   should be enforced in review, not by hope.
4. **Report fatigue.** A 500-clip project with a grade on every clip produces 500
   `Unsupported` entries. The sheet must group by reason with counts
   ("500 clips: grade dropped") and only enumerate on expansion, or users will
   stop reading it — which defeats §1.
5. **`f64` rate mismatch on re-import from a third-party tool.** A tool that
   rewrites `rate` as `29.97` rather than `29.97002997…` falls to ladder step 2,
   which absorbs it within the `1e-6` tolerance. A tool writing `29.970030` also
   passes. A tool writing `29.9` does not, and correctly gets an approximation
   warning.

### 12.2 Open questions needing a product call

- **Q1 — Import destination.** This spec always creates new sequences. Users
  eventually want "import this OTIO *into* the current sequence at the playhead".
  *Recommendation: ship create-only.* Merge needs a conflict model (track matching,
  id collision, marker merge) that is a mini-spec of its own, and create-only is
  strictly forward-compatible with adding merge later.
- **Q2 — Where the exported resolution goes.** OTIO has no resolution field, and
  adapters disagree about which metadata key to use. *Recommendation: write it
  only under `photonic`, and do not guess another tool's key.* Writing a key we
  cannot verify is inventing an interop claim.
- **Q3 — Should export refuse when loss exceeds a threshold?** *Recommendation:
  no.* Export always succeeds with a report; a user exporting for conform does not
  care that grades were dropped, and a tool that refuses to do the thing asked is
  worse than one that explains what it did.

### 12.3 Deliberately out of scope for v1

- `.otioz` / `.otiod` bundles (zip and directory forms). Import of `.otio` only.
- Writing or reading `SchemaDef` / schema-downgrade features.
- OTIO `Timeline` collections / multiple timelines per file.
- Caption tracks in either direction — `import_captions` / `export_captions`
  already own subtitles, and the report says so.
- Any OTIO adapter runtime (§10).
- Round-tripping Photonic effects, grades or node graphs through metadata (§3.7).

## 13. Clean-room provenance

Required by [26 §7](../specs/video-editor/26-kdenlive-mlt-parity.md#7-how-to-read-the-item-tables); this item's
provenance risk is specific (a file format plus a possible dependency), so the
note is explicit rather than inherited.

- **Design source:** the **published OpenTimelineIO schema documentation** — the
  `OTIO_SCHEMA` `"Name.Version"` convention, the JSON object shapes for
  `Timeline`, `Stack`, `Track`, `Clip`, `Gap`, `Transition`, `Marker`,
  `ExternalReference`, `MissingReference`, `GeneratorReference`,
  `ImageSequenceReference`, `LinearTimeWarp`, `FreezeFrame`, `RationalTime` and
  `TimeRange` — together with the arithmetic facts of rational time. Formats are
  facts and interfaces, not expression (34 §1).
- **Not derived from:** the OpenTimelineIO reference implementation's C++ or
  Python source, nor its adapters, nor **Kdenlive's or MLT's OTIO code**, which
  are GPL/LGPL and `REJECT` under 26 §2. OTIO is Apache-2.0 and therefore not an
  *excluded* source in the way MLT is, but the reader and writer are still
  designed from the schema description rather than transcribed from an
  implementation, so that no expression is carried across and the
  [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol)
  attestation is available at merge without qualification.
- **No dependency is introduced.** The reader and writer are Photonic-authored on
  `serde_json`, which `photonic-video` already depends on
  (`crates/photonic-video/Cargo.toml:17`). This is the route ROADMAP §2 states as
  preferred ("Photonic-authored JSON reader/writer preferred over a dependency")
  and 34 §4.2 states as first choice. **Therefore no
  [23 §3.3](../specs/video-editor/23-legal-open-source-implementation-routes.md#33-required-evidence-record)
  evidence record is required for X-2 as specified here.** If a future implementer
  proposes an OTIO crate or C++ binding instead, 26 §2 item 4 and 34 §4.2 require
  that evidence record — transitive licences, build scripts, maintenance owner —
  to be produced and accepted **before** any intake, not alongside it.
- **Fixtures** are Photonic-authored (§9.1); none is copied or adapted from any
  other project's test suite.
- **Naming discipline:** describe the capability as "reads and writes
  OpenTimelineIO files", never as certification, endorsement, or an official
  relationship with the Academy Software Foundation.

## 14. Follow-ups

Recorded here rather than edited into the owning documents, per this proposal's
one-file scope.

1. **[34](../specs/video-editor/34-interchange.md) §4** should absorb §3.6's
   source-timecode rebase rule and mark it, alongside X-1's inclusive-`out`
   off-by-one, as the second named highest-probability interchange defect. 34 §6's
   acceptance table should gain test A.
2. **[34](../specs/video-editor/34-interchange.md) §2** currently sketches
   `InterchangeReport` with a `where_` field; §4.1 here proposes `location:
   Location` (a typed enum rather than a stringly-typed field, per PA-9). One of
   the two should be updated so the type has a single definition.
3. **[36](../specs/video-editor/36-error-model.md) §3.2** should register the five
   `Interchange` diag codes from §8 as owned, since the family currently has none.
4. **[ROADMAP.md](../specs/video-editor/ROADMAP.md) §2** X-2 row should link this
   proposal once accepted, and §10 point 6 would read better with an explicit
   "N/A for items that touch no pixel or audio path" clause — X-2 is the first
   item to need it.
5. **[26 §18 X-3](../specs/video-editor/26-kdenlive-mlt-parity.md#x-3--edl-aaf-fcpxml)**
   should record §10's recommendation that AAF/FCPXML conversion is user-side and
   Photonic ships no adapter runtime, so the "via OTIO adapters" phrasing is not
   read as a bundling commitment.
6. **Caption interchange has no GUI route** (§2.2) — an independent gap, not
   X-2's to fix, but the "Interchange" File-drawer column §7.3 introduces is the
   natural home for "Import/Export Subtitles…" and should be considered when that
   gap is scheduled.
7. When **K-A12** grows a source-timecode field on `MediaAsset`, §3.6's metadata
   fallback should become a compatibility shim and export should write a real
   `available_range.start_time`.
