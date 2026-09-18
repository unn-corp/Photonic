# 204 — K-G4 Project templates (mini-spec)

> Status: **proposed mini-spec — not accepted, no code authorization.** Written to
> satisfy the [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands)
> K-Band 5 exit condition ("an accepted mini-spec exists *before* code, naming its
> data-model change, migration, undo unit, MCP surface and acceptance fixtures.
> No item here starts without one").
> Owner doc: [26 §15 K-G4](../specs/video-editor/26-kdenlive-mlt-parity.md#k-g4--project-templates).
> [23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)'s
> agent-proof boundary still applies: acceptance of this document, plus the
> **S11 amendment in [§3.6](#36-the-exact-s11-amendment)**, is what authorizes K-G4.

**K-G4 is gate-blocked, not effort-blocked.**
[ROADMAP §7](../specs/video-editor/ROADMAP.md#7-legal-content-and-product-gates) (line 331):
*"K-G4 project templates: template storage location and bundled-asset manifests
must be settled first — the same gate S11 places on D-11."*
[ROADMAP §8](../specs/video-editor/ROADMAP.md#8-architecture-decisions-and-defaults)
S11 (line 350) is an **Open gate**: *"D-11 template storage/location must be
chosen before bundled templates."*
Resolving that gate is the primary product of this document, and it is
[§3](#3-storage--the-s11-gate-resolved). Everything after §3 follows from it.

Verified against `feat/video-editor-module` @ `8a33f32`. Every `file:line` below
was read in that tree.

---

## 1. Problem and user outcome

**Today.** Every video project starts from the same blank slate. The two entry
points are the welcome screen's *New Video Project* hero card
(`crates/photonic-gui/src/welcome.rs:1070`) and its `V` shortcut (`welcome.rs:915`),
both of which fire `WelcomeAction::CreateNewVideo` with a hardcoded
`1920×1080 / FPS_30` spec. That lands in `app/mod.rs:2955`, which builds a bare
`Document` and calls `ensure_timeline_project_with`
(`crates/photonic-gui/src/app/monitor.rs:495`), whose entire body is:

```rust
history.execute(Command::Timeline(ops::create_project()), doc);
let seq = Sequence::new("Sequence 1", frame_rate, width, height);
history.execute(Command::Timeline(ops::add_sequence(seq)), doc);
```

`Sequence::new` (`crates/photonic-core/src/timeline/sequence.rs:175`) creates
**zero tracks**, one `16:9` format, no bins, no marker categories, default
`ProjectVideoSettings`. So an editor whose house layout is "4 video / 6 audio
tracks, 25 fps, a 9:16 companion format, bins named Footage / Audio / Graphics,
a house LUT on the sequence master" rebuilds that by hand, every project,
forever. There is no way to record it and no way to hand it to a colleague.

**After K-G4.** A user can:

1. **Save the current project's skeleton as a named template** — tracks,
   per-track properties, sequence formats, frame rate, effect stacks and grades
   at every scope, the audio master, bins, marker categories, project settings.
2. **Start a new project from a template** — the new document opens *untitled*,
   with fresh ids, exactly as if they had built it by hand and not yet saved.
3. **Add a sequence to the current project from a template** — the far more
   common in-flight case, and the one that is a real undoable edit.
4. Do all three from an agent, and see templates as data
   (`list_project_templates`).

**The design constraint that makes this cheap and ungated:** a template carries
**no media bytes and no `MediaAsset` rows at all** ([§3.3](#33-what-a-template-contains--and-what-it-may-never-contain)).
A template that referenced media would need an `AssetRightsManifest` per
[23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest)
the moment anything shipped with the app, which is precisely what S11 is holding
the door shut on. Structure is not content; structure has no rights.

**Non-goal.** A template is not a project format, not a live link, and not a
theme. There is no "update all projects made from this template". Nothing in
`Document` records which template a project came from ([§10.3](#103-deliberately-excluded)).

---

## 2. Current state in code

Exact, as of `8a33f32`. Read this before disagreeing with §3 or §4.

### 2.1 The app-level store family — this is the S11 evidence

There is already **one** answer to "where does user state live", used by five
subsystems, and it resolves through a single function:

```rust
// crates/photonic-core/src/diagnostics.rs:29
pub fn crash_dir() -> Option<PathBuf> {
    if let Ok(appdata) = std::env::var("APPDATA") { return Some(PathBuf::from(appdata).join("Photonic")); }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") { return Some(PathBuf::from(xdg).join("Photonic")); }
    if let Ok(home) = std::env::var("HOME") { return Some(PathBuf::from(home).join(".config").join("Photonic")); }
    None
}
```

| Consumer | Path | Resolved at |
|---|---|---|
| Crash reports | `<config>/crash-reports/` | `diagnostics.rs:43` |
| GUI preferences | `<config>/preferences.json` | `preferences.rs:317` (via `welcome::config_dir`, `welcome.rs:2078`) |
| Recent documents | `<config>/recent_docs.json` | `welcome.rs:2083` |
| Disk-search roots | `<config>/disk_roots.json` | `welcome.rs:2087` |
| Untitled-doc recovery autosaves | `<config>/recovery/` | `app/autosave.rs:14` |
| **Custom export presets** | `<config>/export_presets.json` | `export/presets.rs:407` |

`welcome::config_dir` (`welcome.rs:2078`) and `export::presets::config_dir`
(`crates/photonic-video/src/export/presets.rs:401`) are both one-line
delegations to `photonic_core::crash_dir`, each carrying a doc comment saying so
in terms — `presets.rs:394-400` states it as a rule: *"the **same** directory
family as other app-level prefs (§3.6: 'same directory family as other app-level
prefs, not the project file'). Reuses `photonic_core::crash_dir` … rather than
introducing a second directory-resolution implementation or a new `dirs`-crate
dependency."*

**The export-preset store is the exact shape K-G4 should copy**
(`presets.rs:384-446`): a typed `PresetStoreError` (`:385`) whose first variant
is `NoConfigDir`; `load_custom_presets` / `save_custom_presets` that resolve the
path; and **path-parameterized** `load_custom_presets_from(&Path)` (`:428`) /
`save_custom_presets_to(&Path, …)` (`:437`) whose stated reason is *"for tests
(and any future 'import presets from a specific file' UI action) without
touching the real user config dir."* Its built-ins (`built_in_presets()`,
`presets.rs:368`) are **constructed in Rust**, not loaded from bundled files.

There is **no `dirs`/`directories` crate anywhere** in `photonic-gui` or
`photonic-app` — grepping for `ProjectDirs|BaseDirs|dirs::` returns nothing. Any
proposal that reaches for one is adding a dependency the tree deliberately does
not have.

### 2.2 The document lifecycle seam a template plugs into

| Thing | Where | Note |
|---|---|---|
| `CURRENT_FORMAT_VERSION = 5` | `photonic-core/src/document.rs:117` | with a per-version changelog comment above it |
| `COMPAT_WINDOW = 1` | `migration.rs:16` | how far ahead a file may be and still load leniently |
| `migrations()` chain `V1ToV2 … V4ToV5` | `migration.rs:58` | |
| `detect_version(&Value)` | `migration.rs:295` | reads the version **before** deserialization |
| `Document::from_value_with_report` | `document.rs:1717` | migrates, deserializes, `ensure_default_artboard`, then `finalize_load` |
| Newer-file refusal (`> CURRENT + COMPAT_WINDOW`) | `document.rs:1727-1737` | inside the window it **falls through and loads leniently** |
| `finalize_load` → `LoadReport` | `timeline/load.rs:138`, `:614` | unknown variants, dissolved groups, dangling categories, notices |
| `save_photon(document, history: Option<&HistorySnapshot>)` | `photon_file.rs:36` | *"Pass `None` for `history` to write a document-only file"* |
| `load_photon` lifts sibling keys before migrating | `photon_file.rs:64-83` | `photon_history`, `photon_format` |
| `PHOTON_FORMAT_VERSION = 1`, versioned **independently** of `format_version` | `photon_file.rs:31` | the exact precedent §3.2 reuses |
| Unknown top-level keys are ignored by `Document` deserialization | `photon_file.rs:14-17` (states it as the back-compat guarantee) | a third sibling key is therefore free |
| `Document::new` mints `id: Uuid::new_v4()` | `document.rs:902,912` | |
| `TimelineProject::new` / `insert_sequence` | `sequence.rs:47`, `:69` | |
| `Sequence::duplicate_with_fresh_ids` | `sequence.rs:244` | re-mints `SequenceId`, `TrackId`, `ClipId`, `MarkerId`, `GroupId`, `LinkGroupId`, caption `TrackId`/`CueId` |
| `ops::{create_project, add_asset, add_sequence, create_sequence, set_active_sequence, create_bin, duplicate_sequence}` | `ops.rs:95, 101, 325, 352, 341, 1942, 364` | the pure command constructors both GUI and MCP call |
| `TimelineCmd::AddSequence { sequence: Box<Sequence> }` — carries the **whole** sequence inline | `commands.rs:451` | its inverse is `RemoveSequence` (`commands.rs:2206`) |
| `AddBin` ⇄ `RemoveBin` inverse pair | `commands.rs:2523-2524` | |
| `Command::Batch(Vec<Command>)`; inverse is the reversed batch of inverses | `history/mod.rs:2242`, `:3172-3178` | |
| `create_sequence` MCP handler batches `[CreateProject?, AddSequence]` as **one** `execute_discrete` | `photonic-mcp/src/handlers/video.rs:309-320` | the exact commit shape §6.2 reuses |

### 2.3 The GUI surfaces a template verb has to land on

- `FILE_OPTIONS = &["Document", "Save", "Export"]` (`app/mod.rs:290`), rendered
  by the File drawer (`app/menu_drawer.rs:31`). "New" is at `menu_drawer.rs:46`
  (opens `NewDocumentForm`, `welcome.rs:382`), "Open…" at `:54`, "Save" at
  `:114` gated on `let can_save = self.current_file.is_some();` (`:113`),
  "Save As…" at `:128`, "Export…" at `:163`.
- `run_file_dialog` (`app/mod.rs:1848`), `load_document` (`:2016`),
  `apply_opened_history` (`:2148`), `write_photon_file` (`:2161` — calls
  `history.enforce_size()`, `snapshot_state()`, `save_photon(doc, Some(&snap))`).
- `open_in_new_tab(doc, history, view, new_doc, new_history, file: Option<PathBuf>)`
  (`app/tabs.rs:61`) — *"`file` is its `.photon` path (None for a brand-new
  untitled doc)"*. `tab_title` (`tabs.rs:15`) uses the file stem when saved,
  else `doc.name`, else `"Untitled"`.
- `create_document_from_spec` (`app/dialogs.rs:52`) is the canonical
  "install a fresh document" sequence: `*doc = …; history.reset(); fit_pending =
  true; current_file = None; selected_id = None;`.
- Untitled documents are autosaved to `<config>/recovery/` (`app/autosave.rs:14`)
  and Save on an untitled tab prompts Save-As (`app/close_guard.rs:38-46`).

### 2.4 What does not exist — say it plainly

- **No template anything.** `grep -i template` across `photonic-core`,
  `photonic-video`, `photonic-gui`, `photonic-mcp` returns only:
  `Layer::is_template` (`layer.rs:20` — the *tracing* template layer, unrelated),
  the vector `get_document_template` / `apply_document_template` pair
  (`photonic-mcp/src/handlers/doc_state.rs:491`, `:528`), and the
  `list_title_templates` / `insert_title_template` stubs
  (`handlers/video.rs:6815`, `:6823`). There is no project-template type, store,
  file, directory, command, tool or UI.
- **The vector `get_document_template` is not a store and not a file.** It
  returns a JSON *string* in the tool result (`doc_state.rs:509-521`), stripping
  `nodes` and clearing every `layer.node_ids` (`:496-501`); the agent holds it
  and hands it back to `apply_document_template`, which **merges
  non-destructively** into the current document (canvas → guides → export
  profiles → layers-by-name, `doc_state.rs:544` onward). It never touches disk and never
  touches `timeline`. It is a good precedent for *what to strip*, and no
  precedent at all for *where to keep it*.
- **The title-template library does not exist.** `list_title_templates` returns
  `{"templates": []}` with the text *"no title templates available (the shipped
  library lands in P6)"* (`handlers/video.rs:6817-6821`), and
  `insert_title_template` returns `NotSupportedV1` (`:6828-6831`). 26 K-G4's
  impact line promises *"tracks, formats, bins, **title templates**
  pre-populated"* — the title-template quarter of that promise is blocked on
  [05 §4b](../specs/video-editor/05-import-export.md#4b-starter-title-template-library-d-11-ships-p6),
  not on K-G4. §10.3 records it as excluded.
- **No `ProjectProfile`.** 26 K-G1 says *"grep `project_profile` clean"*, and it
  still is. `ProjectVideoSettings` (`sequence.rs:90`) carries exactly four
  fields: `generate_proxies`, `cache_limit_mb`, `default_frame_rate`,
  `audio_sample_rate`. K-G4 does not need K-G1 and must not wait for it
  ([§10.2 Q3](#102-open-questions-each-with-a-recommendation)).
- **No path sandbox in `photonic-mcp`.** `SecurityPathNotPermitted` exists in
  the diag catalogue (`diag.rs:250`, `:396`, `:421`) and is referenced by
  `crates/photonic-core/tests/diag_catalogue.rs:62` — and **nowhere else in the
  workspace**. `McpServerConfig` is `{ port, secret }` (`server.rs:22-25`); no
  permitted-roots list is implemented. This is why §7 addresses templates by
  **name**, never by path.

### 2.5 The finding that shapes §4: id remapping is per-sequence and incomplete

`Sequence::duplicate_with_fresh_ids` (`sequence.rs:244`) is the only id-remapper
in the tree. Its own doc comment (`sequence.rs:236-243`) states the limit:

> *"Referenced assets, composition graphs and nested sequences are shared (their
> ids are left untouched)."*

Reading the body confirms it. It re-mints `SequenceId` (`:247`), `TrackId`
(`:262`), `ClipId` (`:264`), `LinkGroupId` (`:265-267`), `GroupId` via
`group_remap` (`:252-257`, `:270`, `:277-287`), clip `MarkerId` (`:273`),
sequence `MarkerId` (`:290`), caption `TrackId`/`CueId` (`:293-295`). It does
**not** touch:

- `ClipSource::NestedSequence { sequence }` (`clip.rs:175`) — still points at the
  **original** `SequenceId`;
- `Clip.composition: Option<GraphId>` (`clip.rs:53`) and
  `TimelineProject.{graphs, project_graph}` (`sequence.rs:31,34`);
- `MediaBin.id` / `MediaAsset.bin` (`media.rs:294`, `media.rs:59`);
- `Marker.category: Option<MarkerCategoryId>` against
  `TimelineProject.marker_categories` (`sequence.rs:42`, `:732`).

For `duplicate_sequence` (`ops.rs:364`) that is correct — a duplicate lives in
the *same* project and legitimately shares the project's assets, graphs, bins and
categories. For a template it is **wrong in every one of those four ways**,
because instantiation crosses a document boundary. §4.2 therefore adds a
project-scope remap rather than reusing `duplicate_with_fresh_ids` verbatim, and
[§10.1](#101-risks) ranks the `NestedSequence` case as the highest-blast-radius
defect in the item: a nested clip silently pointing at a `SequenceId` that does
not exist in the new document renders as nothing, with no error.

---

## 3. Storage — the S11 gate, resolved

### 3.1 Location: `<config>/Photonic/templates/`, one file per template

**Decision.** Templates live in a **directory under the existing app config
dir**, resolved through `photonic_core::crash_dir()` like every other app-level
store (§2.1), one file per template:

```
<config>/Photonic/                     photonic_core::crash_dir()   diagnostics.rs:29
├── preferences.json                                                preferences.rs:317
├── recent_docs.json                                                welcome.rs:2083
├── disk_roots.json                                                 welcome.rs:2087
├── export_presets.json                                             presets.rs:407
├── crash-reports/                                                  diagnostics.rs:43
├── recovery/                                                       autosave.rs:14
└── templates/                          ← NEW, the whole of S11
    ├── podcast-2cam.photon
    └── vertical-social.photon
```

The three candidates the gate names, decided:

| Candidate | Verdict |
|---|---|
| **User config dir** (`<config>/templates/`) | **Chosen.** It is where the app already keeps every piece of cross-project user state, it needs no new path-resolution code and no new dependency, it survives project deletion, and it is the only location that makes "my house layout" reusable across projects — which is the entire feature. |
| **Alongside the project file** | **Rejected.** A template is by definition *not* about one project. Putting it next to a `.photon` means it is lost when that project is archived, invisible from the welcome screen before any project is open, and duplicated per project. It also puts template writes inside the user's content tree, where an autosave/export collision is possible. |
| **Repo-bundled read-only set** | **Rejected as a shipping mechanism, and unnecessary as a source of built-ins.** See §3.4 — built-ins are *constructed in Rust*, so no bytes ship and no manifest question arises. |

**Why a directory and not one `templates.json` array** (which would mirror
`export_presets.json` more literally): an export preset is ~20 scalar fields; a
template is a whole `Document`. One aggregate file means one corrupt byte loses
every template, a rewrite of the whole file on every save, and no way for a user
to send a colleague one template. One file per template makes each independently
readable, writable, deletable, copyable and diffable, and makes the store a
directory scan.

### 3.2 Container: a `.photon` file with a third sibling key

**Decision.** A template file **is a `.photon` file** — the same container, the
same extension, produced by the same `save_photon` (`photon_file.rs:36`) — with
one additional top-level sibling key beside `photon_format`:

```json
{
  "…all the Document fields…": "…",
  "format_version": 5,
  "photon_format": 1,
  "photon_template": {
    "v": 1,
    "name": "Podcast — 2 cam",
    "description": "4V/6A, 25 fps, 16:9 + 9:16, house bins",
    "created": "2026-07-28T14:02:11Z",
    "app_version": "0.x.y"
  }
}
```

This is not a new format. It is the exact trick `photon_file.rs` already
documents (`:1-22`) and already relies on: `load_photon` lifts its sibling keys
out of the `Value` *before* migration (`photon_file.rs:70-77`), and `Document`
deserialization ignores unknown top-level keys — which `photon_file.rs:14-17`
states as the load-bearing back-compat guarantee (*"Older Photonic builds …
open new-format files unchanged — serde ignores the two unknown keys"*).

Four properties fall out for free, and they are the argument for this choice:

1. **A template opens as a project.** Drag one onto the app, or File ▸ Open it,
   and it opens as an ordinary (empty-timeline-skeleton) document. Nothing
   breaks; the `photon_template` key is simply ignored. There is no dead-end file
   type a user can be stranded with.
2. **`photon_template.v` is versioned independently of `format_version`**,
   exactly as `PHOTON_FORMAT_VERSION` is (`photon_file.rs:28-31`). Template
   metadata can evolve without touching the document schema, and vice versa.
3. **No new serializer.** `save_photon` / `Document::from_value` are the whole
   read/write path; the template layer is a `serde_json::Value` key lift.
4. **History is dropped by construction.** Save-as-template calls
   `save_photon(&doc, None)` — the documented document-only form
   (`photon_file.rs:34-35`) — so a template never carries the author's undo
   history, which would otherwise leak their editing session, bloat the file, and
   be restored into a document it does not describe.

The `.photon` extension is deliberate: a distinct extension (`.photontemplate`)
would need OS association work, an `rfd` filter, and would break property 1 for
zero benefit. Templates are told apart from projects by **which directory they
are in** and by the presence of `photon_template` — not by their extension.

### 3.3 What a template contains — and what it may never contain

A template is a **project skeleton**. Concretely, from the captured `Document`:

| Kept | Cleared / refused |
|---|---|
| `TimelineProject.sequences` — tracks with all `Track` props (`kind`, `enabled`, `locked`, `effects`, `grade`, `blend`, `opacity`, `audio`), `formats`, `active_format`, `frame_rate`, `start_timecode`, `master_effects`, `master_grade`, `audio_master`, `work_range` | `MediaPool.assets` — **must be empty** |
| `MediaPool.bins` — the bin *hierarchy*, empty of assets (`media.rs:293`) | any clip whose `ClipSource` is `Asset` or `Vector` (`clip.rs:167`, `:171`) — both carry an `AssetId` |
| `TimelineProject.{graphs, project_graph}` reachable from a kept scope | `ClipSource::Unknown` (`clip.rs:195`) — a foreign tag in a file meant to be reused is not preservable with meaning |
| `TimelineProject.marker_categories`, sequence-level `markers` | `Document.nodes`, and every `layer.node_ids` — cleared exactly as `get_document_template` does (`doc_state.rs:497-501`) |
| `ProjectVideoSettings` (`sequence.rs:90`) | `Document.{annotations, recent_colors}`, `selection` (already `#[serde(skip)]`, `document.rs:680`) |
| `Document.{width, height, dpi, color_mode, layers (empty), guides, export_profiles, artboards, history_max_mb}` | the history snapshot (`save_photon(.., None)`) |

**The hard rule, and it is the whole of the second half of S11:**

> **A project template carries no media bytes, no `MediaAsset` row, and no
> `AssetId` in any clip source. `write_template` refuses, naming every
> offending clip, rather than stripping silently.**

Clips are permitted **only** where their source synthesizes from parameters and
references nothing outside the template:

- `ClipSource::SolidColor { color }` (`clip.rs:178`) — allowed;
- `ClipSource::Adjustment` (`clip.rs:182`) — allowed;
- `ClipSource::Text { content }` (`clip.rs:187`) — allowed (styled text through
  the existing `CaptionStyle` cascade; no asset, no font file bundled);
- `ClipSource::NestedSequence { sequence }` (`clip.rs:175`) — allowed **only**
  when the target sequence is itself in the template, checked at capture;
- `Asset`, `Vector`, `Unknown` — **refused**.

Rationale for refusing rather than stripping: a user who saves a cut project as
a template and silently gets an empty timeline back will not notice until they
have built a project on it. Refusing with *"3 clips reference media and cannot
be saved in a template"* and a "Save skeleton only" button is one extra click and
zero surprises. This is [39 §1.1](../specs/video-editor/39-document-lifecycle.md#11-one-verb-one-unit)'s
validate-then-commit discipline applied to a write.

Rationale for the rule at all: the moment a template can reference media, three
things become true simultaneously — a template can be broken by a file move
(offline media in a file with no project to relink against), a template can leak
a user's absolute paths to a colleague, and **any template Photonic itself ships
needs an `AssetRightsManifest`** per 23 §7.2, with the build validation, evidence
digests and reviewer sign-off that section mandates. Refusing media costs one
validation function and takes the whole rights surface off the table.

### 3.4 Built-ins: constructed in Rust, zero bundled bytes

**Decision.** Any built-in templates Photonic ships are **`fn` bodies, not
files** — the shape `built_in_presets()` already uses
(`export/presets.rs:368-380`, nine presets, all constructed):

```rust
/// The built-in project templates. Constructed in code, exactly as
/// `export::presets::built_in_presets` is (presets.rs:368), so nothing is
/// bundled and no AssetRightsManifest (23 §7.2) is implicated.
pub fn built_in_templates() -> Vec<ProjectTemplate> { … }
```

A built-in is a handful of `Sequence::new` / `Track` / `SequenceFormat` /
`ProjectVideoSettings` values. Suggested v1 set, all of which are pure numbers
and names — resolutions, rational frame rates and track counts, i.e. facts, not
expression:

| Built-in | Shape |
|---|---|
| *Blank 1080p 30* | today's `ensure_timeline_project_with` default, made explicit |
| *Interview 2-cam* | 2 video + 4 audio tracks, 1920×1080, 25 fps |
| *Vertical social* | 1 video + 2 audio, `9:16` 1080×1920 active + a `16:9` companion format (CAP-012/PA-6) |
| *Multicam 4-up + music bed* | 4 video + 6 audio, 1920×1080, 30000/1001 |

They are **read-only** in the UI (shown with a lock, "Duplicate to edit" — the
same treatment `presets.rs:366-367` records for built-in export presets), and
they are merged into the listing ahead of user templates, deduped by name with
the user's copy winning.

**Consequence for the gate: there are no bundled asset bytes in K-G4, so there
is no bundled-asset manifest to settle.** That is not an evasion of the second
half of S11 — it is the answer to it, and it is enforced by §3.3's rule plus
test T9.

### 3.5 A shared store module, not a fourth copy of the same code

Put the store in **`crates/photonic-core/src/templates.rs`**: `photonic-core`
owns `Document` (`document.rs:660`), `photon_file` (`lib.rs:17`), the timeline
model and `crash_dir` itself, and it already depends on `serde_json`. No new
crate, no new dependency, no new path resolution.

`photonic-video`'s `export::presets::config_dir` (`presets.rs:401`) and
`photonic-gui`'s `welcome::config_dir` (`welcome.rs:2078`) are already two
one-line delegations to the same function. K-G4 must not add a third. The
proposal is a single `pub fn app_config_dir()` re-export in the new module,
delegating to `crash_dir` with the same comment, and a follow-up
([§12.5](#12-follow-ups)) to point the other two at it.

### 3.6 The exact S11 amendment

S11 (`ROADMAP.md:350`) currently reads:

> | S11 | D-11 template storage/location must be chosen before bundled templates. | Open gate |

Note first that **"D-11" is ambiguous in the source documents** and the
ambiguity should be resolved in the same edit: 23 §5 (`23-…:123-127`) uses
"D-11" for the *SPEC decision* about v1 title templates and stock media, and
explicitly says *"This SPEC decision is not roadmap feature D-11, the
beat-conformed edit-template feature owned by 22-dji-advanced-workflows.md"* —
which is the D-11 at [18 §144](../specs/video-editor/18-dji-parity.md).
ROADMAP §7's *"D-11: template location/format and bundled asset manifests"* sits
in the D-series feature list, so it is the **18/22 edit-template feature**. All
three senses of "template" want the same storage answer, which is why one
amendment can close it.

**Proposed replacement, on acceptance of this document:**

> | S11 | **Template storage is resolved.** All template kinds — project templates (K-G4), beat-conformed edit templates (D-11), and the starter title-template library (05 §4b) — live in `<config>/Photonic/templates/…` under `photonic_core::crash_dir()`, one file per template, in the `.photon` container with a `photon_template` sidecar key. Built-ins are constructed in Rust, never bundled as files. **A template may carry no media bytes and no `AssetId`**, so no `AssetRightsManifest` (23 §7.2) is implicated; any future template kind that needs bundled media re-opens this gate for that kind alone. | **Resolved** [204](../proposals/204-k-g4-project-templates.md) |

And ROADMAP §7's K-G4 bullet (`:331`) becomes: *"K-G4 project templates:
**ungated** — storage settled by S11; templates carry no media, so no
bundled-asset manifest applies."* Both edits are follow-ups
([§12.1](#12-follow-ups)), not made here.

---

## 4. Data-model change

### 4.1 Persisted document model: none

**No field, variant or type is added to, removed from, or retyped in
`Document`, `TimelineProject`, `Sequence`, `Track`, `Clip` or `MediaPool`.** A
document instantiated from a template is byte-indistinguishable from one built
by hand: same shapes, same serde, same `format_version`.

The template's own metadata lives **outside** `Document`, in the
`photon_template` sibling key (§3.2) — the same relationship `photon_history`
has to `Document` today. Nothing records "this project came from template X"
([§10.3](#103-deliberately-excluded)).

New types, all in the new `crates/photonic-core/src/templates.rs`, none
persisted inside `Document`:

```rust
/// Version of the `photon_template` sidecar payload, independent of both
/// `CURRENT_FORMAT_VERSION` and `PHOTON_FORMAT_VERSION` (photon_file.rs:31).
pub const PHOTON_TEMPLATE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemplateMeta {
    #[serde(rename = "v", default = "default_template_version")]
    pub version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// UTC ISO-8601, same shape as `CrashReport::timestamp` (diagnostics.rs:54).
    pub created: String,
    /// `CARGO_PKG_VERSION` at write time — diagnostic only, never gates a read.
    pub app_version: String,
}

/// An in-memory template: its sidecar metadata plus the skeleton document.
pub struct ProjectTemplate { pub meta: TemplateMeta, pub document: Document }

/// One row of the browser listing — cheap, no document parse beyond the header.
pub struct TemplateEntry {
    pub meta: TemplateMeta,
    pub path: Option<PathBuf>,   // None for a built-in
    pub built_in: bool,
    pub sequences: usize,
    pub video_tracks: usize,
    pub audio_tracks: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("could not resolve the app config directory")] NoConfigDir,
    #[error("io error: {0}")] Io(#[from] std::io::Error),
    #[error("json error: {0}")] Json(#[from] serde_json::Error),
    #[error("no template named {0:?}")] NotFound(String),
    #[error("a template named {0:?} already exists")] NameInUse(String),
    /// §5.2 — stricter than a document's, deliberately.
    #[error("template was written by a newer build (document format {file}, this build writes {supported})")]
    DocumentVersionTooNew { file: u32, supported: u32 },
    #[error("template sidecar version {file} is newer than {supported}")]
    TemplateVersionTooNew { file: u32, supported: u32 },
    /// §3.3 — the media rule, refused rather than stripped.
    #[error("{0} clip(s) reference media; a template carries no media")]
    MediaBearing(Vec<ClipId>),
    #[error("{0} media asset(s) present; a template carries no media pool")]
    AssetsPresent(usize),
}
```

Store API, mirroring `presets.rs:414-446` field for field, **including the
path-parameterized pair whose stated purpose is testability without touching the
real config dir**:

```rust
pub fn templates_dir() -> Option<PathBuf>;                                  // crash_dir()/templates
pub fn list_templates() -> Result<Vec<TemplateEntry>, TemplateError>;       // built-ins + dir scan
pub fn list_templates_in(dir: &Path) -> Result<Vec<TemplateEntry>, TemplateError>;
pub fn read_template_at(path: &Path) -> Result<ProjectTemplate, TemplateError>;
pub fn write_template_in(dir: &Path, t: &ProjectTemplate, overwrite: bool) -> Result<PathBuf, TemplateError>;
pub fn delete_template_in(dir: &Path, name: &str) -> Result<(), TemplateError>;
pub fn built_in_templates() -> Vec<ProjectTemplate>;
```

Templates are keyed by **display name** (`meta.name`); the on-disk filename is a
sanitized slug and an implementation detail, resolved by the directory scan
reading each file's `photon_template.name`. Slug collisions between different
display names get a `-2`, `-3` suffix; a same-name write requires `overwrite`.
A missing directory is **not an error** — it means "no user templates yet",
exactly as `load_custom_presets_from` treats a missing file (`presets.rs:429-431`).

### 4.2 The one real core addition: a project-scope id remap

§2.5 established that `Sequence::duplicate_with_fresh_ids` (`sequence.rs:244`)
is deliberately per-sequence and leaves nested-sequence targets, graph ids, bin
ids and marker-category ids pointing at the source project. Instantiation
crosses a document boundary, so it needs a remap that closes all four:

```rust
// timeline/sequence.rs, next to duplicate_with_fresh_ids
/// Re-mint every id in this project so it can be installed into a different
/// document. Unlike `Sequence::duplicate_with_fresh_ids` (sequence.rs:244),
/// which deliberately shares assets/graphs/nested targets with the sequence it
/// copies, this closes the project-scope references too. Returns the mapping so
/// callers can report or re-point.
pub fn remap_all_ids(&mut self) -> IdRemap;

pub struct IdRemap {
    pub sequences: HashMap<SequenceId, SequenceId>,
    pub bins:      HashMap<BinId, BinId>,
    pub graphs:    HashMap<GraphId, GraphId>,
    pub categories:HashMap<MarkerCategoryId, MarkerCategoryId>,
}
```

Order matters and is not negotiable: mint every new `SequenceId` **first**, then
walk clips re-pointing `ClipSource::NestedSequence { sequence }` through the map;
mint bin ids parent-first; re-key `TimelineProject.graphs`, then re-point
`Clip.composition` (`clip.rs:53`) and `TimelineProject.project_graph`
(`sequence.rs:34`); re-key `marker_categories` (`sequence.rs:42`) then re-point
every `Marker.category` at both scopes. A `NestedSequence` or `composition` id
with no entry in the map is a template that failed §3.3's capture validation and
must not have been written — `remap_all_ids` clears it to `None`/refuses rather
than leaving a dangling id, and `finalize_load`'s existing report channel
(`load.rs:614`) names it.

`remap_all_ids` is reusable beyond K-G4 — it is exactly what a future
cross-document paste or "import project" needs, and [§12.4](#12-follow-ups)
records that.

### 4.3 One `ops::` constructor added

`ops::create_bin(name, parent)` (`ops.rs:1942`) mints its own `BinId` inside
`MediaBin::new`, so it cannot place a bin whose id the caller already remapped.
Add the thin sibling, mirroring `add_sequence` (`ops.rs:325`) vs
`create_sequence` (`ops.rs:352`):

```rust
/// Add an already-constructed bin (the caller owns its `BinId`). Thin
/// counterpart of `create_bin` (ops.rs:1942), mirroring `add_sequence`/
/// `create_sequence`.
pub fn add_bin(bin: MediaBin) -> TimelineCmd { TimelineCmd::AddBin { bin } }
```

No new `TimelineCmd` variant: `AddBin` exists (`commands.rs:684`) and already
inverts to `RemoveBin` (`commands.rs:2523-2524`).

### 4.4 Why no plural command is needed here — the §2.4-of-194 trap, avoided

`TimelineCmd::apply` debug-asserts `Sequence::validate()` after **every**
command (`commands.rs:1749-1757`), and `Command::Batch` applies members one at a
time (`history/mod.rs:3172-3178`), so a multi-item edit expressed as per-item
commands can transiently violate an invariant and panic in debug — the finding
[194 §2.4](194-k-a5-general-and-nested-clip-groups.md) is built around.

K-G4 is **not** exposed to it, and the reason is structural rather than lucky:
`TimelineCmd::AddSequence { sequence: Box<Sequence> }` (`commands.rs:451`)
carries the entire sequence — tracks, clips, groups, markers — **inline**, so
each `AddSequence` moves the document from one valid state to another in a
single step. `AddBin` touches no sequence at all. There is no intermediate
half-built sequence to be invalid. §6.2's batch is therefore safe *by
construction*, and T7 pins it in a debug build so a future refactor that splits
`AddSequence` into per-track commands fails loudly.

---

## 5. Migration and format-version impact

### 5.1 `CURRENT_FORMAT_VERSION` stays 5 — K-G4 lands additively inside v5

`CURRENT_FORMAT_VERSION = 5` (`document.rs:117`) is unchanged. Point by point:

- **The persisted model does not grow** (§4.1). `migrations()` (`migration.rs:58`)
  upgrades documents when the model grows; there is nothing here for a
  `V5ToV6` step to do.
- **The precedent cuts against a no-op bump.** `V1ToV2` and `V2ToV3`
  (`migration.rs:70,87`) exist only to stamp a number for purely additive
  changes. Adding a v6 that stamps a number for a change touching **no field**
  would burn the one-version compat window (`COMPAT_WINDOW = 1`,
  `migration.rs:16`) for every user, so that a v6-authored project stops opening
  in the build before it, in exchange for nothing. All four Band-5 mini-specs so
  far land additively; K-G4 makes five.
- **`photon_template.v` is a separate, independently versioned surface**
  (§3.2/§4.1), exactly as `PHOTON_FORMAT_VERSION` is (`photon_file.rs:28-31`).
  It must never be conflated with `format_version`, and bumping it is not a
  document-format change.
- ROADMAP §10 point 5 ("additive serde/migration round-trip passes **when model
  changes**") is satisfied because the model does not change. T10 pins
  `format_version == 5` on a document instantiated from a template.

### 5.2 A template written at v5, instantiated by a later build

This is the question the assignment asks, and it has three distinct answers.

**Forward (a v5 template on a v7 build) — it just works, and the template file is
never rewritten.** `read_template_at` hands the parsed `Value` to
`Document::from_value_with_report` (`document.rs:1717`), which runs
`migration::run_migrations(&mut value, CURRENT_FORMAT_VERSION)`
(`document.rs:1739`) exactly as `Document::from_json` does for a project. The
template migrates v5→v6→v7 **in memory**, the instantiated document is stamped
at that build's `CURRENT_FORMAT_VERSION`, and saving the resulting project
writes v7. **The file on disk stays at v5.** That is deliberate: instantiation
is a read, and silently upgrading a user's template library the first time they
open a new build would make those templates unusable in the older build they
also run. The consequence — a template pays the migration cost on every use — is
microseconds against a directory read.

Corollary worth stating because it is easy to get wrong: because migration
happens *before* the sequences are lifted out, the sequences installed by
[§6.2](#62-new-sequence-from-template--one-undo-unit)'s "New Sequence from
Template" verb are **already at the current schema** when they enter the live
document. A document can never hold a mixed-version sequence.

**Backward (a v6 template on a v5 build) — refuse, do not load leniently.**
`Document::from_value_with_report` refuses only beyond the window
(`document.rs:1727-1737`) and *inside* it "falls through and loads leniently",
dropping unknown fields silently. For a **project** that is the right trade —
39 §2.3's "newer, minor" row exists so a user can still open their own work.
For a **template** it is the wrong trade: leniently loading a v6 template on a
v5 build yields a skeleton quietly missing whatever v6 added, and the user
builds a project on it. So:

> `read_template_at` calls `migration::detect_version(&value)` (`migration.rs:295`)
> **before** deserializing — the same pre-read `load_photon` already performs
> (`photon_file.rs:81`) — and returns `TemplateError::DocumentVersionTooNew` when
> it exceeds this build's `CURRENT_FORMAT_VERSION` **at all**, rather than at
> `+ COMPAT_WINDOW`.

Refusing a template costs the user one click (pick another, or start blank);
refusing a project would cost them their work. The asymmetry is the
justification. It is a deliberate deviation from 39 §2.3's table and
[§12.2](#12-follow-ups) records the amendment.

Same rule, same reason, for the sidecar: `photon_template.v > PHOTON_TEMPLATE_VERSION`
→ `TemplateVersionTooNew`. Note this differs from
[196 §3.7](196-x-2-opentimelineio-interchange.md)'s OTIO namespace policy, which
*ignores* a newer namespace and falls back to native fields — correct there,
because OTIO carries the structure independently of the namespace. Here the
sidecar carries the template's identity; there is nothing to fall back to.

### 5.3 39 §2.2 unknown-preservation applies unchanged

A template **is** a document, so the unknown-preserving variants
(`ClipSource::Unknown` `clip.rs:195`, `GroupKind::Unknown` `sequence.rs:928`,
`EffectKind::Unknown`, `timeline/unknown.rs`) behave exactly as
[39 §2.2](../specs/video-editor/39-document-lifecycle.md#22-generalise-it)
specifies: preserved verbatim, rendered inert, diagnosed once per load through
`LoadReport.unknown_variants` (`load.rs:614-615`). Two consequences:

1. §5.2's strict version refusal removes the *common* route by which unknowns
   reach a template, but not all of them — an effect tag added in a patch release
   at the same `format_version` still arrives as `EffectKind::Unknown`. Those
   load inert, instantiate inert, and fire the existing once-per-load
   diagnostic. That is correct and needs no new code.
2. `ClipSource::Unknown` is nonetheless **refused at capture** (§3.3): a template
   is authored deliberately, and writing a clip whose source this build cannot
   name into a file meant to be reused is not preservation, it is deferral. The
   forward-compat machinery keeps its job (round-tripping *documents*); it does
   not acquire a second one.

---

## 6. Undo unit and its exact inverse

Repo rule: one user verb = one undo unit, fanned-out edits included, and an
operation that cannot be undone atomically must not commit partially
(39 §1.1). Three verbs; two of them correctly produce **no** history entry, and
the document says so plainly rather than inventing one.

| Verb | History | Exact inverse |
|---|---|---|
| **Save as Template…** | **none** | n/a — see §6.1 |
| **New Project from Template…** | **none** | n/a — see §6.1 |
| **New Sequence from Template…** | **one** `Command::Batch` | the reversed batch of inverses — see §6.2 |

### 6.1 The two verbs that record nothing, and why that is correct

**Save as Template** mutates no document state; it writes a file. 39 §1.6 puts
exactly this class outside history, and export is the shipped precedent — the
Export flow (`menu_drawer.rs:163`) records nothing either. If a later revision
adds "remember the last template used", that is view state for 39 §1.6's
sidecar, not `Document`.

**New Project from Template** creates a *new document in a new tab*. It is the
same verb as File ▸ New, and File ▸ New is not undoable: `create_document_from_spec`
(`app/dialogs.rs:52-66`) replaces the document and calls `history.reset()`
(`:60`) precisely so a prior project's history cannot bleed in. Instantiation
follows that sequence exactly, and installs through `open_in_new_tab`
(`app/tabs.rs:61`) with `CommandHistory::default()` and `file: None`. The
previously active document is untouched, so there is nothing to undo *there*
either; the new document's history is empty, so there is nothing to undo *here*.
"Undo" for this verb is closing the tab, which the existing close guard already
handles (`app/close_guard.rs:38`).

This is not a weakening of the one-verb-one-unit rule. The rule governs document
*mutations*; creating a document is not a mutation of one.

### 6.2 New Sequence from Template — one undo unit

The in-flight case, and the only one that touches a live document. One
`execute_discrete`, following `create_sequence`'s shipped shape exactly
(`handlers/video.rs:309-320`):

```rust
history.execute_discrete(Command::Batch(cmds), &mut doc);
```

`cmds`, in order:

1. `Command::Timeline(ops::create_project())` — **only** when
   `doc.timeline.is_none()` (`ops.rs:95`; the same guard `create_sequence` uses
   at `handlers/video.rs:309`).
2. `Command::Timeline(ops::add_bin(bin))` × B — the template's bins, **parents
   before children**, with ids from `IdRemap.bins` (§4.2/§4.3). Skipped for any
   bin whose name already exists at the same parent.
3. `Command::Timeline(ops::add_sequence(seq))` × M — **innermost nested sequence
   first**, so a `ClipSource::NestedSequence` clip never names a sequence that is
   not yet present (the same ordering rule [196 §6.1](196-x-2-opentimelineio-interchange.md)
   states for OTIO import). Each carries its fully-built `Sequence` inline
   (`commands.rs:451`) — a 12-track template is 12 tracks in **one** command, not
   12 commands (§4.4).
4. `Command::Timeline(ops::set_active_sequence(p, Some(top_level)))` (`ops.rs:341`).

**Exact inverse**, mechanical rather than hand-written, because `Command::Batch`
inverts as the reversed batch of inverses (`history/mod.rs:3172-3178`):

> `SetActiveSequence { new: old_active }` → `RemoveSequence` × M in reverse
> creation order (`commands.rs:2206`) → `RemoveBin` × B in reverse order
> (`commands.rs:2524`) → `RemoveProject` (`commands.rs:2151`).

Every member already has a tested inverse. Redo re-applies the forward batch.

**Validate-then-commit** (39 §1.1): the template is read, migrated,
`remap_all_ids`'d and every `Sequence` run through `Sequence::validate`
(`sequence.rs:378`) **before the first command is constructed**. A read,
version, remap or validation failure returns `Err(TemplateError)` and mutates
nothing. A half-instantiated project is not an acceptable outcome and is not
reachable.

**`mem_estimate` is already honest.** `TimelineCmd::AddSequence` estimates via
`json_len` of the boxed sequence (`commands.rs:1638`), so the batch reports its
real weight against the byte budget 39 §1.3 requires — and because a template
carries no media and (usually) no clips, the weight is small.

---

## 7. MCP surface and GUI parity

An MCP surface **is** warranted. CAP-019 parity is ROADMAP §10 point 3, and
26 §5 lists PA-11 (full MCP parity) as explicitly *not yet held* — a GUI-only
project-template feature would widen a gap this programme is closing.

### 7.1 Three new tools — and one deliberate omission

| Tool | Args | Mutating |
|---|---|---|
| `list_project_templates` | — | no |
| `create_sequence_from_template` | `template: String`, `name: Option<String>`, `activate: bool = true` | **yes** — §6.2, one undo unit |
| `save_project_template` | `name: String`, `description: Option<String>`, `sequence_ids: Option<Vec<SequenceId>>`, `overwrite: bool = false` | no (writes a file, not the document) |
| `delete_project_template` | `name: String` | no — mirrors the existing `delete_export_preset` |

`list_project_templates` returns:

```json
{ "templates": [
  { "name": "Interview 2-cam", "description": "…", "built_in": true,
    "sequences": 1, "video_tracks": 2, "audio_tracks": 4,
    "frame_rate": {"num": 25, "den": 1}, "formats": [{"name":"16:9","width":1920,"height":1080}] }
] }
```

**Templates are addressed by `name`, never by path.** That is not cosmetic:
§2.4 established that `photonic-mcp` implements **no** path sandbox —
`SecurityPathNotPermitted` (`diag.rs:250`) has no call site in the workspace and
`McpServerConfig` is `{ port, secret }` (`server.rs:22-25`). A name-keyed API
keeps every filesystem access inside the app-owned `<config>/templates/`
directory, so K-G4 adds no new arbitrary-path surface to an MCP server that
cannot currently refuse one. (`save_document` already accepts an arbitrary path,
`handlers/document.rs:47-72` — an existing gap, not one to widen.)

**The omission, argued rather than skipped: there is no
`create_project_from_template` MCP tool.** MCP binds to the *active* document
and there is no way to address a different one — 39 §3.2 recommends
`get_active_document` / `set_active_document`, and neither exists (they are
absent from the tool list in `docs/mcp-api.md:10`). A tool that created a
document the caller could then neither address nor verify would be an
unverifiable capability, which is worse than a missing one. When 39 §3.2 lands,
adding `create_project_from_template` is a one-handler change; §12.6 records the
dependency.

Wiring, following the existing pattern exactly: arg structs in
`protocol/args/`, handlers in `handlers/video.rs`, dispatch arms in
`dispatch.rs`, names added to the tool-name list (`handlers/video.rs:8389` is
where `list_title_templates` sits), then `schema_gen.rs` regenerated. **CI gates
the docs**: `.github/workflows/ci.yml:162-167` regenerates `docs/mcp-api.md` and
runs `git diff --exit-code` on it, so regeneration is mandatory, not optional.

Both arms call the **same** `photonic_core::templates` functions and the same
`ops::` constructors the GUI calls. 194 §6 records what happens when they do not
(the link-group expansion exists as two hand-mirrored copies in
`app/timeline/ops_bridge.rs:345-430` and `handlers/video.rs:140-220`). Do not repeat it.

### 7.2 GUI route

No new File-drawer column. `FILE_OPTIONS` stays `&["Document", "Save", "Export"]`
(`app/mod.rs:290`) — note that [196 §7.3](196-x-2-opentimelineio-interchange.md)
proposes adding a fourth, "Interchange"; K-G4 deliberately does not, so the two
proposals do not collide.

| Surface | Where | Action |
|---|---|---|
| File ▸ **Document** column | after "New" (`menu_drawer.rs:46`) | **"New from Template…"** → template browser modal → `open_in_new_tab` |
| File ▸ **Save** column | after "Save As…" (`menu_drawer.rs:128`) | **"Save as Template…"** → name/description sheet → `write_template_in` |
| Welcome screen | beside the *New Video Project* hero card (`welcome.rs:1070`) | **"From Template"** card opening the same browser; `T` joins the `N`/`O`/`V` shortcut strip (`welcome.rs:1088`) |
| Sequence tabs | the existing sequence-tab surface that already owns "new sequence" (17 §G-17, `ops::create_sequence` `ops.rs:352`) | **"New Sequence from Template…"** — the §6.2 verb |

The browser is one modal, shared by all three entry points, built on the
`NewDocumentForm` precedent (`welcome.rs:382`) whose doc comment records exactly
this discipline: *"used by the welcome screen's New Canvas panel and by the
in-editor File ▸ New modal, so the two stay identical by construction rather
than by copy-paste."* Built-ins show a lock and a "Duplicate to edit" affordance
(§3.4). Media-bearing save attempts surface §3.3's refusal with the offending
clip count and a "Save skeleton only" button.

**Instantiation differs from Open in exactly six ways, and each is a real code
path — this is the "a template is a document opened as untitled" table:**

| | **Open** (`menu_drawer.rs:54-88`) | **Instantiate template** |
|---|---|---|
| `current_file` | `Some(path)` (`:83`) | **`None`** → Save is disabled and Save-As is prompted (`can_save`, `:113`; `close_guard.rs:38-46`) |
| History | restored from `photon_history` via `apply_opened_history` (`app/mod.rs:2148`) | **`CommandHistory::default()`** — the template never carried one (§3.2 property 4) |
| `Document.id` | the file's `DocumentId` | **fresh `Uuid::new_v4()`** |
| Structural ids | preserved verbatim | **all re-minted** via `remap_all_ids` (§4.2) |
| Recents | `welcome.add_recent(path, name)` (`:78`) | **not added** — a template is not a document you opened |
| Tab title | file stem (`tabs.rs:16-21`) | `doc.name` → "Untitled" (`tabs.rs:22-26`) |

Everything else — auto-entering Video mode when `doc.timeline.is_some()`
(`app/mod.rs:2992-2997`), `fit_pending`, `selected_id = None` — is identical, and
should be identical.

---

## 8. Acceptance fixtures and tests

### 8.1 Fixtures — Photonic-authored, and **K-G4 is not a gated item**

**No rights-cleared content is required.** Every fixture is either built
programmatically in-test (the style `crates/photonic-core/tests/scope_migration.rs`
already uses) or is a small Photonic-authored `.photon` file — the same kind of
artifact `crates/photonic-video/tests/fixtures/title_asset.photon` already is.
No media bytes, no probe, no GPU, no ffmpeg, and by §3.3's rule **no template
can legally contain media at all**, so no `AssetRightsManifest`
(23 §7.2) is implicated and no ROADMAP §7 K/E/X gate applies.

Recording that explicitly is the point of the item: K-G4 goes from
`S11-gated` (`ROADMAP.md:186`) to schedulable.

| Committed fixture | Under | Exercises |
|---|---|---|
| `tpl_basic.photon` | `crates/photonic-core/tests/fixtures/templates/` (new dir + `README.md` recording provenance per [23 §12](../specs/video-editor/23-legal-open-source-implementation-routes.md#12-cross-cutting-provenance-manifests)) | 2V/4A, 25 fps, two formats, two bins, one marker category |
| `tpl_nested.photon` | same | a `ClipSource::NestedSequence` clip and its target sequence — §2.5's trap |
| `tpl_v6.photon` | same | `format_version: 6` — §5.2's strict refusal |
| `tpl_bad_sidecar.photon` | same | `photon_template.v: 2` — sidecar refusal |

Total added fixture weight is text JSON, on the order of 20 KB — negligible
against [11 §1.5](../specs/video-editor/11-testing-phasing.md)'s corpus budget.

### 8.2 Tests

| # | Test | Where | Asserts |
|---|---|---|---|
| T1 | Round trip: capture → `write_template_in` → `read_template_at` → structurally identical skeleton | `templates.rs` `mod tests` (tmpdir, via the path-parameterized pair) | §3.2/§4.1 |
| T2 | **Save-as-template writes no history** — the written JSON has no `photon_history` key, and `photon_template.name` is present | `templates.rs` `mod tests` | §3.2 property 4 (`photon_file.rs:34-35`) |
| T3 | **A template file opens as an ordinary document** — `load_photon` on a template yields a valid `Document` and `None` history; the `photon_template` key is ignored | `crates/photonic-core/tests/` | §3.2 property 1 |
| T4 | **Ids are fully re-minted** — instantiating `tpl_nested.photon` twice yields zero shared `DocumentId`/`SequenceId`/`TrackId`/`ClipId`/`BinId`/`GraphId`/`MarkerCategoryId`, **and** every `ClipSource::NestedSequence` target resolves inside its own document | `crates/photonic-core/tests/project_templates.rs` (new) | §2.5 / §4.2 — the highest-blast-radius defect |
| T5 | **Marker categories re-point** — a marker whose `category` was set in the template still resolves after instantiation, and no duplicate category rows appear on a second instantiation into the same project | same | §4.2 |
| T6 | **New Sequence from Template is one undo unit** — `history.len()` grows by exactly 1; one undo restores a byte-identical `to_json` | same | §6.2 |
| T7 | **No mid-batch validate panic** — a template with 12 tracks and adjacent same-track clips instantiates without tripping `commands.rs:1749`'s debug assert | same (**debug build**) | §4.4 |
| T8 | **Validate-then-commit** — a template that fails `Sequence::validate` produces `Err`, a byte-identical document and **no** history entry | same | §6.2 / 39 §1.1 |
| T9 | **The media rule holds** — capturing a project with an `Asset` clip returns `MediaBearing` naming the clip ids; with a `Vector` clip likewise; a project with assets but no clips returns `AssetsPresent`; `SolidColor`/`Adjustment`/`Text` capture cleanly | `templates.rs` `mod tests` | §3.3 — **this is the test that keeps K-G4 ungated** |
| T10 | **`format_version` unchanged** — a document instantiated from a v5 template saves at `format_version == 5` and reloads unchanged | `crates/photonic-core/tests/forward_compat.rs` (extends) | §5.1 / ROADMAP §10.5 |
| T11 | **Newer template refused** — `tpl_v6.photon` yields `DocumentVersionTooNew` (**not** a lenient load), `tpl_bad_sidecar.photon` yields `TemplateVersionTooNew`, and neither mutates any document | `project_templates.rs` | §5.2 |
| T12 | **Older template migrates** — a hand-built v4 template instantiates through the `V4ToV5` chain (`migration.rs`) and the resulting sequences are at the current schema; the file on disk is **not** rewritten | `project_templates.rs` | §5.2 |
| T13 | **Built-ins are constructed, not loaded** — `built_in_templates()` returns the full v1 set with an empty `templates_dir`, none has any `MediaAsset`, and the set's names/order are pinned (the shape `presets.rs` tests at `:698`) | `templates.rs` `mod tests` | §3.4 |
| T14 | **Store hygiene** — a missing `templates/` dir lists as empty (not an error); a same-name write without `overwrite` returns `NameInUse`; two different display names slugging identically produce two files | `templates.rs` `mod tests` | §4.1 |
| T15 | **GUI arm** — headless "New from Template" (fresh doc, `current_file == None`, empty history, not added to recents) and "New Sequence from Template" through the `ops_bridge` path | `crates/photonic-gui/tests/video_ui_paths.rs` | ROADMAP §10.2, §7.2's table |
| T16 | **CAP-019 parity story** — MCP arm (`save_project_template` → `create_sequence_from_template`) vs GUI arm, structural compare via the existing harness | `crates/photonic-app/tests/acceptance_stories.rs` | ROADMAP §10.10 |

T4 deserves the emphasis 196 gives its source-timecode test: a template with no
nested sequence passes either way, so a fixture that *does* nest is mandatory,
and when it is wrong the clip renders as nothing with no diagnostic at all.

---

## 9. Definition of done (ROADMAP §10), made answerable

| # | Requirement | How K-G4 answers it |
|---|---|---|
| 1 | Core op/engine service with unit tests | `photonic-core/src/templates.rs` (store + capture + instantiate), `TimelineProject::remap_all_ids`, `ops::add_bin`; T1–T14 |
| 2 | GUI route, or a recorded exception | File ▸ Document "New from Template…", File ▸ Save "Save as Template…", welcome card, sequence-tab "New Sequence from Template…" (§7.2); T15. **No exception is sought** |
| 3 | MCP tool/schema/generated docs | `list_project_templates`, `create_sequence_from_template`, `save_project_template`, `delete_project_template`; `docs/mcp-api.md` regenerated, `ci.yml:162-167` drift gate green. The one omission is argued in §7.1 |
| 4 | One verb, one undo unit; undo/redo identity | §6; T6, T8. Two verbs correctly record nothing, with the reasoning in §6.1 |
| 5 | Additive serde/migration round-trip when the model changes | The model does not change (§4.1/§5.1). T10 pins `format_version == 5`; T12 pins the older-template chain |
| 6 | Pixel/audio IR/eval/golden/sync coverage | **N/A — K-G4 touches no pixel or audio path.** No `ContentHash` input changes: templates are edit-time structure, never a graph input. Stated rather than invented (the clause 196 §11 asked ROADMAP §10.6 to grow) |
| 7 | Hard gates green; trend metrics not regressed | No new budgets. One bound worth asserting as a hard gate because it is deterministic: `list_templates` over a 50-template directory completes without parsing more than the header of each file |
| 8 | Offline, privacy, licensing, content, product gates | Offline: a local directory read, no network, no telemetry. Content: **no bundled bytes at all** (§3.4), so no `AssetRightsManifest`. Licensing: §11. **S11 resolved by §3.6** — this row is the item's entire former blocker |
| 9 | Protected surfaces not regressed | Nothing in ROADMAP §9's list is touched. PA-6 (per-sequence formats) is *used* — a template can carry several formats and never imposes a project-wide profile; PA-8 (`Tick` flicks + exact rational `FrameRate`) rides through unchanged because a template stores real `FrameRate` values, never a float fps; PA-9 (typed errors) is why `TemplateError` is an enum |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | L1 module exists → L2 real store round-trips on disk → L3 wired into the File drawer, welcome screen and dispatch → L4 a real template saved from a real project instantiates into a real editable project; T16 pins parity |

---

## 10. Risks, open questions, deliberate exclusions

### 10.1 Risks

1. **Dangling `NestedSequence` / `composition` ids after instantiation (§2.5).**
   Highest blast radius, lowest visibility: the clip renders nothing and nothing
   is reported. `duplicate_with_fresh_ids`' existing behaviour is *correct for
   its own caller* and will look like a working remapper to anyone who reuses it.
   T4 exists solely for this; the fixture must nest.
2. **Template drift against the model.** A template written today captures the
   `Track`/`Sequence` shape of today. Because it is a real `Document` running
   through the real migration chain (§5.2), that is handled — *provided* nobody
   invents a template-specific partial serialization. Mitigation: the container
   rule in §3.2 (a template is a `.photon`, whole), enforced by T3.
3. **The media rule eroding.** The first request after ship will be "let my
   template include my intro sting". That single change re-opens S11 in full:
   rights manifests for anything bundled, relink semantics for anything
   referenced, absolute paths leaking between users. §3.3 is the fence and T9 is
   the guard; a change to either must go through a new S-series amendment, not a
   PR.
4. **Config-dir absence.** `crash_dir()` returns `None` when none of `APPDATA`,
   `XDG_CONFIG_HOME`, `HOME` is set (`diagnostics.rs:29-40`) — real on some CI
   and container environments. Every existing consumer degrades silently
   (`preferences.rs:322-326`, `welcome.rs:2109-2114`). Templates must degrade
   *loudly* for a **write** (`NoConfigDir` surfaced to the user, mirroring
   `PresetStoreError::NoConfigDir`, `presets.rs:385-387`) and quietly for a **read**
   (built-ins only). A save that silently does nothing is the worst outcome here.
5. **Name/slug collisions across platforms.** A case-insensitive filesystem
   makes "Podcast" and "podcast" the same file. §4.1's rule (display name is the
   key; the slug is derived; different names get suffixes) handles it; T14 pins
   it.

### 10.2 Open questions, each with a recommendation

- **Q1 — should a template be applicable to the *current* project (the
  `apply_document_template` analogue, `doc_state.rs:528`)?**
  *Recommendation: no in v1.* Merging a skeleton into a project that already has
  sequences needs a conflict model (name collisions, which frame rate wins, track
  merging) that is a mini-spec of its own — the same argument 196 §12.2 Q1 makes
  for OTIO. "New Sequence from Template" (§6.2) covers the workflow that
  motivates the request, purely additively, and is strictly forward-compatible
  with adding merge later. *No product sign-off needed; this is a scope call.*
- **Q2 — do built-in templates ship in v1 at all, or only the save/instantiate
  machinery?** *Recommendation: ship the four in §3.4.* They cost four `fn`
  bodies, they make the feature discoverable on first run (when the user has no
  templates and would otherwise see an empty browser), and being constructed in
  code they carry no gate. **This is the one genuine product call in the item**:
  the *set* is a product decision even though the *mechanism* is not, and the
  names above should be reviewed by product before they become user-visible
  strings.
- **Q3 — should K-G4 wait for K-G1 project profiles?** *Recommendation: no.*
  26 §15 says K-G4 "compounds with K-G1 and K-G3", which is true and is not a
  dependency: a template already captures the concrete settings a profile would
  *name* (frame rate, formats, sample rate). When K-G1 lands, a template
  additionally capturing a `default_profile` reference is serde-additive and
  needs no format step. Sequencing K-G4 behind K-G1 would trade a shipped
  capability for a naming convenience. **See §12.7 — the storage overlap with the
  sibling K-G1 spec is real and must be reconciled by whoever accepts both.**
- **Q4 — should the store be a git-friendly directory the user can point
  elsewhere (a preference for the templates path)?** *Recommendation: not in v1,
  but leave the door open.* Every store function is already path-parameterized
  (§4.1), so a future `AppPreferences.templates_dir: Option<PathBuf>` is a
  one-line resolver change and no API change. Shipping the preference now adds a
  setting nobody has asked for and a support surface (a stale path) that has to
  be diagnosed.

### 10.3 Deliberately excluded

- **Title templates.** 26 K-G4's "title templates pre-populated" clause is
  blocked on [05 §4b](../specs/video-editor/05-import-export.md#4b-starter-title-template-library-d-11-ships-p6)'s
  `VectorDoc` library, which does not exist (§2.4: the two MCP tools are an empty
  list and a `NotSupportedV1`). When it lands, it lands *in the same directory*
  under §3.6's amended S11 and a project template gains nothing but a longer list.
- **A live template link.** Nothing records which template a project came from,
  and there is no "update projects from template". A template is a starting
  point; 05 §4b makes the identical call for title templates and states the
  reason (*"no live template link — deliberate: no template-versioning
  complexity in v1"*).
- **Template import/export as a distinct verb.** A template *is* a `.photon`
  file in a known directory (§3.2), so "share a template" is "send the file" and
  "install a template" is "drop it in the folder". Building a dedicated
  import/export UI over a copy-a-file operation is UI for its own sake.
- **Applying a template to an existing project** (Q1).
- **`create_project_from_template` over MCP** — blocked on 39 §3.2's document
  identity tools, argued in §7.1, recorded in §12.6.
- **Preferences/keymap/layout in a template.** Those are app-level state, not
  project structure; dock layouts are K-G3's item and belong in the same config
  dir but a different file.

---

## 11. Clean-room provenance

Required by [26 §7](../specs/video-editor/26-kdenlive-mlt-parity.md#7-how-to-read-the-item-tables)
and [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol).
This item's provenance risk is specific (it is the item ROADMAP §7 gates on
bundled assets), so the note is explicit rather than inherited.

- **Sources used.** (a) Photonic's own code and specs, cited by `file:line`
  throughout; (b) 26 K-G4's one-line requirement statement, itself derived from
  Kdenlive's `CC-BY-SA-4.0` user documentation as a *requirements source*, cited
  and never pasted; (c) the general application convention that a "new from
  template" command exists and that user presets live in a per-user config
  directory — a functional idea and an OS convention, not protectable expression.
- **Sources not used.** The Kdenlive source tree, the MLT/`mlt++` source tree,
  frei0r, and any GPL/LGPL derivative were not inspected for this item. No
  identifier, comment, constant, control flow, file layout or test case above
  derives from them. In particular, **nothing here is modelled on Kdenlive's
  template mechanism, file layout or directory names** — the layout in §3.1 is
  derived entirely from Photonic's own existing `crash_dir()` store family
  (`diagnostics.rs:29`, `presets.rs:401`, `welcome.rs:2078`, `autosave.rs:14`),
  and the container in §3.2 from Photonic's own `photon_file.rs:1-31`. The
  implementer records the 23 §3.4 attestation for the `core-timeline` and
  `panels-video` subsystems, and an independent reviewer checks provenance before
  merge (26 §2 point 2).
- **No bundled asset, no dependency, no codec, no patent surface.** Built-ins are
  constructed in Rust (§3.4), so **no `AssetRightsManifest`
  ([23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest))
  is required and none of ROADMAP §7's K/E/X gates apply.** No new crate: the
  store is `serde_json` + `std::fs`, both already used by `photonic-core`, and
  path resolution reuses `crash_dir` rather than introducing `dirs`/`directories`
  (§2.1 — the tree deliberately has neither).
- **Photonic-ahead properties preserved** (26 §5, ROADMAP §9). A template stores
  an exact rational `FrameRate` and `Tick` flicks, never a float fps or a frame
  count (PA-8). Ranges stay half-open (PA-7). Failures are a typed
  `TemplateError`, never a string (PA-9). Per-sequence formats survive a template
  round trip and a template never imposes a project-wide profile (PA-6) —
  **this is the reference NLE limitation K-G1's own entry warns against porting
  backwards, and K-G4 must not reintroduce it by the back door**: a project
  template supplies *defaults for the sequences it creates*, never a lock on
  sequences created later. No graph or cache key changes, so per-node caching is
  untouched (PA-1).
- **Fixtures** are Photonic-authored (§8.1); none is copied or adapted from any
  other project's test suite, and none contains media.
- **Naming discipline:** describe the capability as "start a project from a saved
  template", never in terms of compatibility with, or equivalence to, another
  application's templates.

---

## 12. Follow-ups

Recorded here rather than edited into the owning documents, per this proposal's
one-file scope. Each needs its own change.

1. **`ROADMAP.md` §8 S11 (`:350`) and §7's K-G4 bullet (`:331`)** — replace with
   §3.6's text; S11 moves from **Open gate** to **Resolved**, and K-G4's bullet
   moves from gated to ungated. Also `ROADMAP.md:186`'s K-G row, which currently
   reads "K-G4 templates (S11-gated)". **This is the single edit that unblocks
   the item and it must land with acceptance of this document, not after.**
2. **`39-document-lifecycle.md` §2.3** — the version-policy table's "Newer,
   minor" row should gain a template carve-out: a *template* whose
   `format_version` exceeds the current build is **refused**, not loaded
   leniently (§5.2), because refusing a template costs a click while refusing a
   project costs work. As written, an implementer following 39 §2.3 literally
   would build the wrong behaviour.
3. **`26-kdenlive-mlt-parity.md` K-G4 (`:652-655`)** — the impact line promises
   "title templates pre-populated", which is blocked on 05 §4b's unshipped
   library (§2.4). It should be split: tracks/formats/bins/settings are K-G4;
   title templates join when 05 §4b ships. The **Files** line (currently absent —
   K-G4 is the only K-G item with no Files line) should name
   `photonic-core/src/templates.rs`, the File drawer and `photonic-mcp`.
4. **`Sequence::duplicate_with_fresh_ids` (`sequence.rs:236-243`)** — its doc
   comment correctly warns that assets, graphs and nested sequences are shared,
   but nothing in the tree points a future cross-document caller at the danger.
   Once `remap_all_ids` (§4.2) exists, the comment should name it as the
   cross-document counterpart. `remap_all_ids` is also what a future
   cross-document paste or "import project" needs — 194 §8.1 defect 2 records a
   related paste bug (`command_center.rs:1066-1067` keeps a foreign `GroupId`).
5. **Three copies of `config_dir`** — `welcome::config_dir` (`welcome.rs:2078`)
   and `export::presets::config_dir` (`presets.rs:401`) both delegate to
   `photonic_core::crash_dir`. K-G4 adds a fourth consumer; the three should be
   collapsed onto one re-export (§3.5). Cosmetic, but the fourth copy is the one
   that makes it a pattern.
6. **`create_project_from_template` over MCP** is blocked on 39 §3.2's
   `get_active_document` / `set_active_document`, neither of which exists
   (§7.1). When they land, the tool is a one-handler addition and should be
   scheduled with them.
7. **Overlap with the sibling K-G1 mini-spec
   (`203-k-g1-project-profiles.md`, written in the same round and not readable
   from here).** K-G1 faces the identical "where does named user state live"
   question. This document's answer, stated so it can be reconciled: **the
   directory is `<config>/Photonic/` via `photonic_core::crash_dir()`
   (`diagnostics.rs:29`); the module is `photonic-core/src/templates.rs`; the
   error type is a `thiserror` enum whose first variant is `NoConfigDir`; every
   load/save has a path-parameterized twin for tests; built-ins are constructed
   in Rust, never bundled.** Profiles are small and scalar and should therefore
   be **one JSON file** (`<config>/project_profiles.json`), matching
   `export_presets.json` (`presets.rs:407`) — *not* a directory, which is only
   warranted here because a template is a whole `Document` (§3.1). Whoever
   accepts both documents should confirm that split, and should decide whether
   the shared `app_config_dir()` re-export (§3.5) lands with K-G1 or K-G4 —
   whichever ships first. If K-G1 lands first, K-G4 gains a `default_profile`
   reference in its captured `ProjectVideoSettings` serde-additively, with no
   format step (§10.2 Q3).
8. **`ROADMAP.md` §0 progress table** — add a K-G4 row when the item lands, with
   its commit, per the existing convention.
