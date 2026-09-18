# 01 — Timeline Data Model

**Normative for:** all docs. **Location:** new module `crates/photonic-core/src/timeline/` (pure data + pure functions; no I/O, no GPU, no threads). **Decisions:** D-01, D-06, D-08.

All types are `Clone + Debug + Serialize + Deserialize + PartialEq` unless noted. All IDs are `Uuid` newtypes, matching the existing `pub type NodeId = Uuid` convention (`node.rs:12`) but as distinct newtypes for type safety:

```rust
macro_rules! id_newtype { ... } // ClipId, TrackId, SequenceId, AssetId, GraphId, GraphNodeId, MarkerId, CueId,
                                //  MarkerCategoryId (§4.1), GroupId (§4.2), LinkGroupId (deprecated — §4.2)
pub struct ClipId(pub Uuid); // etc.
```

---

## 1. Time representation

**Ticks (flicks).** All timeline positions and durations are `i64` ticks at **705,600,000 ticks/second** (the "flick": smallest unit that exactly divides 24, 25, 30, 48, 50, 60, 90, 100, 120 fps — including 1001-denominator NTSC rates — and 44.1/48/88.2/96/192 kHz audio rates).

```rust
pub const TICKS_PER_SECOND: i64 = 705_600_000;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Tick(pub i64);            // position or duration; negative allowed for deltas only

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct FrameRate { pub num: u32, pub den: u32 }   // e.g. 30000/1001

impl FrameRate {
    pub fn ticks_per_frame(&self) -> Tick;   // exact: TICKS_PER_SECOND * den / num (always integral for supported rates)
    pub fn frame_at(&self, t: Tick) -> i64;  // floor
    pub fn snap(&self, t: Tick) -> Tick;     // to frame boundary
}
```

Rules (normative):
- Persistence, edit ops, and the engine speak ticks only. Frames are a display/snapping concept derived from the sequence's `FrameRate`.
- No `f32`/`f64` time anywhere in the data model. UI converts at the edge.
- Unsupported/exotic rates: `ticks_per_frame` rounds to nearest tick; a `FrameRate::is_exact()` flag lets UI warn. Sub-tick error over 10 min is < 1 µs — below SS-3 tolerance.

## 2. Top-level container

```rust
// document.rs — additive field, sibling of `constraints`, `symbols`, etc.
pub struct Document {
    ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<TimelineProject>,
}

pub struct TimelineProject {
    pub media: MediaPool,                       // assets (§3)
    pub sequences: HashMap<SequenceId, Sequence>,
    pub sequence_order: Vec<SequenceId>,        // UI ordering
    pub active_sequence: Option<SequenceId>,
    pub graphs: HashMap<GraphId, NodeGraph>,    // all user node graphs (per-clip + project), one arena (§8)
    pub project_graph: Option<GraphId>,         // spliced after active sequence output
    pub settings: ProjectVideoSettings,         // proxy prefs, cache limits, default rates
    pub marker_categories: Vec<MarkerCategory>,  // ordered for display; referenced by stable id (§4.1)
}
```

`Document::new()` leaves `timeline: None`; first video-mode action creates it (undoably, §10).

## 3. Media pool

Media is **referenced, never embedded** (SPEC constraint). Contrast with `RasterImage`'s base64-PNG embedding — video files are orders of magnitude too large.

