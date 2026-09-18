# 202 — K-C5 Project archiving (the archiving half)

> **Status: proposed — Band-5 mini-spec, pre-code. No code authorization.**
> [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands) makes an
> accepted mini-spec the exit condition for every K-Band 5 item: it must name the
> data-model change, migration, undo unit, MCP surface and acceptance fixtures
> *before* code. This document discharges that for the **archiving** half of
> **K-C5** ([26 §11](../specs/video-editor/26-kdenlive-mlt-parity.md#k-c5--project-archiving-and-cache-management)).
> [23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)'s
> stop/go boundary applies until this is accepted.

**Owner ref:** 26 §11 K-C5 · **Territory:** `photonic-video-engine` + `panels-video` · **Effort:** M
**Verified against:** `feat/video-editor-module` @ `8a33f32`, tree clean.

**The load-bearing section is [§3, reference completeness](#3-reference-completeness--the-load-bearing-section).**
Everything else in this document is plumbing. An archiver that omits one
referenced file produces an artefact that looks complete, opens without error,
and is discovered to be broken years later. That is the failure mode this item
is designed against, and §3 exists because the reference scan it must reuse has
a hole in it today.

---

## 1. Problem and user outcome

A Photonic project is a `.photon` document plus a set of **absolute paths** into
the user's filesystem (`AssetSource::File { path, .. }`,
`crates/photonic-core/src/timeline/media.rs:121-131`). Move the project to
another machine, another drive, or another user, and every asset goes offline.
There is no verb that collects a project and what it references into one
relocatable place: `grep -rn 'archive_project\|collect_media\|consolidate'
crates/ --include=*.rs` returns one unrelated comment in
`graph/compile.rs:2478` (verified 2026-07-28), which is exactly the grep 26 §11
records as clean.

The other two thirds of K-C5 **shipped**: `media::summarize_cache`
(`crates/photonic-video/src/media/cache_stats.rs:33`) behind the media-pool
"Cache…" button (`crates/photonic-gui/src/panels/media_pool.rs:735-774`), and
`ops::unused_assets` / `ops::remove_unused_assets`
(`crates/photonic-core/src/timeline/ops.rs:185,246`) behind "Remove unused (N)"
(`media_pool.rs:722-732`). This document does not re-spec either.

After this item, a user can:

1. Choose **File → Archive Project…**, pick a destination, and get a **plan**
   before any bytes move: every file that will be copied, its size, the total,
   and — separately and prominently — every referenced asset that is **offline**.
2. Run the archive and get a directory that is **self-contained and
   relocatable**: copy it to a USB stick, hand it to another editor on another
   OS, open the `.photon` inside it, and every clip is online.
3. Be **refused by default** when a referenced asset is offline, with the list,
   rather than getting a silently incomplete archive (26 §11's watch-out:
   "report, never silently drop").
4. Cancel a running archive and find **nothing** at the destination — not a
   half-populated directory that looks finished.
5. Drive both verbs from an agent (`plan_archive`, `archive_project`), with the
   plan tool read-only so an agent can inspect before it destroys disk space.

What the user does **not** get, deliberately: media trimmed to used ranges
(§6.3), a `.zip` (§10.3 Q2), and any change at all to the project they are working
in (§5).

---

## 2. Current state in code

Exact. Read this before disagreeing with §3–§7.

### 2.1 What exists and is directly usable

| # | Thing | Where | Note |
|---|---|---|---|
| 1 | `AssetSource::File { path: PathBuf, rel_path: Option<PathBuf> }` | `crates/photonic-core/src/timeline/media.rs:121-131` | The lever. See §2.3 — `rel_path` is **inert today** |
| 2 | `ops::unused_assets` (four reference classes, sorted, deterministic) | `crates/photonic-core/src/timeline/ops.rs:185-242` | Fixed this week. §3 extends it |
| 3 | `ops::collect_grade_asset_refs` — counts LUT refs **regardless of `enabled`/`bypass`** | `ops.rs:164-172` | The right conservatism, and the model for §3 |
| 4 | `MediaAsset.content_hash: Option<String>` — xxh3 of head+tail+len | `media.rs:53-55`; built by `media::probe::content_hash` | Dedup key candidate, with the caveat at §6.4 |
| 5 | `ProxyOrigin::{Generated, Attached}` — "`Attached` marks user-owned files that must never be deleted on detach" | `media.rs:231-242`; enforced at `crates/photonic-mcp/src/handlers/video.rs:3771-3776,3793-3799` | Decides which proxies are archived (§6.2) |
| 6 | `atomic_write::{staging_path, write_atomic, sweep_stale_staging}` | `crates/photonic-video/src/media/atomic_write.rs:21,32,60` | [37 §2.3](../specs/video-editor/37-robustness.md)'s temp-and-rename. §7.2 uses all three, and explains why `write_atomic` cannot carry media |
| 7 | `cache_dir_for_project` / `CACHE_DIR_SUFFIX = ".cache"`; `proxy_cache_dir` | `crates/photonic-video/src/media/keyframe_index.rs:42,48`; `crates/photonic-video/src/media/proxy.rs:75` | The sidecar tree the archive excludes (§6.2) |
| 8 | `summarize_cache` → `CacheReport { root, categories, total_bytes }` | `crates/photonic-video/src/media/cache_stats.rs:11-33` | Already enumerates what "cache" means. The archive's exclusion list is exactly its root |
| 9 | `asset_is_offline(asset) -> bool` (`!path.exists()`) | `crates/photonic-gui/src/panels/media_pool.rs:506-511` | 26 §11's named watch-out. **Lives in the GUI crate** — §4.3 |
| 10 | `save_photon(&Document, Option<&HistorySnapshot>) -> String` | `crates/photonic-core/src/photon_file.rs:36-56` | Writes the archived `.photon`; unchanged |
| 11 | `load_document(path)` (GUI open) and the CLI open | `crates/photonic-gui/src/app/mod.rs:2016-2032`; `crates/photonic-app/src/main.rs:124-126` | The only two sites that know a project's own path — §4.3 |
| 12 | `DiagCode::MediaNotFound` with `RemedyKind::Relink` | `crates/photonic-core/src/diag.rs:209,334`; `Remedy::Relink(AssetId)` at `diag.rs:129` | Offline reporting needs **no new code** (§8) |
| 13 | `history.execute_discrete` used for a whole remove-unused batch | `crates/photonic-gui/src/app/panel_actions.rs:108-119` | The existing one-verb-one-unit precedent in this exact panel |
| 14 | `ops::relink_asset` → `TimelineCmd::RelinkAsset { asset, old_path, new_path }` | `ops.rs:253-272`; applied at `crates/photonic-core/src/timeline/commands.rs:1770-1777` | **Rewrites `path` only.** §4.2 |

### 2.2 What does not exist, stated plainly

- **No archive module.** No `photonic-video/src/project/` directory at all
  (`ls crates/photonic-video/src/` → `audio captions contract.rs decode export
  graph lib.rs media playback pool.rs session.rs testing`). 26 §11 names
  `project/archive.rs` as the target file; nothing is there.
- **No `PathPolicy` / `PathVerdict` / `DenyReason`.**
  [28 §3.1](../specs/video-editor/28-security-model.md#31-the-rule) specifies the
  type fully; only the diagnostic code landed (`diag.rs:250`
  `SecurityPathNotPermitted`). `grep -rn 'PathPolicy\|path_policy' crates/`
  returns clean. [195 §7.3.5](195-k-c1-clip-jobs-framework.md) puts implementing
  it on K-C1's critical path; §10.1 risk 4 states what happens if K-C5 lands
  first.
- **No MCP open/load tool.** `grep -n '"open_document"\|"load_document"'
  crates/photonic-mcp/src/dispatch.rs` is clean. MCP can `save_document`
  (`crates/photonic-mcp/src/handlers/document.rs:47`) but never opens a file, so
  §4.3's relocation pass has exactly two call sites, both outside `photonic-mcp`.
- **No free-space query.** Nothing in the workspace reads filesystem capacity,
  and no dependency provides it. §11 refuses to add one, and §10.1 risk 5 states
  the consequence.

### 2.3 `rel_path` is declared, documented, and dead

This is the single most important fact in §2, and it is the reason archiving is
the item that gets to fix it.

`media.rs:122-124` documents the field as normative behaviour:

> "Absolute path plus an optional project-relative fallback. The loader tries
> `rel_path` first (project moves survive), then `path`, then relink-by-hash
> (§9)."

**No loader does any of that.** Verified exhaustively: `grep -rn 'rel_path'
crates/ --include=*.rs` returns 12 hits — one declaration (`media.rs:128`), one
doc comment, one constructor that hardcodes `None` (`media.rs:99`), one
production construction that hardcodes `None`
(`crates/photonic-gui/src/app/command_center.rs:1144-1147`), and eight test
sites. Every consumer in the tree destructures `AssetSource::File { path, .. }`
and discards it — `session.rs:489,859,1662,1708,1874,2184`,
`media_pool.rs:384,496,508,1000`, `handlers/video.rs:734,3421,3607,3842,4643`,
`ops.rs:264`, `commands.rs:1774`, `export/offline_audio.rs:106`,
`panel_actions.rs:183`, `color_page.rs:167`, `source_marks.rs:98`,
`app/timeline/mod.rs:1303`.

So `rel_path` is a promise the format makes and the code does not keep. K-C5 is
the first item that needs it, so K-C5 makes it real (§4.3) — and, because
`AssetSource::File` is already v5, doing so costs no format change (§4.1).

> **Consistency note, not a contradiction.**
> [196 §2.1](196-x-2-opentimelineio-interchange.md)'s table row for
> `AssetSource::File` reads "`rel_path` first, then `path`, then hash". That row
> paraphrases `media.rs:122-124`'s doc comment, i.e. the *intent*. It is a
> correct citation of the comment and an incorrect description of the shipped
> code. K-C5 makes the comment true; the Follow-ups section records the
> re-statement rather than editing 196 here.

### 2.4 The project save is not atomic

`write_photon_file` (`crates/photonic-gui/src/app/mod.rs:2161-2170`) and MCP
`save_document` (`handlers/document.rs:93`) both end in `std::fs::write`. 37 §2.3
requires temp-and-rename for "every job that writes a file", and
`atomic_write::write_atomic` (`atomic_write.rs:32`) implements it — but the
document save predates it and does not use it. **Out of scope for K-C5** (it is
a save-path change, not an archive change), recorded in Follow-ups. It matters
here only because §7 must not inherit the same defect: the archived `.photon`
goes through `write_atomic`, not `fs::write`.

---

## 3. Reference completeness — the load-bearing section

An archive is correct exactly when the set of files it copies is a superset of
the set of files the project can ever need. Getting that set wrong is silent
data loss. This section establishes the set, names the two holes in the shipped
scan, and fixes them.

### 3.1 The closed set of external file references

A `.photon` references files outside itself in exactly **two** places:

1. `AssetSource::File { path }` — `media.rs:126`.
2. `ProxyRef.path` — `media.rs:247`.

Establishing that this is closed, and how: every `AssetId` in the persisted model
was enumerated (`grep -rn 'AssetId' crates/photonic-core/src/timeline/`), and
every `PathBuf` (`grep -rn 'PathBuf' crates/photonic-core/src/`). The `AssetId`
sites are `media.rs` (the pool itself), `graph.rs:130,166`, `clip.rs:168,172`,
`grade.rs:200`, and `commands.rs` (command payloads, not document state). The
`PathBuf` sites in persisted types are `media.rs:126,128,247` and
`export.rs:1000`. Everything else is derived or transient:

- **Vector/raster content is embedded, not linked.** `media.rs:1-8` states it
  ("assets referenced, never embedded. Contrast with `RasterImage`'s base64-PNG
  embedding"), and `grep -rn 'PathBuf|href|image_path|LinkedImage'` over
  `photonic-core/src/raster/`, `node.rs` and `layer.rs` finds nothing. An
  `AssetKind::VectorDoc` pointing at an *external* `.photon`/`.svg` is an
  ordinary `AssetSource::File` and is covered by rule 1.
- **Fonts are family-name strings**, never paths: `CaptionStyle.font_family`
  (`crates/photonic-core/src/timeline/captions.rs:110-111`) and
  `TextGen.font_family` (`graph.rs:96`). Nothing to copy. §6.6 covers what the
  archive says about them instead.
- **`ExportOptions.icc_profile: Option<PathBuf>`** (`export.rs:1000`) is a
  per-call PDF/CMYK argument built from tool args at
  `crates/photonic-mcp/src/handlers/doc_export.rs:278`; it is not a `Document`
  field and does not survive a save.

### 3.2 The five reference classes — and the sixth `unused_assets` misses

`ops::unused_assets` (`ops.rs:185-242`) walks four classes, and its doc comment
(`ops.rs:174-184`) names them: clip sources, grade stacks at all four scopes,
embedded graph grades, and `MediaIn`/`Lut` graph ops. That is correct as far as
it goes. It is not far enough.

**`MulticamAngle.source` is a fifth class, and it is not walked.**

- `MulticamAngle { name, source: ClipSource, source_in }` —
  `crates/photonic-core/src/timeline/clip.rs:249-258`.
- `Clip.multicam: Option<MulticamGroup>` — `clip.rs:88`; `MulticamGroup { angles,
  active }` — `clip.rs:276-281`.
- `ops::create_multicam_group` (`ops.rs:1995-2035`) folds each extra angle's clip
  into `angles` **and emits a `RemoveClip` for it** (`ops.rs:2021-2025`). After
  folding, a non-active angle's asset is referenced **only** from
  `MulticamAngle.source`.
- `unused_assets` reads `clip.source.asset()` (`ops.rs:199`) and nothing else.
  `grep -n 'multicam' crates/photonic-core/src/timeline/ops.rs` returns only the
  multicam ops themselves (`:1987,1995,2029,2036-2057`) and their tests
  (`:3987+`) — never the scan.

Consequence **today**, in shipped code: "Remove unused (N)"
(`media_pool.rs:722-732` → `panel_actions.rs:108-119`) counts and removes the
pool rows for every non-active multicam angle. It is recoverable (the batch is
one `execute_discrete` and `RemoveAsset` deletes no file, `commands.rs:1767-1769`)
and it is invisible until the user switches angle. Consequence **for K-C5**: an
archive built on the same scan omits those media files. That is not recoverable.

**In scope for K-C5, and required before it can be correct.**

### 3.3 One scan, extracted — the structural rule

Two scans will drift, and the day they drift the archiver drops media. So:

```rust
// crates/photonic-core/src/timeline/ops.rs
/// Every asset the project can reach, by any reference class (§3.2).
/// `unused_assets` is defined as `pool.keys() - referenced_assets()`; archiving
/// copies `referenced_assets()`. There must never be a second implementation.
pub fn referenced_assets(p: &TimelineProject) -> std::collections::BTreeSet<AssetId>;
```

`unused_assets` becomes a three-line difference over it, keeping its existing
sorted-`Vec<AssetId>` return so `remove_unused_assets` (`ops.rs:246-251`) and its
test (`ops.rs:2540+`) are untouched. `BTreeSet` (not `HashSet`) so the archive
visit order is deterministic without a separate sort — determinism is what makes
§8's fixture assertions possible and makes a re-archive diff cleanly.

### 3.4 The sixth class cannot be scanned, so it is not scanned

`ClipSource::Unknown` (`clip.rs:190-195`), `GraphOp::Unknown`
(`graph.rs:181-186`) and `GradeOpParams::Unknown` (`grade.rs:210`) preserve a
newer build's payload verbatim ([39 §2.2](../specs/video-editor/39-document-lifecycle.md#22-generalise-it)).
Within `COMPAT_WINDOW = 1` (`crates/photonic-core/src/migration.rs:16`) a v5 file
from a newer build can therefore reference an asset from inside a blob this build
cannot interpret. `ClipSource::asset()` returns `None` for `Unknown` and the
comment calls that "the desired conservative behaviour" (`clip.rs:199-206`) —
which is true for relink and exactly backwards for GC and for archiving.

**Decision: when the project contains any `Unknown`-variant payload, archive the
entire media pool**, ignoring the reference scan, and mark every extra file
`CopiedConservatively` in the manifest. Detection is exact and already
implemented: `GraphOp::is_unknown()` (`graph.rs:217-219`),
`GradeOpParams::is_unknown()` (`grade.rs:238-240`), `GradeOpKind::is_unknown()`
(`grade.rs:110`), and a `matches!(source, ClipSource::Unknown(_))` over clips and
multicam angles.

**Rejected alternative:** scanning the preserved JSON blobs for strings that
parse as an `AssetId`. It is a heuristic — an id nested under an unexpected key,
or encoded as anything but a bare string, is missed — and an archiver must not
depend on a heuristic. The blanket copy costs disk; the heuristic costs the
archive. Cost is bounded: the whole pool, once, only for documents that carry an
unknown variant at all.

### 3.5 Offline assets — refuse by default

26 §11's watch-out is normative: *"archiving must handle offline assets
(`asset_is_offline()`) explicitly — report, never silently drop."* Concretely:

| Situation | Behaviour |
|---|---|
| A **referenced** asset is offline | The plan lists it. `archive_project` **refuses to start** unless `allow_offline: true`. Diagnostic: existing `MediaNotFound` with `Subject::Asset(id)` and `Remedy::Relink(id)` (`diag.rs:209,129`), coalesced by `DiagnosticLog` |
| `allow_offline: true` | Proceeds. The archived asset keeps its **original absolute `path`** and gets `rel_path: None` — it is *not* rewritten into `media/`, because a rewritten path would name a file that does not exist and make a missing asset look archived. K-C6 relink then has the only clue available. Recorded in the manifest as `Offline` |
| An **unreferenced** asset is offline | Never blocks. Recorded as `SkippedOffline`; if `include_unused` is on it is reported, not fatal |
| The archive completes with any `Offline` entry | The report and the manifest both say so, and the GUI toast says "archived, N assets offline" — never plain "archived" |

Refuse-by-default rather than warn-and-continue because the failure is silent and
permanent: nothing later in the archive's life re-checks it. Making the user
press one more button is cheap; making them discover it in three years is not.

---

## 4. Data-model change and migration

### 4.1 Persisted document model: none

No field is added, removed or retyped in `MediaAsset`, `MediaPool`,
`AssetSource`, `ProxyRef`, `TimelineProject` or `ProjectVideoSettings`. The
archive writes `rel_path`, which is already v5 (`media.rs:127-128`).

**`CURRENT_FORMAT_VERSION` stays at 5** (`crates/photonic-core/src/document.rs:117`).

Reasoning, in the terms `migration.rs:43-56` uses — a migration *reinterprets
existing data*, and K-C5 reinterprets nothing:

1. A pre-K-C5 v5 file has `rel_path: None` on every asset. Under §4.3's
   resolution rule that means "no relative fallback", falls straight through to
   `path`, and behaviour is **bit-identical to today**.
2. A K-C5-written v5 file read by a **pre**-K-C5 build ignores `rel_path` (it
   already does — §2.3) and uses the absolute `path`, which is correct on the
   archiving machine. Moved to another machine, an old build shows the assets
   offline. That is honest degradation with an existing remedy (K-C6 relink), not
   corruption — and, decisively, **a version bump would not fix it**, because the
   old build's problem is missing code, not a missing version number.
3. Bumping to v6 would push every existing v5 project through a no-op migration
   step, make a `V5ToV6` entry in `migrations()` (`migration.rs:58-65`) a lie
   about what changed, and shift `COMPAT_WINDOW` (`migration.rs:16`) one version
   away from every user who has not upgraded. All four prior Band-5 mini-specs
   landed additively inside v5; so does this one.

Required migration work is therefore a round-trip test, not a migration (§8 T11).

### 4.2 Command model: one additive pair on `RelinkAsset`

`TimelineCmd::RelinkAsset` (`commands.rs:410-414`) rewrites `path` and leaves
`rel_path` untouched (`commands.rs:1770-1777`). Once §4.3 makes `rel_path`
authoritative-when-it-resolves, a stale `rel_path` surviving a relink would
shadow the file the user just chose. So:

```rust
// crates/photonic-core/src/timeline/commands.rs — RelinkAsset gains:
#[serde(default)] old_rel: Option<PathBuf>,
#[serde(default)] new_rel: Option<PathBuf>,   // K-C5: always None from ops::relink_asset
```

`ops::relink_asset` (`ops.rs:253-272`) captures `old_rel` from the asset and sets
`new_rel: None`. **A relink always clears `rel_path`**, because a relink is the
user naming an absolute file explicitly and there is no basis for asserting a
project-relative location afterwards. Archiving is the only writer of `rel_path`,
and it writes it for every asset in one pass — so the field is never partially
maintained.

This is a `photon_history` payload change, not a document-format change: per
[194 §4](194-k-a5-general-and-nested-clip-groups.md) point 2, `load_photon`
restores history best-effort (`photon_file.rs:64-100`), so an older build reading
a newer history drops it and still opens the document.

### 4.3 The relocation pass — where `rel_path` becomes real

```rust
// crates/photonic-core/src/timeline/load.rs
/// K-C5: repoint file-backed assets whose absolute `path` no longer resolves but
/// whose `rel_path` does, relative to the project file's own directory.
/// Returns the assets repointed, for the 36 diagnostic channel.
pub fn relocate_asset_paths(p: &mut TimelineProject, project_dir: &Path) -> Vec<AssetId>;
```

**Resolution order, normative — `rel_path` first, and only when it resolves:**

1. If `rel_path` is `Some(r)` and `project_dir.join(r)` **exists**, set `path` to
   that canonical absolute path.
2. Otherwise leave `path` as it is.
3. Otherwise (neither resolves) the asset is offline, and K-C6 owns recovery.
   Relink-by-`content_hash` is K-C6's, not this item's.

Why `rel_path` first rather than `path` first: the case archiving exists for is
"the archive directory moved". If `path` won, an archive copied to a second
location on the same machine would resolve back to the *first* copy — silently
editing against media the user thinks they left behind. The stale-`rel_path`
hazard that would otherwise argue for `path`-first is closed by §4.2 (relink
clears it) plus the `exists()` guard in step 1.

**Where it is called, and why not in `finalize_load`.** `finalize_load`
(`load.rs:138`) is invoked from `Document`'s `Deserialize` impl
(`document.rs:1751`) and has **no path context**. The two sites that know a
project's own path are `load_document` (`crates/photonic-gui/src/app/mod.rs:2016-2032`)
and the CLI open (`crates/photonic-app/src/main.rs:124-126`); MCP has none
(§2.2). Both call `relocate_asset_paths` immediately after `load_photon`.

**It mutates at load and is not undoable, and that is correct.** It sits in the
same category as `finalize_effect_ids` (`load.rs:139`) and
`dissolve_degenerate_groups` (`sequence.rs:485`, called from `load.rs:162`) —
load-time repairs that mutate the in-memory project before any history exists.
It must **not** set the document-modified flag: opening a relocated archive is
not an edit. Saving later persists the new absolute paths, which is the right
outcome (the project now belongs to this machine).

### 4.4 Engine types (not serialized)

```rust
// crates/photonic-video/src/project/archive.rs
pub struct ArchiveOptions {
    pub dest: PathBuf,
    pub include_unused: bool,      // §10.3 Q1 — recommended default: true
    pub include_history: bool,     // default true
    pub allow_offline: bool,       // default false (§3.5)
}

pub enum ArchiveDisposition {
    Copied,
    Deduplicated { same_as: AssetId },
    CopiedConservatively,          // §3.4
    Offline,                       // §3.5
    SkippedUnreferenced,
    SkippedOffline,
    SkippedProxyCache,             // §6.2
}

pub struct ArchiveEntry {
    pub asset: AssetId,
    pub original_path: PathBuf,
    pub archived_rel: Option<PathBuf>,   // None for every Skipped/Offline variant
    pub content_hash: Option<String>,
    pub bytes: u64,
    pub disposition: ArchiveDisposition,
}

pub struct ArchivePlan {
    pub entries: Vec<ArchiveEntry>,      // BTreeSet order from §3.3
    pub total_bytes: u64,
    pub offline: Vec<AssetId>,
    pub conservative: bool,              // §3.4 tripped
}

pub struct ArchiveReport { pub plan: ArchivePlan, pub root: PathBuf, pub bytes_written: u64 }

pub enum ArchiveError {                  // typed, never a String (PA-9)
    DestinationNotEmpty(PathBuf),
    DestinationNotPermitted { path: PathBuf, reason: DenyReason },  // 28 §3.1
    ReferencedAssetOffline(Vec<AssetId>),
    ProjectNotSaved,                     // §10 Q4
    Io { path: PathBuf, source: std::io::Error },
    Cancelled,
}
```

---

## 5. Undo unit

**Archiving produces no undo unit, and that is the answer rather than an
omission.** It mutates nothing in the open document: it reads the project, writes
files elsewhere, and returns a report. The inverse of "archive" is "delete that
directory", which is a file-manager action; wiring `remove_dir_all` on a
user-chosen path to Ctrl+Z is precisely the data loss
[195 §5](195-k-c1-clip-jobs-framework.md) rule 1 refuses for job outputs. ROADMAP
§10 point 4 is satisfied by argument; it is recorded here so a reviewer does not
read it as a miss.

Two verbs adjacent to this item **do** have undo units, and both already exist:

| Verb | Command | Exact inverse |
|---|---|---|
| Relink an asset (K-C6; touched by §4.2) | `TimelineCmd::RelinkAsset { asset, old_path, new_path, old_rel, new_rel }` (`commands.rs:410-414` + §4.2) | Swap `old_path`↔`new_path` **and** `old_rel`↔`new_rel`. The existing `invert` already does the first pair; the second is the same mechanical swap |
| Remove unused (shipped) | `Command::Batch([RemoveAsset × n])` via `execute_discrete` (`panel_actions.rs:111-115`) | Reversed batch of `AddAsset`. Unchanged by this item — but its *membership* changes once §3.2 lands, which is the point |

`relocate_asset_paths` (§4.3) is deliberately **not** undoable, on the
`finalize_effect_ids` / `dissolve_degenerate_groups` precedent and under
[39 §1.6](../specs/video-editor/39-document-lifecycle.md#16-what-is-not-undoable)'s
category of load-time repair.

**Explicitly not a verb: "archive and switch to the archived copy."** Archive
never re-points the open document. Doing so would silently change what
`current_file` (`app/mod.rs:739`) means, which sidecar cache is live
(`cache_dir_for_project`, `keyframe_index.rs:48`), and where the next save lands.
The user who wants to work in the archive opens it.

---

## 6. What is copied, what is relinked, what is refused

### 6.1 Layout

```
<dest>/
  <project-stem>.photon        ← rewritten document; written LAST (§7)
  media/
    clip.mp4
    clip-2.mp4                 ← a *different* file also named clip.mp4
    grade.cube
    title.photon
  archive-manifest.json        ← machine-readable record (§6.5)
```

Every `AssetSource::File` in the archived document is rewritten to
`path = <dest>/media/<name>` (absolute, valid on the archiving machine) **and**
`rel_path = Some("media/<name>")` (relative to the `.photon`'s own directory).
Both, not one: `path` keeps the archive working for a pre-K-C5 build and for
every existing consumer that ignores `rel_path` (§2.3), and `rel_path` is what
survives the move (§4.3).

Nothing in the archived document points outside `<dest>` except an `Offline`
asset under `allow_offline` (§3.5), which points at its original path by design.

### 6.2 Proxies and caches — excluded, with one carve-out

**Excluded entirely: the `<project>.photon.cache/` sidecar tree.** Everything
`summarize_cache` enumerates (`cache_stats.rs:38-64`: proxies, posters,
keyframes, waveforms, other) is regenerable from the archived originals by code
that already exists — `generate_proxy` (`proxy.rs:284+`), `ensure_poster`,
`KeyframeIndex::load_or_build`, the waveform pyramid. Copying it would inflate
the archive by the largest single category in the report to preserve derived
bytes that a first playback rebuilds. It is also keyed by `content_hash`
(`proxy.rs:82-87`, `keyframe_index.rs:55-57`), so the rebuild is automatic on the
target machine with no relink.

**Carve-out: `ProxyOrigin::Attached` proxies are copied.** `media.rs:231-235`
states the distinction — "`Attached` marks user-owned files that must never be
deleted on detach" — and MCP `remove_proxy` already enforces it
(`handlers/video.rs:3771-3776`, deleting only `Generated` paths). An attached
proxy is a user file that Photonic cannot regenerate; treating it as cache would
lose it. It is archived into `media/` like any other asset file and its
`ProxyRef.path` is rewritten.

**`ProxyOrigin::Generated` `ProxyRef`s are cleared** in the archived document
(set to `None`), rather than left pointing at a cache path the archive does not
carry. Honest scope of that decision: `resolve_decode_input` (`proxy.rs:98-105`)
already falls back to the original when a proxy file is missing (CAP-014), so a
dangling ref would be harmless at render time. It would **not** be harmless in
the media pool, which reads `proxy_status` (`media_pool.rs`, `list_media` at
`handlers/video.rs:2678`) and would display "Ready" for a file that is not there.
This is a tidiness-and-honesty call, and it is stated as one.

### 6.3 Consolidation (trim to used ranges plus handles) — **rejected for v1**

A real NLE feature with real risk. Rejected, on five independent grounds, three
of which are specific to Photonic's model:

1. **It is lossy and irreversible against the artefact whose purpose is
   permanence.** Extending a clip past its handle in the archived project is
   then impossible. An archive that silently discards media is the inverse of an
   archive.
2. **Photonic's used range is not `[source_in, source_in + duration)`.**
   `Clip.speed: SpeedMap` (`clip.rs:38-39`) makes the source range a function of
   the remap; `GraphOp::TimeOffset { offset }` (`graph.rs:171-174`) and
   `MediaIn { time_source }` (`graph.rs:129-133`) shift it further. The machinery
   that answers this correctly is E-1's `source_range_for_op` /
   `graph_source_range`, with `SOURCE_RANGE_SOFT_CAP = 16`
   (`crates/photonic-video/src/graph/source_range.rs`, as
   [193 §2](193-k-a1-chunked-timeline-preview-rendering.md) row 6 records). A
   naive `source_in + duration` trim cuts the wrong range for any remapped,
   reversed, or graph-offset clip — and cuts it *plausibly*, so nobody notices.
3. **Multicam makes the used range a union the user has not chosen yet.** Each
   `MulticamAngle` carries its own `source_in` (`clip.rs:254-257`), and the whole
   point of keeping non-active angles is switching to them later. Trimming an
   angle to the range the *active* angle happens to use is nonsense.
4. **Stream-copy cannot cut where asked.** A keyframe-aligned copy rounds outward
   to the enclosing GOP — the exact structure `counter.mp4` exists to exercise
   (`crates/photonic-video/tests/fixtures/README.md`: keyframes every 2 s,
   GOP=60). Cutting where asked requires a re-encode, which makes the archive a
   *different master*, which defeats archiving.
5. **It multiplies the failure surface of an operation whose failure mode is
   silent data loss.**

If size reduction is wanted, the honest lever is *excluding unused assets*
(§10.3 Q1) — dropping files the project does not reference — not shortening the
ones it does. Should consolidation ever be built, it is a **separate verb** with
its own mini-spec, never a checkbox on Archive, it must be built on
`graph_source_range`, it must refuse any clip whose source range is not
statically computable, and it must default off.

### 6.4 Collisions and deduplication

A flat `media/` directory collides whenever two source directories contain the
same filename. Rejected alternatives: mirroring the source tree (leaks absolute
paths and drive letters into the archive, unbounded depth, `C:\` is not a legal
path component elsewhere) and content-hash filenames (an archive is a thing
humans open in a file manager). So: **name-preserving with deterministic
disambiguation.**

Normative rules:

1. **Candidate name** = `path.file_name()`, then sanitized.
2. **Sanitization is a security rule, not a cosmetic one.** A media filename in a
   project file is untrusted input ([28 §6](../specs/video-editor/28-security-model.md#6-untrusted-project-files)).
   Reject or replace: path separators, `..`, NUL and control bytes, Windows
   reserved device names (`CON PRN AUX NUL COM1-9 LPT1-9`), trailing dots and
   spaces (Windows strips them, changing the name after the fact), and anything
   over 255 bytes — truncated preserving the extension. An asset whose file name
   is `../../etc/passwd` must not escape `media/`. The destination itself is
   independently checked by `PathPolicy` (§8, §10.1 risk 4); this rule is the second
   layer,
   because the first one is about the *destination root* and this one is about
   *what gets appended to it*.
3. **Disambiguation**: if a **distinct** file already claims the name, insert
   `-2`, `-3`, … before the extension. Assets are visited in `referenced_assets`'
   `BTreeSet<AssetId>` order (§3.3), so two archives of the same project produce
   byte-identical layouts.
4. **Case-insensitive collision detection.** `Clip.MP4` and `clip.mp4` are one
   name on Windows and on default macOS. An archive created on Linux must open
   on Windows, so collisions are detected case-insensitively regardless of the
   archiving host's filesystem.
5. **Deduplication requires hash equality *and* a byte compare.**
   `MediaAsset.content_hash` is "xxh3 of file head+tail+len" (`media.rs:53-55`) —
   **not** a full-file digest, so two distinct files can share one. Two assets
   collapse to one archived copy only when their `content_hash` matches, their
   lengths match, **and** a full byte comparison succeeds. The archiver is
   already streaming every byte, so the extra cost is one read of the smaller
   candidate. Never dedup on hash alone — that is the same reasoning 26 §11's
   K-C6 watch-out gives for relink ("never relink silently on hash match alone…
   a hash collision or a duplicated file would otherwise rebind media
   invisibly").

### 6.5 The manifest

`archive-manifest.json` at the archive root: schema version, Photonic version,
UTC timestamp, source project path, the font families the project uses (§6.6),
and one record per `ArchiveEntry` (§4.4) — `asset_id`, `original_path`,
`archived_rel`, `content_hash`, `bytes`, `disposition`. It is the only record of
where the media came from, which is the thing that matters three years later, and
it makes §8's acceptance assertions a JSON compare rather than a filesystem walk.

**The archive must open without it.** The `.photon` is self-sufficient; deleting
the manifest costs provenance, never function.

### 6.6 Fonts

Recorded, not copied. `CaptionStyle.font_family` (`captions.rs:110-111`) and
`TextGen.font_family` (`graph.rs:96`) are family names; there is nothing in the
document to copy, and shipping font binaries has a licensing dimension this item
will not open. The manifest lists every distinct family the project names, so a
recipient whose machine lacks one knows *before* the titles render differently.
Deliberate exclusion, recorded in §10.

---

## 7. Long-running, cancellable, atomic

### 7.1 Is archiving a clip job? No — a sibling, on the same rails

[195 §3.1](195-k-c1-clip-jobs-framework.md) argues that `RenderQueue` is not
subsumed into `ClipJobQueue` because their preconditions and progress
vocabularies differ. The **same argument applies to archiving, for the same
reasons**, so K-C5 follows the same shape rather than inventing a third one:

- **A clip job's unit of work is one `AssetId`.** K-C1's MCP entry point is
  `start_clip_job { asset_id, kind, … }` and its `JobOutcome` is a closed enum of
  three *document mutations* (195 §3.2). Archiving has no asset, and its outcome
  is `None` — it mutates nothing (§5). Forcing it into `JobKind` would mean an
  `asset_id` that is ignored and a `JobOutcome` variant that exists for one
  caller.
- **`ClipJobQueue` v1 is one worker, FIFO** (195 §11, "Parallel job execution"
  excluded). A one-hour archive would head-of-line-block every proxy transcode
  behind it.
- **Progress vocabularies differ**, exactly as 195 §3.1 says of `RenderQueue`:
  archiving reports bytes copied of bytes planned, which neither `Running { frame,
  total, fps }` nor a transcode's time fraction expresses.

**What is shared, and must be:** K-C1's `jobs::JobView` read model, so one
"Background jobs" panel and one `list_jobs` cover archives too; `PathPolicy`;
`atomic_write`; the cooperative `Arc<AtomicBool>` cancel; and the journal.
Archiving enters as a third producer into `JobView`, precisely as `RenderQueue`
gains a `view()` adapter in 195 §3.1. `get_job_status` and `cancel_job`
(`crates/photonic-mcp/src/dispatch.rs:2628,2634`) resolve archive ids the same
way they will resolve clip-job ids.

### 7.2 Atomicity — the staging directory

37 §2.3's discipline is temp-and-rename, and `atomic_write::staging_path`
(`atomic_write.rs:21`) implements it by appending `.part` to any path. It works
unchanged on a **directory** path, and the rename is same-filesystem because the
staging directory is a sibling of the destination. So:

1. Refuse a **non-empty** `dest`. Never merge into an existing directory: a
   merged archive silently inherits stale media from a previous run, which is a
   wrong archive that looks right.
2. Create `staging_path(dest)` = `<dest>.part/`.
3. Copy media into `<dest>.part/media/`, streaming.
4. Write `<dest>.part/archive-manifest.json` with `write_atomic`.
5. Write `<dest>.part/<project-stem>.photon` **last**, with `write_atomic` over
   `save_photon`'s string — *not* `std::fs::write`, and not repeating §2.4's
   defect.
6. `std::fs::rename(<dest>.part, dest)`.

**Ordering is the atomicity guarantee.** A crash at any instant leaves a `.part`
directory whose root has no `.photon` in it — and a directory with no `.photon`
cannot be mistaken for a project, by a human or by the app. Step 6 is the single
instant at which the archive exists. A cross-device rename failure at step 6
fails the operation and leaves the staging directory; it never partially merges.

**`write_atomic` cannot be used for media, and this is the reason.** Its
signature is `write_atomic(output: &Path, bytes: &[u8])` (`atomic_write.rs:32`) —
it takes the whole payload in memory. Handing it a 40 GB source file is an OOM.
Media is copied by a chunked loop (1 MiB) that checks the cancel flag and
advances the byte-progress counter per chunk. `std::fs::copy` is likewise
unusable: one syscall, no cancel point, no progress. Both facts are stated
because both are the obvious first implementation.

### 7.3 Cancellation and cleanup

- Cancel is the proven mechanism: a per-job `Arc<AtomicBool>` polled between
  chunks, as `generate_proxy` does (`proxy.rs:312-318`). On cancel:
  `remove_dir_all(<dest>.part)` immediately, then `ArchiveError::Cancelled`.
  After a cancel, nothing exists at `dest` and nothing exists at `<dest>.part`.
- On any error: the same immediate `remove_dir_all`, then the typed error.
- **On a crash, the staging directory survives — deliberately.**
  `sweep_stale_staging` (`atomic_write.rs:60-83`) is for cache directories and is
  called at startup; the archive destination is a directory the user chose,
  possibly `~/Documents`. Auto-deleting a directory there unattended is a worse
  failure than leaving a `.part` behind. Instead the staging path is recorded in
  K-C1's journal, and the next project open reports "an archive to <path> was
  interrupted" with a one-click "remove leftovers" — the same journal-do-not-
  resume posture as 195 §9.1.

**Finding, in scope:** `sweep_stale_staging` cannot clean a staging *directory*
even where it is called. It filters on `path.extension() == Some("part")`
(`atomic_write.rs:69-71`) — which a directory named `x.part` passes — and then
calls `std::fs::remove_file` (`atomic_write.rs:79`), which fails on a directory,
so the entry is neither removed nor counted. K-C5 extends it with a directory arm
(`remove_dir_all` under the same age gate and the same never-fail-the-launch
discipline). Four lines, one new test beside the four at `atomic_write.rs:103-161`.

---

## 8. MCP surface

Warranted, and not marginal: 26 §11's own impact grep is
`archive_project|collect_media`, GUI/MCP parity is ROADMAP §10 point 3, and
PA-11 (full MCP parity) is recorded in 26 §5 as **not yet held** — a GUI-only
archive would widen a gap this programme is closing.

| Tool | Args | Notes |
|---|---|---|
| `plan_archive` | `{ dest?, include_unused?, allow_offline? }` → the `ArchivePlan` as JSON | **Read-only. Moves no bytes.** The tool that matters: it makes `dry_run` a first-class verb, so an agent can inspect dispositions, total bytes and the offline list before committing disk. `dest` optional — omit it to get dispositions and sizes without naming a target |
| `archive_project` | `{ dest, include_unused?, include_history?, allow_offline? }` → `{ job_id }` | Async. Status and cancellation go through the **existing** `get_job_status` / `cancel_job` (`dispatch.rs:2628,2634`); do not add archive-specific status tools |

Not added: `list_archives` (an archive is a directory; the OS lists directories)
and `restore_archive` (restoring is opening the `.photon`, which the GUI already
does and MCP deliberately cannot — §2.2).

**Authorization asymmetry, recorded rather than hidden.**
[28 §3.2](../specs/video-editor/28-security-model.md#32-default-roots) sets
`allow_read_outside` **true for read, false for write**. Archiving to an external
drive — the actual use case — is therefore a write outside the roots. Resolution:

- **GUI:** the native directory picker *is* the user's explicit authorization;
  the user typed the path. 28 §3.4's confirmation requirement is satisfied by the
  picker itself, and the app adds the chosen root to the session's write roots
  for that operation only.
- **MCP:** an out-of-root `dest` is **refused** with `PathNotPermitted`
  (`SecurityPathNotPermitted`, `diag.rs:250`) unless the user has configured the
  root. An agent must not be able to write a multi-gigabyte tree to a path it
  derived from a filename in a transcript.

This is not a parity exception — it is the same tool with the same schema and a
different authorization gate, which is what 28 §2's trust boundaries call for.

Wiring follows the shipped pattern exactly: arg structs in
`crates/photonic-mcp/src/protocol/args/video.rs` (beside `ImportMediaArgs` at
`:834`), handlers in `handlers/video.rs` near the media block, dispatch arms in
`dispatch.rs` beside `"import_media"` (`:2479`), schema entries in
`schema_gen.rs` (`tool_list()` at `:7`; the media block is at `:5631+`), and the
test-side consistency constant `VIDEO_TOOL_NAMES` (`handlers/video.rs:8277+`).
**`docs/mcp-api.md` is regenerated in the same change** — `ci.yml:163-167`
regenerates and `git diff --exit-code`s it, so this is mandatory, not optional.

Failing results carry the full `Diagnostic` per
[36 §5](../specs/video-editor/36-error-model.md), so an agent gets
`code`/`subject`/`consequence` rather than prose.

**One new `DiagCode`: `ProjectArchiveFailed`**, in the **existing** `Project`
family (`diag.rs:142-165`) — no new family is needed, which is a deliberate
contrast with 195 §9.3's new `Job` family and a cheaper change. Offline reporting
reuses `MediaNotFound` (`diag.rs:209`) with `Remedy::Relink`, and path refusal
reuses `SecurityPathNotPermitted` (`diag.rs:250`). Adding the one code requires
the matching entry in `EXPECTED_WIRE_CODES`
(`crates/photonic-core/tests/diag_catalogue.rs:27+`) in the same change, or the
frozen-catalogue gate trips — which is the gate working as designed.

`K-H` obligation: these tools land **with** the GUI verb, in the same change, per
26 §19.1's Trail row.

---

## 9. Acceptance fixtures and tests

**No rights-cleared content is required. K-C5 is not a gated item.** Added
fixture bytes: **zero**. The existing corpus
(`crates/photonic-video/tests/fixtures/`, ~2.5 MiB against a 5 MB budget) already
contains one of each reference class this item must prove:

- `color_bars.mp4` (5 KiB) and `beep_flash.mp4` — ordinary media;
- `channel_swap_rgb_to_gbr.cube` (126 B) — a real `AssetKind::Lut3d`, the class a
  clip-only scan drops;
- `title_asset.photon` / `title_doc_asset.photon` — `AssetKind::VectorDoc`.

**No test in this item needs ffmpeg.** Archiving is a byte copy; the plan reads
`MediaAsset` fields that tests set directly. That is unusual for this crate and
worth stating — it means the whole suite runs in the cheap CI job.

| # | Test | Where | Proves |
|---|---|---|---|
| T1 | `referenced_assets` returns clip sources, all four grade scopes, embedded graph grades, `MediaIn`/`Lut` — and **`MulticamAngle.source`** | `photonic-core/src/timeline/ops.rs` `mod tests`, beside `set_asset_rating_and_unused_assets` (`ops.rs:2540`) | §3.2. **Fails today** |
| T2 | Fold two clips into a multicam group (`create_multicam_group`, `ops.rs:1995`), then assert `unused_assets` is **empty** | `ops.rs` `mod tests` | The shipped remove-unused defect, as a regression test |
| T3 | `unused_assets == pool.keys() − referenced_assets()` over a project touching every class | `ops.rs` `mod tests` | §3.3's one-scan rule, mechanically |
| T4 | Archive a project with a video, a `.cube` and a `.photon` asset → all three land in `media/`; the archived document's `path` and `rel_path` both point inside | `photonic-video/tests/archive.rs` | §6.1 |
| T5 | Two distinct files both named `clip.mp4` → `clip.mp4` + `clip-2.mp4`, deterministic across two runs; a third asset pointing at the **same bytes** as the first dedups to one copy | `photonic-video/tests/archive.rs` | §6.4 rules 3 and 5 |
| T6 | Sanitization table: file names `../escape.mp4`, `a/b.mp4`, `CON.mp4`, `trailing. `, a 300-byte name, and one with a control byte — each lands **inside** `media/` with a legal name | `photonic-video/src/project/archive.rs` unit tests | §6.4 rule 2 |
| T7 | Referenced asset offline → `archive_project` refuses with `ReferencedAssetOffline`, **nothing** created at `dest` or `<dest>.part`; with `allow_offline: true` it completes, the entry is `Offline`, and its archived `path` is the **original** | `photonic-video/tests/archive.rs` | §3.5 |
| T8 | `ProxyOrigin::Generated` proxy → excluded, `ProxyRef` cleared in the archived doc; `ProxyOrigin::Attached` → copied into `media/` and its path rewritten; the whole `.photon.cache/` tree is absent | `photonic-video/tests/archive.rs` | §6.2 |
| T9 | Relocation: archive, then **move the archive directory**, then `load_photon` + `relocate_asset_paths` from the new location → every asset online. Also: `rel_path` pointing at a file that does not exist falls through to `path` | `photonic-core/tests/timeline.rs` + `photonic-video/tests/archive.rs` | §4.3 |
| T10 | Relink clears `rel_path`: archive, relink one asset elsewhere, move the archive → the relinked asset resolves by `path`, not by a stale `rel_path` | `photonic-core/tests/timeline.rs` | §4.2 |
| T11 | Serde: a v5 document with `rel_path: Some(...)` round-trips; one without it loads `None` and re-serializes without the key; `CURRENT_FORMAT_VERSION` still 5 | `photonic-core/tests/timeline.rs`, `tests/forward_compat.rs` | §4.1 |
| T12 | Atomicity: cancel mid-copy → `dest` absent, `<dest>.part` absent. Inject a failure after media but before the `.photon` → `<dest>` never exists. Non-empty `dest` → `DestinationNotEmpty`, existing contents untouched | `photonic-video/tests/archive.rs` | §7.2, §7.3 |
| T13 | `sweep_stale_staging` removes an aged `.part` **directory** as well as an aged `.part` file, and leaves fresh ones and non-`.part` entries alone | `photonic-video/src/media/atomic_write.rs` tests, beside `sweep_removes_aged_part_only` (`:140`) | §7.3's finding |
| T14 | Conservative mode: a project carrying a `GraphOp::Unknown` archives the **whole pool**, every extra entry marked `CopiedConservatively` | `photonic-video/tests/archive.rs` | §3.4 |
| T15 | MCP: `plan_archive` returns dispositions and writes nothing; `archive_project` → `job_id` → `get_job_status` → completion; an out-of-root `dest` returns `PathNotPermitted` | `photonic-mcp/src/handlers/video.rs` tests | §8 |
| T16 | GUI arm: File → Archive Project… headless, producing the same archive as the MCP arm — structural compare | `photonic-gui/tests/video_ui_paths.rs` + `photonic-app/tests/acceptance_stories.rs` | ROADMAP §10 points 2 and 10 |

---

## 10. Risks, open questions, deliberate exclusions

### 10.1 Risks

1. **Two reference scans.** If an implementer writes an archive-side scan
   "just for now", the archiver silently diverges from remove-unused and starts
   dropping media on the next model addition. Mitigation: §3.3's
   `referenced_assets` is the only scan, `unused_assets` is defined in terms of
   it, and T3 asserts the identity mechanically rather than by convention.
2. **The multicam hole is live in shipped code.** Until §3.2 lands, "Remove
   unused (N)" is wrong for any project with a folded multicam clip. T2 is the
   regression test; the fix is small and should land first, independently of the
   archiver, so the shipped defect closes even if archiving slips.
3. **Reusing `write_atomic` for media.** The obvious first implementation, and an
   OOM on any real project (§7.2). Reviewers should reject any media path that
   loads a file into a `Vec<u8>`.
4. **`PathPolicy` does not exist and is on the critical path** (§2.2), shared
   with K-C1. Whichever of K-C1 and K-C5 lands first implements it in
   `photonic-core` per 195 §7.3.5, with 28 §8's acceptance rows as its tests; the
   second consumes it. It must not be implemented twice.
5. **Very large archives.** 100 GB is an ordinary project. Progress must be
   byte-based (a file count lies badly when one file is 90% of the total), the
   cancel check must be per chunk rather than per file, and the operation must
   survive being the only thing running for an hour.

### 10.2 Deliberate exclusions

- **Consolidation / trim-to-used-range** — argued at §6.3. Separate verb,
  separate mini-spec, never a checkbox on Archive.
- **`.zip` output** — §10.3 Q2.
- **Archiving fonts** — §6.6. Recorded in the manifest, not copied.
- **Copying the sidecar cache** — §6.2. Everything in it is regenerable, and the
  one non-regenerable thing (`Attached` proxies) is not in it.
- **"Archive and switch"** — §5. Archive never re-points the open document.
- **Restore / unarchive** — an archive is a project; opening it is the restore.
- **Relink-by-content-hash** — K-C6 owns it (26 §11 K-C6). §4.3 stops at two
  resolution steps and hands offline assets to K-C6 deliberately.
- **The cache-purge pane** — the other half of K-C5's 26 §11 text. `summarize_cache`
  shipped as reporting only; per-category purge is a separate slice and is not
  re-specified here.
- **Making the project save atomic** — §2.4. Real, cited, and a save-path change.

### 10.3 Open questions needing a product call

- **Q1 — does Archive include unreferenced pool assets by default?**
  *Recommendation: **yes**, `include_unused: true` by default,* with an opt-out
  that shows exactly what it would drop. The whole posture of this document is
  "when in doubt, copy" (§3.4), and a user's unused B-roll sitting in a bin is
  part of the project *to them* even though `referenced_assets` cannot see why.
  The counter-argument — archives should be lean — is real, and the opt-out plus
  the plan's byte total serves it without making the destructive choice the
  default. This is a product call because it decides what "the project" means,
  not an engineering one.
- **Q2 — directory or `.zip`?** 26 §11 says "one folder / bundle".
  *Recommendation: **directory only** in v1.* A zip is not workable in place,
  doubles peak disk during creation, cannot be partially verified, and needs a
  new dependency for no new capability. A user who wants one archive file can zip
  a directory with tools they already have. Revisit only if a real workflow
  demands single-file transport.
- **Q3 — Archive an unsaved project?** `current_file` is `Option<PathBuf>`
  (`app/mod.rs:739`) and `proxy_cache_dir(None)` (`proxy.rs:75-80`) shows the
  codebase already treats "no project path" as a degraded temp mode.
  *Recommendation: **refuse**, with `ProjectNotSaved` and a "Save first" action.*
  The archive's name and its `rel_path` base both derive from the project file;
  inventing them for an unsaved document produces an archive whose identity is
  arbitrary. One dialog, no design change.
- **Q4 — how much of the plan does the GUI show before the user commits?**
  *Recommendation: the totals and the offline list always; the full per-asset
  table behind a disclosure.* A 400-asset table in a modal is not a decision aid.
  Pure UX call.

---

## 11. Clean-room provenance

Per [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
and [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol):

- **What was read.** Kdenlive's user-facing documentation (`docs.kdenlive.org`,
  `CC-BY-SA-4.0`) for the *existence and shape* of an "Archive Project" command —
  that such a feature collects referenced media into one location, that it
  presents a size total before running, and that offline media is its classic
  failure mode. That is a requirements source under 26 §2, cited and never
  pasted. No format specification was needed: the archive's format is Photonic's
  own `.photon` in a directory.
- **What was not read.** The Kdenlive source tree, the MLT/`mlt++` source tree,
  and any GPL/LGPL derivative. No identifier, constant, directory name, argument
  ordering, control flow or test case here derives from either. The implementer
  records the 23 §3.4 attestation for the `photonic-video-engine` subsystem, and
  an independent provenance reviewer checks identifiers, comments, constants and
  test provenance before merge (26 §2 point 2).
- **Where the design actually comes from.** Every concrete decision is derived
  from Photonic's own code, cited by `file:line` throughout: the staging-directory
  discipline from `atomic_write::staging_path` (`atomic_write.rs:18-25`); the
  copy/exclude line from `ProxyOrigin`'s existing "never deleted on detach" rule
  (`media.rs:231-235`) and from `summarize_cache`'s category list
  (`cache_stats.rs:38-64`); the relative-path scheme from `AssetSource::File`'s
  own documented-but-unimplemented contract (`media.rs:122-124`); the
  conservative-copy rule from `collect_grade_asset_refs`' existing "counted
  regardless of `enabled`/`bypass`" comment (`ops.rs:165-166`); the dedup caution
  from 26 §11 K-C6's own hash-collision watch-out; and the job shape from
  [195 §3.1](195-k-c1-clip-jobs-framework.md)'s sibling-not-merge argument.
- **A reference NLE limitation is not a requirement.** §6.3 rejects
  trim-to-used-range consolidation specifically because Photonic's exact rational
  time (**PA-8**), half-open ranges (**PA-7**) and E-1 source-range contract make
  the "used range" a computed thing rather than an arithmetic one — porting the
  simple version backwards would cut the wrong frames on any remapped clip.
  `ArchiveError` is a typed enum, never a string (**PA-9**). Nothing here touches
  the frame graph, `ContentHash`, or any cache key, so **PA-1** is untouched —
  and §6.2's refusal to archive derived cache is that property being consumed
  rather than worked around.
- **Bundled bytes: none.** No asset ships with this item, so
  [23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest)'s
  `AssetRightsManifest` gate is not engaged. **K-C5 is not legal- or
  fixture-gated.**
- **No new dependency.** Nothing in 26 §2's reject list, directly or
  transitively. `std::fs`, `serde_json` and the existing `photonic-core` types
  are the whole toolkit — including the deliberate refusal to add a free-space
  crate (§2.2 → §10.1 risk 5's honest consequence: a full disk fails mid-copy
  with `ArchiveError::Io` and leaves the `.part` directory, which §7.3 already
  handles).

---

## 12. Definition of done → ROADMAP §10

| # | ROADMAP §10 point | Answered by |
|---|---|---|
| 1 | Core op/engine service with unit tests | `ops::referenced_assets` (`photonic-core`) + `photonic-video/src/project/archive.rs`; T1–T8, T12–T14 |
| 2 | GUI route, or a recorded exception | File → Archive Project… with a plan dialog and a progress row in the background-jobs panel; T16. **No exception claimed** |
| 3 | MCP tool/schema/generated docs | §8: `plan_archive`, `archive_project`; `docs/mcp-api.md` regenerated under `ci.yml:163-167`. **No parity exception** — the MCP/GUI difference is an authorization gate on the same tool, argued at §8 |
| 4 | One user verb = one undo unit | §5: archiving mutates nothing and has **no** undo unit, argued not omitted. The one command touched (`RelinkAsset`) gains an additive pair whose inverse is the mechanical swap; T10 |
| 5 | Additive serde/migration round-trip | §4.1: stays v5, no document field added; T11 |
| 6 | IR/eval/golden/sync coverage for new pixel/audio paths | **None needed** — K-C5 adds no pixel or audio path. `grep '\.group\|ContentHash'`-equivalent: the archiver never touches `photonic-render` or the graph |
| 7 | Hard gates green; trend metrics not regressed | Archiving runs off the engine thread; the present loop and every 37 §4.2 hard gate are untouched. No new budget |
| 8 | Offline, privacy, licensing, content, product gates | §11: no bundled bytes, no new dependency, no network. The manifest contains paths, ids and hashes — **no media content** (36 §7's rule). Q1/Q2/Q3 in §10.3 are the product gates and are named with recommendations |
| 9 | No protected-surface regression | PA-1/PA-7/PA-8/PA-9 addressed in §11. Linked A/V, groups, sync-lock and the undo model are not touched. `unused_assets`' contract **widens** (more assets counted as used), which can only make removal safer |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | §1's five outcomes are the L4 script; T16 is the parity story, T9 is the "move it and it still opens" proof that L4 actually delivers relocatability |

---

## Follow-ups — other documents and code that need a change, **not made here**

1. **`crates/photonic-core/src/timeline/media.rs:122-124`.** The doc comment
   describes a resolution order (`rel_path` → `path` → hash) that **no code
   implements** (§2.3). K-C5 implements the first two steps; the comment should
   be restated as normative *at that point*, naming `load::relocate_asset_paths`
   and marking relink-by-hash as K-C6's.
2. **[196 §2.1](196-x-2-opentimelineio-interchange.md)'s `AssetSource::File`
   table row** ("`rel_path` first, then `path`, then hash") paraphrases that
   comment and therefore describes intent, not shipped behaviour. It becomes
   accurate when K-C5 lands; until then it should carry a "(intent; lands in
   K-C5)" note.
3. **`ops::unused_assets`' doc comment (`ops.rs:174-184`)** says "all four
   reference classes". With `MulticamAngle.source` it is five. Correct it in the
   same change as §3.2, or the comment becomes the next person's evidence that
   the scan is complete.
4. **`atomic_write::sweep_stale_staging` (`atomic_write.rs:60-83`)** removes only
   files, so a stale `.part` **directory** is never swept even in a cache dir
   (§7.3). 37 §2.3's "startup sweeps stale temps in the cache directory" should
   say directories too.
5. **[36 §3.2](../specs/video-editor/36-error-model.md)'s family table** gains
   `ProjectArchiveFailed` under the existing `Project` family, and
   `crates/photonic-core/tests/diag_catalogue.rs:27+`'s `EXPECTED_WIRE_CODES`
   gains the matching entry in the same change.
6. **[02 §1](../specs/video-editor/02-engine.md)'s crate module map** should list
   `photonic-video/src/project/`.
7. **[10-mcp-tools.md](../specs/video-editor/10-mcp-tools.md)** should document
   `plan_archive` / `archive_project`, and record the plan-before-act pattern —
   it is the first read-only planner tool in the video surface and is worth
   generalizing.
8. **26 §11 K-C5's Impact line** bundles archiving, remove-unused and the cache
   pane. Two of the three shipped; the item text should be re-scoped to archiving
   so the effort estimate is not read as including work that is done.
9. **The project save is not atomic** (§2.4): `write_photon_file`
   (`crates/photonic-gui/src/app/mod.rs:2169`) and MCP `save_document`
   (`crates/photonic-mcp/src/handlers/document.rs:93`) both end in
   `std::fs::write`, against 37 §2.3's "every job that writes a file". Out of
   scope here; it deserves its own small item, because a crash during save loses
   the previous file as well as the new one.
10. **ROADMAP.md §0 / §2** — add a K-C5 row when the item lands, per the existing
    convention, noting that the archiving half closes and the cache-purge half
    remains.
