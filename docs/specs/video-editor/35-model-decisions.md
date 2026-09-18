# 35 — Model Decisions: Markers, Effect Scopes, Groups

**Status:** Recommendations **applied to the owning specs 2026-07-20**; this document is retained as the decision record (rationale and rejected alternatives). No code authorization.
**Date:** 2026-07-20
**Audience:** data-model owner, engine maintainers, timeline panel owner

**Depends on:** [01-data-model.md](01-data-model.md), [02-engine.md](02-engine.md), [26-kdenlive-mlt-parity.md](26-kdenlive-mlt-parity.md), [30-effect-catalogue.md](30-effect-catalogue.md).

**Owns:** the model decisions behind [26 K-A2](26-kdenlive-mlt-parity.md#k-a2--marker-system-depth) (markers), [26 K-B1](26-kdenlive-mlt-parity.md#k-b1--track-and-master-effect-stacks)/[K-B2](26-kdenlive-mlt-parity.md#k-b2--asset-level-bin-effects) (effect scopes), and [26 K-A5](26-kdenlive-mlt-parity.md#k-a5--general-and-nested-clip-groups) (groups).

**Where each decision now lives.** §1 markers → [01 §2](01-data-model.md), [01 §4.1](01-data-model.md), [01 §10](01-data-model.md) · §2 effect scopes → [01 §3](01-data-model.md)/[§4](01-data-model.md) (fields) and [02 §2](02-engine.md) (normative pipeline order) · §3 groups → [01 §4.2](01-data-model.md), [01 §5](01-data-model.md), [01 §10](01-data-model.md). This document keeps the *why*; those keep the *what*.

**Why these three.** Each is a **data-model change with a migration**, each has a decision that is cheap to make now and expensive to discover later, and each has a reference implementation whose choice I think is wrong. This document makes the calls and says why; it is a decision record, not a full contract.

---

## 1. Markers

### 1.1 State and questions

`Marker { id, at, name, color: Option<Color>, note }` lives on `Sequence.markers` only. No categories, no duration, no clip-level markers, no panel. Five decisions.

### 1.2 One marker type, with a scope

**Recommend:** a single type, anchored either to a sequence or to a clip.

```rust
pub struct Marker {
    pub id: MarkerId,
    pub at: Tick,                              // sequence-relative, or clip-relative when clip-scoped
    pub duration: Tick,                        // §1.4 — 0 = point marker
    pub name: String,
    pub note: String,
    pub category: Option<MarkerCategoryId>,    // §1.3
    pub color: Option<Color>,                  // per-marker override of the category colour
    pub anchor: MarkerAnchor,                  // §1.5
}
```

Sequence markers stay on `Sequence.markers`; clip markers go on `Clip.markers`. Scope is therefore implied by **location**, not carried as a field — no invalid states to validate.

**Why.** Kdenlive ships two concepts with two UIs (timeline "guides" and clip "markers") and then renamed guides to *Timeline Markers*, which is an admission they are the same thing. The data is identical and every consumer — panel, search, category filter, snapping, export, chapters — wants to treat them uniformly. Two types means writing all of that twice.

### 1.3 Categories keyed by stable id, not index

**Recommend:** a project-level registry, referenced by UUID.

```rust
pub struct MarkerCategory { pub id: MarkerCategoryId, pub name: String, pub color: Color }
// TimelineProject.marker_categories: Vec<MarkerCategory>   (ordered for display)
```

**Why not the reference's model.** Kdenlive stores a marker's category as an **index** into a project array. Deleting or reordering categories silently changes the meaning of every marker below the edit — which is precisely why it needs a remap-and-prompt pass on delete. A stable id makes reordering free and deletion honest.

**Deletion policy:** removing a category is an undoable op taking an explicit disposition — reassign to another category, or clear to uncategorised. A marker referencing a **missing** category renders with a neutral fallback and is flagged in the panel; it is never silently remapped, because a silent remap changes what a marker means.

**Categories are optional.** Seed a small default set lazily on first use; do not force categorisation on a user who just wants a note.

### 1.4 Ranged markers via `duration: Tick`, defaulting to 0

**Recommend:** every marker is a range; a point marker is a range of length zero.

**Why not `Option<Tick>`.** Optionality forces a branch in hit-testing, snapping, painting, export and chapter generation — six places that would each need a point/range special case. With `duration: Tick`, `[at, at + duration]` is the only expression anyone writes, and `duration == 0` degenerates correctly everywhere. The UI shows a range handle only when duration > 0.

This subsumes regions, chapters and export ranges into one concept, which is what makes [26 K-F2](26-kdenlive-mlt-parity.md#k-f2--marker-zone-and-per-segment-multi-export)'s per-segment multi-export cheap.

### 1.5 Anchoring per marker, not a global ripple toggle

**Recommend:**

```rust
pub enum MarkerAnchor { Timecode, Content }
```

- `Timecode` — stays where it is when the timeline ripples. Chapter marks, delivery-spec beats, "ad break here".
- `Content` — moves with the material. "Fix the focus on this shot", "bad take".

Defaults: **clip-scoped markers are always `Content`** (they travel with the clip by construction and propagate to copies of it); **sequence markers default to `Timecode`**.

**Why not Shotcut's toggle.** Shotcut has a global *Ripple markers* mode (`Alt+R`), and it correctly identifies the underlying problem — markers are sometimes anchored to content and sometimes to timecode, and only the user knows which. But a global mode makes the outcome of an undoable edit depend on hidden state the user set earlier, which is a bad property for an operation that has to be described in an undo label. Anchoring is a property of what the marker *means*, so it belongs on the marker.

Offer the global toggle as a **bulk action** ("set all sequence markers to…"), not a mode.

### 1.6 Migration and integration

Additive; a no-op for existing data. Part of the consolidated v4→v5 step ([01 §9.1](01-data-model.md#91-the-v4--v5-migration--one-step-nine-changes)). `color` is retained and reinterpreted as an override of the category colour, so existing coloured markers keep looking the same.

- **Snapping:** markers become snap candidates, closing part of [26 K-A4](26-kdenlive-mlt-parity.md#k-a4--snap-target-completeness). A ranged marker contributes **two** candidates, start and end.
- **Clip markers must survive `duplicate_with_fresh_ids`** with remapped ids, like every other id-bearing structure.
- **Export templates** (`{{timecode}}`, `{{comment}}`, `{{frame}}`) and chapter export read the same structure; chapters use ranges directly.

---

## 2. Effect scopes and the adjustment-clip interaction

### 2.1 The question

Adding track, master and asset effect stacks means deciding where each runs relative to the others — and specifically, what happens when a track carries an **adjustment clip**, whose whole purpose is to modify the tracks *below* it.

Today `compile.rs:358-360` handles adjustment clips inline in the track-fold loop.

### 2.2 The invariant that settles it

`Track.clips` is **sorted and non-overlapping**, enforced by `Sequence::validate()`. Therefore **at any tick, a track has at most one clip**. There is no "a track with both content and an adjustment at time t" case to disambiguate — it cannot exist.

So at tick `t`, each track is in exactly one of three states: empty · carrying a **content** clip · carrying an **adjustment** clip.

### 2.3 Recommended pipeline order

```
per content clip:
    asset effects      (K-B2 — inherited by every instance)
  → source op          (decode / raster-vector / nested, at mapped source time)
  → clip transform     (AnimProps + reframe for the active format)
  → clip effects
  → clip grade

per track:
    the track's covering content clip (plus transition partner)
  → track effects                                   ← applies to this track's own content only
  → track grade

cross-track, bottom → top:
    acc = Merge(acc, track_result, track.blend, track.opacity)
    if this track's covering clip is an Adjustment:
        acc = adjustment.grade(adjustment.effects(acc))    ← operates on everything below

master:
    → master effects → master grade
    → CaptionOverlay
    → project graph
    → Output
```

### 2.4 The four calls, and why

**(a) Track effects apply to the track's own content, never to the accumulator.**
This is the disambiguation. If you want to affect lower tracks, that is what an adjustment clip is for — a second mechanism for the same thing would make "what does this stack apply to?" unanswerable by looking at the UI. A track whose covering clip is an adjustment has **no own content**, so its track effects simply do not apply at that tick.

**(b) Adjustment operators run after their own track merges.**
So an adjustment affects everything below it *and* nothing above it, which is the universal convention and what the existing implementation already does.

**(c) Asset effects sit beneath clip effects.**
A per-camera LUT or lens correction belongs to the *material* and should be inherited by every instance, with per-instance work stacking on top. Putting them above clip effects would make a clip-level grade unable to correct the asset-level one.

**(d) Master effects run *before* `CaptionOverlay`.**
Captions are authored in final display colour and must not be re-graded by a master LUT — this is standard broadcast practice (subtitles are burned after grade) and it is what a user expects when they add a master look and their subtitles do not shift colour. This also preserves the existing compile order, where `CaptionOverlay` is step 5.

Master effects run **before** the project graph, so the node graph remains the final-look surface ([08](08-fusion-node-flows.md)) — the same relationship the audio master bus has to the output.

### 2.5 Applicability is enforced, not advisory

[30 §2.3](30-effect-catalogue.md#23-capability-and-applicability)'s `Applicability` flags become load-bearing here: an effect that assumes clip bounds is meaningless at master scope, and the manifest must be able to say so. Offering every effect at every scope and letting users discover which ones misbehave is the failure mode to avoid.

### 2.6 Migration

All additive, all defaults preserving current output:

| Type | Adds | Default |
|---|---|---|
| `Track` | `effects: Vec<ClipEffect>`, `grade: Option<Grade>`, `blend: BlendMode`, `opacity: f32` | empty, `None`, `Normal`, `1.0` |
| `Sequence` | `master_effects`, `master_grade` | empty, `None` |
| `MediaAsset` | `effects`, `grade` | empty, `None` |

`Track.blend`/`opacity` also close [26 K-A9](26-kdenlive-mlt-parity.md#k-a9--track-compositing-control). They are **not** gated on the unresolved canvas colour question — see §5 — so model and renderer ship together.

No cache-key change: every new stage is an IR node and participates in the content hash automatically.

---

## 3. Groups

### 3.1 State and questions

Photonic has A/V link groups only: `Clip.link_group: Option<LinkGroupId>`, with `clips_in_link_group` resolving membership by scanning every clip in every sequence. Four decisions.

### 3.2 A parent-pointer tree on the sequence

**Recommend:**

```rust
pub struct GroupNode { pub id: GroupId, pub kind: GroupKind, pub parent: Option<GroupId> }
pub enum GroupKind { Normal, AvLink }
// Sequence.groups: HashMap<GroupId, GroupNode>
// Clip.group: Option<GroupId>            — immediate parent
```

**Invariants** (enforced in `Sequence::validate()`, alongside the existing sorted/non-overlapping checks):
- the parent chain terminates — no cycles;
- every referenced `GroupId` exists in the same sequence;
- no empty groups — a group losing its last member dissolves;
- no single-member `Normal` groups — they dissolve too, since a group of one is just a clip.

**Why parent pointers.** "Topmost group of this clip" is a short walk up; membership is a filter; nesting is explicit. The current `link_group` design cannot express nesting at all and answers membership by scanning the whole project.

### 3.3 Subsume A/V linking into the tree

**Recommend:** `link_group` becomes an `AvLink` group. Retain the field deprecated for one format version, populated by projection so older builds still load — the same transition pattern [30 §10](30-effect-catalogue.md#10-compatibility) uses for `EffectKind`.

**Why.** Two mechanisms for "these clips move together" is one too many, and today's is the weaker one. `link_clips` / `unlink_clip` / `clips_in_link_group` become thin wrappers over group ops.

**Constraint:** linked A/V is a [ROADMAP §9](ROADMAP.md#9-protected-surfaces) protected surface. The migration must be behaviour-preserving and covered by the existing link tests before the field is removed — this is a refactor, not a redesign, and it must be visible as one.

### 3.4 Selection is not a group

**Recommend:** selection stays session state in `photonic-gui`; groups are document state.

**Why.** Kdenlive models transient selection as a group *type* in the same tree. That means every selection change is a document mutation — which either pollutes the undo stack or requires a carve-out from [SPEC.md](SPEC.md)'s "every document mutation is undoable, without exception" rule. Photonic already has one such carve-out under debate ([27 A-8](27-spec-audit.md#a-8--p1--track-height-is-not-undoable-against-an-absolute-constraint), track height); it should not acquire a second, larger one.

### 3.5 Operation semantics

| Op | Behaviour |
|---|---|
| **Move** | Moving any member moves the **topmost** group, preserving relative offsets and track deltas. Validate all members first; if any would land illegally, the whole move is refused — atomic, matching the existing validate-then-commit discipline in `ops.rs` |
| **Trim** | Trims **only the trimmed clip** by default — trimming "a group's edge" is ambiguous when members have different bounds. **Exception:** `AvLink` groups propagate trims to linked partners, which is today's behaviour and must not regress |
| **Split** | Splits **every member covering the split tick**; the two halves form two groups mirroring the original structure |
| **Delete** | Removes all members; empty groups dissolve |
| **Isolate** | `Alt`+click selects one member for independent editing — **session state**, per §3.4 |
| **Group / Ungroup** | Group promotes each selected item to its topmost existing group before nesting, so grouping never produces a partial overlap of two groups |

One user verb is one undo unit, including the fanned-out edits.

### 3.6 Interaction with sync-lock

Groups (clip-level, "these clips move together") and sync-lock (track-level, "these tracks ripple together") are orthogonal but both affect ripple, so precedence must be stated rather than discovered:

> **Group membership binds first** — the group moves as a unit and computes one shift. **Sync-lock then propagates that shift** to other sync-locked tracks.

Note that sync-lock is currently **inert** ([26 K-0.9](26-kdenlive-mlt-parity.md#8-k-0--foundations)); this rule is what it should implement when it is wired, and it should be written into that work rather than invented then.

---

## 4. Summary of recommendations

| # | Decision | Departs from the reference? |
|---|---|---|
| 1.2 | One marker type, scope implied by location | **Yes** — Kdenlive has two |
| 1.3 | Categories by stable id, not index; explicit deletion disposition | **Yes** — and it removes a whole failure mode |
| 1.4 | `duration: Tick` defaulting to 0, not `Option` | Refines Shotcut's ranged markers |
| 1.5 | Per-marker anchoring, defaults by scope | **Yes** — Shotcut uses a global mode |
| 2.4a | Track effects apply to the track's own content only | New — the reference has no track/adjustment interaction to resolve |
| 2.4d | Master effects before `CaptionOverlay` | Follows broadcast practice |
| 3.2 | Parent-pointer group tree with dissolve invariants | Follows Kdenlive's tree, with tighter invariants |
| 3.3 | A/V link subsumed as a group kind | **Yes** — removes a duplicate mechanism |
| 3.4 | Selection is not a group | **Yes** — Kdenlive conflates them |
| 3.6 | Groups bind before sync-lock propagates | New — currently unspecified in both |

**Sequencing.** §1 markers is independent and can start immediately; it unblocks [26 K-F2](26-kdenlive-mlt-parity.md#k-f2--marker-zone-and-per-segment-multi-export). §2 effect scopes should land with [30](30-effect-catalogue.md)'s manifest, since applicability (§2.5) is what makes it safe. §3 groups is independent of both, but its A/V-link migration should not run concurrently with other edits to `ops.rs` trim paths.

## 5. Resolved: `Track.blend`/`opacity` is not gated on the colour question

An earlier draft of §2.6 said these fields ship inert, gated on [26 K-0.3](26-kdenlive-mlt-parity.md#8-k-0--foundations), which was in turn gated on [27 A-1](27-spec-audit.md#a-1--p0--the-live-canvas-composites-in-gamma-headless-composites-in-linear). **The middle link was wrong**, and removing it unblocks the chain:

- **A-1** is a defect in the **vector/raster canvas** compositor. `COMPOSITE_SHADER` is correct only when its render target is sRGB, so the hardware decodes on sample and encodes on write. Headless pins `Rgba8UnormSrgb`; the live canvas selects a **non-sRGB** swapchain (because egui shares that surface) and therefore blends gamma-encoded values. Resolution: render the document **offscreen** at the headless format and present it into the egui surface — canvas and headless then agree by construction, which is what the video path already does with `EngineFrame`.
- **K-0.3** is the **video graph's** `Merge`, and the video graph is `Rgba16Float` **linear premultiplied** throughout (D-09). There is no format ambiguity, and the CPU reference already fixes the maths: `ops.rs::merge_pixel` unpremultiplies, applies `blend_rgb`, backdrop-blends, then does premultiplied source-over. K-0.3 is *port that to WGSL*.

Two different compositors, two different colour states, two independent defects. **K-0.3 is unblocked today**, so `Track.blend`/`opacity` ship live rather than inert, and [26 K-A9](26-kdenlive-mlt-parity.md#k-a9--track-compositing-control) track compositing lands with them.

The one thing both share is a **product question worth answering once and writing down**: W3C blend functions are defined on transfer-encoded values, and Photonic blends in **linear** — deliberately, per issue #145. That makes `Multiply` and friends differ from Photoshop/CSS. Recommend keeping linear (it is physically correct, it is already the canvas's documented intent, it avoids two transfer evaluations per merge on the hottest path in the compositor, and it makes CPU and GPU agree trivially) and **stating it in [03](03-render-color-pipeline.md)** as a product position rather than leaving it to be rediscovered per surface — which is exactly how A-1 and [27 A-3](27-spec-audit.md#a-3--p0--grade-operators-apply-transfer-functions-to-premultiplied-alpha) both happened.