```rust
pub struct MediaPool {
    pub assets: HashMap<AssetId, MediaAsset>,
    pub bins: Vec<MediaBin>,                    // folders; flat list with parent refs
}

pub struct MediaAsset {
    pub id: AssetId,
    pub kind: AssetKind,
    pub source: AssetSource,
    pub probe: Option<MediaProbe>,              // filled by engine after ffprobe; cached in file
    pub proxy: Option<ProxyRef>,                // engine-managed; path + status
    pub content_hash: Option<String>,           // xxh3 of file head+tail+len; relink identity
    pub effects: Vec<ClipEffect>,               // asset-level stack, inherited by every instance (35 §2)
    pub grade: Option<Grade>,                   // asset-level grade — e.g. a per-camera LUT
}

pub struct MarkerCategory {
    pub id: MarkerCategoryId,
    pub name: String,
    pub color: Color,
    /// Non-colour distinguisher — colour alone must never carry meaning (41 §7).
    /// Six is the practical limit at ruler scale.
    pub glyph: MarkerGlyph,
}
pub enum MarkerGlyph { Diamond, Circle, Square, Triangle, Flag, Bar }

pub enum AssetKind { Video, Audio, Image, VectorDoc, Lut3d }
// VectorDoc: this document or external .photon/.svg
// Lut3d: .cube file, referenced not embedded (07 §1) — gets the same offline/relink handling as media

pub enum AssetSource {
    File { path: PathBuf },                     // absolute; relative-to-project fallback on load (§9)
    EmbeddedVector { root: VectorRef },         // lives inside this Document (artboard or node subtree)
}

pub enum VectorRef { Artboard(usize), Node(NodeId), WholeDocument }

pub struct MediaProbe {                          // subset of ffprobe output we persist
    pub duration: Tick,
    pub video: Option<VideoStreamInfo>,          // w, h, frame_rate: FrameRate, pixel_aspect, color: ProbedColor, keyframe_index_cached: bool
    pub audio: Option<AudioStreamInfo>,          // sample_rate, channels, codec
    pub container: String, pub codec: String,
}
```

Offline/missing media is a first-class state: `MediaAsset` with no reachable file renders diagonal-stripe placeholder; relink matches `content_hash` then filename.

## 4. Sequence, tracks

```rust
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub frame_rate: FrameRate,
    pub formats: Vec<SequenceFormat>,           // multi-aspect (CAP-012): ≥1 entries
    pub active_format: usize,
    pub video_tracks: Vec<Track>,               // index 0 = bottom of composite stack
    pub audio_tracks: Vec<Track>,
    pub caption_tracks: Vec<CaptionTrack>,      // §7
    pub markers: Vec<Marker>,                   // sequence-scoped markers (§4.1)
    pub groups: HashMap<GroupId, GroupNode>,    // clip grouping (§4.2)
    pub master_effects: Vec<ClipEffect>,        // master stack (35 §2)
    pub master_grade: Option<Grade>,
    pub audio_master: MasterBus,                // 09-audio-mixer.md
    pub work_range: Option<(Tick, Tick)>,       // in/out for preview + export
}

pub struct SequenceFormat {                      // one aspect-ratio variant
    pub name: String,                            // "16:9", "9:16", ...
    pub width: u32, pub height: u32,
    // per-clip reframe overrides live on the clip (§5, `reframe`), keyed by format index
}

pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,                         // Video | Audio
    pub clips: Vec<Clip>,                        // sorted by start, non-overlapping (invariant, enforced by edit ops)
    pub enabled: bool,                           // video: hidden; audio: muted
    pub locked: bool,
    pub audio: Option<TrackAudio>,               // volume/pan/solo, fx chain, automation (09)
    pub effects: Vec<ClipEffect>,                // track stack — applies to THIS track's own content only (35 §2)
    pub grade: Option<Grade>,
    pub blend: BlendMode,                        // how this track composites onto the accumulator
    pub opacity: f32,                            // 0.0..=1.0
    pub height_px: f32,                          // UI-only but persisted (like existing panel prefs)
}
```

### 4.1 Markers

