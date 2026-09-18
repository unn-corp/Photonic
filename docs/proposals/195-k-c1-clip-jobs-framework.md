# 195 — K-C1 Clip-Jobs Framework

> **Status: Proposed — Band-5 mini-spec, pre-code.**
> [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands) makes an accepted mini-spec the
> exit condition for every K-Band 5 item: it must name the data-model change,
> migration, undo unit, MCP surface and acceptance fixtures *before* code. This
> document discharges that for **K-C1** ([26 §11](../specs/video-editor/26-kdenlive-mlt-parity.md#k-c1--clip-jobs-framework)).
> No code authorization until accepted ([23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)).

**Owner ref:** 26 §11 K-C1 · **Territory:** `photonic-video-engine` · **Effort:** M–L
**Security posture:** this is the item that introduces user-supplied process
execution. §7 is the load-bearing section, not an appendix.

---

## 1. Problem and user outcome

Photonic already runs background work against media — import probing, poster
extraction, keyframe indexing, waveform pyramids, proxy transcodes, MCP
transcodes — but each one is **hand-rolled at its call site**. There is no
catalogue a user can point at a bin selection, no unified progress surface, no
shared cancellation, no failure reporting, and no persistence story. `grep -rn
'clip_job\|ClipJob\|JobTemplate' crates/ docs/` returns clean (verified
2026-07-28, excluding 26 itself).

After this item, a user can:

1. Select one or more assets in the media pool, choose **Jobs →** from a
   catalogue (Transcode to edit-friendly · Extract audio · Generate proxy ·
   Scan loudness), and watch them run in one queue panel with per-job progress
   and a working Cancel.
2. See a failed job as a **badge on the failing asset** plus one coalesced
   toast, with the ffmpeg stderr tail available on demand — instead of the
   current behaviour, where a failed proxy transcode sets
   `ProxyStatus::Failed` and says nothing (`crates/photonic-gui/src/panels/media_pool.rs:395-412`).
3. Restart the editor after a crash and be told *which* jobs were interrupted,
   with partial outputs already swept, and re-queue them in one click.
4. Drive the same catalogue from an agent over MCP, with GUI/MCP parity for
   every built-in kind (§6 records the one deliberate exception).
5. **Opt in**, deliberately and out-of-band, to running their own tool as a job
   — an argv template, never a shell string, never enabled by default, never
   enabled by opening a project (§7).

The value is not the four built-in jobs. It is that proxy generation, K-C4
generators' bake step, D-12 stabilisation, D-15 scene detection, K-C5 archiving
and the E-2 analysis consumers all stop being bespoke threads.

---

## 2. Current state in code

**Three unrelated job mechanisms exist today, plus two raw threads.** All five
are real and all five are cited here because the framework must generalise
*these*, not an imagined case.

| # | Mechanism | Where | Shape |
|---|---|---|---|
| 1 | `JobRegistry` / `JobId` / `JobStatus{Queued,Running,Done,Failed,Cancelled}` | `crates/photonic-mcp/src/handlers/video_jobs.rs:210-330` | `HashMap<Uuid, JobHandle>`, cooperative `Arc<AtomicBool>` cancel, `JOB_RETENTION = 600s` GC. **No queue** — each start tool spawns its own `std::thread`. Lives in the MCP crate, so the GUI cannot see it |
| 2 | `RenderQueue` / `QueueJobId` / `QueueJobStatus` | `crates/photonic-video/src/export/job_queue.rs:30-206` | A real FIFO with one worker thread, `ensure_worker(gpu, tools)`, frozen `Arc<TimelineProject>` snapshot per job, `enqueue_multi`, `MAX_FINISHED = 64` retention. Shipped for K-F1/F2/F3/F4 |
| 3 | Media-pool import worker | `crates/photonic-gui/src/panels/media_pool.rs:127-330` (`spawn_import`) | One detached `std::thread` per import batch, results back over `mpsc` as `ImportMetaResult`. No id, no cancel, no status beyond a `importing: usize` counter |
| 4 | Media-pool proxy worker | `crates/photonic-gui/src/panels/media_pool.rs:353-437` (`spawn_proxy_generation`) | One detached `std::thread`, `proxy_in_flight: HashSet<AssetId>` as the only dedup, results over `mpsc` as `ProxyJobResult`. **`generate_proxy` is passed `&|| false` — cancellation is wired but never triggerable** (`media_pool.rs:404`) |
| 5 | MCP `transcode_media` | `crates/photonic-mcp/src/handlers/video.rs:3993-4110` | Uses (1) for status; spawns its own thread; builds an argv `Vec` correctly; polls `try_wait` with a 100 ms sleep; kills + `remove_file` on cancel. **Does not register the child with `child_registry`**, and **does not validate `args.out_path` at all** — this is finding #4 of [28 §1](../specs/video-editor/28-security-model.md#1-why-this-document-exists) |

Reusable primitives that already exist and must not be re-implemented:

- `crates/photonic-video/src/media/ffmpeg_locate.rs:59` — `locate()` resolves
  `ffmpeg`/`ffprobe` from `$PHOTONIC_FFMPEG_DIR` then `PATH`.
- `crates/photonic-video/src/media/child_registry.rs:140,258` —
  `reap_orphans(cache_dir)` and `arm_parent_death_signal(&mut Command)`
  (Linux `PR_SET_PDEATHSIG`, Windows job object). `ChildRecord.kind` is a
  `&'static str` tag; `proxy.rs:308` already arms it, `transcode_media` does not.
- `crates/photonic-video/src/media/atomic_write.rs` — `staging_path`,
  `write_atomic`, `sweep_stale_staging` (temp-and-rename, 37 §2.3).
- `crates/photonic-video/src/media/proxy.rs:284-337` — `generate_proxy` is the
  reference subprocess loop: staging path, `lower_background_priority`,
  parent-death arming, cooperative cancel, `stderr_tail` on failure, rename on
  success.
- `crates/photonic-video/src/graph/analysis.rs:26-260` — E-2's substrate:
  typed `AnalysisResult`, `AnalysisCache` keyed on `ContentHash`,
  `analyze_loudness`/`loudness_cached`, `analyze_histogram`/`histogram_cached`.
- `crates/photonic-core/src/diag.rs:75-105,142-165,434-521` — `Diagnostic`,
  `Subject`, `DiagFamily` (ten families), `DiagnosticLog` with coalescing.
- `crates/photonic-core/src/timeline/ops.rs:100-320` — `add_asset`,
  `remove_asset`, `set_asset_proxy`, `set_asset_meta`, `relink_asset`.
- `crates/photonic-core/src/history/stacks.rs:403` — `execute_discrete`, the
  "commit exactly one undo unit from a background completion" primitive the
  proxy path already uses (`crates/photonic-gui/src/app/mod.rs:2596`).
- `crates/photonic-video/src/export/presets.rs:401-420` —
  `config_dir()` (= `photonic_core::crash_dir()`, `crates/photonic-core/src/diagnostics.rs:29`)
  and `export_presets.json`: the precedent for a user-editable JSON catalogue
  outside the document.
- `crates/photonic-mcp/src/auth.rs:52-76` — owner-only (`0o600`) config-file
  write, the precedent for template-file permissions.

**Does not exist yet, stated plainly:**

- No `PathPolicy` / `PathVerdict` / `DenyReason` type. [28 §3.1](../specs/video-editor/28-security-model.md#31-the-rule)
  specifies it fully; only the *diagnostic code* landed
  (`crates/photonic-core/src/diag.rs:250`, `SecurityPathNotPermitted`).
  `grep -rn 'PathPolicy\|path_policy' crates/` returns clean.
- `RenderQueue` has **no MCP surface** — `grep -rn 'RenderQueue\|render_queue'
  crates/photonic-mcp/` returns clean. MCP `export_sequence`
  (`handlers/video.rs:4134`) spawns its own thread against `JobRegistry`
  (`handlers/video.rs:4257-4267`) rather than the shared queue.
- No job persistence of any kind. `RenderQueue` and `JobRegistry` are both
  process-lifetime only.
- No `Job` diagnostic family and no job-related `DiagCode`.

---

## 3. Data-model change

### 3.1 Decision: `photonic-video/src/jobs/` — a sibling of `RenderQueue`, not a merge

`RenderQueue` is **not** subsumed and clip jobs are **not** folded into it.
The reasoning, which is the main architectural call in this document:

- `RenderQueue::ensure_worker` **requires a `GpuContext`**
  (`job_queue.rs:82`) and every entry carries a frozen
  `Arc<TimelineProject>` (`job_queue.rs:43`). Clip jobs must run with no GPU
  adapter (CI, headless, the adapter-skip convention) and must run when the
  document has no sequence at all — an asset transcode is meaningful before a
  timeline exists.
- Merging would mean either making `GpuContext` optional on a shipped, tested
  export path, or forcing every clip job to snapshot a whole project it never
  reads. Both are regressions in working code for no user-visible gain.
- Their progress vocabularies are genuinely different: `QueueJobStatus::Running
  { frame, total, fps }` is frame-accurate because export knows its frame
  count; a transcode knows only a byte/time fraction.

What *is* shared: **one status vocabulary and one read model.** `jobs::JobView`
is a read-only projection that `RenderQueue`, `ClipJobQueue` and the legacy
`JobRegistry` all map into, so the GUI queue panel and MCP `list_jobs` show one
list. `RenderQueue` gains a `fn view(&self) -> Vec<JobView>` adapter and is
otherwise untouched.

**Subsumed by K-C1 (deleted at their call sites):** `spawn_proxy_generation`
(`media_pool.rs:353`) becomes `JobKind::GenerateProxy`; MCP `transcode_media`'s
bespoke thread (`handlers/video.rs:4026-4110`) becomes `JobKind::Transcode`
behind the unchanged wire tool. **Not subsumed in v1:** `spawn_import`
(`media_pool.rs:127`) — it is the L0→L5 readiness ladder from
[24](../specs/video-editor/24-preview-media-load.md), not a user-invoked job;
migrating it is a follow-up.

### 3.2 New engine types (not serialized)

```rust
// crates/photonic-video/src/jobs/mod.rs
pub struct ClipJobId(pub u64);                  // monotonic, like QueueJobId

pub enum JobKind {
    Transcode { preset: TranscodePreset },      // "edit-friendly" (26 §11 item 1)
    ExtractAudio { format: AudioExtractFormat },
    GenerateProxy,
    Analyze { pass: AnalysisPass },             // v1: Loudness only
    UserDefined { template: TemplateId, params: Vec<(ParamId, ParamValue)> },
}

pub enum JobState {
    Queued,
    Running { fraction: Option<f32>, message: String },
    Done { outcome: JobOutcome },
    Failed { diagnostic: Diagnostic },          // 36's type, not a String
    Cancelled,
}

/// The complete set of document mutations a job may request. A job — including
/// a user-defined one — cannot express any other change to the model.
pub enum JobOutcome {
    NewAsset { path: PathBuf, derived_from: AssetId, role: DerivationRole },
    AttachProxy { asset: AssetId, proxy: ProxyRef },
    Analysis { asset: AssetId, result: AnalysisResult },   // no model change
    None,
}
```

`JobOutcome` being a closed enum is a **security property**, not just tidiness:
it bounds the blast radius of a user-defined template to "add a bin row",
"attach a proxy", or "nothing".

### 3.3 The one serialized change: `MediaAsset.derived_from`

```rust
// crates/photonic-core/src/timeline/media.rs — appended to MediaAsset
/// K-C1: this asset was produced by a clip job from another asset.
/// Additive; absent in files written before K-C1.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub derived_from: Option<AssetDerivation>,

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssetDerivation {
    pub source: AssetId,
    pub role: DerivationRole,      // Transcode | ExtractedAudio | UserDefined
    pub job_kind: String,          // stable kind tag, for display and K-C5
}
```

Why it is needed rather than nice-to-have: without it, "transcode to
edit-friendly" drops an orphan row into the bin with no relationship to its
source. K-C5 archiving cannot tell a derived file from an original, K-C6 relink
cannot follow the chain, and re-running a job cannot be idempotent. It is three
fields.

**Jobs never mutate an existing asset's `source`.** Replacing what a bin row
points at is relink (K-C6) and is a different user intent with a different undo
unit. A transcode adds a row; the user chooses whether to use it.

**No other model change.** Templates are *not* in the document (§7.2). Job
state is *not* in the document. `ProjectVideoSettings`
(`crates/photonic-core/src/timeline/sequence.rs:90-121`) gains nothing.

---

## 4. Migration

**`CURRENT_FORMAT_VERSION` stays at 5. This does not need a v6.**

`crates/photonic-core/src/document.rs:117` pins the current version at 5, and
`crates/photonic-core/src/migration.rs:43-56` defines a `Migration` as a
function that *reinterprets existing data* on the way from N to N+1. K-C1
reinterprets nothing:

- `derived_from` is `#[serde(default, skip_serializing_if = "Option::is_none")]`
  — byte-identical to how `probe`, `proxy`, `content_hash`, `bin`, `rating` and
  `tags` were each added to `MediaAsset` (`media.rs:48-73`). `rating`/`tags`
  landed for K-C2 the same way, inside v5, with no bump.
- An old file loads with `derived_from: None`, which is the correct and
  complete meaning ("not known to be derived").
- A K-C1 file opened by an older build omits the field entirely when `None`,
  and when `Some` it is preserved by [39 §2.2](../specs/video-editor/39-document-lifecycle.md)'s
  unknown-preserving machinery (landed, `a05ec8e`; see
  `crates/photonic-core/tests/forward_compat.rs`).

A version bump would be actively wrong here: it would force every v5 project
through a no-op migration step and would make `MigrationV5ToV6` a lie about
what changed. **Bump only when data must be reinterpreted.**

Required migration work is therefore one round-trip test, not a migration:
`crates/photonic-core/tests/timeline.rs` gains an assertion that a v5 document
containing `derived_from` round-trips, and that a v5 document *without* it
loads with `None` and re-serializes without the key.

---

## 5. Undo unit

**One user verb = one undo unit. Submitting a job is not the verb; completing
it is.**

Starting an ffmpeg process is not a document change, so it produces no history
entry. "Undoing" a running job would have to kill a process, which is *cancel*
— a different affordance with a different button. Every terminal job commits
**exactly one** `Command` through `history.execute_discrete`
(`crates/photonic-core/src/history/stacks.rs:403`), which is precisely how
proxy completions already behave (`crates/photonic-gui/src/app/mod.rs:2582-2599`).

| `JobOutcome` | Command | Exact inverse |
|---|---|---|
| `NewAsset` | `TimelineCmd::AddAsset { asset }` (`ops.rs:100`) | `RemoveAsset { asset }` (`ops.rs:107`) — removes the bin row **and does not delete the file on disk** |
| `AttachProxy` | `TimelineCmd::SetAssetProxy { asset, old, new }` (`ops.rs:277-290`) | restores `old` verbatim, including `None` |
| `Analysis` | none | none — see below |
| `None` | none | none |

Two rules that follow and must be implemented as written:

1. **Undo never deletes a produced file.** A transcode's output is a user
   artefact in a user-chosen location; silently unlinking it on Ctrl+Z is data
   loss. Undo detaches the bin row. Redo re-adds the same row pointing at the
   same still-present file.
2. **A batch of N assets produces N undo units, not one.** The completions
   arrive minutes apart; coalescing them would require holding a history
   gesture open across background work, which the coalescing model
   (`history/stacks.rs:403-410` deliberately clears `coalescing`) does not
   support. Precedent: `drain_finished_proxies` already emits one
   `execute_discrete` per completion. The GUI's "N jobs finished" toast is the
   affordance for the batch, not a history entry.

`Analyze` produces **no undo unit**, and this is the honest answer rather than
an omission: the verb writes into `AnalysisCache` (`graph/analysis.rs:72-100`),
which is a content-hash-keyed derived cache, not model state. There is nothing
to invert; the verb is idempotent and re-runnable. ROADMAP §10 point 4 is
satisfied vacuously — recorded here so a reviewer does not read it as a miss.

---

## 6. MCP surface

GUI/MCP parity holds for every **built-in** kind. It is deliberately broken for
one thing, argued in §7.4.

| Tool | Args | Notes |
|---|---|---|
| `list_job_kinds` | — | Built-in kinds + their params. **Generated** from the `JobKind` enum under the existing doc-drift gate, exactly as `list_effect_kinds` is generated from the manifest catalogue (landed `48fb5da`). Never hand-maintained |
| `start_clip_job` | `{ asset_id, kind, params?, out_path? }` → `{ job_id }` | `out_path` goes through §7.3 containment; refused paths return `PathNotPermitted` |
| `list_jobs` | `{ state? }` | The unified `JobView` list over `ClipJobQueue` + `RenderQueue` + legacy `JobRegistry`. **New capability**: today `RenderQueue` is invisible to MCP entirely |
| `get_job_status` | `{ job_id }` | **Existing tool** (`crates/photonic-mcp/src/dispatch.rs:2628`), extended to resolve ids from all three sources. Do not add a second status tool |
| `cancel_job` | `{ job_id }` | **Existing tool** (`dispatch.rs:2634`), same extension |
| `transcode_media` | unchanged | **Wire-compatible**: the shipped schema (`schema_gen.rs:5864`) does not change. Its handler is re-pointed at `start_clip_job` internally and the bespoke thread at `handlers/video.rs:4026-4110` is deleted. It gains `out_path` validation it does not have today |

Not exposed, at all: **any tool that creates, edits or runs a user-defined job
template.** No `create_job_template`, no `run_user_job`, no `template_id`
argument anywhere on the MCP surface. §7.4 argues it.

Every failing tool result carries the full `Diagnostic` in its data payload per
[36 §5](../specs/video-editor/36-error-model.md#5-mcp-mapping), so an agent gets
`code`/`subject`/`consequence` rather than the prose string
`transcode_media` returns today.

`K-H` obligation: these tools land **with** the GUI verbs, in the same change,
per 26 §19.1's Trail row.

---

## 7. Security model — the centre of this spec

[26 §11](../specs/video-editor/26-kdenlive-mlt-parity.md#k-c1--clip-jobs-framework)
states the requirement directly: "a user-defined job is arbitrary command
execution. It must be explicit, template-validated, non-shell-interpolated, and
off by default — call this out in the mini-spec rather than inheriting a
permissive design." Each clause below is one of those words made concrete, plus
the containment [28](../specs/video-editor/28-security-model.md) requires.

### 7.1 Off by default, and enabled from exactly one place

`AppPreferences` (`crates/photonic-gui/src/preferences.rs:9`) gains:

```rust
/// K-C1: allow user-defined clip-job templates to execute. Default false.
/// Not settable from any MCP tool, and not read from any project file.
#[serde(default)]
pub clip_jobs_user_defined_enabled: bool,
```

**The flag lives in the GUI preference file, never in the `.photon`.** This is
the single most important decision in this section. [28 §2](../specs/video-editor/28-security-model.md#2-trust-boundaries)
classifies a project file from elsewhere as *untrusted structure*; if the
enable flag were a document field, then opening a stranger's project would
enable process execution, and the "off by default" requirement would be
defeated by a file. Preferences are user state, not document state.

With the flag off, `JobKind::UserDefined` is rejected at admission with
`DiagCode::JobRefused` before any path resolution or process construction.

### 7.2 Templates: an allowlisted catalogue outside the document

Templates live in `<config_dir>/clip_job_templates.json`, where `config_dir()`
is `photonic_core::crash_dir()` (`crates/photonic-core/src/diagnostics.rs:29`)
— the same directory and the same loader shape as
`export_presets.json` (`crates/photonic-video/src/export/presets.rs:401-420`).
The file is written with owner-only permissions using the same `0o600` path the
MCP bearer token already uses (`crates/photonic-mcp/src/auth.rs:52-66`).

- **Photonic ships zero templates.** The catalogue is empty on a fresh install,
  so the default product has no user-defined job even with the flag on.
- **A project may reference a template by `TemplateId`; it may never define
  one.** A referenced-but-unknown id **fails closed** with
  `DiagCode::JobRefused` and a diagnostic naming the id. It never prompts to
  create the template, because "a file you opened asked to create a command" is
  the attack.
- Template load is a bounded parse per [28 §5.3](../specs/video-editor/28-security-model.md#53-parsers):
  total file size bound, argv-length bound, param-count bound, no panics on
  malformed input.

### 7.3 Execution: argument vectors by construction, never a command string

```rust
pub struct JobTemplate {
    pub id: TemplateId,
    pub name: String,
    pub program: Program,
    pub argv: Vec<ArgTemplate>,          // NOT a String, NOT Vec<String>
    pub params: Vec<ParamDecl>,
    pub outcome: OutcomeDecl,            // NewAsset | AttachProxy | None
    pub timeout_secs: u32,
}

pub enum Program {
    Ffmpeg,                              // resolved via ffmpeg_locate::locate()
    Ffprobe,
    External(PathBuf),                   // §7.3.4
}

pub enum ArgTemplate {
    Literal(String),
    Placeholder(Placeholder),
}

pub enum Placeholder {                   // closed set; nothing else expands
    Input, Output, OutputDir, AssetName,
    Width, Height, FrameRateNum, FrameRateDen, DurationSeconds,
    Param(ParamId),
}
```

Four normative rules:

1. **One `ArgTemplate` expands to exactly one argv element.** There is no
   textual substitution into a joined string, so there is nothing for a
   metacharacter to be interpreted *by*. A `Literal("; rm -rf ~")` is passed to
   `Command::arg` as one opaque argument and is not a command.
2. **No shell, ever.** `std::process::Command` with `.arg()` per element,
   `.stdin(Stdio::null())`. No `sh -c`, no `cmd /C`, no `powershell`. [28 §5.1](../specs/video-editor/28-security-model.md#51-the-decode-subprocess)
   asks that this be "an asserted invariant, not a habit" — §8 makes it a
   CI-enforced source lint.
3. **Scrubbed environment.** `Command::env_clear()` then an explicit minimal
   set: `PATH`, `HOME`/`USERPROFILE`, `TMPDIR`/`TEMP`, `SystemRoot` (required
   on Windows), and the locale vars. A template must not be able to read the
   parent's environment, and `env_clear` makes the argv the entire interface.
4. **Bounded.** Per-job wall-clock `timeout_secs` (default 600, hard max 3600)
   — [28 §5.1](../specs/video-editor/28-security-model.md#51-the-decode-subprocess)'s
   first row, and the gap `MAX_RESTARTS` does not cover; memory ceiling via
   `RLIMIT_AS` / Windows job object; `arm_parent_death_signal`
   (`child_registry.rs:258`) and a `ChildRecord` with a new `"clipjob"` kind so
   `reap_orphans` collects strays on the next launch; background priority via
   the same `lower_background_priority` the proxy path uses (`proxy.rs:346-370`).

**7.3.4 — `Program::External`.** Rejected unless *all* of: the flag in §7.1 is
on; the path is absolute, canonicalizes to a **regular file**, and does not
escape via symlink; and **the user has confirmed that specific binary once in
the GUI**, with its path and a content digest recorded in the preference file.
A digest change re-prompts. This is [28 §5.2](../specs/video-editor/28-security-model.md#52-bring-your-own-ffmpeg)'s
bring-your-own-ffmpeg reasoning applied verbatim, including its rule that the
confirmation "is not settable by any MCP tool". Signature verification is
explicitly not attempted, for 28 §5.2's stated reason.

**7.3.5 — Path containment.** Every path a job touches — input, output,
`OutputDir` — is resolved through [28 §3.1](../specs/video-editor/28-security-model.md#31-the-rule)'s
`PathPolicy`. That type **does not exist** (§2). K-C1 therefore **implements
it**, in `crates/photonic-core/src/path_policy.rs` next to `diag.rs` so
`photonic-video` and `photonic-mcp` share one implementation rather than each
growing a private one. Scope for K-C1 is 28 §3.1's five resolution steps,
`PathVerdict`/`DenyReason`, and 28 §3.2's default roots; applying it to *every*
path-taking MCP handler stays 28 §9 row 3's work. Job outputs default to the
project sidecar cache (`<project>.photon.cache/jobs/`), which is always in-root.

**7.3.6 — Output must not collide with live media.** A job may never write to a
path that any asset in the open project currently references. An in-place
overwrite leaves `MediaAsset.content_hash` stale and every downstream cache
silently wrong — the worst failure mode available here, because it is
invisible. Enforced at admission, refused with `DiagCode::JobOutputCollides`.

### 7.4 Why user-defined jobs have no MCP surface

[28 §2](../specs/video-editor/28-security-model.md#2-trust-boundaries) classifies
an MCP client as *semi-trusted*: "an agent's arguments derive from content the
agent read, and that content is not the user." For built-in kinds that is
tolerable, because the argv is fully determined by Photonic's own code and the
agent only chooses an asset and a preset from a closed enum. For user-defined
templates it is not: the agent would be choosing *which process to run*, one
step from a filename in a transcript to a spawned binary. The prompt-injection
path is short and entirely mechanical.

This is a **recorded GUI/MCP parity exception** under ROADMAP §10 point 2, not
an oversight, and it is cheap: an agent that needs a transcode has
`transcode_media`; an agent that needs something else is asking for a new
built-in kind, which is a code review.

---

## 8. Acceptance fixtures and tests

**No rights-cleared content is required. K-C1 is not a gated item.** Every
fixture is synthesized by `tools/gen-test-fixtures.py` from ffmpeg lavfi
sources, exactly as the existing corpus is
(`crates/photonic-video/tests/fixtures/README.md`); the corpus already contains
`color_bars.mp4` (4 s, 320×180, ~5 KiB) and `beep_flash.mp4`, which cover every
input this item needs. Added bytes: **zero** — no new fixture file is required,
so neither the 5 MB corpus budget nor 23 §7.2's `AssetRightsManifest` gate is
touched.

ffmpeg-dependent tests use the established skip-with-message convention
(`ffmpeg_locate::locate_for_test`, `tests/export_synthetic.rs:37-45`).

| # | Test | Where | Proves |
|---|---|---|---|
| 1 | Queue lifecycle: enqueue → run → done; cancel-while-queued; cancel-while-running; GC retention | `photonic-video/src/jobs/` unit tests, mirroring `job_queue.rs:307-405` and `video_jobs.rs:342-396` | The queue itself |
| 2 | **`no_shell_invocation`** — source lint over `photonic-video/src/jobs/` asserting no `sh -c`, `cmd /C`, `powershell`, or `Command::new` on a shell stem | `crates/photonic-video/tests/clip_jobs_lint.rs`, patterned on `photonic-gui/tests/keyboard_gate_lint.rs` | §7.3 rule 2 as an invariant, not a habit |
| 3 | Template expansion table: a `Literal` containing `;`, `&&`, `$(id)`, backticks, a NUL byte, a newline, and `../` each produces **one** argv element and no execution | `photonic-video/src/jobs/template.rs` unit tests | §7.3 rule 1 |
| 4 | Flag off ⇒ `UserDefined` refused before any path resolution or `Command` construction; unknown `TemplateId` refused | jobs unit tests | §7.1, §7.2 |
| 5 | Path containment table transcribed from [28 §8](../specs/video-editor/28-security-model.md#8-acceptance) rows 3–7: `../` traversal, absolute escape, symlink escape, component-wise `/a/proj` vs `/a/proj-evil`, FIFO/device refusal | `photonic-core/tests/path_policy.rs` | §7.3.5 |
| 6 | Output collision: a job whose output path equals a referenced asset path is refused with `JobOutputCollides` | jobs unit tests | §7.3.6 |
| 7 | Timeout: a job whose child never exits is killed at `timeout_secs`, reported `Failed`, and the worker survives to run the next job | `photonic-video/tests/clip_jobs.rs` (a `sleep`-style synthetic child, no ffmpeg needed) | §7.3 rule 4, and 28 §8 row 8 |
| 8 | Cancel leaves no partial output: cancel mid-transcode of `color_bars.mp4`, assert neither the staging path nor the final path exists | `photonic-video/tests/clip_jobs.rs` | The cancel path `media_pool.rs:401` currently cannot reach |
| 9 | Restart: write a journal with two `Running` entries, run recovery, assert both report interrupted, staging files are swept, and nothing auto-resumes | `photonic-video/tests/clip_jobs.rs` | §9 |
| 10 | Undo identity: proxy job completion → one `SetAssetProxy`; undo restores `old`; transcode completion → one `AddAsset`; undo removes the row **and the output file still exists on disk** | `photonic-core/tests/timeline.rs` + a GUI path test in `photonic-gui/tests/video_ui_paths.rs` | §5, including the no-delete rule |
| 11 | Serde: v5 doc with `derived_from` round-trips; v5 doc without it loads `None` and re-serializes without the key; `CURRENT_FORMAT_VERSION` still 5 | `photonic-core/tests/timeline.rs`, `tests/forward_compat.rs` | §4 |
| 12 | Invalidation: a `GenerateProxy` completion evicts **only** that asset's decode sources/uploads/stills and leaves other assets' primed rings intact | `photonic-video/tests/preview_media_load.rs` | §10 |
| 13 | Diagnostics: a failed job produces exactly one coalesced entry per `(code, subject)` with the ffmpeg stderr tail in `detail` and never in `message` | `photonic-core/tests/diag_taxonomy.rs` + jobs tests | 36 §4.1/§4.2 |
| 14 | MCP: `start_clip_job` + `get_job_status` + `cancel_job` end-to-end; `transcode_media`'s existing schema is byte-identical; an out-of-root `out_path` returns `PathNotPermitted` | `photonic-mcp/src/handlers/video.rs` tests, beside the existing job tests at `video.rs:8441-8472` | §6 |

Note for the implementer: `crates/photonic-core/tests/diag_catalogue.rs:27+`
holds a deliberately frozen `EXPECTED_WIRE_CODES` list. The three new codes
(§11) must be added there in the same change, or the gate trips — which is the
gate working as designed.

---

## 9. Persistence, cancellation, and failure reporting

### 9.1 Persistence across restart — journal, do not resume

**Decision: no job resumes across a restart.** Instead, an append-only journal
at `<project>.photon.cache/jobs/journal.jsonl` records
`{ job_id, kind, asset, started_unix, state }` on every state transition. On
project open:

1. Any entry still `Queued`/`Running` is reported as **interrupted** — an
   `Info` diagnostic per asset, with `Remedy::Retry`.
2. `atomic_write::sweep_stale_staging` clears the partial outputs.
3. `child_registry::reap_orphans` (`child_registry.rs:140`, already called at
   startup) kills any surviving children with its pid-reuse guard intact.
4. The user re-queues with one click. Nothing runs by itself.

Why not resume: a job's inputs are a snapshot of document state at submission.
By the time the editor restarts, the asset may have been removed, relinked, or
the file replaced — so a silent resume can produce a correct-looking result for
a state the user has already left. Re-queue is explicit, cheap, and cannot
surprise. It also avoids fighting `reap_orphans`, which is *designed* to kill
exactly the children a resume would want to adopt.

The journal is a **cache-sidecar file, not project data** — it is inside the
`.photon.cache/` tree that K-C5's cache pane already enumerates, so purging the
cache purges it, and a project sent to someone else carries no job history.

### 9.2 Cancellation

One mechanism, the one already proven: a per-job `Arc<AtomicBool>` polled
between units of work, exactly as `generate_proxy` (`proxy.rs:312-318`) and
`run_export_job` do. On cancel: `child.kill()`, `child.wait()`, remove the
staging path, transition to `Cancelled`. Cancelling an already-terminal job
returns "already finished" rather than an error — `JobRegistry::request_cancel`
(`video_jobs.rs:308-315`) already has the right three-way return and it is
carried over. Cancelling the whole queue cancels the running job and drops the
pending ones.

This closes a live gap: `spawn_proxy_generation` passes `&|| false` to
`generate_proxy` (`media_pool.rs:404`), so the proxy cancel path exists in the
engine and is unreachable from the UI today.

### 9.3 Failure reporting into the 36 taxonomy

`JobState::Failed` carries a `photonic_core::diag::Diagnostic`
(`diag.rs:434-450`), **not** a `String` — this is the difference from
`QueueJobStatus::Failed { message: String }` (`job_queue.rs:35`) and from
`JobStatus::Failed { error_code: String, .. }` (`video_jobs.rs:219`), both of
which stringify at the point where structure is still available.

- `subject: Subject::Asset(id)` (`diag.rs:77`) — makes the failure navigable:
  the pool badge, the panel row and the MCP payload all address the same asset.
- `detail: Some(stderr_tail)` — 36 §4.2's technical detail, never in
  `message`. `proxy.rs:375` already captures the tail; today it is discarded.
- Coalesced on `(code, subject)` through `DiagnosticLog` (`diag.rs:515`), so a
  batch of 40 failing assets is 40 badges and **one** toast, per 36 §4.1.
- `Remedy::Retry` (`diag.rs:133`) on transient failures. **No auto-retry** —
  a job that failed because ffmpeg is missing will fail identically forever, and
  an automatic retry loop against a subprocess is a CPU burner nobody asked for.

Three new codes in a new `Job` family (§11 lists the doc amendment this
implies): `JobRefused` (admission-time: flag off, unknown template, unconfirmed
binary, param out of range), `JobFailed` (non-zero exit, timeout, spawn
failure), `JobOutputCollides` (§7.3.6).

---

## 10. Frame-graph invalidation

The rule is: **a job result never touches the graph directly.** Every outcome
is committed as a `TimelineCmd`, and the existing revision-driven invalidation
does the rest. Concretely:

- `AttachProxy` → `SetAssetProxy` → the document revision moves → the session
  calls `media.invalidate_assets(&ids)` (`session.rs:908`, impl at
  `session.rs:1599-1607`), dropping **only** that asset's decode sources,
  pending builds, uploads and stills; and `GpuCache::invalidate_matching`
  (`graph/cache.rs:130`) evicts textures whose `ContentHash` matches. Other
  assets keep their primed rings. This already works for proxies today; clip
  jobs inherit it for free by routing through the same command.
- `NewAsset` → `AddAsset` → a fresh `AssetId` with no cached anything. Nothing
  to invalidate.
- `Analysis` → writes into `AnalysisCache` keyed by
  `analysis_key(kind_tag, input_hash)` (`graph/analysis.rs:103`). Analysis is a
  pull-based pure function over content hashes (E-2), so there is no graph
  invalidation at all — a changed input yields a different key.

`ContentHash` (`graph/ir.rs:38`) is `hash(op, resolved params, input hashes)`
(`ir.rs:294`). Nothing about a job appears in it, and nothing should: **do not
add a job→graph invalidation channel.** The single way to get this wrong is an
in-place file overwrite, which §7.3.6 refuses at admission precisely because
the content hash would go stale while every cache kept serving the old bytes.

PA-1 (content-hashed frame graph with hash-natural invalidation) is a protected
surface; this design consumes it rather than working around it.

---

## 11. Risks, open questions, and deliberate exclusions

### Deliberately out of scope

- **`spawn_import` migration** (`media_pool.rs:127`). It is the
  [24](../specs/video-editor/24-preview-media-load.md) L0→L5 readiness ladder,
  not a user-invoked verb; folding it in would entangle K-C1 with import
  semantics. Follow-up.
- **Merging `RenderQueue`** — argued at §3.1. It gains a `view()` adapter and
  an MCP presence via `list_jobs`; nothing else.
- **Caption / TTS jobs.** They keep using the legacy `JobRegistry` until
  migrated; `list_jobs` already surfaces them, so the user-visible unification
  is complete even while the internals are not.
- **Duplicate-with-speed-change** (named in 26 §11's job list). In Photonic,
  speed is a clip property owned by G-11 time remap, on a model that has exact
  rational rates and flicks `Tick` (PA-7/PA-8). Making it an async transcode job
  would be porting a reference NLE's *limitation* backwards. Excluded on those
  grounds; a "bake the speed change to a file" job is a later convenience, not
  the primitive.
- **Stabilise (D-12) and scene-split (D-15).** Both are gated items with their
  own owners. K-C1 ships the `Analyze` extension point they will land on, with
  Loudness as the one real v1 pass so the point is proven rather than
  speculative (`graph/analysis.rs:213,225` already implement it).
- **Parallel job execution.** v1 is one worker, FIFO, matching `RenderQueue`.
  ffmpeg is internally threaded and the proxy path deliberately drops priority;
  N-way concurrency needs a `max_concurrent` preference and a fairness policy,
  which is a separate decision.

### Risks

1. **`PathPolicy` is on K-C1's critical path and does not exist.** If §7.3.5
   slips, job outputs are as unvalidated as `transcode_media`'s `out_path` is
   today. Mitigation: `PathPolicy` lands first, in `photonic-core`, with 28 §8's
   acceptance rows as its tests; job admission is written against it from the
   first commit rather than retrofitted.
2. **The `Job` diagnostic family widens a frozen catalogue.** `diag.rs:140`
   documents "the ten error families" and `tests/diag_catalogue.rs` freezes the
   code list on purpose. This is anticipated — 36 §3.2 has a standing
   "Registered by later specs" mechanism — but it is a two-file change plus a
   doc amendment, and skipping the amendment is exactly the doc-drift failure
   this repo has already paid for.
3. **Scope creep into a plugin system.** `UserDefined` with an argv template is
   one step from "an effect ABI". It is not one: `JobOutcome` is a closed enum
   of three model mutations, and a template cannot register an effect, a node,
   or a UI. Reviewers should reject any change that widens `JobOutcome`.

### Open questions needing a product call

1. **Should `UserDefined` ship in v1 at all, or should the framework land with
   built-ins only?** *Recommendation: ship it, behind the off-by-default flag,
   with zero templates in the box.* An extension point that no code path
   exercises rots; shipping it with tests 2–4 above keeps it honest, and the
   default install is unchanged in behaviour. The alternative — defer it — is
   defensible and costs one enum variant to add later.
2. **Where does the job queue panel live in the GUI?** It is the same content
   as the render-queue panel (`crates/photonic-gui/src/panels/video/render_queue_panel.rs`).
   *Recommendation: one "Background jobs" panel with a source column, not two
   panels*, since `JobView` already unifies them and two panels means two places
   to look for "why is my machine busy".
3. **Should a job's output land in the bin automatically, or be offered?**
   *Recommendation: automatic for `AttachProxy` (invisible, already the shipped
   behaviour), offered for `NewAsset`* — a transcode that silently adds rows to
   the bin on a 40-asset batch is clutter, and the offer is one toast action.
   This is a UX call, not an engineering one.

---

## 12. Clean-room provenance

Per [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
item 2 and §7's per-item requirement:

- **What was read.** Kdenlive's user-facing documentation (`docs.kdenlive.org`,
  `CC-BY-SA-4.0`) for the *existence and shape* of a bin-scoped job catalogue
  and a user-definable job with an argument template — readable as a
  requirements source under 26 §2, cited never pasted. FFmpeg's own published
  CLI documentation for encoder argument strings; FFmpeg is invoked across a
  process boundary as an external program, which is the model Photonic already
  ships (`export/encoder.rs`, `media/proxy.rs`) and introduces no linkage
  question.
- **What was not read.** The Kdenlive source tree, the MLT source tree, and any
  GPL/LGPL derivative. No symbol, constant, argument ordering, control flow or
  test was taken from either. The implementer records the
  [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol)
  attestation for this subsystem, and an independent provenance reviewer checks
  identifiers, comments, constants and test provenance before merge.
- **Where the design actually comes from.** The security model is derived from
  [28](../specs/video-editor/28-security-model.md) and from an invariant
  Photonic already holds — 28 §1 records that `export/encoder.rs` "builds a
  `Vec<String>` argv and never a shell string, so there is no command-injection
  surface." K-C1 generalises Photonic's own property; it does not adopt a
  reference implementation's posture. The queue, cancel, retention and
  child-reaping shapes are taken from Photonic's own shipped
  `RenderQueue`/`JobRegistry`/`child_registry`.
- **Bundled bytes: none.** No asset ships with this item, so
  [23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest)'s
  `AssetRightsManifest` gate is not engaged, and K-C1 is **not** a
  legal- or fixture-gated item.
- **No new dependency.** Nothing in 26 §2's reject list, directly or
  transitively. Everything needed (`std::process`, `serde_json`, the existing
  ffmpeg boundary) is already in the build.

---

## 13. Definition of done → ROADMAP §10

| # | ROADMAP §10 point | Answered by |
|---|---|---|
| 1 | Core op/engine service with unit tests | `photonic-video/src/jobs/` + `photonic-core/src/path_policy.rs`; §8 tests 1–9 |
| 2 | GUI route, or a recorded exception | Media-pool "Jobs →" menu + unified background-jobs panel. **Recorded exception:** none on the GUI side |
| 3 | MCP tool/schema/generated docs | §6; `list_job_kinds` generated under the drift gate; `docs/mcp-api.md` regenerated. **Recorded exception:** `UserDefined` is GUI-only, argued at §7.4 |
| 4 | One user verb = one undo unit | §5; `execute_discrete` per completion, exact inverses tabulated, `Analyze` explicitly has none |
| 5 | Additive serde/migration round-trip | §4; stays v5, `derived_from` additive, test 11 |
| 6 | IR/eval/golden/sync coverage for new pixel/audio paths | **None needed** — K-C1 adds no pixel or audio path. `Analyze` reuses E-2's shipped `analyze_loudness` |
| 7 | Hard gates green; trend metrics not regressed | Jobs run off the engine thread at background priority; the present loop is untouched. No hard gate is on this path |
| 8 | Offline, privacy, licensing, content, product gates | §12: no bundled bytes, no new dependency, no network. Journal contains paths and ids, **no media content** (36 §7 row 9) |
| 9 | No protected-surface regression | PA-1 consumed as designed (§10); PA-9 strengthened (`Failed` carries a typed `Diagnostic`, not a string) |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | §1's five outcomes are the L4 script; parity holds for built-ins, with §7.4's exception recorded |

---

## Follow-ups (other documents that need a change — **not** made here)

1. **[28-security-model.md](../specs/video-editor/28-security-model.md)** has no
   section on user-defined job execution. It should gain a §5.4 "User-defined
   jobs" carrying §7.1–§7.4 of this document as normative rules (off by default,
   preference-not-document, closed placeholder set, argv-only, scrubbed env,
   no MCP surface), and §9's sequencing table should gain a row for it. 28 §5.2's
   bring-your-own-ffmpeg confirmation rule should be cross-referenced as the
   model for `Program::External`.
2. **[36-error-model.md](../specs/video-editor/36-error-model.md)** §3.2's family
   table needs a `Job` row (`JobRefused`, `JobFailed`, `JobOutputCollides`), and
   `diag.rs:140`'s "the ten error families" doc comment becomes eleven.
3. **[02-engine.md](../specs/video-editor/02-engine.md)** §1's crate module map
   should list `photonic-video/src/jobs/`.
4. **[10-mcp-tools.md](../specs/video-editor/10-mcp-tools.md)** §6's job pattern
   currently describes only the `JobRegistry` shape; it should describe the
   three-source `JobView` and name `list_jobs`.
5. **ROADMAP.md** §2 should record that `RenderQueue` gained an MCP surface via
   `list_jobs`, closing a K-F1 residual that is not currently tracked.
6. **26 §11 K-C1** lists "duplicate-with-speed-change" as a job; §11 of this
   document excludes it with reasons. If that exclusion is accepted, 26's item
   text should be amended to point at G-11 instead.
