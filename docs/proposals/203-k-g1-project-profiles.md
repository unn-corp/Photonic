# 203 — K-G1 Project Profiles (mini-spec)

> Status: **proposed mini-spec — not accepted, no code authorization.** Written to
> satisfy the [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md) K-Band 5
> exit condition: *"an accepted mini-spec exists **before** code, naming its
> data-model change, migration, undo unit, MCP surface and acceptance fixtures.
> No item here starts without one."* Acceptance of this document is what
> authorizes K-G1; nothing here authorizes an edit to a product crate.

Owner refs: [26 §15 K-G1](../specs/video-editor/26-kdenlive-mlt-parity.md#k-g1--project-profiles)
(the requirement), [26 §5 PA-6](../specs/video-editor/26-kdenlive-mlt-parity.md#5-photonic-ahead-register-pa---do-not-port-backwards)
(the constraint this item is most likely to violate),
[38 §3](../specs/video-editor/38-sequence-semantics.md#3-frame-rate-conform)
(what happens to footage when a rate changes),
[ROADMAP §10](../specs/video-editor/ROADMAP.md#10-definition-of-done) (done).

Siblings written in the same round and deliberately not contradicted:
[193 K-A1](193-k-a1-chunked-timeline-preview-rendering.md) (which also appends a
field to `ProjectVideoSettings` — see [§4](#4-migration-and-format-version-impact)),
[194 K-A5](194-k-a5-general-and-nested-clip-groups.md) (the one-plural-command
rule this document obeys in [§5](#5-undo-unit-and-its-exact-inverse)),
[196 X-2](196-x-2-opentimelineio-interchange.md) (the `dry_run` pre-flight shape
reused in [§7](#7-mcp-surface)), and **204 K-G4 project templates**, being
specified concurrently — [§3.4](#34-where-presets-live-and-the-k-g4-s11-gate)
states the compatibility contract with it explicitly.

---

## 1. Problem and user outcome

**Today.** Creating a sequence means typing four numbers. In the GUI the `+` on
the sequence tab strip silently inherits the *active* sequence's rate and its
active format's dimensions (`crates/photonic-gui/src/panels/video/seq_tabs.rs:68-75`
→ `create_sequence_tab`, `crates/photonic-gui/src/app/timeline/ops_bridge.rs:100`),
falling back to 24/1 and 1920×1080 when there is no active sequence. Over MCP,
`create_sequence` requires `name`, `frame_rate` and `formats` on every call
(`crates/photonic-mcp/src/protocol/args/video.rs:98-104`). There is no named,
reusable, manageable preset anywhere; `grep -r project_profile crates/` is clean.

Worse, two things a user asks for constantly are **not expressible at all**:

1. **"Make this sequence 3840×2160 at 23.976."** Nothing in the codebase writes
   `Sequence.frame_rate` after `Sequence::new` (`sequence.rs:129`, constructed at
   `sequence.rs:175`). There is no command, no op, no tool. A sequence's frame
   rate is fixed at birth.
2. **"Match this sequence to that clip."** 26 K-G1 calls this *adjust-profile-to-clip*.
   `MediaProbe`'s `VideoStreamInfo { width, height, frame_rate, … }`
   (`crates/photonic-core/src/timeline/media.rs:185-189`) already holds exactly
   the three numbers needed, and nothing reads them for this purpose.

**After K-G1** a user can:

- Pick a named profile — **1080p25**, **2160p29.97**, **DCI 4K 24** — when
  creating a sequence, in the GUI and from an agent, with the project's chosen
  default pre-selected.
- Save the current sequence's shape as a named profile, and manage that list.
- Apply a profile to an **existing** sequence, or match it to a clip, as one
  undo step, having first been shown exactly what that will change.
- Do all of it without any sequence being forced to agree with any other.

**The last clause is the requirement, not a nicety.**
[26 §5 PA-6](../specs/video-editor/26-kdenlive-mlt-parity.md#5-photonic-ahead-register-pa---do-not-port-backwards)
records per-sequence formats as a property Photonic **already holds** and the
reference lacks: *"All Kdenlive sequences share one project profile; per-sequence
settings are impossible."* PA-6 is on
[ROADMAP §9](../specs/video-editor/ROADMAP.md#9-protected-surfaces)'s protected
list. 26 K-G1 says it in its own words: *"adopt the concept, not Kdenlive's
constraint… A profile here is a **named default applied when creating a
sequence**, never a global lock."*

So: **a profile is a value, not a constraint.** It is copied into a sequence at
creation or on explicit apply, and thereafter the sequence owns its own shape.
There is no project-wide profile that sequences must satisfy, no conform-on-open,
no validation that cross-checks sequences against a project setting, and no code
path that mutates more than one sequence. [§8.2 Q4](#82-open-questions-each-with-a-recommendation)
records the one place a reader might reasonably want a global and why the answer
is still no.

---

## 2. Current state in code (exact)

As of `feat/video-editor-module` @ `8a33f32`. Read this before disagreeing with §3.

### 2.1 The state that exists and is directly reusable

| Thing | Where | Note |
|---|---|---|
| `ProjectVideoSettings { generate_proxies, cache_limit_mb, default_frame_rate, audio_sample_rate }` | `crates/photonic-core/src/timeline/sequence.rs:90-103` | `TimelineProject.settings`, `sequence.rs:36`, `#[serde(default)]` |
| `Sequence { id, name, frame_rate, formats, active_format, … }` | `sequence.rs:126-133` | the sequence **owns** its rate and its formats |
| `SequenceFormat { name, width, height }` | `sequence.rs:609-613` | **no rate, no pixel aspect** — rate lives on the `Sequence` |
| `FrameRate { num: u32, den: u32 }`, exact rational | `crates/photonic-core/src/timeline/time.rs:72-75` | named constants `FPS_24/25/30/60/23_976/29_97/59_94`, `time.rs:78-96`; `const fn new`, `time.rs:99` |
| `FrameRate::ticks_per_frame / snap / is_exact / nominal_fps / is_drop_frame_rate` | `time.rs:108, 126, 141, 150, 157` | `is_exact()` is already the "this rate does not divide the flick" flag |
| `Tick` = `i64` flicks, `TICKS_PER_SECOND = 705_600_000` | `time.rs:13, 23` | PA-8 — the reason §6 is short |
| `ops::create_sequence(name, frame_rate, width, height)` → `AddSequence` | `crates/photonic-core/src/timeline/ops.rs:352-359` | one command, already undoable |
| `TimelineCmd::AddSequence { sequence }` / inverse `RemoveSequence` | `crates/photonic-core/src/timeline/commands.rs:451`, invert at `:2206` | |
| `FormatOp::{Add, Insert, Update, Remove}` + `SetSequenceFormat` | `commands.rs:106-126`, `:480`; apply `:1845-1872`; invert `:908-932` | the CAP-012 multi-aspect editor |
| `TimelineCmd::SetGenerateProxiesOnImport { old: bool, new: bool }` | `commands.rs:447`; apply `:1805`; invert `:2200`; op `ops.rs:316` | **the exact shape §3.2's new commands copy** — the one existing settings command |
| `ops::fit_clip_to_format(content, target) -> ClipTransform` | `ops.rs:1312` | center-fill retarget; its doc comment fixes `formats[0]` as the reframe baseline |
| Export-preset store: `config_dir()` → `crash_dir()`, `presets_path()`, `load/save_custom_presets{,_from,_to}` | `crates/photonic-video/src/export/presets.rs:401, 407, 414, 421, 428, 437`; `crash_dir` at `crates/photonic-core/src/diagnostics.rs:29` | **the storage precedent §3.4 copies**, including the path-parameterized test hooks |
| MCP `list_export_presets` / `save_export_preset` / `delete_export_preset` | `crates/photonic-mcp/src/handlers/video.rs:4427, 4444, 4487` | built-ins + customs with a `built_in` flag; built-in names refused with `NotSupportedV1` (`:4459`, `:4493`); **"app-level config… no document mutation, no undo step"** (`docs/mcp-api.md:3607`) |
| `CompileCode::FrameRateConformed` | `crates/photonic-video/src/graph/compile.rs:144`; emitted `:1314` (per clip) and `:1472` (per nest) | 38 §3.5 / §2.2 — **already shipped**, and it is the answer to "what happened to my footage" |
| `ASPECT_PRESETS` + `switch_to_aspect` | `ops_bridge.rs:88`, `:47`; rendered at `crates/photonic-gui/src/app/monitor.rs:1264` | a built-in **aspect** table — GUI-only, no frame rate. See §2.3 |

### 2.2 What does not exist — say it plainly

1. **No `SequenceProfile` type, no registry, no picker.** `grep -r project_profile`
   is clean, as 26 K-G1 states.
2. **No command writes `Sequence.frame_rate`.** Verified: the only assignment in
   the whole workspace is the field initializer inside `Sequence::new`
   (`sequence.rs:175`, the field at `:179`). Changing a sequence's rate is
   currently impossible through any surface.
3. **No command writes `ProjectVideoSettings.default_frame_rate` or
   `audio_sample_rate`.** `SetGenerateProxiesOnImport` (`commands.rs:1805`) is
   the *only* writer of any field on `settings`. `default_frame_rate` is read at
   exactly one place — `crates/photonic-gui/src/app/timeline/mod.rs:905`, as the
   fallback rate when no sequence is active — and is otherwise inert.
   `audio_sample_rate` is read by the export mixer
   (`crates/photonic-video/src/export/job.rs:200`,
   `export/offline_audio.rs:54`) and likewise never written.
4. **Nothing consumes a *sequence* pixel aspect.** `VideoStreamInfo.pixel_aspect`
   is probed (`media.rs:190`, parsed at
   `crates/photonic-video/src/media/probe.rs:332`) and read by **no** production
   code — the three other occurrences in the tree are `1.0` literals in tests and
   a media-pool stub (`compile.rs:4152`, `video_ui_paths.rs:519`,
   `panels/media_pool.rs:1266`). Composition is square-pixel throughout.
5. **There is no per-sequence colour space.** Photonic's working space is
   linear-light Rec.709 by construction (PA-2/PA-14, 03). A per-sequence HDR
   working state is [ROADMAP §8 S3](../specs/video-editor/ROADMAP.md#8-architecture-decisions-and-defaults)
   — accepted as a decision, not implemented.
6. **Scanning is per-source, not per-project.** K-G6 landed since 26 was written:
   `ScanType` on `VideoStreamInfo` (`media.rs:167-182, 198`), `IrOp::Deinterlace`
   and `DeinterlaceMethod` (`crates/photonic-video/src/graph/ir.rs:275, 106`),
   auto-inserted from the **asset probe** at `compile.rs:1343`
   (`deinterlace_for_asset`, `compile.rs:72`), with `DiagCode::MediaInterlaced`
   (`crates/photonic-core/src/diag.rs:214`). §3.3 argues this is why a profile
   "scanning" field must not be added.
7. **Clip positions are not frame-snapped, and nothing snaps them.**
   `Sequence::validate` (`sequence.rs:378`) checks positive duration, sorted
   order, non-overlap, transitions and groups — nothing rate-dependent. No
   `ops::` function calls `FrameRate::snap` (`time.rs:126`). Sub-frame positions
   are legal in the model. This fact is load-bearing for §6.

### 2.3 A profile is not a format — do not collapse them

The single most likely way to implement K-G1 wrongly is to route it through the
existing multi-format machinery. They are different concepts with different
arities and different owners:

| | **Profile** (K-G1) | **Format** (CAP-012, PA-6) |
|---|---|---|
| Arity | one per sequence — its native shape | `Vec<SequenceFormat>`, ≥1, per sequence (`sequence.rs:131`) |
| Carries | resolution **and frame rate** | resolution only |
| Verb | "this sequence is 2160p29.97" | "also deliver this as 9:16" |
| Surface | new-sequence picker, sequence settings | monitor format bar (`monitor.rs:1264` → `switch_to_aspect`, `ops_bridge.rs:47`) |
| Per-clip effect | none | `Clip.reframe: HashMap<usize, ClipTransform>` keyed by **format index** (`crates/photonic-core/src/timeline/clip.rs:44`) |

`ops::fit_clip_to_format`'s doc comment (`ops.rs:1301-1311`) settles the
relationship: *"`content` is the sequence's format at index 0 — the sequence's
original/native format, which is exactly what a clip's un-reframed
(identity-scale) `transform` already fills at authoring time."* **Format index 0
is the profile's slot.** Everything above index 0 is deliverable variants that
K-G1 must leave alone.

### 2.4 A pre-existing hazard the design must route around

`FormatOp::Insert` and `FormatOp::Remove` shift `formats` indices
(`commands.rs:1849-1866`) and **do not remap `Clip.reframe` keys**
(`clip.rs:44`). Today this is latent because the only GUI path appends
(`switch_to_aspect`, `ops_bridge.rs:71-83`), but the MCP `set_sequence_format`
tool exposes `op=remove` with a `format_index` (`docs/mcp-api.md:4142-4149`), so
an agent removing format 0 silently shifts every clip's per-format override up
by one. K-G1 does not fix this (see [Follow-ups](#follow-ups)), and it is the
reason [§3.2](#32-two-new-commands) writes `formats[0]` **in place** via
`FormatOp::Update` semantics rather than remove-then-add.

---

## 3. Data-model change

### 3.1 Two new types, one of them persisted

```rust
// crates/photonic-core/src/timeline/profile.rs — new module, exported from
// timeline/mod.rs alongside the other timeline types.

/// The shape a sequence has: its native raster and its exact rational rate.
/// Nameless on purpose — a `Sequence` never records which profile it came from
/// (§8.2 Q2). This is what a profile *applies* and what an undo *restores*.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceShape {
    pub frame_rate: FrameRate,
    /// Written to `Sequence.formats[0]` — the native format (§2.3).
    pub format: SequenceFormat,
}

/// A named, reusable `SequenceShape`. Built-in (§3.5) or user-saved (§3.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceProfile {
    pub name: String,
    #[serde(flatten)]
    pub shape: SequenceShape,
}
```

Nothing else is added to the model. No new field on `Sequence`, `Track`, `Clip`,
`MediaAsset` or `Document`. **A `Sequence` gains no profile id and no profile
name** — its `frame_rate` and `formats[0]` *are* the applied profile, and a
stored name would drift the instant a user edits the format ([§8.2 Q2](#82-open-questions-each-with-a-recommendation)).

The one persisted addition:

```rust
// crates/photonic-core/src/timeline/sequence.rs — appended to
// ProjectVideoSettings (sequence.rs:90-103), beside `default_frame_rate:99`.

/// K-G1: the profile offered when creating a sequence in this project.
/// Stored **resolved**, not as a name, so it cannot dangle on a machine whose
/// user catalogue (§3.4) does not contain it. `None` = fall back to
/// `default_frame_rate` and the existing inherit-from-active behaviour.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub default_profile: Option<SequenceProfile>,
```

`default_frame_rate` (`sequence.rs:99`) **stays**. When `default_profile` is
`Some`, it wins; when `None`, `default_frame_rate` is the fallback exactly as
`app/timeline/mod.rs:905` already uses it. Collapsing the two is a field removal
and therefore a v6 change ([Follow-ups](#follow-ups) 1) — the same
one-version-deprecation shape [194 §2.2](194-k-a5-general-and-nested-clip-groups.md)
gives `Clip.link_group`.

### 3.2 Two new commands

```rust
// crates/photonic-core/src/timeline/commands.rs — new arms on TimelineCmd:396

/// Apply a shape to one sequence: `frame_rate` + `formats[0]`, atomically.
/// Touches no clip, no track, no other format, no `active_format`.
SetSequenceProfile {
    seq: SequenceId,
    old: SequenceShape,
    new: SequenceShape,
},

/// The project's default profile for new sequences. `None` clears it.
SetDefaultProfile {
    old: Option<SequenceProfile>,
    new: Option<SequenceProfile>,
},
```

Both are modelled field-for-field on `SetGenerateProxiesOnImport`
(`commands.rs:447`, apply `:1805`, invert `:2200`) — the project already has
exactly one settings command and this follows it rather than inventing a shape.

`SetSequenceProfile::apply`: `s.frame_rate = new.frame_rate` and
`s.formats[0] = new.format.clone()`. `formats` is never empty
(`ValidationError::NoFormats`, `sequence.rs:379`), so index 0 always exists;
`formats[1..]`, `active_format`, `start_timecode`, `work_range` and every clip
are untouched. `invert()`: swap `old`/`new`. `mem_estimate` (`commands.rs:1631`)
is two small structs. `label()` (`commands.rs:1671`): `"Sequence profile"` /
`"Default profile"`, matching the terse existing wording (`"Switch format"`,
`"Edit format"`, `commands.rs:1683-1684`).

**Why one command and not `Command::Batch([rate, format])`.**
[194's finding](194-k-a5-general-and-nested-clip-groups.md) — `TimelineCmd::apply`
debug-asserts `Sequence::validate()` after **every** command
(`commands.rs:1748-1757`) and `Command::Batch` applies members one at a time — is
what makes plural edits dangerous. Here, honestly stated, **neither intermediate
would actually be invalid**: `validate` checks nothing rate-dependent (§2.2 item
7) and an in-place format update changes no invariant. The reason for a single
command is different and still decisive:

1. There is **no `SetSequenceFrameRate` command today** (§2.2 item 2), so a rate
   command has to be invented either way. Inventing it *as* the profile pair
   keeps one user verb at one command instead of adding a second settings
   command that must then always be batched with the first.
2. `label()` and `ChangeSummary` describe one verb, not two, which matters for
   K-G5's shipped history browser.
3. A batch would tempt an implementer toward `FormatOp::Insert`/`Remove`, which
   is precisely the reframe hazard in §2.4.

### 3.3 What a profile deliberately does **not** carry

26 K-G1 lists five properties. Three are excluded, and the exclusions are
arguments, not omissions:

| Property | Decision | Why |
|---|---|---|
| Resolution | **In** | `SequenceFormat`, already modelled |
| Frame rate | **In** | `FrameRate`, already modelled |
| Display aspect | **Derived**, not stored | With square pixels, DAR *is* `width/height`. An independently-stored DAR that disagrees with the raster is a second source of truth |
| Sample/pixel aspect | **Out** | Nothing consumes a sequence PAR (§2.2 item 4). A field with no consumer that appears in a settings dialog is a lie about what the app does |
| Colour space | **Out** | PA-2/PA-14: linear-light Rec.709 by construction. A per-sequence working space is [S3](../specs/video-editor/ROADMAP.md#8-architecture-decisions-and-defaults), an open gate. Shipping the dropdown before the pipeline would advertise a capability that does not exist |
| Scanning / field order | **Out** | K-G6 landed and derives it **per source** from the probe (`compile.rs:1343`), which is strictly better than one project-wide flag on a mixed-source timeline. A project scanning field would be porting a reference limitation backwards — exactly what 26 §5 forbids |

Audio sample rate is likewise **out**: `audio_sample_rate` is already project-wide
(`sequence.rs:102`) and is consumed by the export mixer (`export/job.rs:200`).
Duplicating it per-profile would create two answers to one question.

### 3.4 Where presets live, and the K-G4 S11 gate

Three locations, one per lifetime, and the split is the whole answer:

| What | Where | Undoable? | Rationale |
|---|---|---|---|
| **Built-in profiles** (§3.5) | compiled in — `profile::built_in_profiles()` | n/a | Facts from published standards; no file, no rights surface |
| **User profiles** (the catalogue) | `crash_dir()/sequence_profiles.json` | **No** | Verbatim the export-preset pattern (`presets.rs:401-443`). App-level config is not document state, so an undo of it would be an undo of something outside the undo model |
| **The project's default** | `ProjectVideoSettings.default_profile`, resolved | **Yes** — `SetDefaultProfile` | Different projects have different deliverables; storing it resolved means a `.photon` opened on another machine still knows its own shape |
| **The applied profile** | the `Sequence`'s own `frame_rate` + `formats[0]` | **Yes** — `SetSequenceProfile` | Already the model. No new state |

The store lives in **`photonic-core`**, not `photonic-video`, unlike export
presets. Justification: `SequenceProfile` is built from `FrameRate` and
`SequenceFormat`, both core types, and it is persisted inside
`ProjectVideoSettings`, so it *must* be in core. `photonic-core` already owns
`crash_dir` (`diagnostics.rs:29`, which `presets.rs:402` calls into) and already
depends on `serde_json` (`crates/photonic-core/Cargo.toml:12`), so this
introduces **no dependency and no second directory-resolution implementation**.
The path-parameterized `load_profiles_from(&Path)` / `save_profiles_to(&Path)`
hooks are copied from `presets.rs:428, 437` so tests never touch a real user
config dir.

**Explicitly no in-document profile *registry*.** 26 K-G1's Files line proposes
*"a `ProjectProfile` registry + a `default_profile`"* in `sequence.rs`. This
document takes the `default_profile` and **rejects the registry**, for three
reasons:

1. A registry is only useful if something *references* it, and a reference from
   `Sequence` to a registry entry is a dangling-id class of bug the group model
   already had to defend against (`ValidationError::UnknownGroup`,
   `sequence.rs:439`). A resolved value cannot dangle.
2. A referencing field on `Sequence` that an older build drops while keeping the
   registry produces a document that is *wrong*, not merely less convenient —
   which is the test [§4](#4-migration-and-format-version-impact) uses for "does
   this need v6". The registry design would force a format version; the value
   design does not.
3. The registry duplicates the user catalogue and creates a sync problem nobody
   has specified (which wins when a project registry and a user catalogue hold
   the same name with different numbers?).

**Compatibility with K-G4 (204), whose S11 gate is exactly this question.**
[ROADMAP §7](../specs/video-editor/ROADMAP.md#7-legalcontentproduct-gates) and
[§8 S11](../specs/video-editor/ROADMAP.md#8-architecture-decisions-and-defaults)
block K-G4 on *"template storage location and bundled-asset manifests"*.
[204 §3.1](204-k-g4-project-templates.md) resolves S11 as
`<config>/Photonic/templates/`, one `.photon`-container file per template, under
the same `photonic_core::crash_dir()` (`diagnostics.rs:29`). K-G1 is consistent
with that resolution, and would also have been consistent with the alternatives:

- K-G1 claims **one flat file name** in the config dir,
  `sequence_profiles.json`, sitting beside `export_presets.json`,
  `preferences.json` and `disk_roots.json` — inside the flat-file family 204 §3.1
  enumerates and deliberately leaves alone. It claims no directory, no bundle
  format and no manifest convention, so it cannot collide with `templates/`.
- **K-G1 must not add a fourth `config_dir` delegation.** 204 §3.5 records that
  `export::presets::config_dir` (`presets.rs:401`) and `welcome::config_dir`
  (`welcome.rs:2078`) are already two one-line delegations to `crash_dir` and
  proposes a single `app_config_dir()` in `photonic-core`. K-G1's store calls
  `crash_dir()` directly, and switches to `app_config_dir()` if 204 lands first.
  Whichever of the two items lands first owns that consolidation; the other
  follows it.
- K-G1 introduces **no bundled asset bytes**, so
  [23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest)'s
  `AssetRightsManifest` — the second half of S11 — has nothing to attach to.
  **K-G1 is therefore not S11-gated and not rights-gated**
  ([§9](#9-clean-room-provenance)). 204 reaches the same conclusion for the same
  reason about its own item.
- A K-G4 template stores whole sequences in a `.photon` container
  (204 §3.1), so it already carries a **resolved** rate and raster rather than a
  profile name — the same anti-dangling choice §3.4 item 1 makes. The two
  designs agree without either depending on the other, and 204 states explicitly
  that K-G4 *"does not need K-G1 and must not wait for it"*. The converse also
  holds: K-G1 does not wait for S11.

### 3.5 The built-in profile set

Derived from published standards and from arithmetic. A raster size and a frame
rate are facts; the table below is Photonic-authored and cites the standard that
*defines* each raster, not any product's preset list.

| Profile name | Raster | `FrameRate` | Raster source |
|---|---|---|---|
| `1080p23.976` | 1920×1080 | `24000/1001` | ITU-R BT.709 HDTV raster |
| `1080p24` | 1920×1080 | `24/1` (`FPS_24`) | ITU-R BT.709 |
| `1080p25` | 1920×1080 | `25/1` (`FPS_25`) | ITU-R BT.709 |
| `1080p29.97` | 1920×1080 | `30000/1001` (`FPS_29_97`) | ITU-R BT.709 |
| `1080p30` | 1920×1080 | `30/1` (`FPS_30`) | ITU-R BT.709 |
| `1080p50` | 1920×1080 | `50/1` | ITU-R BT.709 |
| `1080p59.94` | 1920×1080 | `60000/1001` (`FPS_59_94`) | ITU-R BT.709 |
| `1080p60` | 1920×1080 | `60/1` (`FPS_60`) | ITU-R BT.709 |
| `720p50` | 1280×720 | `50/1` | ITU-R BT.709 720-line |
| `720p59.94` | 1280×720 | `60000/1001` | ITU-R BT.709 720-line |
| `2160p23.976` | 3840×2160 | `24000/1001` | ITU-R BT.2020 / SMPTE ST 2036-1 UHD-1 |
| `2160p25` | 3840×2160 | `25/1` | ITU-R BT.2020 / ST 2036-1 |
| `2160p29.97` | 3840×2160 | `30000/1001` | ITU-R BT.2020 / ST 2036-1 |
| `2160p50` | 3840×2160 | `50/1` | ITU-R BT.2020 / ST 2036-1 |
| `2160p59.94` | 3840×2160 | `60000/1001` | ITU-R BT.2020 / ST 2036-1 |
| `DCI 4K 24` | 4096×2160 | `24/1` | SMPTE ST 2048-1 |
| `Vertical 1080×1920 30` | 1080×1920 | `30/1` | Photonic's own `ASPECT_PRESETS` (`ops_bridge.rs:88`) |
| `Square 1080×1080 30` | 1080×1080 | `30/1` | Photonic's own `ASPECT_PRESETS` |

Every rate above satisfies `FrameRate::is_exact()` (`time.rs:141`) — the flick
divides all of them, which is `TICKS_PER_SECOND`'s stated purpose
(`time.rs:11-13`). A test asserts that, so a future addition cannot quietly
introduce a non-dividing built-in ([§8 T1](#8-acceptance-fixtures-and-tests)).

The two social entries are labelled as Photonic-derived, not standards-derived,
because they are platform conventions. They are included because the aspects
already ship in `ASPECT_PRESETS` (`ops_bridge.rs:88`) and a profile picker that
omits the shapes the format bar already offers would be incoherent.

**No interlaced or non-square-pixel built-ins** (no 720×576, no 720×480). §3.3
explains why: the two fields such a profile would need — PAR and scanning — have
no consumer. See [§8.2 Q3](#82-open-questions-each-with-a-recommendation).

---

## 4. Migration and format-version impact

**`CURRENT_FORMAT_VERSION` stays at 5** (`crates/photonic-core/src/document.rs:117`).
K-G1 lands additively inside v5.

1. **The only persisted change is one `Option` field with a serde default**
   (§3.1). `TimelineProject.settings` is itself `#[serde(default)]`
   (`sequence.rs:35`), and `default_profile` is
   `#[serde(default, skip_serializing_if = "Option::is_none")]` — byte-identical
   in shape to how `cache_limit_mb` (`sequence.rs:96`), `markers`, `groups` and
   `MediaAsset.rating`/`tags` were each added inside their versions. A pre-K-G1
   v5 file loads with `default_profile: None`, which is the complete and correct
   meaning: "this project has no default; use `default_frame_rate`."
2. **Sibling consistency.** [193 §3.2](193-k-a1-chunked-timeline-preview-rendering.md)
   appends `preview_profile: Option<PreviewProfile>` to the *same* struct with
   the *same* serde attributes and argues the same conclusion in its §4
   (*"Bump only when data must be reinterpreted"*). The two fields are
   independent and do not collide. Landing one at v5 and the other at v6 would be
   incoherent.
3. **The test for whether a bump is required is: does an older build reading this
   file produce a document that is *wrong*?** Here, no. An older build drops
   `default_profile` and creates its next sequence at `default_frame_rate` and
   1920×1080 — the behaviour it has today. No content, no timing, no appearance
   and no reference is lost. Contrast v3→v4 (`document.rs:107`), which
   *reinterpreted* existing anchor coordinates, and v4→v5, which projected
   `link_group` into a group tree (`crates/photonic-core/src/migration.rs:212`).
4. **A bump has a real cost.** `COMPAT_WINDOW = 1` (`migration.rs:16`): a file may
   be one version ahead and still load. Spending that slot on a preference means
   a genuinely dangerous future change has less room. Spend the window on changes
   that can be wrong.
5. **What *would* force v6**, recorded so it is decided here rather than
   discovered mid-implementation: (a) the in-document **registry** design §3.4
   rejects, because a `Sequence`→registry reference dropped by an older build
   leaves a dangling id; (b) removing `default_frame_rate` in favour of
   `default_profile` ([Follow-ups](#follow-ups) 1); (c) adding a per-sequence
   colour space, which is S3's business and not K-G1's.

**Round-trip obligation.** A v5 document with a `default_profile` must survive
`to_json` → `from_json` → `finalize_load` byte-identically, and a v5 document
*without* the key must load with `None` and re-serialize without the key
(`skip_serializing_if` guarantees no gratuitous diff on an untouched project).
Both are pinned in [§8](#8-acceptance-fixtures-and-tests) (T7, T8).

---

## 5. Undo unit and its exact inverse

Repo rule: one user verb = one undo unit (01 §10.0, 39 §1). Every row is **one**
history entry.

| User verb | Command | Exact inverse |
|---|---|---|
| **New sequence from a profile** (`+` in the tab strip, `create_sequence`) | one `AddSequence` (`commands.rs:451`) — **unchanged**; the profile only supplies the constructor arguments | `RemoveSequence` (`commands.rs:2206`) |
| **First video action + new sequence** | `Command::Batch([CreateProject, AddSequence])` — **unchanged**; this is what `handlers/video.rs:310-320` already emits | reversed batch of inverses (`history/mod.rs:3172`) |
| **Apply a profile to an existing sequence** (settings sheet, `set_sequence_profile`) | one `SetSequenceProfile { seq, old, new }` | swap `old`/`new` — restores the exact prior `frame_rate` **and** the exact prior `formats[0]`, including its `name` |
| **Match a sequence to a clip** (adjust-profile-to-clip) | the *same* `SetSequenceProfile`; the shape is read from the clip's `VideoStreamInfo` (`media.rs:185-189`) before the command is built | same |
| **Set the project default profile** | one `SetDefaultProfile { old, new }` | swap `old`/`new`; `new: None` clears, and its inverse restores |
| **Save / rename / delete a catalogue profile** | **none** — app-level config | **No undo entry, deliberately.** See below |

**Why catalogue edits are not undoable, stated rather than assumed.**
`CommandHistory` inverts commands against a `Document`
(`crates/photonic-core/src/history/`); a JSON file in the user config directory
is not in the `Document`, so an "undo" of a catalogue write would be a history
entry that mutates state the history does not own — the same reasoning that made
`save_export_preset` *"app-level config, 05 §3.6 — no document mutation, no undo
step"* (`docs/mcp-api.md:3607`, handler `handlers/video.rs:4444`). This is a
deliberate asymmetry with `SetDefaultProfile`, which **is** undoable precisely
because it *is* document state.

**Atomicity.** `ops::set_sequence_profile` validates before it returns: the
sequence must exist, `format.width`/`height` must be non-zero, and
`frame_rate.num` must be > 0 (`ticks_per_frame` debug-asserts it, `time.rs:109`).
A rejection yields `EditError` and produces no command and no document change —
the existing `ops::` contract, which groups do not get an exception from either
([194 §5](194-k-a5-general-and-nested-clip-groups.md)).

**Coalescing.** Both commands commit through `execute_discrete`, as
`set_generate_proxies_on_import` and every settings-shaped edit already do
(`ops.rs:316`; `handlers/video.rs:320`), so a profile change never folds into an
adjacent timeline gesture.

---

## 6. What happens to existing clips — the PA-8 payoff

This is the question that makes profile changes frightening in a
frame-count-based NLE, and the answer here is short because
[26 §5 PA-8](../specs/video-editor/26-kdenlive-mlt-parity.md#5-photonic-ahead-register-pa---do-not-port-backwards)
already did the work.

### 6.1 Timing: nothing moves

`Tick` is absolute flicks (`time.rs:13, 23`) and every clip stores `start`,
`duration` and `source_in` in ticks (`clip.rs:27-38`). **A frame-rate change
moves no clip, changes no duration, and changes no source offset.** Not "moves
them consistently" — moves them *not at all*. The reference's `mlt_position` is
an `int32_t` frame count in one profile timebase (PA-8's right column), which is
exactly why changing a project profile there re-times a timeline. Photonic has no
such coupling, and K-G1 must not introduce one by "helpfully" rescaling anything.

`SetSequenceProfile` therefore writes two fields and reads no clip. That is the
whole apply step.

### 6.2 What genuinely changes

| Aspect | Effect | Already handled by |
|---|---|---|
| **Frame grid / labelling** | The ruler, timecode, step and snap all re-derive from `frame_rate` (`time.rs:108-157`). A cut that was on frame 100 at 25 fps is at the same *tick* and a different *frame index* | existing derivation |
| **Drop-frame labelling** | `is_drop_frame_rate()` (`time.rs:157`) flips when moving to/from 30000/1001 or 60000/1001. `start_timecode` is a `Tick` (`sequence.rs:166`) so it does not move; its **label** changes | K-A12, landed |
| **Conform** | Clips whose source rate now differs are sampled by nearest-covering source frame (38 §3.2), and the compiler already emits `CompileCode::FrameRateConformed` once per clip (`compile.rs:1314`) and once per nest (`compile.rs:1472`) | 38 §3.5, landed |
| **Frame alignment of existing cuts** | Edges frame-aligned at the old rate generally are not at the new one. The model permits this (§2.2 item 7) | nothing — see §6.3 |
| **Raster** | A larger/smaller `formats[0]` changes the output canvas. `Clip.transform` uses `AnchorSpace::CenterOffset` by default (`clip.rs:563-570`), so a centred clip stays centred; legacy `AnchorSpace::Absolute` transforms (v3 files) are in output pixels and *will* shift | v3→v4 tagged them (`document.rs:107`) |
| **Reframes above index 0** | `Clip.reframe[n]` for `n > 0` was authored against the **old** `formats[0]` box, per `fit_clip_to_format`'s content-box convention (`ops.rs:1301-1311`) | nothing — see §6.3 |

### 6.3 The two things K-G1 must decide, and the decisions

**Do not re-snap clip edges.** An implementer will want to call `FrameRate::snap`
(`time.rs:126`) on every clip after a rate change. Refuse, for three reasons:
it moves user content inside a verb the user called "settings"; it is a second,
hidden edit that would have to be part of the same undo unit; and moving edges
can create overlaps, which trips `ValidationError::OverlapOrUnsorted`
(`sequence.rs:393`) and therefore the `validate()` debug assert after every
command (`commands.rs:1748-1757`). **Report it instead** (§6.4). A user who wants
alignment has per-clip verbs already.

**Do not rescale reframes.** `formats[1..]` and their `Clip.reframe` entries are
left exactly as authored. Silently rewriting user transforms is worse than a
warning, and `ops::fit_clip_to_format` (`ops.rs:1312`) already gives the user an
explicit, undoable re-fit path if they want one. Report which format indices are
now stale.

### 6.4 The pre-flight impact report

One pure core function, called by both arms — the CAP-019 parity mechanism, and
the same `dry_run` shape [196 §7.1](196-x-2-opentimelineio-interchange.md) uses:

```rust
// crates/photonic-core/src/timeline/ops.rs — pure, no mutation, no command.
pub fn profile_impact(
    p: &TimelineProject,
    seq: SequenceId,
    new: &SequenceShape,
) -> Result<ProfileImpact, EditError>;

/// Not persisted, not a diagnostic — the answer to "what will this do?"
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileImpact {
    /// Clips whose start or end is not on a frame boundary at `new.frame_rate`.
    pub off_frame_clips: usize,
    /// Clips whose source rate differs from `new.frame_rate` (i.e. will be
    /// conformed) — matching `compile.rs:1314`'s rational comparison.
    pub conformed_clips: usize,
    /// Format indices > 0 whose reframes were authored against the old
    /// `formats[0]` box and are now stale (§6.3).
    pub stale_reframe_formats: Vec<usize>,
    /// The drop-frame labelling convention flips (`time.rs:157`).
    pub timecode_relabelled: bool,
    /// `new.frame_rate.is_exact()` — false means `ticks_per_frame` rounds.
    pub rate_is_exact: bool,
    /// The raster changes, so `AnchorSpace::Absolute` transforms shift.
    pub absolute_anchor_clips: usize,
}
```

**Counts, not clip lists.** A 500-clip sequence changing rate would produce 500
entries, which is
[196 §12.1's](196-x-2-opentimelineio-interchange.md) report-fatigue failure and
the same reason `DiagnosticLog` coalesces by `Subject` (`diag.rs:69`). An agent
or a user who wants the specific clips has `list_clips`.

**No new `DiagCode`.** The conform condition already has one in the right place
(`CompileCode::FrameRateConformed`, `compile.rs:144`, which 38 §3.5 owns and
`compile.rs:132-133` records as a mechanical rename away from the shared registry).
`ProfileImpact` is a *return value* of a verb the user just invoked, not an
asynchronous condition the user must be notified about — the distinction 36 draws.
Adding `Project*` codes for it would put a synchronous answer on an asynchronous
channel.

---

## 7. MCP surface

Warranted, and not optional. CAP-019 parity is
[ROADMAP §10](../specs/video-editor/ROADMAP.md#10-definition-of-done) point 3,
and [26 §5 PA-11](../specs/video-editor/26-kdenlive-mlt-parity.md#5-photonic-ahead-register-pa---do-not-port-backwards)
records full MCP parity as **not yet held** — a GUI-only profile picker would
widen the gap this programme exists to close.

### 7.1 Three new tools

| Tool | Args | Notes |
|---|---|---|
| `list_sequence_profiles` | — | Built-ins (§3.5) then user profiles, each with a `built_in: bool` flag, plus the project's `default_profile`. Mirrors `list_export_presets` (`handlers/video.rs:4427`) exactly, including "use it as the template for save" |
| `save_sequence_profile` | `name: String`, `profile: object` **or** `sequence_id: SequenceId` | Two ways to author: paste a shape, or capture the shape of an existing sequence. Built-in names refused with `NotSupportedV1`, verbatim the guard at `handlers/video.rs:4459`. **No undo step** (§5) |
| `delete_sequence_profile` | `name: String` | Built-ins refused with `NotSupportedV1` (`handlers/video.rs:4493`). **No undo step** |
| `set_sequence_profile` | `sequence_id`, one of {`profile: String` \| `shape: object` \| `from_clip_id: ClipId`}, `dry_run: bool = false` | The apply verb, **undoable**. `from_clip_id` is 26 K-G1's adjust-profile-to-clip, resolved from the clip's `VideoStreamInfo` (`media.rs:185-189`); a clip with no probe, or a non-video clip, is an error, not a guess. Returns `ProfileImpact` in both modes |
| `set_default_profile` | `profile: String?` \| `shape: object?` (both omitted = clear) | **Undoable** (`SetDefaultProfile`) |

`dry_run: true` executes no command, writes no history entry, and returns the
same `ProfileImpact` payload — so the GUI confirmation sheet and an agent see
byte-identical text. This is [196 §7.1](196-x-2-opentimelineio-interchange.md)'s
argument applied to a smaller verb, and it is what makes T14 meaningful.

### 7.2 One additive change to an existing tool

`create_sequence` gains `profile: Option<String>`, and `frame_rate` / `formats`
become optional (`#[serde(default)]` on
`CreateSequenceArgs`, `crates/photonic-mcp/src/protocol/args/video.rs:98-104`):

- `profile` set → `frame_rate` and `formats[0]` come from it; an explicitly
  supplied `formats` beyond index 0 is still honoured (a caller may want the
  profile plus extra CAP-012 aspects in one call).
- `profile` omitted, `frame_rate` + `formats` supplied → **exactly today's
  behaviour**, byte-for-byte. Every existing caller and the AS-1 acceptance story
  (`crates/photonic-app/tests/acceptance_stories.rs:35`) are unaffected.
- Neither supplied → fall back to the project's `default_profile`; if that is
  `None` too, error naming both remedies. `name` stays required.

A separate `create_sequence_from_profile` tool was considered and rejected: 26
§16 records MCP surface debt as a real cost, and a whole tool for one optional
argument grows the catalogue for nothing.

`list_sequences` needs **no change** — it already returns `frame_rate`,
`formats` and `active_format` (`handlers/video.rs:373-382`), which is the full
applied shape.

### 7.3 Wiring and the docs gate

Arg structs in `crates/photonic-mcp/src/protocol/args/video.rs` beside
`CreateSequenceArgs` (`:98`); handlers in `handlers/video.rs` beside
`create_sequence` (`:292`) and the preset trio (`:4427-4514`); dispatch arms in
`crates/photonic-mcp/src/dispatch.rs` beside `"create_sequence"` (`:2150`);
names added to the tool-name lists (`handlers/video.rs:8278`, and the
`VIDEO_TOOL_NAMES` consistency test at `:8277`); then `schema_gen.rs`.

**CI gates the docs**: `.github/workflows/ci.yml:163-167` regenerates
`docs/mcp-api.md` from `dump_tools` and fails on any diff. Regeneration is
mandatory, not a follow-up.

### 7.4 GUI route

1. **New-sequence picker.** `seq_tabs.rs:241` currently calls
   `create_sequence_tab(doc, history, frame_rate, width, height, …)` with the
   *active* sequence's values (`seq_tabs.rs:68-75`). K-G1 resolves the shape as:
   `settings.default_profile` → else the active sequence's shape (today's
   behaviour) → else `settings.default_frame_rate` + 1920×1080 (today's fallback,
   `seq_tabs.rs:71-75`). A caret beside `+` opens the profile list.
   `ops_bridge::create_sequence_tab` (`ops_bridge.rs:100`) keeps its current
   signature and gains a `create_sequence_tab_with_shape(&SequenceShape)` sibling
   it delegates to, so the headless GUI-arm test path does not change shape.
2. **Sequence settings.** The tab-strip right-click menu (`Duplicate`
   `seq_tabs.rs:165`, `Rename…` `:169`) gains **"Sequence settings…"**, opening a
   sheet with the profile list, a free-form rate/raster entry, "Match to
   selected clip", and the `ProfileImpact` summary rendered **before** the commit
   button. "Save as profile…" and "Manage profiles…" live in the same sheet.
3. **Explicitly unchanged.** The monitor's format bar (`monitor.rs:1264` →
   `switch_to_aspect`, `ops_bridge.rs:47`, over `ASPECT_PRESETS`,
   `ops_bridge.rs:88`) is untouched. It authors *additional* CAP-012 formats on
   one sequence; K-G1 never writes through it (§2.3). Conflating the two would
   be the PA-6 regression this document exists to prevent.

---

## 8. Acceptance fixtures and tests

**No rights-cleared content. K-G1 is not a gated item.** Every test below builds
its document programmatically with `ClipSource::SolidColor` / `Adjustment` clips
and synthetic `MediaProbe` values — the convention
`crates/photonic-app/tests/acceptance_stories.rs:33-35` already records
(*"Solid-color clips are used deliberately: they carry no media asset"*) and the
one `compile.rs:4143-4160` already uses for probe-driven rate tests. No media
bytes, no ffmpeg, no GPU, no `AssetRightsManifest`
([23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest)).
Fixture weight: **zero bytes**.

| # | Test | Where | Asserts |
|---|---|---|---|
| T1 | Built-in table: exact count, exact order, and `is_exact()` true for **every** built-in rate | `timeline/profile.rs` `mod tests` | §3.5; mirrors `presets.rs:457`'s `catalog_has_exactly_nine_built_ins_in_table_order` |
| T2 | `assert_undo_roundtrip` (`ops.rs:2921`) for `SetSequenceProfile` and `SetDefaultProfile` | `ops.rs` `mod tests` | Apply→undo is identity; redo re-applies |
| T3 | **Applying a profile moves no clip**: every `start`/`duration`/`source_in` byte-identical after a 25 → 24000/1001 change; exactly **one** history node | `crates/photonic-core/tests/timeline.rs` | §6.1 — the PA-8 claim, proven not asserted |
| T4 | Apply preserves `formats[1..]`, `active_format`, `start_timecode`, `work_range`, and **every** `Clip.reframe` key | `ops.rs` `mod tests` | §2.4 / §6.3 — the reframe-orphan guard |
| T5 | `profile_impact` counts: a sequence with 3 frame-aligned and 2 off-frame edges at 25 fps reports the right `off_frame_clips` at 24000/1001; `timecode_relabelled` flips only across the drop-frame boundary | `ops.rs` `mod tests` | §6.4 |
| T6 | After a rate change, `CompileCode::FrameRateConformed` fires **once per clip** whose source rate now differs, and **not** for clips that still match | `crates/photonic-video` compile tests (beside the `video_asset_rate` helper, `compile.rs:4144`) | §6.2; 38 §3.5 not regressed |
| T7 | A document with `default_profile` saves at `format_version == 5` and round-trips `to_json`→`from_json`→`finalize_load` unchanged; one without the key loads with `None` **and re-serializes without the key** | `crates/photonic-core/tests/forward_compat.rs` | §4 |
| T8 | Forward-compat: a v5 document carrying `default_profile` loads in a build that does not know the field, dropping it, with the document otherwise intact | `tests/forward_compat.rs` (beside `newer_build_document_loads_and_preserves_all_unknown_variants:181`) | §4 point 3 |
| T9 | Catalogue store: save → list → delete round trip through `load_profiles_from` / `save_profiles_to` on a temp path; a missing file is not an error | `timeline/profile.rs` `mod tests` | §3.4; never touches the real config dir (`presets.rs:428, 437` pattern) |
| T10 | `save_sequence_profile` / `delete_sequence_profile` refuse a built-in name with `NotSupportedV1` | `handlers/video.rs` `mod tests` | §7.1; mirrors `:4459`, `:4493` |
| T11 | Adjust-to-clip: a clip whose probe reports 3840×2160 @ 24000/1001 yields exactly that `SequenceShape`; a clip with `probe: None` errors rather than guessing | `ops.rs` `mod tests` | §7.1 |
| T12 | `dry_run: true` returns a populated `ProfileImpact`, writes **no** history entry, and leaves `to_json` byte-identical | `handlers/video.rs` `mod tests` | §7.1 |
| T13 | A non-`is_exact()` rate (e.g. `47952/1000`) is **accepted** and flagged `rate_is_exact: false`, not refused | `ops.rs` `mod tests` | §8.1 risk 5 — not porting a limitation |
| T14 | GUI arm, headless: create-from-profile and sequence-settings-apply through `ops_bridge` | `crates/photonic-gui/tests/video_ui_paths.rs` | ROADMAP §10 point 2 |
| T15 | **CAP-019 parity story**: MCP arm (`create_sequence` with `profile` → `set_sequence_profile`) vs GUI arm (`ops_bridge`), structural compare | `crates/photonic-app/tests/acceptance_stories.rs` | ROADMAP §10 point 10 |
| T16 | `create_sequence` with `frame_rate` + `formats` and **no** `profile` produces a byte-identical document to today's | `handlers/video.rs` `mod tests` | §7.2 back-compat |

### Definition of done → [ROADMAP §10](../specs/video-editor/ROADMAP.md#10-definition-of-done)

| # | Requirement | How K-G1 answers it |
|---|---|---|
| 1 | Core op/engine service with unit tests | `timeline/profile.rs` (types, built-ins, store) + `ops::set_sequence_profile` / `set_default_profile` / `profile_impact`; T1–T5, T9, T11, T13 |
| 2 | GUI route, or a recorded exception | §7.4 — picker, settings sheet, profile manager. **No exception sought**; T14 |
| 3 | MCP tool/schema/generated docs | §7.1–7.3; `docs/mcp-api.md` regenerated, `ci.yml:163-167` gate green |
| 4 | One verb = one undo unit; undo/redo identity | §5; T2, T3, T12 |
| 5 | Additive serde/migration round-trip when the model changes | §4; T7, T8 |
| 6 | Pixel/audio IR/eval/golden/sync coverage | **Partial and stated**: K-G1 adds no IR op and no pixel path, but it *changes the sequence rate*, which the compiler observes. T6 is the coverage that owes, and it is a compile-diagnostic assertion, not a new golden |
| 7 | Hard gates green; trend metrics not regressed | No new budget. `profile_impact` is O(clips) and runs once per user gesture, never per frame. Assert it stays off the paint path (§8.1 risk 4) |
| 8 | Offline/privacy/licensing/content/product gates | Offline: a JSON file in the config dir, no network. Content: **none** — no bundled bytes (§9). **Not S11-gated** (§3.4) |
| 9 | Protected surfaces not regressed | **PA-6** is the whole point (§1, §2.3); **PA-8** is what §6.1 proves; **PA-9** — failures are `EditError`, never strings. T3, T4, T15 are their anchors |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | L1 `profile.rs` exists → L2 real built-ins + real store → L3 wired into `seq_tabs`/`dispatch` → L4 a real project created from a profile and re-shaped; T15 pins parity |

---

## 8.1 Risks

1. **Porting the global-profile limitation backwards.** The highest-consequence
   failure: an implementer reads 26 K-G1's title, builds a project-wide profile,
   and adds a "sequences must match the project" validation. That regresses PA-6,
   a protected surface, under cover of a new feature. Mitigation: there is no
   command in this design that writes more than one sequence, and `SequenceShape`
   is never stored at project scope except as an unenforced *default*. Review
   should reject any `for seq in project.sequences` loop in a K-G1 diff.
2. **Collapsing profile into `FormatOp`.** Implementing apply as
   remove-then-add on `formats` would delete CAP-012 variants and shift every
   `Clip.reframe` key (§2.4). Mitigation: `SetSequenceProfile` writes
   `formats[0]` in place; T4.
3. **The re-snap temptation** (§6.3). It would move content inside a settings
   verb and can trip the `validate()` debug assert (`commands.rs:1748`). T3 is
   the regression anchor, and it asserts byte-identity rather than "close enough".
4. **`profile_impact` on the paint path.** It walks every clip on every track. It
   is a gesture-time call, not a frame-time one. The settings sheet must compute
   it once when the candidate shape changes, not per repaint — the same discipline
   [194 §8.1](194-k-a5-general-and-nested-clip-groups.md) records for
   `group_members`.
5. **Non-dividing custom rates.** A user-authored profile at a rate that does not
   divide the flick makes `FrameRate::is_exact()` false (`time.rs:141`) and
   `ticks_per_frame` round (`time.rs:108-115`). **Accept and flag; do not
   refuse** — the model supports it, `is_exact` exists precisely to surface it,
   and refusing would be inventing a limitation. T13.
6. **Catalogue portability.** A profile saved on one machine is not in the
   `.photon`. Mitigated by `default_profile` storing *resolved* values (§3.1), so
   the project's own shape always travels; only the *list* is machine-local.
   That is the same trade export presets already make (`presets.rs:401`).
7. **Config-dir absence.** `crash_dir()` returns `None` when neither `APPDATA`,
   `XDG_CONFIG_HOME` nor `HOME` is set (`diagnostics.rs:29-40`). The catalogue
   must degrade to "built-ins only", never fail a sequence creation — matching
   `PresetStoreError::NoConfigDir` (`presets.rs:387`), which is surfaced and not
   panicked on.

## 8.2 Open questions (each with a recommendation)

- **Q1 — May a profile be applied to a sequence that already has clips?**
  *Recommendation: **yes**, with the §6.4 impact report shown first.* Refusing
  would make the feature useless for the case that motivates it (footage imported
  before the project shape was settled), and §6.1 shows the operation is
  non-destructive to timing. This is a product call because it decides how
  destructive a "settings" dialog is allowed to be.
- **Q2 — Should a `Sequence` record the name of the profile it came from?**
  *Recommendation: **no**.* The moment a user edits the raster the label is
  false, and a label that lies is worse than none. If product wants it for
  display, `Sequence` gaining an `Option<String>` later is serde-additive and
  needs no format step — the decision is reversible in one direction only, so
  take the reversible side now.
- **Q3 — Should the built-in table ship SD / interlaced profiles (720×576,
  720×480)?** *Recommendation: **no in v1**.* Both would need a sequence PAR and
  a scanning field, and neither has a consumer (§2.2 items 4 and 6). Shipping
  them would put two inert controls in a settings dialog. Product call because it
  excludes a real archival workflow; the honest answer is "when K-G6's scan
  handling and a square-pixel-aware composite both reach the sequence level".
- **Q4 — Should the default live in the document or in user preferences?**
  *Recommendation: **the document** (`ProjectVideoSettings`).* Different projects
  have different deliverable shapes, and `default_frame_rate` is already there
  (`sequence.rs:99`) and already read (`app/timeline/mod.rs:905`). A user-level
  default would be wrong the first time someone works on two projects. Genuine
  product call; if product prefers user-level, the fallback is to keep
  `SetDefaultProfile` and add a user-preference layer *below* it, which is
  strictly additive.
- **Q5 — Should `ExportPreset`'s `ResolutionSpec` + `FrameRatePolicy`
  (`presets.rs:20-33`) unify with `SequenceShape`?** *Recommendation: **not in
  K-G1**.* They answer different questions (what the timeline *is* vs what a
  delivery *becomes*), and `FrameRatePolicy::MatchSequence`
  (`panels/video/export_dialog.rs:854`) already expresses the link. Recorded as a
  follow-up so it is a decision, not an oversight.

## 8.3 Deliberately excluded

- **A global project profile, or any cross-sequence enforcement.** 26 §5 PA-6.
- **A sequence pixel/sample aspect and anamorphic support.** Nothing consumes it
  (§2.2 item 4). It is a genuine gap for anamorphic acquisition, and it is a
  pipeline item, not a profile item.
- **A per-sequence colour space or HDR working state.** [S3](../specs/video-editor/ROADMAP.md#8-architecture-decisions-and-defaults).
- **A project scanning / field-order setting.** K-G6 derives it per source
  (`compile.rs:1343`), which is better; a project flag would be a regression.
- **Auto-applying a profile on import** ("this footage is 4K, change the
  project?"). That is K-C7's import-triage surface, and a prompt that mutates the
  project on file open is a different product decision.
- **Removing `default_frame_rate`.** v6 ([Follow-ups](#follow-ups) 1).
- **Fixing the `FormatOp::Insert`/`Remove` reframe-key shift** (§2.4). Real,
  pre-existing, reachable from MCP today, and not K-G1's to fix inside a feature
  change ([Follow-ups](#follow-ups) 3).
- **K-G4's template storage decision.** S11 owns it; §3.4 states compatibility
  with either outcome and claims nothing beyond one file name.

---

## 9. Clean-room provenance

Per [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md) and
[23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol):

- **Sources used.** (a) Photonic's own code and specs, cited by `file:line`
  throughout and re-verified against the tree at `8a33f32`; (b) 26 K-G1's
  requirement statement, itself derived from Kdenlive's `CC-BY-SA-4.0` user
  documentation as a *requirements* source, cited and never pasted; (c) the
  **published raster standards** named in §3.5 — ITU-R BT.709, ITU-R BT.2020 /
  SMPTE ST 2036-1, SMPTE ST 2048-1 — used only for the integer dimensions they
  define. A raster size and a frame rate are facts, not protectable expression.
- **Sources not used.** The Kdenlive source tree (including its profile
  database), the MLT/`mlt++` source tree (including `mlt_profile` and its
  bundled profile files), frei0r, and any GPL/LGPL derivative were not inspected
  for this item. **No profile name, no table ordering, no field set and no
  default value below is transcribed from any of them** — the built-in table in
  §3.5 is generated from the cross-product of standard rasters and standard
  rates, and its two non-standard entries come from Photonic's own
  `ASPECT_PRESETS` (`ops_bridge.rs:88`). The implementer records the 23 §3.4
  attestation for the `core-timeline` subsystem and an independent reviewer
  checks provenance before merge (26 §2 point 2).
- **Design origin.** Every concrete decision derives from a Photonic constraint,
  not from a reference product: the profile-as-value rule from 26 §5 PA-6; the
  single-command shape from `SetGenerateProxiesOnImport` (`commands.rs:447`); the
  storage split from `export/presets.rs:401-443` and the "no undo step" wording
  at `docs/mcp-api.md:3607`; the `formats[0]` slot from `fit_clip_to_format`'s
  own doc comment (`ops.rs:1301-1311`); the "nothing moves" guarantee from
  `TICKS_PER_SECOND` (`time.rs:13`); the reporting-not-refusing posture from
  `FrameRate::is_exact` (`time.rs:141`) already existing for the same purpose.
- **Photonic-ahead properties preserved** (26 §5, ROADMAP §9). **PA-6** is
  actively defended, not merely not-regressed: profiles are values applied to one
  sequence, never a global constraint. **PA-8** — shapes carry exact rational
  `FrameRate` and every position stays a `Tick`; no float rate and no frame count
  enters the model. **PA-7** — half-open ranges are untouched; K-G1 reads no
  range. **PA-9** — failures are `EditError`, and the impact report is a typed
  struct, never a formatted string. **PA-1** — no graph or cache key changes;
  `SequenceShape` is not a graph input, and the eval canvas is already a runtime
  argument to `GpuEvaluator::evaluate` rather than part of any content hash, so
  changing `formats[0]` invalidates nothing spuriously and, equally, is not
  smuggled into a persisted key.
- **No reference limitation is ported.** Nesting of formats is unbounded, rates
  outside the built-in table are accepted (T13), sequences in one project may
  differ freely, and no scanning or PAR field is introduced merely because the
  reference has one.
- **No dependency, no bundled asset, no codec, no patent surface.** None of
  [ROADMAP §7](../specs/video-editor/ROADMAP.md#7-legalcontentproduct-gates)'s
  K/E/X gates applies, and **K-G1 is not `legal-or-fixture-blocked`** — it is
  schedulable the moment this document is accepted.

---

## Follow-ups

Changes this document deliberately did **not** make to existing files. Each needs
its own change; none is authorized here.

1. **`ProjectVideoSettings.default_frame_rate` is redundant once
   `default_profile` ships** (`sequence.rs:99`). Removing it is a field deletion
   and therefore a **v6** item, together with re-pointing its one reader
   (`crates/photonic-gui/src/app/timeline/mod.rs:905`). Recorded as a
   one-format-version deprecation, mirroring `Clip.link_group`'s treatment in 35
   §3.3.
2. **26 §15 K-G1's Files line** proposes *"a `ProjectProfile` registry + a
   `default_profile`"* in `sequence.rs`. §3.4 accepts the default and **rejects
   the registry**, with reasons. 26 should be corrected so an implementer does
   not build the registry from the parity doc, and so the effort estimate does
   not include it.
3. **`FormatOp::Insert` / `FormatOp::Remove` do not remap `Clip.reframe` keys**
   (`commands.rs:1849-1866` vs `clip.rs:44`). Reachable today via MCP
   `set_sequence_format` `op=remove` (`docs/mcp-api.md:4142`). This is a real
   defect, pre-dating K-G1 and outside its scope; it deserves its own item, with
   the remap written once in `commands.rs` so both directions of the inverse
   agree.
4. **`ProjectVideoSettings.audio_sample_rate` has no writer either**
   (§2.2 item 3) — it is read by `export/job.rs:200` and
   `export/offline_audio.rs:54` and can only ever be its default. A "project
   audio settings" verb is K-G-shaped and belongs with K-G1's sibling items, not
   inside K-G1.
5. **`ASPECT_PRESETS` is GUI-only** (`ops_bridge.rs:88`, consumed at
   `monitor.rs:1264`). Once §3.5's built-in table exists in core, the GUI table
   should read from it so the two lists cannot diverge — a small cleanup, and
   deliberately not done inside K-G1 so `switch_to_aspect` (a protected CAP-012
   surface) is not touched by a profile change.
6. **[38 §3.5](../specs/video-editor/38-sequence-semantics.md#35-diagnostics)**
   describes the conform diagnostic as `Media::FrameRateConformed`; the shipped
   code has it as a compiler-local `CompileCode::FrameRateConformed`
   (`compile.rs:144`), with `compile.rs:129-133` recording the fold into 36's
   registry as pending. 38 and 36 §3.2 should be reconciled with the code, or the
   fold scheduled. Not K-G1's to do, but K-G1 depends on that code staying put.
7. **[ROADMAP §0](../specs/video-editor/ROADMAP.md) K-G row** (line 186) lists
   K-G1 as open; it should link this proposal once accepted, and gain a K-G1 row
   in the progress table when the item lands, per the existing convention.
8. **Q5 (§8.2)** — whether `ExportPreset`'s `ResolutionSpec` / `FrameRatePolicy`
   (`presets.rs:20-33`) and `SequenceShape` should share a type. Recorded so the
   duplication is a decision rather than drift.
