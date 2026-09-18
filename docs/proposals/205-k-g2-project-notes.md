# 205 — K-G2 Project notes (mini-spec)

> Status: **proposed mini-spec — not accepted, no code authorization.** Written to
> satisfy the [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands)
> K-Band 5 exit condition ("an accepted mini-spec exists *before* code, naming its
> data-model change, migration, undo unit, MCP surface and acceptance fixtures.
> No item here starts without one").
> Owner doc: [26 §15 K-G2](../specs/video-editor/26-kdenlive-mlt-parity.md#k-g2--project-notes).
> [23 §14](../specs/video-editor/23-legal-open-source-implementation-routes.md#14-stopgo-checklist-before-any-code)'s
> agent-proof boundary applies: acceptance of this document is what authorizes K-G2.

**K-G2 is not gated.** [ROADMAP §7](../specs/video-editor/ROADMAP.md#7-legal-content-and-product-gates)
lists no K-G2 entry; the only K-G gate is S11 on K-G4 (`ROADMAP.md:331`), which
this item does not touch. K-G2 needs no bundled asset, no codec, no dependency
and no rights-cleared fixture ([§10.1](#101-fixtures--k-g2-is-not-a-gated-item)).
[ROADMAP.md:186](../specs/video-editor/ROADMAP.md) currently records it as simply
"Open".

Verified against `feat/video-editor-module` @ `8a33f32`. Every `file:line` below
was read in that tree.

---

## 1. Problem and user outcome

**Today.** A review pass produces a list of change requests, and Photonic has
nowhere to put them that survives closing the app. The nearest thing that exists
is `Marker.note: String` (`crates/photonic-core/src/timeline/sequence.rs:841`) —
one string per marker, editable only through the ruler's marker context menu
(`crates/photonic-gui/src/app/timeline/ops_bridge.rs:1004`), invisible unless you
already know which marker to hover, and unlistable: there is no panel and no
"show me every note in this project". The other nearest thing,
`Document.annotations` (`crates/photonic-core/src/document.rs:684`), is the
**vector**-mode review channel, is keyed on `NodeId`, has no GUI at all, and is
documented as deliberately outside undo (`annotation.rs:9-13`). So a director's
note — *"the cut at 00:01:23 is two frames late; the interview clip's audio dips
at the end"* — lives in a text file, an email, or nowhere.

**After K-G2.** A user can:

1. **Write a note that points at something** — a timecode, a clip, or a marker —
   and see every note in the project in one list, sorted by where it points.
2. **Click a note and land there.** The playhead seeks, the sequence activates,
   and a clip anchor also selects the clip. Navigation costs **zero** undo units,
   exactly as K-G5's history browser does (`ROADMAP.md:74`).
3. **Turn a note into a marker** in one click, so a review note becomes a
   timeline object the editor can snap to.
4. **Resolve** notes as they are addressed, and filter the resolved ones out.
5. **Export the note list** as Markdown or JSON, with timecodes formatted exactly
   as the ruler formats them — a review document that can be sent to the person
   who wrote the notes.
6. Do all of it from an agent: `add_project_note`, `list_project_notes`,
   `update_project_note`, `remove_project_notes`, `export_project_notes`.

**The design claim this document defends** ([§3.1](#31-why-anchored--the-easy-design-is-already-shipped)):
a free-floating project text box is not a smaller version of this feature — it is
a feature Photonic **already has**, in `Document.annotations`. Choosing it means
shipping nothing. The anchor is the whole value.

**Non-goals.** Notes are not comments-with-replies, not a collaboration channel,
not versioned, not burned into any render, and never parsed for meaning
([§12.3](#123-deliberately-excluded)).

---

## 2. Current state in code

Exact, as of `8a33f32`. Read this before disagreeing with §3 or §4.

### 2.1 The marker model is real, and half of it is wired

| Thing | Where | Status |
|---|---|---|
| `Marker { id, at, duration, name, note, category, color, anchor }` | `timeline/sequence.rs:833-851` | model **shipped** |
| Ranged markers (`duration`, `end()`, `is_range()`) | `sequence.rs:838`, `:881`, `:887` | shipped |
| `MarkerAnchor { Timecode, Content, Unknown }` (`#[serde(other)]`, `#[non_exhaustive]`) | `sequence.rs:820-826` | shipped |
| `MarkerCategory { id, name, color, glyph }` keyed by stable `MarkerCategoryId` | `sequence.rs:732-738`; id in `timeline/ids.rs:83-86` | model **shipped** |
| `MarkerCategory::default_seed()` — "Marker / Cut / Note / Todo / Chapter" | `sequence.rs:756-789` | written, **called by nothing** |
| `TimelineProject.marker_categories: Vec<MarkerCategory>` + `marker_category(id)` | `sequence.rs:42`, `:63` | shipped, **always empty** |
| Clip-scoped markers `Clip.markers: Vec<Marker>` + `Clip::marker_sequence_tick` | `timeline/clip.rs:68-72`, `:128` | model shipped; **no producer** |
| `TimelineCmd::{AddMarker, RemoveMarker, SetMarker}` + inverses | `timeline/commands.rs:660`, `:666`, `:671`; apply `:2104-2121`; inverse `:2504-2515`; labels `:1719-1721` | shipped, **sequence markers only** |
| `sort_markers` — deterministic `(at, id)` order after every mutation | `commands.rs:881-885` | shipped |
| `ops::{add_marker, remove_marker, set_marker}` | `timeline/ops.rs:1879`, `:1888`, `:1905` | shipped |
| GUI: ruler paints markers; rename/retime via `set_marker_field` | `app/timeline/ruler.rs:154`, `:321`; `ops_bridge.rs:972`, `:1004`, `:1016` | shipped |
| MCP: `add_marker` / `remove_marker` / `list_markers` | `photonic-mcp/src/handlers/video.rs:514`, `:559`, `:578` | shipped |
| Markers are a **protected surface** | `ROADMAP.md:362` ("Insert/Overwrite/Lift/Extract; razor split; markers") | must not regress |

`resolve_tick(at_ticks, at_tc, at_seconds, rate)` (`handlers/video.rs:84`) already
accepts a timecode **string** on the MCP side and is what `add_marker` uses
(`:526`). `Timecode::parse_to_tick` (`timeline/time.rs:244`) and
`Timecode::format_tick` (`time.rs:297`) are the exact-rational, drop-frame-aware
parse/format pair; the latter takes `Sequence::start_timecode` (`sequence.rs:166`)
as its origin.

### 2.2 K-A2 has **not** landed — precisely what is missing

26 §19.2's dependency graph draws `K-A2 → K-G2` (`26-…:851`). Verified state of
K-A2, by grep:

- **No marker-category CRUD anywhere.** `grep -n "marker_categories\|MarkerCategory"`
  across `timeline/ops.rs`, `timeline/commands.rs`, `photonic-mcp/src` and
  `photonic-gui/src` returns **nothing**. The registry exists and nothing writes
  to it; `default_seed()` (`sequence.rs:756`) has no caller.
- **No clip-marker command.** Every marker write in `commands.rs` is
  `s.markers` on the *sequence* (`:2106`, `:2112`, `:2117`). Nothing constructs a
  `Clip.markers` entry. Clip markers are *read* by the snapping engine
  (`app/timeline/interact.rs:377-390`) and by nothing else.
- **No markers panel.** There is no `panels/video/markers.rs`
  (`ls crates/photonic-gui/src/panels/video/`); K-A2's Files line names one.
- **No `set_marker` MCP tool** (`VIDEO_TOOL_NAMES`, `handlers/video.rs:8277`,
  carries `add_marker` / `remove_marker` / `list_markers` and no editor), **no
  marker export**, **no marker lock**.

So the marker *container* is done and the marker *workflow* is not — the same
shape [194 §2.1](194-k-a5-general-and-nested-clip-groups.md) found for groups.
[§3.5](#35-what-k-g2-needs-from-k-a2-and-what-ships-without-it) states exactly
which half of K-G2 that blocks (a small half) and which it does not.

### 2.3 The free-text feature Photonic already has

`Annotation` (`crates/photonic-core/src/annotation.rs:16-28`):

```rust
pub struct Annotation {
    pub id: AnnotationId,            // = Uuid  (annotation.rs:5)
    pub node_id: Option<NodeId>,     // None == document-level
    pub text: String,
    pub resolved: bool,
    pub author: Option<String>,
    pub created_at: String,          // ISO-8601, via the private `chrono_now` (:44)
}
```

Its module doc states the two rules this item inherits and the one it must
break: *"Annotations are stored in the `.photonic` file but stripped from all
export formats"* (`annotation.rs:9-10`) — inherit; *"They are not part of the undo
history"* (`:10-11`) — **must not inherit**, see below. Three MCP tools ship
(`photonic-mcp/src/handlers/annotations.rs:5`, `:32`, `:76`) and **there is no
GUI surface at all** — `grep -rn "add_annotation\|\.annotations" crates/photonic-gui/src`
returns only the unrelated `DimensionAnnotation` measurement feature
(`panels/arrange.rs:381`, `app/panel_actions.rs:7102`).

Critically, `add_annotation` mutates the document **directly**
(`handlers/annotations.rs:25`: `doc.add_annotation(ann)`) with no
`CommandHistory` involvement. That is a live violation of ROADMAP §10 point 4
and of `SPEC.md`'s "every document mutation, without exception, is undoable" as
quoted by [39 §1.6](../specs/video-editor/39-document-lifecycle.md#16-what-is-not-undoable).
K-G2 must not copy it ([§6](#6-undo-unit-and-its-exact-inverse)), and it is
recorded as a follow-up ([§14.3](#14-follow-ups)).

### 2.4 The drawer machinery a notes panel lands in

- Right rail: `RightDrawerGroup` (`panels/mod.rs:1392`), `VIDEO_ALL` = five
  entries (`:1418-1424`), `ALL` = three for vector (`:1409`), plus `icon()`
  (`:1435`) and `title()` (`:1446`). Rail buttons at `app/mod.rs:3667-3686`; the
  animated drawer and its per-group `match` at `app/mod.rs:3739-3790`.
- The `ColorControls` arm is the shape to copy: it receives `ui, doc, history,
  &mut self.pending_panel_actions, …` (`app/mod.rs:3762-3771`), and the drawer
  wraps the whole `match` in `let right_rev_before = history.revision();`
  (`:3739`) so an edit committed inside the drawer marks the document dirty.
- Left rail `DrawerGroup::VIDEO_ALL` already carries **nine** content panels
  (`panels/mod.rs:1285-1295`).
- Read-only session state reaches video panels through `VideoPanelUi`
  (`app/mod.rs:2238-2262`), including `playhead: self.playhead` — a **copy**.
  Seeking is `app/engine.rs:311` `seek` / `:319` `scrub_seek`, driven from
  `app/monitor.rs:911-939` off `self.playhead`. A panel therefore cannot seek
  directly; it must queue an action.
- `AppPreferences.open_right_drawer: Option<RightDrawerGroup>` is **persisted**
  (`preferences.rs:113`) — see [§4.5](#45-the-one-prerequisite-defect-k-g2-makes-reachable).

### 2.5 How text editing and coalescing actually work — the load-bearing finding

The brief asks K-G2 to follow the `SetEffect` coalescing added this week
(`commands.rs:2634-2650`). Read in context, that mechanism **cannot fire for
typing**, and building on it would produce one undo step per keystroke:

1. `TimelineCmd::coalesce` (`commands.rs:2538`) is consulted only from
   `CommandHistory::execute` and only when `self.coalescing && self.coalesce_started`
   (`history/stacks.rs:325`).
2. `coalescing` is opened by exactly one call site:
   `if ctx.input(|i| i.pointer.any_down()) { history.begin_coalescing(); }`
   (`app/mod.rs:2680-2682`), and closed on `pointer.any_released()`
   (`app/mod.rs:6345-6347`). It is **pointer-gated**. A keyboard-only gesture
   never opens it.
3. `execute_discrete` (`stacks.rs:403-412`) deliberately forces it off for every
   non-GUI edit source, so the MCP arm never coalesces either.
4. [39 §1.2](../specs/video-editor/39-document-lifecycle.md#12-coalescing)'s
   time bounds (gap < 500 ms, span ≤ 5 s) are **not implemented** — there is no
   clock anywhere in `stacks.rs:304-380`. An open-ended coalesce over a typing
   session would have nothing to stop it.

The shipped answer for text is different and correct, in
`panels/video/caption_editor.rs:411-430`: keep the draft in egui temp memory
(`ui.data`), render a `TextEdit`, and on `resp.lost_focus()` commit **one**
command if the text differs, with `Escape` cancelling; the buffer is cleared on
commit. Its neighbour states the general rule in terms (`caption_editor.rs:438-443`):
*"`CaptionCmd` has no `TimelineCmd::coalesce` arm … so committing every changed
frame via the coalescing path would still emit one undo entry per tick — gating
here is the only way to get one undo step per drag gesture."*

§6 builds on `caption_editor.rs`, not on `SetEffect`.

### 2.6 What does not exist — plainly

- **No notes anything.** `grep -rn "project_notes\|ProjectNote\|NoteId"` across
  `crates/` and `docs/` returns **zero** hits. No type, no field, no command, no
  op, no tool, no panel.
- **No note-adjacent report writer.** `photonic-video/src/export/` has no
  document/report writer; the closest text emitters are the caption writers
  (`captions/interchange/srt.rs:59`, `vtt.rs:173`) and the MCP `export_captions`
  handler (`handlers/video.rs:5410`), which formats and writes to an
  **arbitrary path** (`:5437`).
- **No path sandbox in `photonic-mcp`** — as
  [204 §2.4](204-k-g4-project-templates.md) established, `SecurityPathNotPermitted`
  (`photonic-core/src/diag.rs:250`) has no call site outside
  `crates/photonic-core/tests/diag_catalogue.rs`. §7.3 chooses consistency with
  the shipped `export_captions` rather than inventing a one-off sandbox.
- **No i18n mechanism.** `grep -rn "fl!\|i18n\|pseudo" crates/photonic-gui/src`
  returns nothing: [42](../specs/video-editor/42-localization.md)'s Tier B has
  not landed. Every panel string in the tree is a literal `en-US` string.
- **No non-Latin font is loaded.** `photonic-app/src/main.rs:503-505` installs
  `egui::FontDefinitions::default()` plus `egui_phosphor`. Nothing else calls
  `set_fonts`. See [§9.4](#94-cjk-renders-as-tofu-in-the-panel-today--say-so).

---

## 3. Design: notes are anchored, and the argument for it

### 3.1 Why anchored — the easy design is already shipped

The cheap design is a project-level text box: one `String` on `TimelineProject`,
one multiline `TextEdit`, one `SetProjectNotes { old, new }` command. It is
half a day's work. It is also, field for field, **`Document.annotations` with
`node_id: None`** (`annotation.rs:18`), which already persists in `.photon`,
already has `resolved` and `author`, and already has three MCP tools
(`handlers/annotations.rs:5-88`). Shipping it would add a second free-text store
to a document that has one, and the only user-visible delta would be a GUI for a
feature that already exists.

So the honest options are: (a) **give `Document.annotations` a GUI** — a real
improvement, and a *vector*-mode item that K-G2 does not own; or (b) build the
thing 26 K-G2 actually describes, whose impact line is *"auto-converts timecodes
in the note text into clickable seeks and can create markers directly from a
note — turning review notes into navigation"* (`26-…:642`). **This document
chooses (b)** and records (a) as a follow-up ([§14.3](#14-follow-ups)).

The value is entirely in the anchor. A review note is a *coordinate plus a
sentence*. Without the coordinate, the editor re-derives the coordinate by hand
from prose, for every note, every time — which is the manual step the feature
exists to remove. With it, the note list becomes a work queue: sorted by timeline
position, filterable, resolvable, and one click from the frame in question.

### 3.2 The anchor is a typed id, not a substring of the note text

The reference implementation's published behaviour scans the note *text* for
timecode-shaped runs and makes them clickable. Photonic must **not** persist that
mechanism. Three reasons, each concrete:

1. **PA-9 (typed model, no stringly-typed state)** is a protected property
   (`ROADMAP.md:367`). "The anchor is wherever `\d\d:\d\d:\d\d:\d\d` happens to
   appear" is precisely the stringly-typed design the register exists to keep out.
2. **It is ambiguous the moment a sequence has a start timecode.** `01:00:00:00`
   in a note means one tick if it is a *display* label on a sequence whose
   `start_timecode` (`sequence.rs:166`) is `01:00:00:00`, and a different tick
   if the sequence starts at zero — and `Timecode::format_tick` (`time.rs:297`)
   adds that origin. A stored `Tick` has no such ambiguity.
3. **It breaks on bidirectional text.** In a note whose paragraph direction is
   RTL, an embedded LTR timecode run is *logically* contiguous but *visually*
   reordered by UAX #9. A hit region derived from a substring index is therefore
   in the wrong place on screen, and Photonic cannot even see the problem because
   egui does not implement bidi at all ([42 §3.2](../specs/video-editor/42-localization.md#32-why-rtl-ui-is-refused-not-deferred)
   blocker 3, and [§9.3](#93-rtl-and-bidi--storage-is-correct-display-is-not-and-we-say-so) here).

**What ships instead:** typing a timecode into the note's *anchor* field parses it
once, through `Timecode::parse_to_tick` (`time.rs:244`) against the target
sequence's `frame_rate`, and stores a `Tick`. The panel renders the anchor as a
**chip** above the body — a typed, clickable, focusable object — not as hot text
inside the prose. The prose stays prose.

### 3.3 One note, one anchor

A note carries exactly one `NoteAnchor`. Rationale: "click it and go there" has
no meaning with two targets; a single anchor makes the list sortable by timeline
position (the ordering that makes it a work queue); and it makes the export a
table rather than a tree. A note about two places is two notes, which is also how
it reads in review. Recorded in [§12.3](#123-deliberately-excluded).

### 3.4 Where notes live — in the document, on `TimelineProject`

**Decision.** `TimelineProject.notes: Vec<ProjectNote>` — inside the document, so
notes travel with the project through `save_photon` (`photon_file.rs:36`),
Save-As, a copy to another machine and a hand-off, with no new file and no config
directory.

Checked against [39](../specs/video-editor/39-document-lifecycle.md):

- **39 §1.6** moves *persisted view preferences* out of `Document` into a sidecar
  — `Track.height_px`, panel sizes, sequence tabs, selection. A note is not a
  view preference: it is authored content, it is the reason the user opened the
  file, and no user expects it to be machine-local. It belongs in the document,
  and the `Annotation` precedent (`annotation.rs:9-10`, "stored in the `.photonic`
  file") already makes that call for the same class of data.
- **39 §1.1** then applies in full: notes are document mutations, so every note
  verb is undoable (§6). This is where K-G2 diverges from `Annotation`
  (`annotation.rs:10-11`) rather than inheriting.
- **39 §2.2** applies to `NoteAnchor`, which gains an unknown-preserving variant
  (§4.1).

**Why `TimelineProject` and not `Document`:**

1. Every anchor names timeline objects, so anchor validation needs the project.
2. `TimelineCmd::apply_in(&mut TimelineProject)` (`commands.rs:1761`) is the
   existing home for project-scope edits (`AddAsset`, `AddSequence`, marker
   commands). A `Document`-level field would need a new `Command` arm outside
   `Command::Timeline`, i.e. new plumbing in `history/mod.rs` for no benefit.
3. `Document.annotations` keeps the vector/design-review niche. Two stores with
   two clear scopes beats one store with a mode discriminator.

Consequence, stated so it is not a surprise: **notes require a timeline project**.
A note verb on a document with `doc.timeline == None` either creates the project
first (the batch `create_sequence` already uses, `handlers/video.rs:309-320`) or
returns `EditError::NoProject` (`ops.rs:35`). §6 chooses: the GUI panel is
video-mode only and the project always exists by then; the MCP arm returns
`NoProject`, matching every other video tool.

**Interaction with K-G4 (project templates).** [204 §3.3](204-k-g4-project-templates.md)'s
kept/cleared table predates this field. Notes must be **cleared at template
capture**: a template is a project *skeleton*, and one project's review notes are
not part of another project's structure. Recorded as a follow-up to 204
([§14.4](#14-follow-ups)) rather than edited into it here.

### 3.5 What K-G2 needs from K-A2, and what ships without it

The 26 §19.2 edge `K-A2 → K-G2` is real but **partial**. Split explicitly:

**Ships now, with no K-A2 work at all:**

| Capability | Because |
|---|---|
| `NoteAnchor::Timecode` and `NoteAnchor::Clip` | Pure `Tick`/`ClipId` addressing; nothing marker-related |
| `NoteAnchor::Marker` (sequence markers) | `Marker` is addressable by stable `MarkerId` today (`sequence.rs:834`), `AddMarker`/`RemoveMarker`/`SetMarker` ship (`commands.rs:660-676`), and the ruler paints them (`ruler.rs:154`) |
| **"Create a marker from this note"** — 26 K-G2's second headline verb | `ops::add_marker` (`ops.rs:1879`) + `TimelineCmd::AddMarker` (`commands.rs:660`) already exist. This verb has **no K-A2 dependency**, and believing otherwise is the most likely way to defer K-G2 for no reason |
| The panel, undo, MCP, export, resolve, filter, sort | None of it touches categories |

**Needs K-A2, and is therefore scoped out of v1 or ships inert:**

| Capability | Blocked on |
|---|---|
| Choosing a note's **category** in the UI | No command creates a `MarkerCategory` (§2.2). `ProjectNote.category: Option<MarkerCategoryId>` ships in the model and is **inert** — always `None` until K-A2 seeds `default_seed()` (`sequence.rs:756`, whose entries are already named "Note" and "Todo") |
| `NoteAnchor::ClipMarker` (a clip-scoped marker) | Model-expressible (`clip.rs:68-72`) but **no user verb creates a clip marker** (§2.2), so the variant ships with no producer — reachable only by a file written by a later build, which §4.1's forward-compat rule already handles |
| Cross-navigation to a Markers panel | The panel does not exist (`panels/video/`) |
| Sharing one export template with marker export | K-A2 owns `{{timecode}}`/`{{comment}}` templates; §8 ships two fixed formats and states the merge point |
| Category colour/glyph on the note row | `MarkerGlyph` exists (`sequence.rs:796`) but no note can carry a category yet |

**The category field is deliberately included now even though it is inert.**
26 K-C2 states the rule for exactly this situation — *"mirror the `MarkerCategory`
registry from K-A2 so there is one taxonomy pattern, not two"* (`26-…:452`).
Adding it later is serde-additive and free; inventing a second, note-only
taxonomy later is not.

---

## 4. Data-model change

### 4.1 Three new types in `photonic-core`, one new field

**One field is added to the persisted model.** In
`crates/photonic-core/src/timeline/sequence.rs`, on `TimelineProject`
(`sequence.rs:21`), beside `marker_categories` (`:42`):

```rust
/// Project notes (26 K-G2): anchored review notes that travel with the
/// document. Ordering is `(created, id)`, kept by `sort_notes`, so undo/redo
/// is byte-identical. Optional — absent from the JSON when empty, exactly as
/// `marker_categories` is.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub notes: Vec<ProjectNote>,
```

`TimelineProject::new` (`sequence.rs:47`) initialises it empty.

```rust
// timeline/ids.rs — joins the existing id_newtype! family (ids.rs:62-90)
id_newtype! {
    /// Identifies a [`ProjectNote`](crate::timeline::ProjectNote) in
    /// [`TimelineProject::notes`](crate::timeline::TimelineProject).
    NoteId,
}

// timeline/sequence.rs, next to `Marker` (sequence.rs:833)
/// A project note (26 K-G2). Free user text plus **one** typed anchor, so a
/// note is a coordinate and a sentence rather than prose to be re-parsed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectNote {
    pub id: NoteId,
    /// One-line title for the list row. May be empty; the panel then shows the
    /// first line of `body`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// The note. Plain UTF-8, stored in logical order, never normalized and
    /// never reordered (§9.2). Byte-capped by `NOTE_BODY_MAX_BYTES` (§9.5).
    pub body: String,
    /// Where this note points (§3.2). `Project` = a note about the whole
    /// project, with no target.
    #[serde(default)]
    pub anchor: NoteAnchor,
    /// Shares the K-A2 `MarkerCategory` registry — one taxonomy, not two
    /// (26 K-C2 states the rule). Inert until K-A2 ships category CRUD (§3.5).
    /// A missing category renders neutral and is flagged, never remapped —
    /// the same rule `Marker.category` carries (sequence.rs:842-845).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<MarkerCategoryId>,
    /// Review state. Mirrors `Annotation::resolved` (annotation.rs:23).
    #[serde(default)]
    pub resolved: bool,
    /// Human name or agent id. Mirrors `Annotation::author` (annotation.rs:25).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// UTC ISO-8601, from the same clock helper `Annotation::new` uses
    /// (annotation.rs:45). A sort key and an export column — never parsed,
    /// never localized in storage (§9.1).
    pub created: String,
}

/// What a [`ProjectNote`] points at. Internally tagged with a verbatim-
/// preserving `Unknown` arm, mirroring `ClipSource` (clip.rs:163-196) — the
/// 39 §2.2 pattern.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "anchor", rename_all = "snake_case")]
pub enum NoteAnchor {
    /// The project as a whole; no target, no navigation.
    #[default]
    Project,
    /// An absolute position in a sequence. Navigating activates the sequence
    /// and seeks. `at` is a `Tick`, never a timecode string (§3.2).
    Timecode { sequence: SequenceId, at: Tick },
    /// A clip. Navigating seeks to `clip.start` and selects the clip.
    Clip { sequence: SequenceId, track: TrackId, clip: ClipId },
    /// A sequence marker (35 §1). Navigating seeks to `marker.at`.
    Marker { sequence: SequenceId, marker: MarkerId },
    /// A clip-scoped marker (35 §1.5); its sequence tick is
    /// `Clip::marker_sequence_tick` (clip.rs:128). No producer until K-A2
    /// (§3.5) — present so a file written by a later build round-trips.
    ClipMarker { sequence: SequenceId, track: TrackId, clip: ClipId, marker: MarkerId },
    /// 39 §2.2 forward-compat: an anchor kind this build does not know. The
    /// whole object is retained verbatim and re-emitted unchanged; the panel
    /// shows a disabled "unknown target" chip and navigation is refused —
    /// never guessed. Declared last so serde tries the known tags first.
    #[serde(untagged)]
    Unknown(serde_json::Map<String, serde_json::Value>),
}
```

`#[serde(untagged)]` on one variant of an internally-tagged enum is exactly what
`ClipSource::Unknown` does (`clip.rs:194-195`), including the "declared last"
comment; `ClipSource::unknown_tag()` (`clip.rs:210`) is the accessor shape
`NoteAnchor::unknown_tag()` copies.

**`NoteAnchor` reuses `MarkerScope` in spirit but not in code.** `MarkerScope`
(`timeline/load.rs:196-205`) has the same `Sequence`/`Clip` split, but it is a
**load-report** type: it lives in `load.rs`, derives no serde, and is not part of
the persisted model. Persisted model does not belong in the loader module, and
an anchor needs the `MarkerId` alongside the scope anyway. Two small types with
clear homes beats one type with two jobs.

### 4.2 Command model: three arms, plural by construction

```rust
// timeline/commands.rs, beside AddMarker/RemoveMarker/SetMarker (:660-676)

/// Add project notes (26 K-G2). Plural so "paste three notes" and the inverse
/// of `RemoveNotes` are one command each.
AddNotes { notes: Vec<ProjectNote> },
/// Remove project notes, carrying each whole so the inverse is self-contained
/// (the shape `RemoveMarker` uses at :666).
RemoveNotes { notes: Vec<ProjectNote> },
/// Edit one note's fields — old/new full notes, self-inverting exactly as
/// `SetMarker` (:671) is.
SetNote { id: NoteId, old: Box<ProjectNote>, new: Box<ProjectNote> },
```

`ProjectNote` is boxed in `SetNote` because it carries two bodies of up to
`NOTE_BODY_MAX_BYTES` each (§9.5); `AddSequence` boxes for the same reason
(`commands.rs:451`).

**Why plural, when a `Command::Batch` would also be one undo step.** Notes live
on `TimelineProject`, not on `Sequence`, and `TimelineCmd::apply`'s debug assert
walks only `p.sequences` (`commands.rs:1749-1757`) — so a batch of per-note
commands would *not* trip [194 §2.4](194-k-a5-general-and-nested-clip-groups.md)'s
mid-batch validation panic today. Plural is chosen anyway, for two reasons that
do not depend on that accident: (a) `mem_estimate` and `description` are then
honest per verb — "Delete 12 notes", one estimate, one label — instead of twelve
entries the history graph must summarise; (b) it does not encode an assumption
that notes will stay outside `validate()` forever. The house rule is *plural
edits are single plural commands*; K-G2 follows it rather than arguing an
exemption.

**Three sibling additions, in the three places every variant already appears:**

- `sort_notes(&mut [ProjectNote])` sorting by `(created, id)`, called after every
  mutation — the direct analogue of `sort_markers` (`commands.rs:881-885`) and
  the reason undo/redo is byte-identical rather than merely equivalent.
- `mem_estimate` arms (`commands.rs:1631`): `AddNotes`/`RemoveNotes` →
  `json_len(notes)`; `SetNote` → `json_len(old) + json_len(new)`, the same shape
  as `SetEffect` (`:1653`). **This is mandatory, not cosmetic.** Marker commands
  currently fall into the `_ => 64` catch-all (`commands.rs:1655`), so an
  `AddMarker` carrying an arbitrarily long `Marker.note` (`sequence.rs:841`)
  reports 64 bytes against the byte budget — an existing instance of exactly what
  39 §1.3 warns about (*"each should report its `mem_estimate` honestly, or the
  budget is enforced against a fiction"*). K-G2 must not add a fourth.
  ([§14.5](#14-follow-ups) records the marker one.)
- `description` arms (`commands.rs:1660`): "Add note" / `format!("Delete {n} notes")`
  / "Edit note".

Inverses (`commands.rs:2504-2515` is the pattern): `AddNotes ⇄ RemoveNotes` with
the same payload; `SetNote` swaps `old`/`new`.

### 4.3 Ops and errors

```rust
// timeline/ops.rs, beside add_marker (:1879)
pub fn add_note(p: &TimelineProject, note: ProjectNote) -> Result<TimelineCmd, EditError>;
pub fn set_note(p: &TimelineProject, new: ProjectNote) -> Result<TimelineCmd, EditError>;
pub fn remove_notes(p: &TimelineProject, ids: &[NoteId]) -> Result<TimelineCmd, EditError>;
/// Every anchor target must resolve in `p`, or the edit is refused whole.
pub fn validate_anchor(p: &TimelineProject, a: &NoteAnchor) -> Result<(), EditError>;
```

`EditError` (`ops.rs:34`) gains three variants:

```rust
/// No note with this id.
NoNote(NoteId),
/// No marker with this id in the addressed scope.
NoMarker(MarkerId),
/// A note body or title exceeded its byte cap (§9.5). Refused, never truncated.
NoteTooLong { bytes: usize, max: usize },
```

`map_edit_error` (`handlers/video.rs:250`) has an `other =>` catch-all (`:269`),
so the additions are non-breaking there; each still gets an explicit arm.
`validate_anchor` reuses `NoSequence`/`NoTrack`/`NoClip` (`ops.rs:36-38`).
`NoteAnchor::Unknown` **validates vacuously** — it names nothing this build can
check, and refusing it would make a forward-compatible file uneditable.

Validate-then-commit (39 §1.1): `add_note` and `set_note` run `validate_anchor`
and the byte caps **before** constructing a command. A failure returns `Err` and
mutates nothing.

### 4.4 Dangling anchors are reported at load, never repaired

A note's target can be deleted — that is normal, not corruption. The precedent is
exact: a `Marker.category` pointing at an absent category is collected into
`LoadReport.dangling_categories` (`load.rs:620-624`) and, in that field's own
words, *"flagged for the panel, never remapped"*; `finalize_load`
(`load.rs:138`, comment at `:175-177`) repeats it: *"A marker whose category is
missing renders neutral and is flagged; it is NEVER silently remapped"*.

So:

- `LoadReport` (`load.rs:614`) gains
  `pub dangling_note_anchors: Vec<(NoteId, NoteAnchor)>`, populated by a new
  `dangling_note_anchors(project)` pass beside `dangling_marker_categories`
  (`load.rs:214`).
- The note is **kept**, with its anchor **intact**. The panel renders the chip
  disabled — "target no longer exists" — and offers "clear anchor" as an explicit
  user edit (one `SetNote`). Nothing is repaired behind the user's back, because
  the anchor is the evidence of what the note was about.
- `UnknownSite` (`load.rs:530`) gains a `Note { note: NoteId }` arm and
  `collect_unknown_variants` (`load.rs:676`) gains a notes pass, so a
  `NoteAnchor::Unknown` is diagnosed once per load like every other unknown
  variant (39 §2.2 rule 3).
- Notes are **not** part of `Sequence::validate` (`sequence.rs:378`) and a
  dangling anchor is **never** a load rejection. A note is metadata; refusing to
  open a project because a note points at a deleted clip would be hostile, and it
  is the argument `finalize_load` already makes for degenerate groups
  (`load.rs:145-151`).

### 4.5 The one prerequisite defect K-G2 makes reachable

Adding `RightDrawerGroup::Notes` (§7.4) writes a new enum token into
`<config>/Photonic/preferences.json` via `AppPreferences.open_right_drawer`
(`preferences.rs:113`). `AppPreferences::load` is
`serde_json::from_str(&json).unwrap_or_default()` (`preferences.rs:322-331`), and
serde's `#[serde(default = …)]` (`preferences.rs:112`) applies to a **missing**
field, not to one that fails to deserialize. So a user who opens the Notes drawer
and then runs an older build **loses every preference** — keymap
(`preferences.rs:152`), hotbar usage (`:145`), drawer widths, snap toggle — not
just the drawer choice.

This is latent today (the enum has not changed since it was introduced) and K-G2
is the change that makes it reachable, so K-G2 fixes it, in the spirit of
[194 §8.1](194-k-a5-general-and-nested-clip-groups.md) defect 2:

```rust
// panels/mod.rs:1392 — RightDrawerGroup
    /// A group written by a newer build. Never offered by `all_for_mode`;
    /// normalized to the default at load so one unknown token cannot discard
    /// the whole preferences file.
    #[serde(other)]
    Unknown,
```

plus one line in `AppPreferences::load` mapping `Some(RightDrawerGroup::Unknown)`
to `default_open_right_drawer()` (`preferences.rs:170`). `DrawerGroup`
(`panels/mod.rs:1209`, persisted at `preferences.rs:106`) takes the identical
treatment in the same change — it has the identical bug, and fixing one of two
identical enums is worse than fixing neither because it looks done.

---

## 5. Migration and format-version impact

**`CURRENT_FORMAT_VERSION` stays 5. K-G2 lands additively inside v5.**
(`crates/photonic-core/src/document.rs:117`.)

Point by point:

- **The change is purely additive.** `TimelineProject.notes` is
  `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, exactly as
  `marker_categories` is (`sequence.rs:41-42`). A v5 file written before K-G2
  carries no `notes` key and deserializes with an empty vec; a v5 file written
  after K-G2 with no notes is **byte-identical** to one written before. There is
  nothing for a `V5ToV6` step to do — `migrations()` (`migration.rs:58`) exists
  to upgrade files whose *meaning* changed, and no existing field changes
  meaning.
- **A no-op bump costs users real compatibility.** `COMPAT_WINDOW = 1`
  (`migration.rs:16`) is how far ahead a file may be and still load. Minting v6
  to stamp a number would push every v6-authored project out of the build before
  it, in exchange for nothing. `V1ToV2`/`V2ToV3` (`migration.rs:70`, `:87`) are
  already precedents for stamping a number on an additive change; repeating that
  for a change touching no existing field is the version-inflation
  [204 §5.1](204-k-g4-project-templates.md) argues against. All five prior
  Band-5 mini-specs land additively; K-G2 makes six.
- **Backward: an older build opening a K-G2 file.** `TimelineProject` derives
  `Deserialize` with no `deny_unknown_fields` (`grep -rn deny_unknown_fields crates/`
  → zero hits workspace-wide), so an older build **ignores** the `notes` key and
  opens the project normally. It then **drops the notes on save.** That is the
  standard consequence of any additive field and needs no new mechanism — but it
  is the one behaviour worth telling the user about, and 39 §2.3's "newer, minor"
  row already requires exactly that warning ("*warn that saving may lose
  newer-only data*"). No change to 39 is needed; K-G2 simply must not assume the
  warning exists — as of `8a33f32`, `Document::from_value_with_report`
  (`document.rs:1717`) falls through and loads leniently inside the window with
  no such warning ([§14.6](#14-follow-ups)).
- **Forward: a K-G2 file carrying an anchor kind this build lacks.** Handled by
  `NoteAnchor::Unknown` (§4.1) under 39 §2.2's rules: preserved verbatim,
  re-emitted unchanged, rendered inert (a disabled chip), diagnosed once per load
  (§4.4). Never dropped, never guessed.
- **New `TimelineCmd` variants are not a format change.** They appear only in the
  sibling `photon_history` key, and `load_photon` restores history best-effort —
  a payload that fails to deserialize yields `None` history while the document
  still opens (`photon_file.rs:60-64`). The same reasoning
  [194 §4](194-k-a5-general-and-nested-clip-groups.md) point 2 gives.

ROADMAP §10 point 5 ("additive serde/migration round-trip passes **when model
changes**") is answered by T10/T11 (§10.2), not waived: the model *does* change
here, unlike in 194 and 204, so the round-trip obligation is live.

---

## 6. Undo unit and its exact inverse

Repo rule: one user verb = one undo unit, fanned-out edits included; an operation
that cannot be undone atomically must not commit partially (39 §1.1).

| User verb | History | Command | Exact inverse |
|---|---|---|---|
| **Add note** | one | `AddNotes { notes: [n] }` | `RemoveNotes { notes: [n] }` |
| **Delete note(s)** (row menu, multi-select, "Clear resolved") | one | `RemoveNotes { notes }` | `AddNotes { notes }` |
| **Edit note text** (a whole editing session) | **one** | `SetNote { id, old, new }` | `SetNote { id, old: new, new: old }` |
| **Re-anchor** (chip → "Set to playhead" / "Anchor to selected clip" / clear) | one | `SetNote` | swap |
| **Resolve / unresolve** | one | `SetNote` | swap |
| **Set category** (inert until K-A2) | one | `SetNote` | swap |
| **Create marker from note** | one | `AddMarker` (`commands.rs:660`) via `ops::add_marker` (`ops.rs:1879`) | `RemoveMarker` (`:2504`) |
| **Navigate to a note's anchor** | **none** | — | n/a, see below |

### 6.1 Text editing is one undo unit per editing session

Mechanism, following `panels/video/caption_editor.rs:411-430` exactly and **not**
the `SetEffect` coalescing path (§2.5 explains why that path cannot fire for
typing):

1. The row's editor keeps the draft in egui temp memory keyed by `NoteId`
   (`ui.data(|d| d.get_temp(...))` / `insert_temp`), rendering a
   `TextEdit::multiline` over the draft. **No command is constructed while the
   user types.**
2. On `resp.lost_focus()`: if `Escape` was pressed, discard the draft and commit
   nothing; otherwise, if the draft differs from the stored body, build **one**
   `SetNote` through `ops::set_note` and commit it. The draft is cleared either
   way.
3. The same gate covers the title field and the two of them commit **one**
   `SetNote` when both changed in the same focus cycle, because `SetNote` carries
   the whole note.
4. Losing focus by clicking another note, switching drawer, switching mode, or
   saving commits the pending edit first — the boundary set 39 §1.2 names as
   coalescing breakers, enforced here by focus rather than by a timer that does
   not exist.

**A `SetNote` coalesce arm is added anyway** (`commands.rs:2538`), keyed on
`id`, mirroring `SetEffect` (`:2634-2650`) — keep the anchor's `old`, adopt the
incoming `new`. It is defence in depth for the one path that *is* pointer-driven
(dragging a note's anchor chip along the ruler to re-anchor, which streams
`SetNote` under an open gesture) and for any future streamed producer. It is
explicitly **not** the mechanism that makes typing one undo unit; the commit gate
in step 2 is. Saying so matters, because a reviewer who sees the coalesce arm and
assumes it covers typing will accept a per-keystroke implementation.

### 6.2 Navigation records nothing, and that is the rule not an exception

Clicking a note seeks the playhead, activates a sequence and may change
`timeline_selection` — all **session** state. `self.playhead` is not in
`Document`; selection is session state by 35 §3.4; sequence activation *is*
document state (`TimelineProject.active_sequence`, `sequence.rs:28`) and is the
one thing worth being careful about.

**Decision:** navigating to a note in a *different* sequence commits the same
`SetActiveSequence` command any other sequence switch commits
(`ops::set_active_sequence`, `ops.rs:341`; label "Switch sequence",
`commands.rs:1681`) — one undo unit, identical to clicking the sequence tab.
Navigating within the current sequence commits **nothing**. This is the shipped
K-G5 rule ("Navigation produces **zero** undo units, pinned by test",
`ROADMAP.md:74`) applied consistently: what is not a document mutation is not
undoable, and what already *is* one does not become exempt because a note
triggered it.

### 6.3 Atomicity

`RemoveNotes` over a multi-selection validates every id first and refuses whole
on any miss (`EditError::NoNote`). `AddNotes`/`SetNote` validate the anchor and
the byte caps first (§4.3). There is no partial-commit path.

---

## 7. MCP surface and GUI route

### 7.1 An MCP surface is warranted — and this is the item where it is obvious

CAP-019 parity is ROADMAP §10 point 3, and 26 §5 lists PA-11 (full MCP parity) as
explicitly **not yet held**. Beyond parity, notes are the natural *output* channel
for an agent review pass: an agent that can watch a sequence and leave "the
audio dips at 00:01:23" as an anchored, resolvable, exportable note is doing the
thing agents are useful for. `Annotation` already ships that idea for vector mode
(`handlers/annotations.rs:5`) and it is the most-used shape of the existing
annotation tools.

### 7.2 Five tools

| Tool | Args | Mutating |
|---|---|---|
| `add_project_note` | `body: String`, `title: Option<String>`, `author: Option<String>`, anchor args (below) | **yes** — one undo unit |
| `list_project_notes` | `include_resolved: bool = false`, `sequence_id: Option<SequenceId>` | no |
| `update_project_note` | `note_id: NoteId`, `title/body/author/resolved: Option<…>`, anchor args, `clear_anchor: bool = false` | **yes** — one undo unit |
| `remove_project_notes` | `note_ids: Vec<NoteId>` | **yes** — one undo unit |
| `export_project_notes` | `path: String`, `format: Option<String>` (`md` \| `json`), `include_resolved: bool = false` | no (writes a file) |

**Anchor args mirror the shipped marker addressing rather than inventing one.**
`add_marker` takes `at_ticks` / `at_tc` / `at_seconds` and resolves them through
`resolve_tick` (`handlers/video.rs:84`, used at `:526`), which already parses a
drop-frame timecode string against the sequence's rate. Notes take the same
three, plus `sequence_id`, `clip_id` and `marker_id`; precedence is
`clip_id` → `marker_id` → tick-family → none, and supplying two anchor families
is an error rather than a silent preference. `update_project_note` with
`clear_anchor: true` sets `NoteAnchor::Project`.

`update_project_note` is **one** tool that sets any subset of fields, not five
setters, because one user verb is one undo unit: an agent changing the body and
resolving in one call must produce one `SetNote`, and five tools would produce
five history entries for what a human would do in one edit.

`list_project_notes` returns:

```json
{ "notes": [
  { "note_id": "…", "title": "late cut", "body": "two frames late",
    "anchor": { "anchor": "timecode", "sequence": "…", "at": 84672000 },
    "timecode": "00:00:12;00", "resolved": false, "author": "review-agent",
    "created": "2026-07-28T14:02:11Z", "category": null }
], "total": 1 }
```

`timecode` is a **derived, read-only** convenience computed with
`Timecode::format_tick(at, seq.frame_rate, seq.start_timecode, prefer_drop)`
(`time.rs:297`), so an agent never re-derives drop-frame labelling. `anchor` is
the stored typed value; the two never disagree because one is computed from the
other.

**Both arms call the same `ops::` functions.** [194 §6](194-k-a5-general-and-nested-clip-groups.md)
records what happens otherwise (the link-group expansion exists as two
hand-mirrored copies, `ops_bridge.rs:345-430` and `handlers/video.rs:140-220`).
Do not repeat it. Handlers use `history.execute_discrete(Command::Timeline(cmd), &mut doc)`
— the shape `add_marker` uses at `handlers/video.rs:552` — and **never** the
direct-mutation shortcut `add_annotation` takes (`handlers/annotations.rs:25`),
which is the bug §2.3 names.

### 7.3 `export_project_notes` writes to a path, consistently with what ships

§2.6 established that `photonic-mcp` implements no path sandbox.
`export_project_notes` therefore behaves **exactly** like the shipped
`export_captions` (`handlers/video.rs:5410-5444`): format inferred from the
extension or given explicitly, `std::fs::write` to the caller's path, path echoed
in the result. That widens no surface `save_document` (`handlers/document.rs`)
and `export_captions` do not already have, and inventing a one-off sandbox for
notes while two neighbours write anywhere would be theatre. The real fix is a
server-wide permitted-roots list, recorded as a follow-up
([§14.7](#14-follow-ups)) and owned by 28-security-model, not by K-G2.

Wiring, following the existing pattern exactly: arg structs in
`protocol/args/video.rs`, handlers in `handlers/video.rs` beside the marker
handlers (`:514-590`), dispatch arms in `dispatch.rs` beside `:2194`, names added
to `VIDEO_TOOL_NAMES` (`handlers/video.rs:8277`), then `schema_gen.rs`
regenerated. **CI gates the docs**: `.github/workflows/ci.yml:163-167`
regenerates `docs/mcp-api.md` and runs `git diff --exit-code` on it, so
regeneration is mandatory, not optional.

### 7.4 GUI route — a right-rail drawer

**Decision:** `RightDrawerGroup::Notes` (`panels/mod.rs:1392`), added to
`VIDEO_ALL` (`:1418-1424`) between `AudioMixer` and `Chat`, making six. `ALL`
(`:1409`, vector mode) is **unchanged** — notes are a timeline feature.
`icon()` (`:1435`) takes `ph::NOTE_PENCIL`; `title()` (`:1446`) is "Notes".
Panel interior: a new `crates/photonic-gui/src/panels/video/notes.rs`, drawn
from the `match right_render_group` at `app/mod.rs:3758-3789` with the
`draw_color_controls` signature shape (`ui, doc, history, &mut self.pending_panel_actions, …`,
`app/mod.rs:3762-3771`), so `right_rev_before` (`:3739`) marks the document dirty
for free.

**Why the right rail rather than the left.** The right rail already holds the
*meta* surfaces — History (K-G5's edit tree) and AI Chat — while the left rail is
content authoring (Media Pool, Clip Inspector, Effects, Captions, Node Editor,
Titles, Source Monitor, Multicam, Transcript: nine entries already,
`panels/mod.rs:1285-1295`). A note is commentary about the project, not a thing
you author into a frame. It also sits beside Chat, which is where an agent's
notes come from.

The panel:

| Element | Behaviour |
|---|---|
| Header | count, "Show resolved" toggle, "New note" button, sort selector (timeline position \| created \| author) |
| Row | category glyph/colour when present (inert pre-K-A2), anchor chip, title or first body line, resolved checkbox, author, overflow menu |
| Anchor chip | click ⇒ navigate (§6.2); disabled with "target no longer exists" when the anchor dangles (§4.4); disabled with "unknown target" for `NoteAnchor::Unknown` |
| Expanded row | `TextEdit::multiline` over the draft buffer, commit-on-blur (§6.1), byte counter, "Set to playhead" / "Anchor to selected clip" / "Clear anchor" / "Create marker here" / "Delete" |
| Empty state | one line explaining what a note anchors to — the discoverability the feature needs on first run |

**Accessibility is not optional here** ([41 §3](../specs/video-editor/41-accessibility.md#3-keyboard-access)).
R-8 ("no new pointer-only interaction ships") means every row action has a
keyboard path: `Tab` reaches rows in list order (R-1), `Enter` navigates,
`Space` toggles resolved, `Delete` deletes, `Esc` follows the R-6 ladder (cancel
edit → clear selection → leave the region). R-20 (colour is never the sole
carrier) is satisfied for the category chip by `MarkerGlyph` (`sequence.rs:796`),
which 41 §7 added for markers for exactly this reason — a second reason the note
category must be the *marker* category and not a new one. `crates/photonic-gui/tests/hit_target_lint.rs`
and `keyboard_gate_lint.rs` already exist and will cover the new panel.

---

## 8. Exportable and reportable — yes, one direction

**Decision: notes export; notes do not import.**

- **Markdown** — the review document a human reads. One `##` section per
  sequence in timeline order, then project-level notes; each note is a bullet
  carrying its timecode label, title, body, author and resolved state.
  Timecodes come from `Timecode::format_tick` (`time.rs:297`) with the
  sequence's `start_timecode` and drop-frame preference, so **the exported label
  is the label the ruler shows**. Anything else produces a review document whose
  timecodes do not match the editor's screen, which is worse than no export.
- **JSON** — the same content, machine-shaped, identical to
  `list_project_notes`'s payload so there is one schema and not two.
- **Not CSV.** A note body contains newlines and commas; CSV forces a quoting
  dialect argument for zero benefit over JSON.
- **No import.** Reading notes back would need identity reconciliation (is this
  the same note?) and anchor resolution against ids minted in another document —
  X-series interchange scope ([196](196-x-2-opentimelineio-interchange.md)), not
  K-G2. Recorded in §12.3.
- **Notes never reach a rendered frame.** No burn-in, no overlay, no export
  metadata track. `Annotation` states the same rule for the vector side
  (`annotation.rs:9-10`, "stripped from all export formats") and K-G2 inherits it
  verbatim. A note is not content.

Placement: the writer is a pure function in
`crates/photonic-core/src/timeline/` (it needs `TimelineProject`, `Timecode` and
nothing else — no ffmpeg, no GPU), called by both the GUI's "Export notes…"
(`rfd` file dialog, the shape `run_file_dialog` uses at `app/mod.rs:1848`) and
the MCP tool (§7.3). One writer, two callers.

**Where this meets K-A2.** K-A2 owns marker export with
`{{timecode}}`/`{{comment}}`/`{{frame}}` templates (`26-…:224`). K-G2 ships two
fixed formats rather than a template engine, and when K-A2's templates land the
correct move is for note export to *use* them — one template engine, two
producers. Recorded in §14.2 so it is not re-invented.

---

## 9. Text, i18n and script coverage

Notes are the first surface in the tree whose content is *prose the user writes
in their own language*. Caption text already is (`caption_editor.rs:415`), but a
caption is short and passes through a shaping-capable render path; a note is a
paragraph that only ever appears in egui.

### 9.1 Notes are user content, not UI chrome

[42 §6.1](../specs/video-editor/42-localization.md#61-the-rule-that-decides-the-architecture)'s
rule decides the architecture: the note **body** is content, and none of 42's
string-externalization machinery applies to it. The panel's **labels** are UI and
join 42 §4's Fluent sweep when Tier B lands — which it has not (§2.6: no `fl!`,
no `i18n` anywhere in `photonic-gui`), so K-G2 writes literal `en-US` strings
exactly like every other panel and inherits the sweep later. Two rules apply from
day one so the sweep is mechanical rather than a rewrite: **no user-visible
sentence is built by concatenation or by `format!` over fragments** (42 §4's
concatenation rule), and **a note title is never interpolated into a sentence**
until FSI/PDI isolation exists (42 §4 point 5).

`ProjectNote.created` is stored as UTC ISO-8601 (§4.1), never as a locale string.
Display formatting is 42 §5's business and happens at paint time.

### 9.2 Storage is exactly what the user typed

UTF-8, logical order, **no normalization** (no NFC/NFD pass) and no reordering.
Two reasons: normalizing changes bytes, which breaks the byte-identical
undo/redo and round-trip assertions §10.2 pins; and a user who typed a specific
sequence of code points is entitled to get it back. `String` is already the right
container — Rust guarantees UTF-8 and serde_json escapes correctly.

### 9.3 RTL and bidi — storage is correct, display is not, and we say so

[42 §3.2](../specs/video-editor/42-localization.md#32-why-rtl-ui-is-refused-not-deferred)
blocker 3 is unambiguous: *"egui cannot render RTL text at all. `epaint` has no
bidi dependency in any version; the upstream issue has been open since 2021."*
So an Arabic or Hebrew note **will display in logical order — i.e. wrong — in the
notes panel**, today and for as long as that upstream issue is open. This is an
app-wide limitation, not a K-G2 defect, and K-G2's obligation is to be honest
about it rather than to pretend or to hide it:

1. **The panel detects and discloses.** A small `text_metrics::starts_rtl(&str)`
   helper — same module, same integer-only, table-driven discipline as
   `is_scriptio_continua` (`text_metrics.rs:84`) — flags a note whose first
   strong character is RTL, and the row shows a once-per-session inline notice:
   *"contains right-to-left text; Photonic cannot yet display it in visual order.
   The note is stored and exported correctly."* One notice, per 39 §2.2 rule 3's
   diagnose-once discipline.
2. **Export is unaffected and is the escape hatch.** §8's Markdown and JSON carry
   the logical string, which every conformant renderer displays correctly. The
   content survives the display gap intact.
3. **Photonic never reorders.** No RLM/LRM insertion, no direction override, no
   "helpful" mirroring. 42 §3.2's own conclusion — *"Mirroring the layout while
   the text inside still renders in logical order is worse than not mirroring"* —
   applies unchanged to a single text field.
4. This is also the second reason §3.2 refuses to parse anchors out of note text:
   under bidi there is no stable mapping from a substring index to a screen
   region.

### 9.4 CJK renders as tofu in the panel today — say so

`crates/photonic-app/src/main.rs:503-505` is the only `set_fonts` call in the
workspace:

```rust
let mut fonts = egui::FontDefinitions::default();
egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
egui_ctx.set_fonts(fonts);
```

`FontDefinitions::default()` carries no CJK, Arabic, Hebrew or Devanagari
coverage, and egui performs no system-font fallback. A Japanese note therefore
renders as replacement boxes in the panel. `crates/photonic-gui/tests/no_tofu_glyphs.rs`
does **not** catch this — it scans *source literals* for four known-missing
symbols (`no_tofu_glyphs.rs:1-11`), not user content.

**K-G2 does not ship a font loader.** That is an app-wide change belonging to
42 §7's fallback work, it needs a licensing decision about which faces ship (a
ROADMAP §7 content gate, which K-G2 otherwise does not touch), and a notes panel
is the wrong place to invent a font stack. K-G2 ships §9.3's disclosure notice
extended to any script with no coverage in the loaded fonts, and hands the
follow-up one useful verified fact: **`photonic-gui` already depends on
`photonic-render`, which depends on `glyphon`** (`crates/photonic-render/Cargo.toml:13`)
and therefore on cosmic-text's font database — so a system-font fallback loader
needs **no new third-party dependency**, only a small bridge that registers
resolved face bytes into `FontDefinitions`. Recorded as §14.8.

### 9.5 Length caps: bytes, and refuse rather than truncate

`SetNote` carries two whole bodies into a byte-budgeted history (39 §1.3). Caps:

```rust
pub const NOTE_BODY_MAX_BYTES: usize = 8192;
pub const NOTE_TITLE_MAX_BYTES: usize = 200;
```

Measured in **UTF-8 bytes**, deliberately:

- Bytes are the unit the history budget is denominated in, so the cap and the
  budget agree.
- A **character** or **cell** cap would make the same note legal in English and
  illegal in Japanese — three bytes per ideograph against one per ASCII letter
  cuts both ways, and any of the three units is arbitrary; only bytes are
  *relevant*. 42 §6.3 fixed the caption budget's unit for a different reason
  (determinism of a *persisted boundary*); here there is no boundary to persist,
  only a ceiling.
- `ops::add_note`/`set_note` **refuse** with `EditError::NoteTooLong { bytes, max }`
  rather than truncating — truncating a UTF-8 string at a byte offset can split a
  grapheme, and silently discarding a user's last paragraph is the failure mode
  39 §1.1's validate-then-commit exists to prevent. The panel shows a live byte
  counter and disables commit past the cap, so the refusal is a guard, not the
  normal path.

**`text_metrics::cell_width` (`text_metrics.rs:35`) must not be used anywhere in
this item.** 42 §6.3 states why: *"`unicode-width` is used here and nowhere else
— it reports terminal cell widths, which are wrong for hit-testing or caret
placement in a proportional font."* A note-list preview truncated at N "cells"
would be wrong for exactly the CJK case it appears to serve. Truncation and
eliding are egui's layout job (`Label::truncate`), which measures real advances.

### 9.6 Search is substring, not tokens

`draw_drawer` gives each left-rail group a shared search bar
(`panels/mod.rs:1480-1490`); the notes panel gets its own filter field. It
matches by **substring over the logical string** with simple ASCII-plus-Unicode
case folding. It must **not** be word-tokenized: `text_metrics::is_scriptio_continua`
(`text_metrics.rs:84`) exists precisely because Han, Kana, Thai, Lao, Khmer and
Myanmar are written without inter-word spaces, so a word index would return
nothing for a Japanese note. Substring is both simpler and correct.

---

## 10. Acceptance fixtures and tests

### 10.1 Fixtures — K-G2 is not a gated item

**No rights-cleared content is required.** Every fixture is built
programmatically in-test, the style `crates/photonic-core/tests/scope_migration.rs`
already uses, with `ClipSource::Adjustment` / `SolidColor` clips carrying no
media asset — the choice `crates/photonic-app/tests/acceptance_stories.rs:30-35`
already documents. No media bytes, no probe, no GPU, no ffmpeg, no new
dependency. No `AssetRightsManifest`
([23 §7.2](../specs/video-editor/23-legal-open-source-implementation-routes.md#72-manifest))
is implicated and **no ROADMAP §7 gate applies to this item**.

The one fixture worth committing is a **script corpus**, and it is text, not
media: a small `notes_scripts.json` of note bodies in Japanese, Simplified
Chinese, Korean, Arabic, Hebrew, Hindi, Thai and a ZWJ emoji sequence — the same
script set 42 §8 already names for captions (`caption_ja`, `caption_ar`, …),
authored by Photonic, a few hundred bytes.

### 10.2 Tests

| # | Test | Where | Asserts |
|---|---|---|---|
| T1 | Add / edit / remove round-trip; `sort_notes` order is `(created, id)` after each | `timeline/ops.rs` `mod tests` | §4.2 |
| T2 | `assert_undo_roundtrip` (`ops.rs:2921`) for `AddNotes`, `RemoveNotes`, `SetNote`; apply→undo yields a **byte-identical** `to_json`, redo re-applies | `ops.rs` `mod tests` | §6, ROADMAP §10.4 |
| T3 | **Typing is one undo unit** — a simulated edit session (buffer mutated N times, one `lost_focus` commit) grows `history.len()` by exactly **1**; and the same N mutations committed per-keystroke would grow it by N (the negative control, so the test proves the gate and not the absence of typing) | `photonic-gui/tests/video_ui_paths.rs` | §6.1 — **the regression this design exists to prevent** |
| T4 | `Escape` during an edit commits nothing and leaves the document byte-identical | `video_ui_paths.rs` | §6.1 step 2 |
| T5 | Delete 12 notes = **one** history entry, one label, and `mem_estimate` > the `_ => 64` catch-all | `ops.rs` `mod tests` | §4.2 |
| T6 | Anchor validation: every `NoteAnchor` variant against a present and an absent target; absent ⇒ `Err`, no command, document unchanged | `ops.rs` `mod tests` | §4.3 |
| T7 | **Dangling anchor survives load** — delete a clip an anchor names, save, reload: the note is present, its anchor is **unchanged**, and `LoadReport.dangling_note_anchors` names it | `photonic-core/tests/timeline.rs` | §4.4 — the highest-blast-radius behaviour |
| T8 | `NoteAnchor::Unknown` round-trips **verbatim** through `to_json`→`from_json`→`finalize_load`, is reported once in `unknown_variants`, and navigation is refused | `photonic-core/tests/forward_compat.rs` | §4.1 / 39 §2.2 |
| T9 | Byte caps refuse rather than truncate; a body one byte over is `NoteTooLong`; a body of multi-byte graphemes exactly at the cap is accepted intact | `ops.rs` `mod tests` | §9.5 |
| T10 | **Additive serde** — a v5 document with no `notes` key loads with an empty vec, and re-serializing a project with no notes emits **no** `notes` key (byte-identical to a pre-K-G2 file) | `photonic-core/tests/forward_compat.rs` | §5 |
| T11 | **Migration round-trip** — a v4 fixture migrates through `V4ToV5` (`migration.rs`), gains an empty `notes`, accepts a note, and re-saves at `format_version == 5` | `photonic-core/tests/scope_migration.rs` (extends) | §5, ROADMAP §10.5 |
| T12 | **Script corpus** — every body in `notes_scripts.json` survives add → save → load → export(md) → export(json) **byte-identical**, with no normalization applied | `photonic-core/tests/timeline.rs` | §9.2 |
| T13 | `starts_rtl` classifies the corpus correctly (Arabic/Hebrew true; Japanese/Hindi/Thai/Latin false) and is pure integer table lookup | `photonic-core/src/text_metrics.rs` `mod tests` | §9.3 |
| T14 | **Export timecodes match the ruler** — a sequence at 30000/1001 with `start_timecode = 01:00:00:00` exports labels equal to `Timecode::format_tick` for the same ticks, drop-frame separator included | `photonic-core/tests/timeline.rs` | §8 |
| T15 | **Preferences survive an unknown drawer group** — a `preferences.json` naming a group this build lacks loads with every *other* field intact and the drawer defaulted | `photonic-gui/tests/` (new) | §4.5 |
| T16 | **Create marker from note** is one undo unit, the marker lands at the anchor's tick, and undo removes it | `ops.rs` `mod tests` | §6, §3.5 |
| T17 | **Navigation records nothing** — navigating within the active sequence grows `history.len()` by **0**; navigating across sequences grows it by exactly **1** (`SetActiveSequence`) | `video_ui_paths.rs` | §6.2 |
| T18 | **GUI arm** — headless add / edit / resolve / delete / navigate through the notes panel path | `photonic-gui/tests/video_ui_paths.rs` | ROADMAP §10.2 |
| T19 | **CAP-019 parity story** — MCP arm (`add_project_note` → `update_project_note` → `list_project_notes`) vs GUI arm, structural compare via the existing harness | `photonic-app/tests/acceptance_stories.rs` | ROADMAP §10.10 |
| T20 | **Markers are not regressed** — the existing marker suite passes unchanged, and a note anchored to a marker does not alter that marker on add, edit, resolve or delete | `photonic-core/tests/timeline.rs` | ROADMAP §9 protected surface |

T3 deserves the emphasis: a per-keystroke implementation passes every other test
in this table, ships, and is discovered by the first user who types a paragraph
and presses Ctrl+Z.

---

## 11. Definition of done (ROADMAP §10), made answerable

| # | Requirement | How K-G2 answers it |
|---|---|---|
| 1 | Core op/engine service with unit tests | `ProjectNote`/`NoteAnchor`/`NoteId` in `photonic-core`; `ops::{add_note, set_note, remove_notes, validate_anchor}`; the note-export writer; T1, T2, T5, T6, T9, T14, T16 |
| 2 | GUI route, or a recorded exception | `RightDrawerGroup::Notes` + `panels/video/notes.rs` (§7.4), keyboard-complete per 41 §3; T18. **No exception is sought** |
| 3 | MCP tool/schema/generated docs | Five tools (§7.2); `docs/mcp-api.md` regenerated; `ci.yml:163-167` drift gate green; `VIDEO_TOOL_NAMES` (`handlers/video.rs:8277`) updated |
| 4 | One verb, one undo unit; undo/redo identity | §6; T2, T3, T4, T5, T17. The typing case is the one that needs a mechanism, and it has one (§6.1) |
| 5 | Additive serde/migration round-trip **when the model changes** | The model **does** change (§4.1), so this is live, not waived: T8, T10, T11 |
| 6 | Pixel/audio IR/eval/golden/sync coverage | **N/A — K-G2 touches no pixel or audio path.** No `ContentHash` input changes: notes are document metadata, never a graph input, and never composited. Stated rather than invented (the clause [196 §11](196-x-2-opentimelineio-interchange.md) asked ROADMAP §10.6 to grow) |
| 7 | Hard gates green; trend metrics not regressed | No new budgets. Two deterministic bounds worth asserting: `mem_estimate` is honest for all three arms (§4.2, T5), and the note byte cap bounds any single history entry (§9.5, T9) |
| 8 | Offline, privacy, licensing, content, product gates | **Offline:** an in-document field; no network, no telemetry. **Privacy:** notes may contain client names, so — verified — `CrashReport` (`diagnostics.rs:50-66`) collects only version/timestamp/os/arch/panic message/location/backtrace and its `capture` doc states *"no document, filesystem, or environment state is touched"* (`:69-70`); the one obligation K-G2 carries is that **no `panic!`/`debug_assert!` message in the notes path may interpolate a note body**, or it would leak into `panic_message`. **Content:** no bundled bytes. **Licensing:** §13 |
| 9 | Protected surfaces not regressed | Markers are protected (`ROADMAP.md:362`) and are read, never rewritten, except by the explicit "create marker from note" verb, which uses the shipped op (T16, T20). PA-8 (`Tick` flicks, exact rational `FrameRate`) — an anchor stores a `Tick`, never a float or a frame count. PA-7 half-open ranges — a note anchors at a point, never a range. PA-9 typed errors — `EditError::{NoNote, NoMarker, NoteTooLong}`, never a string. PA-1 untouched: no graph or cache-key change |
| 10 | Goal-backward L1–L4, incl. GUI/MCP parity | L1 types + ops exist → L2 notes round-trip through a real `.photon` and a real export file → L3 wired into the right rail, the dispatch table and the generated docs → L4 a note written in the panel is listed by an agent, navigated to, resolved and exported; T19 pins parity |

---

## 12. Risks, open questions, deliberate exclusions

### 12.1 Risks

1. **Per-keystroke undo.** The single most likely way to get this item wrong, and
   the one an implementer following the brief's `SetEffect` pointer would reach
   naturally — because that arm looks like it covers typing and, per §2.5, cannot
   fire for it. Mitigation: §6.1's commit-on-blur gate, T3 with its negative
   control.
2. **A dangling anchor being "repaired".** The reflex is to clear an anchor whose
   target is gone. That destroys the only evidence of what the note was about,
   and it is what 35 §1.3 and `finalize_load` already forbid for the analogous
   marker-category case (`load.rs:175-177`). Mitigation: §4.4, T7.
3. **The category field becoming a second taxonomy.** Pre-K-A2 the category is
   always `None`, and the temptation is to ship a note-only colour enum "just for
   now". 26 K-C2 (`26-…:452`) is explicit that the `MarkerCategory` registry is
   the one taxonomy. Mitigation: the field is typed `Option<MarkerCategoryId>`
   from day one and there is no other colour field on `ProjectNote`.
4. **Unbounded bodies against the byte budget.** A note is the first document
   field a user can paste a transcript into. Mitigation: §9.5's caps and honest
   `mem_estimate` arms; the existing marker-note gap (§4.2) is a live example of
   what happens without them.
5. **Silent script failure.** CJK renders as tofu and RTL renders in the wrong
   order **today** (§9.3/§9.4). The risk is not the limitation, it is shipping it
   without saying so and having a user conclude their text was corrupted.
   Mitigation: the disclosure notice plus T12's byte-identical round-trip, which
   proves storage is intact even when display is not.
6. **Preferences loss on downgrade** (§4.5). Verified, latent, and made
   reachable by this item; fixed here rather than logged.

### 12.2 Open questions, each with a recommendation

- **Q1 — should notes be per-sequence rather than per-project?**
  *Recommendation: per-project, with the anchor naming the sequence.* The item is
  called project notes; a note about the delivery, the client or the grade has no
  sequence; and a per-project list is the review queue that makes the feature
  useful. Filtering by sequence is a view concern and ships in the panel header
  (§7.4). *No product sign-off needed; this is a scope call.*
- **Q2 — should a note be able to carry an attachment (a reference frame)?**
  *Recommendation: no in v1.* An attachment is media bytes inside a document,
  which reopens every question 204 §3.3 closed — rights manifests, absolute
  paths, relink — for a convenience that "extract frame to file" (K-E4, already
  shipped per `26 §19.1` Band 1) plus a filename in the body covers. Recorded in
  §12.3.
- **Q3 — should K-G2 wait for K-A2?** *Recommendation: no.* §3.5 enumerates
  exactly what K-A2 gates (category *selection*, clip-marker anchors having a
  producer, a Markers panel to cross-navigate to) and it is a minority of the
  item; the marker-creation verb 26 K-G2 names does not need K-A2 at all.
  Sequencing K-G2 behind K-A2 trades a shipped capability for a colour picker.
- **Q4 — should the notes panel show *marker* notes (`Marker.note`,
  `sequence.rs:841`) in the same list?** *Recommendation: yes, read-only, and
  only after K-A2.* Two review surfaces in one project is exactly the
  fragmentation this item exists to fix, and a marker note is a note anchored to
  a marker by construction. But it needs the K-A2 marker panel to own the *edit*
  side, or the same string becomes editable in two places with two undo paths.
  **This is the one genuine product call in the item** — whether marker notes and
  project notes are one list or two — and it should be decided before the panel's
  empty state and header copy become user-visible strings.
- **Q5 — should an unresolved-note count appear somewhere persistent (a rail
  badge)?** *Recommendation: yes, a count on the rail icon, and nothing more.*
  It is the affordance that stops notes being written and never read, it is
  derived state (no model change), and it is one number rather than a
  notification system.

### 12.3 Deliberately excluded

- **Threaded replies, mentions, assignment, due dates.** A note has an author and
  a resolved flag; anything more is a collaboration product, and Photonic has no
  identity model to hang it on.
- **A live "timecodes in the body are clickable" scan** (§3.2). The anchor is a
  typed field. Timecode *parsing* survives as an input convenience on that field.
- **Multiple anchors per note** (§3.3).
- **Note import** (§8) — identity reconciliation is X-series scope.
- **Attachments / reference frames** (Q2).
- **A `modified` timestamp.** It would change on every `SetNote`, doubling the
  diff and making "did anything change" ambiguous for the byte-identical undo
  assertions in T2. `created` plus the history tree already answers "when".
- **Burn-in, overlay, or any path from a note to a rendered pixel** (§8).
- **A GUI for `Document.annotations`** — a real gap (§2.3), and a vector-mode
  item, recorded as §14.3 rather than absorbed here.
- **Marker-category CRUD** — that is K-A2, and building half of it inside K-G2
  would put a protected-surface change and a new feature in one item, the trap
  [194 §4](194-k-a5-general-and-nested-clip-groups.md) point 4 names.

---

## 13. Clean-room provenance

Required by [26 §7](../specs/video-editor/26-kdenlive-mlt-parity.md#7-how-to-read-the-item-tables)
and [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol).

- **Sources used.** (a) Photonic's own code and specs, cited by `file:line`
  throughout; (b) 26 K-G2's one-line requirement statement, itself derived from
  Kdenlive's `CC-BY-SA-4.0` user documentation as a *requirements source*, cited
  and never pasted; (c) the general convention that a review note can point at a
  position in a timeline — a functional idea, not protectable expression;
  (d) published Unicode standards (UAX #9 bidi, UAX #29 segmentation) and the
  egui/`epaint` public issue tracker's documented absence of bidi support, both
  cited via [42](../specs/video-editor/42-localization.md).
- **Sources not used.** The Kdenlive source tree, the MLT/`mlt++` source tree,
  frei0r, and any GPL/LGPL derivative were not inspected for this item. No
  identifier, comment, constant, control flow, file layout or test case above
  derives from them. In particular **nothing here is modelled on Kdenlive's
  project-notes implementation**: the data model is derived from Photonic's own
  `Marker` (`sequence.rs:833`) and `Annotation` (`annotation.rs:16`); the command
  triple is the shape `AddMarker`/`RemoveMarker`/`SetMarker` (`commands.rs:660-676`)
  already uses; the text-commit discipline is copied from Photonic's own
  `caption_editor.rs:411-430`; the forward-compat anchor arm is copied from
  Photonic's own `ClipSource` (`clip.rs:163-196`); the dangling-reference policy
  is copied from Photonic's own `dangling_marker_categories` (`load.rs:214`).
  The implementer records the 23 §3.4 attestation for the `core-timeline` and
  `panels-video` subsystems, and an independent reviewer checks provenance before
  merge (26 §2 point 2).
- **The one place a reference *limitation* was explicitly rejected** (26 §5's
  standing warning): the reference turns timecodes into links by scanning the
  note text. That is a workaround for not having a typed model. Photonic has one
  (PA-9), so the anchor is a typed field and the text stays text (§3.2). A
  reference NLE limitation is not a requirement.
- **Photonic-ahead properties preserved.** PA-8 — an anchor is a `Tick` in
  flicks, resolved against an exact rational `FrameRate`; no float seconds and no
  frame count is ever stored. PA-7 — half-open ranges are untouched; a note
  anchors at a point. PA-9 — failures are typed `EditError` variants. PA-6 — a
  note anchors to a `SequenceId`, never to a project-wide profile, so per-sequence
  formats are unaffected. PA-1 — no graph, IR or cache-key change; notes are
  never a `ContentHash` input.
- **No dependency, no bundled asset, no codec, no patent surface.** The store is
  `serde` + `String`, the export writer is `std::fmt` + `serde_json`, and the
  script corpus is Photonic-authored text. `unicode-segmentation`/`unicode-width`
  are already direct dependencies of `photonic-core`
  (`crates/photonic-core/Cargo.toml:28`, `:32`); §9.5 uses neither for layout.
  None of ROADMAP §7's K/E/X gates apply.
- **Naming discipline:** describe the capability as "anchored project notes",
  never in terms of compatibility with, or equivalence to, another application's
  notes feature.

---

## 14. Follow-ups

Recorded here rather than edited into the owning documents, per this proposal's
one-file scope. Each needs its own change.

1. **`26-kdenlive-mlt-parity.md` K-G2 (`:640-644`) and §19.2 (`:851`).** The
   impact line says *"auto-converts timecodes in the note text into clickable
   seeks"*, which §3.2 deliberately does **not** implement; it should be restated
   as "a typed anchor, with timecode parsing as an input convenience". The
   `KA2 --> KG2` edge should be annotated **partial** with §3.5's split, so the
   graph is not read as a hard block. The **Files** line should name
   `photonic-core/src/timeline/sequence.rs`, `ops.rs`, `commands.rs`,
   `photonic-gui/src/panels/video/notes.rs` and `photonic-mcp`.
2. **K-A2's marker-export templates and K-G2's note export should converge.**
   When K-A2 lands `{{timecode}}`/`{{comment}}`/`{{frame}}` templates
   (`26-…:224`), note export should use the same engine (§8) rather than keeping
   two fixed formats. Schedule with K-A2.
3. **`Document.annotations` has no GUI and no undo** (§2.3). Two separable
   defects: `handlers/annotations.rs:25` mutates the document outside
   `CommandHistory`, violating ROADMAP §10.4 and `SPEC.md`'s absolute rule as
   quoted by 39 §1.6; and there is no vector-mode surface for a feature with
   three MCP tools. Owned by the vector side, not by K-G2.
4. **`204-k-g4-project-templates.md` §3.3's kept/cleared table** predates
   `TimelineProject.notes`. Notes must be **cleared** at template capture (§3.4);
   the table needs a row, and 204's T-series a case.
5. **Marker commands under-report `mem_estimate`.** `AddMarker`/`RemoveMarker`/
   `SetMarker` fall into `commands.rs:1655`'s `_ => 64` catch-all while carrying
   `Marker.note` (`sequence.rs:841`), an unbounded `String`. That is the fiction
   39 §1.3 names. Add explicit `json_len` arms; independent of K-G2, but K-G2's
   §4.2 is where the pattern becomes visible.
6. **39 §2.3's "newer, minor" warning is specified but not implemented.**
   `Document::from_value_with_report` (`document.rs:1717-1740`) refuses beyond
   the window and otherwise loads leniently with no "saving may lose newer-only
   data" warning. K-G2's additive field makes the consequence concrete (an older
   build silently drops a user's notes on save), but the gap is 39's, not K-G2's.
7. **`photonic-mcp` has no permitted-roots list** (§7.3). `export_project_notes`
   joins `export_captions` (`handlers/video.rs:5437`) and `save_document` in
   writing to an arbitrary path, and `SecurityPathNotPermitted`
   (`photonic-core/src/diag.rs:250`) still has no call site. Owned by
   28-security-model.
8. **No system-font fallback** (§9.4). `photonic-app/src/main.rs:503-505` loads
   the egui default set plus phosphor and nothing else, so CJK/Arabic/Devanagari
   user content renders as tofu app-wide. The useful fact for whoever takes it:
   `photonic-gui` already reaches cosmic-text's font database through
   `photonic-render`'s `glyphon` dependency
   (`crates/photonic-render/Cargo.toml:13`), so no new third-party crate is
   needed. Owned by 42 §7.
9. **`annotation::chrono_now` (`annotation.rs:45`) is private** and would become
   the second caller's dependency (§4.1). It should be `pub(crate)` in a shared
   location, or `ProjectNote` will grow a third copy of an epoch-to-ISO
   converter. Cosmetic, but the second copy is where it becomes a pattern.
10. **Overlap with the sibling K-G3 mini-spec (`206-k-g3-layout-presets.md`,
    written in the same round and not readable from here).** K-G3 serializes
    panel/drawer visibility, which means it touches the *same two enums*
    §4.5 fixes. This document's position, stated so it can be reconciled:
    **`RightDrawerGroup` and `DrawerGroup` both gain `#[serde(other)] Unknown`
    and are normalized to their defaults in `AppPreferences::load`
    (`preferences.rs:322-331`), in whichever item ships first** — the fix is
    three lines and belongs to neither item exclusively, but shipping either
    item without it costs users their whole preferences file on a downgrade.
    Whoever accepts both documents should assign it once. K-G3 additionally
    owns dock layouts, which K-G2 explicitly excludes (§12.3); K-G2 owns
    document content, which K-G3 must not persist to the config dir.
11. **`ROADMAP.md` §0 progress table and the K-G row (`:186`)** — add a K-G2 row
    when the item lands, with its commit, per the existing convention.