One type; **scope is implied by location** — sequence markers on `Sequence.markers`, clip markers on `Clip.markers`. Rationale and alternatives: [35 §1](35-model-decisions.md#1-markers).

```rust
pub struct Marker {
    pub id: MarkerId,
    pub at: Tick,                                // sequence-relative, or clip-relative when clip-scoped
    pub duration: Tick,                          // 0 = point marker; every marker is a range
    pub name: String,
    pub note: String,
    pub category: Option<MarkerCategoryId>,      // into TimelineProject::marker_categories (§2)
    pub color: Option<Color>,                    // per-marker override of the category colour
    pub anchor: MarkerAnchor,
}

pub enum MarkerAnchor { Timecode, Content }      // stays put under ripple / moves with material
```

Normative rules:
- **`duration: Tick`, not `Option<Tick>`.** A point marker is a zero-length range, so `[at, at + duration]` is the only expression any consumer writes.
- **Categories are referenced by stable id, never by index.** Deleting a category is an undoable op carrying an explicit disposition (reassign or clear); a marker referencing a missing category renders with a neutral fallback and is flagged — never silently remapped.
- **Anchoring defaults by scope:** clip markers are always `Content` (they travel with the clip and propagate to its copies); sequence markers default to `Timecode`.
- Markers are snap candidates; a ranged marker contributes **two** (start and end).
- Clip markers get fresh ids under `duplicate_with_fresh_ids`.

### 4.2 Groups

A parent-pointer tree per sequence. Rationale: [35 §3](35-model-decisions.md#3-groups).

```rust
pub struct GroupNode { pub id: GroupId, pub kind: GroupKind, pub parent: Option<GroupId> }
pub enum GroupKind { Normal, AvLink }            // AvLink subsumes the former link_group
```

Invariants (checked by `Sequence::validate()` alongside the sorted/non-overlapping clip checks):
- the parent chain terminates — **no cycles**;
- every referenced `GroupId` exists in the same sequence;
- **no empty groups** and **no single-member `Normal` groups** — both dissolve automatically;
- selection is **not** a group — it is session state, never document state.

Invariants (enforced by edit ops in §6, checked by `Sequence::validate()` in debug + on load):
- Clips within a track are sorted, non-overlapping, `duration > 0`.
- `clip.start + clip.duration` may exceed any other clip freely across tracks.
- Track vectors are the z/mix order; no separate order table.
- **A clip's `transition_out` must be `None` when another clip starts exactly at its end.** A transition at a cut is owned by the *incoming* clip's `transition_in`; `transition_out` is a fade into a gap or the sequence end ([38 §1.3](38-sequence-semantics.md#13-one-transition-per-cut)). Migration where both are set: keep `transition_in`, drop `transition_out`, diagnose.
- **Transitions borrow, they do not overlap.** During a transition window the compositor samples the incoming clip normally *and* samples the outgoing clip past its own out point into its remaining source handle. Timeline layout is unchanged and the non-overlap invariant above holds. Where the handle is short the transition is **clamped and diagnosed**, never silently extended ([38 §1.2](38-sequence-semantics.md#12-insufficient-handle)).
- At any tick a track has **at most one** clip (a consequence of non-overlap). This is what makes the effect-scope pipeline in [02 §2](02-engine.md) unambiguous: at time `t` a track is empty, carries **content**, or carries an **Adjustment** operator — never two of those.

## 5. Clip

```rust
pub struct Clip {
    pub id: ClipId,
    pub name: String,                            // defaults to asset name
    pub start: Tick,                             // position in sequence
    pub duration: Tick,
    pub source: ClipSource,
    pub source_in: Tick,                         // offset into source media (trim); 0 for generators
    pub speed: SpeedMap,                         // §5.1
    pub transform: AnimProps<ClipTransform>,     // pos/scale/rotation/anchor/opacity — §6
    pub reframe: HashMap<usize, ClipTransform>,  // per-SequenceFormat static override (CAP-012)
    pub effects: Vec<ClipEffect>,                // ordered stack; each param animatable — §6.3
    pub grade: Option<Grade>,                    // 07-color-grading.md; stored here, evaluated as graph nodes
    pub composition: Option<GraphId>,            // per-clip node graph (D-06); substitutes the clip's SOURCE op only —
                                                 // transform/effects/grade/reframe still apply on top (02 §2 step 3)
    pub transition_in: Option<Transition>,       // {kind, duration, params}; borrows the previous clip's
                                                 // source handle — clips do NOT overlap (38 §1.1)
    pub transition_out: Option<Transition>,      // only meaningful with no following clip (38 §1.3)
    pub audio: Option<ClipAudio>,                // gain, fades, channel map (09)
    pub enabled: bool,
    pub color_label: Option<u8>,                 // timeline colour label
    pub markers: Vec<Marker>,                    // clip-scoped markers, travel with the clip (§4.1)
    pub group: Option<GroupId>,                  // immediate parent in Sequence::groups (§4.2)
    pub multicam: Option<MulticamGroup>,         // G-20; data-model only until its gate clears
    #[deprecated = "migrated to GroupKind::AvLink (35 §3.3); retained one format version"]
    pub link_group: Option<LinkGroupId>,
}

pub enum ClipSource {
    Asset { asset: AssetId },                    // video/audio/image via media pool
    Vector { asset: AssetId },                   // AssetKind::VectorDoc — rasterized per frame (CAP-006/021)
    NestedSequence { sequence: SequenceId },     // CAP-005; cycle-checked at edit time
    SolidColor { color: Color },
    Adjustment,                                  // affects everything below (effects/grade apply to composite)
}
```

`ClipTransform.anchor_space` makes anchor coordinates unambiguous. New v4
transforms use `CenterOffset`, where `(0, 0)` is the output-frame center;
`Absolute` stores legacy output-pixel pivots. The v3→v4 document migration tags
existing base and per-format reframe transforms as `Absolute` without changing
their numeric values or keyframes.

### 5.1 Speed

```rust
pub struct SpeedKey {
    pub at: Tick,
    pub ratio: Ratio,
    pub interp: Interp,                          // Hold default; Linear/Bezier ease toward next key
}

pub enum SpeedMap {
    Constant(Ratio),                             // Ratio { num: i32, den: u32 }; 1/1 default; negative num = reverse
    Keyframed { keys: Vec<SpeedKey> },            // additive serde tag
}
```

Mapping: source time = `source_in + speed.source_delta(t - start)`. `Constant` and all-`Hold` ramps integrate with exact integer/rational arithmetic. `Linear`/`Bezier` ramps integrate deterministically and round to nearest source tick. First/last ratios hold outside keyed span; empty ramp is identity. Current code contract is normative. [20 §5](20-pro-workflows.md#5-g-11--speed-and-time-remap-ramps) owns residual on-clip UI, audio mapping, validation, and golden closure.

## 6. Animation (keyframes)

One generic system animates everything: clip transforms, effect params, audio automation, vector-node properties.

```rust
pub struct AnimProps<T: PropSet> { pub base: T, pub tracks: Vec<PropertyTrack> }

pub struct PropertyTrack {
    pub property: PropPath,                      // e.g. "transform.x", "params.blur_radius" — §6.2
    pub keyframes: Vec<Keyframe>,                // sorted by `at`, unique `at`
}

pub struct Keyframe {
    pub at: Tick,                                // clip-relative
    pub value: PropValue,
    pub interp: Interp,
}

pub enum PropValue { Float(f64), Vec2([f64;2]), Color(Color), Bool(bool), Enum(u32) }

pub enum Interp {
    Hold,
    Linear,
    Bezier { out_handle: [f64;2], in_handle: [f64;2] },  // normalized cubic-bezier ease handles (CSS-like)
}
```

- Evaluation: `fn eval(track: &PropertyTrack, base: &PropValue, t: Tick) -> PropValue` — pure, in core, unit-tested against closed-form expectations (CAP-007 test).
- Bool/Enum interpolate as Hold regardless of `interp`.

### 6.2 PropPath

String paths, validated against a static registry per target kind (`prop_registry.rs`): each effect kind, `ClipTransform`, `TrackAudio`, and the animatable subset of `SceneNode` (transform, opacity, fill colors, stroke width, effect-stack params — all already serde per explorer findings) publish `(path, PropValueKind, range)` entries. Unknown path on load → track kept, flagged `orphaned` (asset may re-register later); never dropped silently.

### 6.3 Effects

```rust
pub struct ClipEffect {
    pub kind: EffectKind,                        // registry enum: Blur, Sharpen, Glow, ChromaKey, LumaKey, Invert, ... — 08 §3's catalog is normative
                                                 // (Transform2D/Crop are dedicated IrOps in 02, NOT effects)
    pub enabled: bool,
    pub params: AnimProps<EffectParams>,         // EffectParams = ordered map PropPath→PropValue seeded from kind defaults
}
```

Raster `AdjustmentSpec`/filter algorithms are ported as effect kinds where GPU-implementable; CPU reference implementations stay for export determinism tests (03 §6).

## 7. Captions

```rust
pub struct CaptionTrack {
    pub id: TrackId,
    pub name: String,
    pub cues: Vec<CaptionCue>,                   // sorted, non-overlapping
    pub style: CaptionStyle,                     // track default
    pub enabled: bool,
}

pub struct CaptionCue {
    pub id: CueId,
    pub start: Tick, pub end: Tick,
    pub words: Vec<CaptionWord>,                 // word-level timing (CAP-009/010)
    pub style_override: Option<CaptionStyle>,
    pub position_override: Option<[f32;2]>,      // normalized sequence coords
}

pub struct CaptionWord { pub text: String, pub start: Tick, pub end: Tick, pub style_override: Option<CaptionStyle> }

pub struct CaptionStyle {
    pub font_family: String, pub font_size: f32, pub weight: u16,
    pub fill: Color, pub stroke: Option<(Color, f32)>,
    pub background: Option<CaptionBackground>,   // {color, corner_radius, padding}
    pub highlight: Option<KaraokeStyle>,         // {mode: FillSweep|WordPop|Underline, active_color, inactive_color}
    pub position: [f32;2], pub max_width: f32,   // normalized
    pub animation: CaptionAnim,                  // None | FadeWords | SlideUp | Typewriter (per-word timing driven)
}
```

Rendering uses the existing text pipeline (glyphon on GPU; CPU compositor path for export) — 06 owns details.

## 8. Node graphs

One arena for all user graphs (per-clip compositions and the project graph):

```rust
pub struct NodeGraph {
    pub id: GraphId,
    pub name: String,
    pub nodes: HashMap<GraphNodeId, GraphNode>,
    pub edges: Vec<GraphEdge>,                   // {from: (GraphNodeId, OutPort), to: (GraphNodeId, InPort)}
    pub output: GraphNodeId,                     // exactly one Output node
    pub ui: HashMap<GraphNodeId, NodePos>,       // editor positions
}

pub struct GraphNode {
    pub id: GraphNodeId,
    pub op: GraphOp,                             // catalog in 08 (MediaIn, ClipIn, Merge, Transform, Blur, Key, Grade, Lut, Text, Mask, Switch, Time, Output, ...)
    pub params: AnimProps<EffectParams>,
}
```

Normative semantics:
- DAG only; edge insertion cycle-checks (edit op fails, never panics).
- `ClipIn` node = "the clip's source after trim/speed, before default effects" — the splice point for per-clip compositions.
- Data-model graphs are *descriptions*; evaluation semantics, port types, and caching live in 02/08. Same `AnimProps` animation system as clips.

## 9. Serialization & migration

### 9.0 Forward compatibility (normative) — [39 §2](39-document-lifecycle.md#2-forward-compatibility-cap-020)

**Every open-ended enum in the persisted model carries an unknown-preserving variant** — `EffectId`, `GraphOp`, `AudioFxKind`, `TransitionKind`, `GradeOpKind`, `MarkerAnchor`, `GroupKind`, `ClipSource`. `GradeOpParams`'s `#[serde(other)]` inert-load is the existing model and generalises. Rules:

- **Preserve the original serialized form verbatim** and re-emit it unchanged on save — a round-trip through an older build is lossless.
- **Render inert** — unknown effect = passthrough, unknown transition = cut, unknown source = placeholder.
- **Diagnose once per load**, not per frame: `Project::UnknownVariantPreserved`.
- **Never drop, never guess.** Approximating an unknown effect with a similar one is worse than omitting it, because the user cannot see it is wrong.

| `format_version` | Behaviour |
|---|---|
| Older, inside `COMPAT_WINDOW` | Migrate forward, save at current |
| Older, outside | Refuse — `Project::VersionTooOld`, naming the version that can read it |
| Equal | Load |
| **Newer, minor** | Load with unknown-preservation, **warn before the first save** that newer-only data may be lost, offer save-as-copy |
| Newer, major | Refuse — `Project::VersionTooNew` |

The "newer, minor" row is the one most products get wrong: silently loading and re-saving a newer file is how a user loses work created on another machine.

Validation rejections (`finalize_load`'s overlap/unsorted check) surface as `Project::ValidationFailed` **naming the offending clip**, not as an opaque failure.

- **Done:** the timeline field landed additively at v3 and `CURRENT_FORMAT_VERSION` is now **5** (`document.rs:117`); `docs/format-versions.md` documents v1–v5, including the v3→v4 `anchor_space` migration described in §5 and the nine-change `V4ToV5` step (35).
<!-- spec-assert: const photonic_core::document::CURRENT_FORMAT_VERSION == 5 -->
<!-- SD-3 (27 §3): historically drifted "bump 2 → 3"; the const is machine-pinned here so a further bump without a doc edit reds the drift gate. -->


### 9.1 The v4 → v5 migration — one step, nine changes

Nine model changes are specified across seven documents, each of which independently describes itself as "additive". **They are one migration and must land as one**, or the format version becomes meaningless and a document written mid-sequence is readable by nothing. This section owns the consolidated inventory; the owning docs keep the rationale.

| Change | Shape | Owner | Data migration? |
|---|---|---|---|
| `MarkerCategory` registry on `TimelineProject`; `Marker` gains `duration`, `category`, `anchor`; `Clip.markers` | additive | [35 §1](35-model-decisions.md#1-markers) | **No** — defaults reproduce current behaviour |
| `MarkerCategory.glyph` | additive | [41 §7](41-accessibility.md#7-colour-only-information) | **No** — default glyph per seeded category |
| `Sequence.groups` + `Clip.group`; `link_group` deprecated | additive + **projection** | [35 §3](35-model-decisions.md#3-groups) | **Yes** — every `link_group: Some(g)` becomes an `AvLink` group. Behaviour-preserving, and covered by the existing link tests before the field is removed |
| `Track.effects`/`.grade`/`.blend`/`.opacity`; `Sequence.master_*`; `MediaAsset.effects`/`.grade` | additive | [35 §2](35-model-decisions.md#2-effect-scopes-and-the-adjustment-clip-interaction) | **No** |
| `ClipEffect` gains `id: EffectId` + `version`; `kind: EffectKind` deprecated | additive + **projection** | [30 §10](30-effect-catalogue.md#10-compatibility) | **Yes** — `id` derived from `kind`; `kind` retained one version |
| Unknown-preserving variants on every open-ended enum (§9.0) | additive | [39 §2.2](39-document-lifecycle.md#22-generalise-it) | **No** — but it must land **first**, or a v5 document is unreadable by the build that introduces the rest |
| `CaptionTrack.language`; `CaptionStyle.direction` | additive | [42 §6.4](42-localization.md#64-per-language-budgets), [§7.3](42-localization.md#73-refused-cleanly-in-v1) | **No** — `None` falls back to the Latin budget and emits a hint, never a guess |
| `ClipAudio.stream` + `.offset` | additive | [31 §7](31-audio-architecture.md#7-per-stream-and-per-channel) | **No** |
| `Track.height_px` **removed** to a sidecar | **removal** | [39 §1.6](39-document-lifecycle.md#16-what-is-not-undoable) | **Yes** — read from v4, write to the sidecar, drop from the document |

**Ordering is not free.** §9.0's unknown-preserving variants land **before** everything else, because they are what lets a v4 build open a v5 document without data loss — introducing new enum variants first would strand any document written in between. The two projections (`link_group` → group tree, `kind` → `EffectId`) keep their deprecated field for exactly one version, per [30 §10](30-effect-catalogue.md#10-compatibility). `height_px`'s removal is the only lossy step and is the only one that cannot be reverted by loading in an older build.

**A single `docs/format-versions.md` v5 entry covers all nine.** Nine separate entries would imply nine version numbers.
- `timeline` is `Option` + `#[serde(default)]` → v2 files load untouched; v3 files without video features omit the key entirely (COMPAT_WINDOW satisfied).
- Asset paths serialize absolute + project-relative (`path`, `rel_path`); loader tries relative first (project moves survive), then absolute, then relink-by-hash.
- Probe data, proxy refs, waveform/keyframe-index caches: probe persists in-file (it's small, needed for offline layout); waveforms/keyframe indices/thumbnails go to a **cache sidecar dir** `<project>.photon.cache/` — never in the JSON (file bloat + churn).

## 10. Undo integration

### 10.0 The undo contract (normative) — [39 §1](39-document-lifecycle.md#1-undo-cap-018)

- **One user verb is one undo step**, including fanned-out edits: a group move of nine clips, an import of forty assets, a category deletion reassigning two hundred markers. Corollary: **an operation that cannot be undone atomically must not commit partially** — validate every member, then commit.
- **Coalescing is bounded**, never open-ended: same command kind *and* same subject · gap < 500 ms · total span ≤ 5 s · broken by selection change, tool change, save, or any other command kind. A continuous drag is one step; a drag, a pause, and another drag are two.
- **Bounded by both a step count and a byte budget**, the byte budget dominating (one `BulkInsertCues` outweighs a thousand slider steps). **Branches are never auto-trimmed.** A retention floor guarantees a minimum step count regardless of size. Trimming is silent — a memory policy, not an event. Commands carrying bulk payloads (`BulkInsertCues`, `ApplyDuckingPreset`, `SetGrade`, composition paste) are acceptable **because** the byte budget bounds them, but each must report `mem_estimate` honestly or the budget is enforced against a fiction.
- **Undo is global and does not respect mode.** Undoing a video edit from vector mode is allowed and switches to the mode the command belongs to. Per-mode stacks would break the single-history property that makes a vector asset a first-class timeline citizen.
- **Jobs:** a job result commits as a normal undoable command at completion; the job captured a document snapshot at submission and is unaffected by later edits or undos; if its target no longer exists, the commit is **skipped with an `Info`**, never resurrected. **Undo never cancels a running job.**

New nested command keeps `history/mod.rs` churn to one arm per group:

```rust
// history: one new variant
Command::Timeline(TimelineCmd)

pub enum TimelineCmd {
    CreateProject { .. }, AddAsset { .. }, RemoveAsset { .. }, RelinkAsset { asset, old_path, new_path },
    AddSequence { .. }, RemoveSequence { .. },
    SetActiveSequence { old, new }, SetActiveFormat { seq, old, new },  // active_* are document state; changes are undoable
    SetSequenceFormat { .. },                    // covers add/update/remove of format entries (op field; 10 §3.2)
    AddTrack { .. }, RemoveTrack { .. }, SetTrackProp { .. },
    InsertClip { .. }, RemoveClip { .. }, MoveClip { .. }, TrimClip { .. }, SplitClip { .. },
    RippleEdit { .. }, RollEdit { .. }, SlipClip { .. }, SlideClip { .. },
    SetClipProp { old, new },                    // universal property change, mirrors UpdateNode{old,new}
    SetKeyframe { .. }, RemoveKeyframe { .. }, SetKeyframeInterp { .. },
    AddEffect { .. }, RemoveEffect { .. }, ReorderEffects { .. },   // scope: clip | track | master | asset (35 §2)
    AddMarker { .. }, RemoveMarker { .. }, SetMarker { old, new },  // sequence- or clip-scoped (§4.1)
    AddMarkerCategory { .. }, RemoveMarkerCategory { disposition, .. }, SetMarkerCategory { old, new },
    GroupClips { .. }, UngroupClips { .. }, SetClipGroup { old, new },   // §4.2
    SetGrade { old, new },
    GraphEdit(GraphCmd),                         // add/remove node/edge, set param — 08
    CaptionEdit(CaptionCmd),                     // add/split/merge cues, set text/timing/style — 06
    TtsEdit(TtsCmd),                             // atomic voiceover generate/place (asset+clip+optional captions) — 06 §6
    AudioEdit(AudioCmd),                         // 09
}
```

- Each variant implements `apply`/`inverse`/`description` inside `timeline/commands.rs`; `history/mod.rs` delegates (`Command::Timeline(c) => c.apply(doc)`), so the 4–5-touch-point friction is paid once.
- `coalesce`: drag-move/trim and keyframe-drag coalesce by (variant, clip/keyframe id) like existing `UpdateNode` coalescing.
- Edit ops (`timeline/ops.rs`) are pure functions `fn move_clip(seq: &mut Sequence, ...) -> Result<TimelineCmd, EditError>` producing the command — GUI and MCP both call them (CAP-019 parity).
- **Memory rule:** commands store deltas/ids, never media. `mem_estimate` for timeline commands is O(bytes of changed structs). Playhead position, selection, scroll are **not** document state (no undo entries) — they live in GUI/engine session state.

## 11. What is deliberately NOT in the data model

- Decoded frames, textures, waveform pyramids, thumbnails, keyframe indices → engine caches (02) / sidecar dir (§9).
- Playhead, selection, in/out *while scrubbing* → session state (work_range persists, playhead does not).
- Render/export settings presets → app-level config, except per-sequence format which is document state (§4).
