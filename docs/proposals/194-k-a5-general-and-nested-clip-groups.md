# 194 — K-A5 General and Nested Clip Groups (mini-spec)

## Status: Draft mini-spec — pre-code gate

This document exists to satisfy [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md)'s
K-Band 5 exit condition: *"an accepted mini-spec exists before code, naming its
data-model change, migration, undo unit, MCP surface and acceptance fixtures."*
It carries **no code authorization** ([26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md)
point 5); acceptance of this document is what authorizes K-A5.

Owner refs: [26 K-A5](../specs/video-editor/26-kdenlive-mlt-parity.md#k-a5--general-and-nested-clip-groups)
(the requirement + ranking), [35 §3](../specs/video-editor/35-model-decisions.md#3-groups)
(the model decisions, already landed), [01 §4.2](../specs/video-editor/01-data-model.md)
(the normative model text). Where this document and 35 §3 disagree, the
disagreement is called out explicitly in [§8](#8-risks-open-questions-and-exclusions)
and [Follow-ups](#follow-ups) rather than silently resolved.

---

## 1. Problem and user outcome

**Today.** A user can select several timeline clips (`timeline_selection: Vec<ClipId>`,
marquee at `app/timeline/interact.rs:502`) and can *link* them — but link is a flat,
project-wide A/V-partner relation that only expands **move** and **plain delete**
(`ops_bridge.rs:314-336` states this scope in prose). There is no way to say
"these five clips are one editorial unit", no nesting, no way to grab the unit
and drag it, and no group verb at all in the GUI timeline or over MCP. The
`Ctrl+G` a user presses in video mode today runs the **vector** grouping path
(`tool_handlers.rs:96` `handle_global_shortcuts` → `:192` `do_group_selected`,
which is not mode-gated).

**After K-A5.** A user can:

1. Select any set of clips across video and audio tracks and press **Ctrl+G** to
   make them one group; **Ctrl+Shift+G** peels the outermost group back off.
2. Group a group with another clip — groups **nest**, and clicking any member
   selects the whole topmost group.
3. **Alt+click** a member to isolate it and edit it alone without ungrouping.
4. Drag / arrow-nudge / `move_clip` any member and have **every** member move by
   the same tick delta (and the same track-index delta within its own kind lane),
   as **one** undo step, or have the move refused whole if any member cannot land.
5. Delete a group, or split a group at the playhead, as one undo step each.
6. Drive all of the above from an agent (`group_clips` / `ungroup_clips` /
   `list_groups`, plus group-aware `move_clip` / `remove_clip`), with GUI↔MCP
   structural parity proven by the acceptance-story harness.

**Not** in the outcome: an effect edit propagated to all members with a
member-count badge (26 K-A5's last clause) — see [§8](#8-risks-open-questions-and-exclusions).

---

## 2. Current state in code (exact)

### 2.1 What already exists — the container is done

The 35 §3 group model **landed**. It is not the gap.

| Thing | Where |
|---|---|
| `Sequence.groups: HashMap<GroupId, GroupNode>` | `photonic-core/src/timeline/sequence.rs:147` |
| `GroupNode { id, kind, parent }` (parent-pointer forest) | `sequence.rs:898` |
| `GroupKind { Normal, AvLink, Unknown }` (`#[serde(other)]`, `#[non_exhaustive]`) | `sequence.rs:928` |
| `Clip.group: Option<GroupId>` — immediate parent | `photonic-core/src/timeline/clip.rs:76` |
| `group_chain` (leaf→root, cycle-guarded) | `sequence.rs:313` |
| `topmost_group` | `sequence.rs:331` |
| `group_members` (transitive), `direct_group_members`, `child_groups` | `sequence.rs:337`, `:352`, `:365` |
| `validate_groups` — no cycles, no dangling ids, no empty groups, no singleton `Normal` | `sequence.rs:433`; errors at `sequence.rs:546-560` |
| `dissolve_degenerate_groups` — bottom-up dissolve + reparent, returns dissolved ids | `sequence.rs:485` |
| Load-time repair + `LoadReport.dissolved_groups` | `timeline/load.rs:138`, `:154-165` |
| Duplicate-sequence group remap (fresh ids per copy) | `sequence.rs:252-286` |
| v4→v5 `link_group` → `GroupKind::AvLink` projection | `migration.rs:212` |
| Coverage: dissolve/cycle/reparent unit tests, migration integration test | `sequence.rs:1272-1400`, `load.rs:1090-1140`, `tests/scope_migration.rs:63-135` |

### 2.2 `GroupId` and `LinkGroupId` are two different things — do not conflate

| | `GroupId` (`Clip.group`) | `LinkGroupId` (`Clip.link_group`) |
|---|---|---|
| Shape | **Tree**, per sequence, parent pointers (`sequence.rs:898`) | **Flat tag**, resolved by a **whole-project scan** (`ops.rs:1282`) |
| Scope | Within one `Sequence` (`validate_groups` rejects a foreign id, `sequence.rs:439`) | Project-wide by construction |
| Meaning | Editorial grouping, arbitrary membership, nests | "These are the A/V halves of one import" |
| Status | Live model, **no ops, no GUI, no MCP** | Live model **and** ops (`ops.rs:1194` `link_clips`, `:1223` `unlink_clip`, `:1243` `split_av_link`), GUI menu (`app/timeline/mod.rs:2165`, `:2219`, `:2257`), MCP (`link_clips` / `unlink_clips`, `handlers/video.rs:1742`, `:1774`) |
| Disposition | The mechanism K-A5 builds on | **Deprecated for one format version** (35 §3.3; `01 §4.2` marks the field `#[deprecated]`). Still the *only* thing wired |

`GroupKind::AvLink` is the bridge: the v5 migration already mints one `AvLink`
`GroupNode` per distinct `link_group` and points both clips at it
(`migration.rs:212-268`), while **retaining** `link_group` so an older build still
loads. So a v4-authored A/V pair opened today has *both* a `link_group` and an
`AvLink` group — and only the `link_group` half does anything.

**Rule for this item:** K-A5 must not conflate them and must not remove
`link_group`. Linked A/V is a [ROADMAP §9](../specs/video-editor/ROADMAP.md#9-protected-surfaces)
protected surface; every existing link test must still pass unchanged. Field
removal is a separate, later v6 item ([§4](#4-migration)).

### 2.3 What is genuinely missing

1. **No group edit ops.** `grep "fn group" ops.rs` → nothing. `ops.rs` has no
   group/ungroup, and no group-aware move/trim/split/delete. Nothing in the
   product ever writes `Clip.group` except the v4→v5 migration.
2. **No command can mutate `Sequence.groups`.** `TimelineCmd` (`commands.rs:394`,
   `#[serde(tag = "cmd")]`) has no group arm; `grep -i group commands.rs` returns
   one unrelated comment. Group membership *can* ride `SetClipProp`
   (`commands.rs:568`, whole-clip old/new) but the `GroupNode` itself cannot be
   created or removed by any command, so group/ungroup is currently unexpressible
   as an undoable edit.
3. **No GUI surface.** `Ctrl+G`/`Ctrl+Shift+G` are bound to the vector
   `object.group` / `object.ungroup` (`commands.rs:263-271`) and dispatched
   unconditionally (`tool_handlers.rs:96`+`:192`) — **not** mode-gated. The
   timeline drag commits a single clip (`app/timeline/mod.rs:1803` `commit_drag`
   → `:1825`/`:1835`), expanded only across `link_group`.
4. **No MCP surface.** No group tool; `list_clips` (`handlers/video.rs:1899-1920`)
   omits group membership. `get_clip` (`:1926`) serializes the whole `Clip`, so
   `group` is already visible there for free.
5. **No lock story.** No core op consults `Track.locked`. Only
   `expand_sync_lock_ripple` filters it (`ops.rs:888`, `!t.locked`); the GUI
   enforces it at hit-test time (`interact.rs:104`) and some MCP handlers check it
   ad hoc (`handlers/video.rs:1498`).
6. **Three latent defects the group model turns on** — see
   [§8.1](#81-risks). Notably paste keeps `Clip.group` verbatim
   (`command_center.rs:1066-1067`), which after a cross-sequence copy produces a
   `ValidationError::UnknownGroup` state.

### 2.4 The invariant that constrains every design choice below

`TimelineCmd::apply` **debug-asserts `Sequence::validate()` after every single
command** (`commands.rs:1748-1757`), and `Command::Batch` applies its elements one
by one (inverse is the reversed batch of inverses, `history/mod.rs:3173-3175`).
Therefore **every intermediate state inside a batch must be valid, in both
directions.** Consequences, all load-bearing:

- Removing 2 of 3 members as separate `RemoveClip`s transiently leaves a
  singleton `Normal` group → `SingletonNormalGroup` → debug panic.
- Moving group members one `MoveClip` at a time transiently overlaps neighbours
  on a shared track → `OverlapOrUnsorted` → debug panic. (It also fails
  *planning*: `ops::move_clip`'s `overlaps_other` check at `ops.rs:84` sees the
  other members still at their old positions and returns `Err(Overlap)`.)
- Restoring clips before restoring their `GroupNode` on undo → `UnknownGroup`.

So group fan-out that changes membership counts or shared-track geometry **must
be one command, not a batch of per-clip commands**. This is the single most
important finding in this document, and it is why [§3](#3-data-model-change)
adds command variants rather than reusing `MoveClip`/`RemoveClip`.

---

## 3. Data-model change

**Persisted document model: none.** `Sequence.groups`, `GroupNode`, `GroupKind`
and `Clip.group` are already exactly what 35 §3.2 and 01 §4.2 specify. No field
is added, removed or retyped. `Clip.link_group` stays (35 §3.3's one-version
deprecation window).

**Command model: three additions** (persisted only inside `photon_history`, see
[§4](#4-migration)).

```rust
// commands.rs — one shared payload so group/ungroup, dissolve-on-delete and
// split-mirroring all invert by a mechanical swap.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupTreeDelta {
    /// Nodes this delta creates. The inverse removes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<GroupNode>,
    /// Nodes this delta removes, captured whole. The inverse re-inserts them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<GroupNode>,
    /// Surviving nodes whose parent changes: (group, old_parent, new_parent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reparents: Vec<(GroupId, Option<GroupId>, Option<GroupId>)>,
    /// Clip rebinding: (track, clip, old_group, new_group).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rebinds: Vec<(TrackId, ClipId, Option<GroupId>, Option<GroupId>)>,
}
```

Apply order **inside** the command: `added` → `reparents` + `rebinds` →
`removed`. `invert()` swaps `added`/`removed` and swaps each pair, keeping the
same order — so nodes always exist before anything points at them, in both
directions. That is what keeps `validate()` true at every command boundary.

```rust
pub enum TimelineCmd {
    // …existing arms…

    /// 35 §3: one atomic edit to a sequence's group forest. Group / ungroup.
    SetGroupTree { seq: SequenceId, tree: GroupTreeDelta },

    /// A group move: N clips repositioned and/or re-tracked in ONE pass
    /// (detach all → re-attach all → sort), so no intermediate state violates
    /// the non-overlap invariant (§2.4). `to == from` for an in-track move.
    MoveClips {
        seq: SequenceId,
        moves: Vec<(TrackId /*from*/, TrackId /*to*/, ClipId, Tick /*old*/, Tick /*new*/)>,
    },

    /// A group delete: N clips lifted in one pass, with the group nodes their
    /// removal dissolves captured in the same command.
    RemoveClips {
        seq: SequenceId,
        clips: Vec<(TrackId, Box<Clip>)>,
        #[serde(default)]
        tree: GroupTreeDelta,
    },
}
```

Inverses: `SetGroupTree` → `tree.invert()`. `MoveClips` → swap `from`/`to` and
`old`/`new` per entry. `RemoveClips` → re-insert every clip at its captured
`(track, start)` (the `Clip` carries its own timing and its own `group`) and
apply `tree.invert()`.

Also add, in the same three places every variant already appears:
`mem_estimate` arms (`commands.rs:1646`, `json_len` sum), `label` arms
(`commands.rs:1698` — "Group clips" / "Move clips" / "Delete clips"), and
`EditError::TrackLocked(TrackId)` (`ops.rs:34`). `map_edit_error`
(`handlers/video.rs:250`) already has an `other =>` catch-all so the new error is
non-breaking there; it still gets an explicit arm whose text matches the existing
locked-track wording at `handlers/video.rs:1499`.

**Why not reuse existing variants.** `SetClipProp` can carry a membership change,
but it costs **two full `Clip` serializations per member** in `mem_estimate`
(`commands.rs:1646`) — a nine-clip group move would push a payload two orders of
magnitude larger than the timing delta it represents, against a byte-budgeted
history (01 §10.0). `MoveClip`/`RemoveClip` cannot be batched per member without
breaking §2.4.

**Rendering is unaffected.** `grep '\.group' crates/photonic-{video,render}/src`
is clean: groups are edit-time metadata, never graph inputs, so there is **no
`ContentHash` / cache-key change** and PA-1 is untouched.

---

## 4. Migration

**No `v6`. This lands additively inside `v5`.** `CURRENT_FORMAT_VERSION = 5`
(`document.rs:117`) stands.

Reasoning, point by point:

1. **The document model does not change** ([§3](#3-data-model-change)). Every
   group field already ships in v5; the v4→v5 step already populated the tree
   from `link_group` (`migration.rs:212`). A v5 file written before K-A5 and one
   written after are the same schema.
2. **New `TimelineCmd` variants are not a document-format change.** They appear
   only in the sibling `photon_history` key. `load_photon` restores history
   **best-effort**: a payload that fails to deserialize yields `None` history
   while the document still opens (`photon_file.rs:63-100`). So an older build
   opening a newer file drops undo history rather than failing — the existing,
   accepted degradation, not a format break.
3. **`GroupKind::Unknown` already handles the forward direction**
   (`sequence.rs:928`, `#[serde(other)]` + `#[non_exhaustive]`), and
   `dissolve_degenerate_groups` deliberately never dissolves `Unknown` or
   `AvLink` (`sequence.rs:491`). A future group kind therefore survives a
   round-trip through this build.
4. **What *would* force a v6 is explicitly out of scope**: deleting
   `Clip.link_group`. 35 §3.3 grants it one format version; removing it is a
   separate item that also has to re-point `ops::link_clips`/`unlink_clip`/
   `clips_in_link_group` (`ops.rs:1194-1290`) and both link-expansion mirrors
   (`ops_bridge.rs:345`, `handlers/video.rs:154`) at the tree. Doing that inside
   K-A5 would put a protected-surface refactor and a new feature in one change.

**Round-trip obligation.** Serde-additive check: a v5 document containing a
nested `Normal` group must survive `to_json` → `from_json` → `finalize_load`
byte-identically, and a v4 document with a `link_group` must still produce the
`AvLink` tree that `tests/scope_migration.rs:67` already asserts, **and** that
tree must now move as a unit through the new group path.

---

## 5. Undo unit

Repo rule: one user verb = one undo unit, "including fanned-out edits… an
operation that cannot be undone atomically must not commit partially"
(01 §10.0; 39 §1). Every row below is **one** history step.

| User verb | Command(s) | Exact inverse |
|---|---|---|
| **Group** (Ctrl+G / `group_clips`) | one `SetGroupTree` — `added: [new node]`, `reparents` for each promoted root group, `rebinds` for each ungrouped clip | remove the node, restore each prior parent / `None` membership |
| **Ungroup** (Ctrl+Shift+G / `ungroup_clips`) | one `SetGroupTree` — `removed: [topmost node]`, `reparents` for its child groups → its parent, `rebinds` for its direct clips → its parent | re-insert the node, restore parents and membership |
| **Move a group** (drag, K-A7 nudge, `move_clip`) | one `MoveClips` (or plain `MoveClip` when the resolved member set is a single clip — preserving today's undo shape exactly as `ops_bridge::commit_group` already does at `ops_bridge.rs:430`) | swap `from`/`to` and `old`/`new` per entry |
| **Delete a group** (Delete / `remove_clip`) | one `RemoveClips` carrying the dissolve in `tree` (or plain `RemoveClip` for a lone clip) | re-insert every clip at its captured position + `tree.invert()` |
| **Split a group** (razor / `split_clip`) | one `Command::Batch([SplitClip × n, SetGroupTree])` | reversed batch of inverses — legal here because every intermediate is valid (see below) |
| **Ripple that touches a group** (`ripple_trim`, `ripple_delete`, `insert_edit`, `extract_edit`, `close_gap`) | the existing batch **plus** one extra `RippleEdit` per track carrying un-shifted members | existing `RippleEdit` inversion (`commands.rs:902`) |

**Why split may be a batch when the others may not.** Split's intermediates are
valid in both directions: `SplitClip` clones the left half, so each right half
*inherits* `group` (`commands.rs:1960-1985`) — the group only ever **grows**
during the split pass, and a growing group can violate nothing. The trailing
`SetGroupTree` then moves the right halves into the mirror group; on undo it runs
first, putting them back into the original group (still valid) before the merges.
Move and delete have no such ordering, per §2.4.

**Coalescing.** All group commands commit through `execute_discrete`
(`history/stacks.rs:403`) — the same call the existing link path uses
(`app/timeline/mod.rs:2214`, `handlers/video.rs:1767`) — so a group edit never
folds into an adjacent drag gesture. A *continuous* group drag still coalesces
into one step by the existing gesture rules (01 §10.0), because it commits once
on release (`commit_drag`, `app/timeline/mod.rs:1803`).

**Atomicity.** Every op validates **all** members before returning a command
(35 §3.5's validate-then-commit). One illegal landing → `Err`, no command, no
document change. This is already how `ops.rs` works; groups do not get an
exception.

---

## 5a. Edit semantics (the actual content of K-A5)

### 5a.1 Target resolution and partial selection

For any verb, the **target set** is computed once:

1. For each selected clip, if `clip.group` is `Some(g)`, expand to
   `seq.group_members(seq.topmost_group(g))` (`sequence.rs:331`, `:337`);
   otherwise the clip alone.
2. Union across the selection, de-duplicated by `ClipId`. Two selected clips in
   different groups therefore edit as two whole groups, one step.
3. **Isolation overrides expansion**: if the clip was selected with the isolate
   modifier (Alt, GUI) or the verb carries `isolate: true` (MCP), step 1 is
   skipped for that clip.

Nested traversal is always to the **topmost** group (35 §3.5). There is no
"select the middle group" verb in v1; Alt+click isolates a single clip, and
ungroup peels exactly one level.

### 5a.2 Move

- Tick delta δ is identical for every member.
- Track delta is applied **within the member's own kind lane**: video members
  shift by the same number of video-track indices, audio members by the same
  number of audio-track indices. A member with no destination track of its kind,
  or that would overlap on its destination, **refuses the whole move**
  (`EditError::Overlap` / `NoTrack`). Rationale: a group that half-moves is not a
  group; 35 §3.5 already mandates atomic refusal.
- Occupancy is computed with **all moving members excluded** — the reason
  `ops::move_clip`'s per-clip `overlaps_other` (`ops.rs:84`) cannot be reused.

### 5a.3 Trim — **only the trimmed clip**

Trim, ripple-trim, roll, slip and slide **never** fan out to other group members,
including `AvLink` members. Trimming "the group's edge" is ambiguous the moment
members have different bounds (35 §3.5's own argument), and this preserves
today's behaviour verbatim: `ops_bridge.rs:314-336` and `handlers/video.rs:140-149`
both state "trim is independent, move is linked", and the protected link tests
assert it.

> **This contradicts 35 §3.5's parenthetical** that `AvLink` trim propagation "is
> today's behaviour and must not regress". It is *not* today's behaviour. See
> [Follow-ups](#follow-ups).

### 5a.4 Split

Every member covering the split tick splits. Right halves inherit `group` from
the clone (`commands.rs:1972`). Then, per group in the affected subtree: if **≥2**
right halves came out of it, they are rebound into a mirrored new group with the
same `kind` and mirrored parent structure; if exactly **one** did, that right half
is rebound to the original group's parent (`None` at a root). The single-right-half
rule is forced by the no-singleton-`Normal` invariant (`sequence.rs:472`) — a
mirror group of one would be rejected by `validate` and dissolved by
`dissolve_degenerate_groups`.

### 5a.5 Delete

All members are removed in one `RemoveClips`. Groups left empty are dissolved in
the **same** command via `tree` (mirroring `dissolve_degenerate_groups`'s reparent
rule, `sequence.rs:506-522`), so no intermediate degenerate state exists.

**Gap-closing does not fan out.** A ripple-delete of a group removes every member
but only ripples the **primary** clip's own track (plus sync-locked siblings, see
§5a.7). Closing a gap on each member's track would require inventing multi-track
ripple semantics with staggered members; that is deliberately not invented here.

### 5a.6 Locked tracks — the rule

Track lock means "this track's content is not edited". A group can legally *span*
a locked track (a track can be locked after the group is formed), so the rule is
about verbs, not about membership:

| Verb on a group with a member on a locked track | Behaviour |
|---|---|
| Move / delete / split / ripple fan-out | **Refused whole**, `EditError::TrackLocked(track)`. No partial commit. |
| Group (create) | **Refused**, `EditError::TrackLocked(track)` — never place locked content under a mover the user cannot see move. |
| **Ungroup** | **Allowed.** It removes a constraint rather than editing content, and it is the user's only escape hatch from a frozen group without unlocking. |
| Sync-lock expansion onto a locked track | **Skipped**, unchanged (`ops.rs:888`) |

Groups **refuse**; sync-lock **skips**. That asymmetry is deliberate: a
skipped group member breaks the group's defining promise, whereas sync-lock is a
per-track convenience whose skip is already the documented contract
(`ops.rs:873`, "`locked` always wins over it").

### 5a.7 Interaction with `expand_sync_lock_ripple` — precedence made concrete

35 §3.6: *"Group membership binds first… sync-lock then propagates that shift."*
Concretely, every ripple op computes its shift in three ordered passes:

1. **Primary** — the edited track's own `RippleEdit` (existing code:
   `ops.rs:937` `ripple_trim`, `:815` `ripple_delete`, and the insert/extract
   paths at `ops.rs:2428`, `:2503`), yielding `(point, δ)`.
2. **Group** *(new)* — for every group with **at least one** clip shifted in
   pass 1, shift its **remaining** members by the same δ. Refuse the whole edit
   if any cannot take δ. Emitted as one extra `RippleEdit` per affected track.
3. **Sync-lock** — `expand_sync_lock_ripple` over the tracks **not already
   touched by passes 1 and 2**.

Pass 3's exclusion set is the change required in existing code:
`expand_sync_lock_ripple(p, seq, edited_track, point, delta)` (`ops.rs:874`)
excludes exactly one track. Generalize it to
`expand_sync_lock_ripple_multi(p, seq, edited_tracks: &[TrackId], point, delta)`
and keep the current signature as a one-element wrapper, so none of the four
existing call sites (`ops.rs:851`, `:1012`, `:2430`, `:2506`) change shape.
**Without this, a group member on a sync-locked track is shifted twice** — once
by pass 2 and once by pass 3.

Note pass 2 is required *whether or not* K-A5 adds any group verb: any ripple
today can already tear a v4-migrated `AvLink` group whose members straddle tracks
with different `sync_lock` settings.

---

## 6. MCP surface

GUI/MCP parity is CAP-019 and a definition-of-done item (ROADMAP §10 point 3),
and 26 §5 lists full MCP parity (PA-11) as *not yet held* — so a GUI-only group
feature would widen a gap this programme is closing. **An MCP surface is
warranted.** Three new tools plus two additive args:

| Tool | Args | Notes |
|---|---|---|
| `group_clips` | `clip_ids: Vec<ClipId>` (≥2 distinct topmost groups) | Sequence inferred via `locate_clip` (`handlers/video.rs:126`); all ids must resolve to one sequence, else error (same guard `link_clips` uses at `:1756`) |
| `ungroup_clips` | `clip_ids: Vec<ClipId>` | Peels the topmost group of each; no-op ids skipped |
| `list_groups` | `sequence_id: Option<SequenceId>` | Returns `[{ group_id, kind, parent, direct_clip_ids, member_clip_ids }]` — the read side an agent needs before editing |
| `move_clip`, `remove_clip` (existing) | `+ isolate: bool` (`#[serde(default)]`) | Group-aware by default; `isolate: true` is the MCP analogue of Alt+click |
| `list_clips` (existing) | — | Payload gains `"group_id"` (`handlers/video.rs:1913-1920`). `get_clip` already exposes it — it serializes the whole `Clip` (`:1939`) |

Wiring, following the existing pattern exactly: arg structs in
`protocol/args/video.rs` (10 §285 designates this file), handlers in
`handlers/video.rs` next to `link_clips` (`:1742`), dispatch arms in
`dispatch.rs` beside `:2383`, names added to the tool-name list at
`handlers/video.rs:8317`, then `schema_gen.rs` regenerated. **CI gates the docs**:
`ci.yml:162-167` regenerates `docs/mcp-api.md` and fails on any diff, so the
regeneration is mandatory, not optional.

Group fan-out for MCP `move_clip`/`remove_clip` must call the **same `ops::`
functions** the GUI calls. The link-group precedent duplicated the expansion
logic in two crates (`ops_bridge.rs:345-428` and `handlers/video.rs:154-200`,
which is explicitly "a parallel implementation… mirroring field-for-field").
**Do not repeat that.** Group expansion lives in `photonic-core/src/timeline/ops.rs`
once; both arms call it. This is the single largest divergence risk in the item.

---

## 7. Acceptance fixtures and tests

**No rights-cleared content is required. This item is not gated.** Every test
below uses `ClipSource::Adjustment` / solid clips — no media bytes, no probe, no
GPU, no ffmpeg. That is the same choice `acceptance_stories.rs:30-35` already
documents ("Solid-color clips are used deliberately: they carry no media asset").
No `AssetRightsManifest` (23 §7.2) is needed.

| # | Test | Where | Asserts |
|---|---|---|---|
| T1 | group / ungroup / nested group / ungroup-peels-one-level | `ops.rs` `mod tests` | Tree shape after each, via `group_chain` / `child_groups` |
| T2 | `assert_undo_roundtrip` (`ops.rs:2921`) for every new command | `ops.rs` `mod tests` | Apply→undo is identity; redo re-applies |
| T3 | 9-clip group move across 3 tracks = **one** history node | `ops.rs` / `history` tests | `history.len()` grows by exactly 1; every member shifted by δ |
| T4 | Group move refused whole when one member would overlap | `ops.rs` `mod tests` | `Err`, document byte-identical afterwards |
| T5 | Group move refused with `TrackLocked` when a member is on a locked track; **ungroup still succeeds** | `ops.rs` `mod tests` | The §5a.6 asymmetry |
| T6 | Adjacent same-track members move without tripping the `validate` debug assert | `ops.rs` `mod tests` (debug build) | §2.4 — this is the regression this whole design exists to prevent |
| T7 | Split a 2-member group → two 2-member groups; split where one member covers → right half ungrouped | `ops.rs` `mod tests` | §5a.4 + no `SingletonNormalGroup` |
| T8 | Delete a 3-member group → one step, group dissolved, undo restores clips **and** the `GroupNode` | `tests/timeline.rs` | §5a.5 |
| T9 | Ripple trim on a track carrying one member of a cross-track group shifts the other member **once** | `tests/timeline.rs` | §5a.7 double-shift guard |
| T10 | Trim one member → other members unchanged (incl. `AvLink`) | `ops.rs` `mod tests` | §5a.3 / protected surface |
| T11 | v4 `link_group` doc → v5 `AvLink` tree → moves as a unit through the new path; `link_group` still retained | `tests/scope_migration.rs` (extends `:67`) | Migration + protected surface |
| T12 | v5 doc with a nested `Normal` group round-trips `to_json`→`from_json`→`finalize_load` unchanged | `tests/forward_compat.rs` | ROADMAP §10 point 5 |
| T13 | Newer-history tolerance: a `photon_history` containing an unknown `"cmd"` drops history, document still opens | `tests/forward_compat.rs` | §4 point 2 |
| T14 | GUI arm: `ops_bridge` group move + group delete headless | `photonic-gui/tests/video_ui_paths.rs` | ROADMAP §10 point 2 |
| T15 | **CAP-019 parity story**: MCP arm (`group_clips` → `move_clip`) vs GUI arm (`ops_bridge`), structural compare | `photonic-app/tests/acceptance_stories.rs` | ROADMAP §10 point 10 |
| T16 | Copy a grouped clip in sequence A, paste into sequence B → pasted clip carries no dangling `GroupId` | `photonic-gui/tests/video_ui_paths.rs` | §8.1 defect 2 |

Fixture bytes required: **none**. Fixture *documents* are built programmatically
in-test, matching `tests/scope_migration.rs`'s existing style.

### Definition-of-done mapping (ROADMAP §10)

| # | Answered by |
|---|---|
| 1 Core op + unit tests | `ops.rs` group ops; T1–T7, T10 |
| 2 GUI route | Ctrl+G/Ctrl+Shift+G (mode-gated), Alt+click isolate, group-aware drag/nudge/delete, context menu; T14 |
| 3 MCP tool/schema/docs | [§6](#6-mcp-surface); `ci.yml:162-167` docs gate |
| 4 One verb = one undo unit | [§5](#5-undo-unit); T2, T3, T8 |
| 5 Additive serde/migration round-trip | [§4](#4-migration); T11–T13 |
| 6 Pixel/audio coverage | **N/A** — no render path touched (`grep '\.group'` clean in `photonic-video`/`photonic-render`); no `ContentHash` change |
| 7 Hard gates / trend metrics | No new budgets. Watch `group_members`'s cost — [§8.1](#81-risks) defect 3 |
| 8 Legal/content/product gates | None. No bundled assets, no dependency, no codec ([§9](#9-clean-room-provenance)) |
| 9 Protected surfaces | Linked A/V untouched (T10, T11); sync-lock semantics extended, not changed (T9) |
| 10 Goal-backward L1–L4 | T15 parity story + T14 GUI path |

---

## 8. Risks, open questions and exclusions

### 8.1 Risks

1. **Mid-batch `validate` panic (§2.4).** The highest-probability way to get this
   item wrong is to implement group move/delete as a `Batch` of per-clip
   commands; it will pass a release build and panic in tests. T6 exists solely to
   catch it. Mitigation: the plural command variants in [§3](#3-data-model-change).
2. **Pre-existing: paste keeps a foreign `GroupId`.** `command_center.rs:1066-1067`
   clones the clipboard clip and mints a fresh `ClipId` but leaves `group`
   untouched. Copy in sequence A → switch tab → paste into sequence B yields a
   clip whose `group` names a group that does not exist there
   (`ValidationError::UnknownGroup`, `sequence.rs:439`) — a debug panic and an
   invalid document in release. Latent today only because nothing but the v4→v5
   migration writes `Clip.group`; K-A5 makes it reachable in one keystroke.
   **In scope**: paste must remap or clear groups, reusing the remap already
   written for `duplicate_sequence` (`sequence.rs:252-286`). T16.
3. **Pre-existing: multi-select delete is N undo steps.**
   `command_center.rs:935-954` loops `ops_bridge::remove_clip` per selected clip,
   so deleting 5 clips takes 5 Ctrl+Z. Routing the timeline delete through the
   new `RemoveClips` fixes the group case **and** this one. **In scope.**
4. **`group_members` is O(clips × chain) per call** (`sequence.rs:337` walks every
   track and calls `group_chain` per clip). Fine for a fan-out; **not** fine if
   the timeline paint calls it per member per frame to highlight a group. The GUI
   must build one membership map per frame, not query per clip.
5. **Divergent GUI/MCP expansion.** See [§6](#6-mcp-surface) — the link-group
   precedent is two hand-mirrored copies. Fan-out belongs in `ops.rs` once.
6. **`Ctrl+G` collision.** `object.group` fires unconditionally today
   (`tool_handlers.rs:96`, `:192`). New `video.group_clips` / `video.ungroup_clips`
   command ids (following the `video.copy` / `edit.copy` precedent at
   `commands.rs:467` — same default binding, different mode, dispatched from
   `monitor.rs:1626`'s video-mode poll) **plus** a `self.mode != AppMode::Video`
   gate on the two `object.*` arms in `handle_global_shortcuts`. Without the gate
   both fire in one frame.

### 8.2 Open questions (each with a recommendation)

- **Q1 — does `AvLink` trim propagate?** 35 §3.5 says yes and calls it today's
  behaviour; the code says no in three places (`ops_bridge.rs:322-326`,
  `handlers/video.rs:147-149`, and the absence of any propagation in
  `ops::trim_clip`). **Recommendation: keep the code's behaviour** (trim never
  propagates) and correct 35 §3.5. Propagating would change a protected surface
  under cover of a new feature. *Needs product sign-off because it is a
  documented-intent change, not a code change.*
- **Q2 — should ungroup on a lock-spanning group be allowed?**
  **Recommendation: yes** (§5a.6), because refusing leaves no escape hatch. If
  product prefers strict locking, the fallback is to refuse and surface "unlock
  track V2 to ungroup" — one message, no design change.
- **Q3 — should a group carry a name and a colour?** Kdenlive does not, and 35 §3
  does not model it. **Recommendation: no** in v1; `GroupNode` gaining an
  `Option<String>` later is serde-additive and needs no format step.

### 8.3 Deliberately excluded

- **Effect propagation across group members + the "how many carry it" badge**
  (26 K-A5's final clause). Groups are a *timing and selection* concept here;
  propagating an effect stack is the same verb as
  [K-B15 Paste Attributes](../specs/video-editor/26-kdenlive-mlt-parity.md#k-b15--paste-attributes-copy-an-effect-stack-between-clips),
  and it needs 35 §2's effect-scope rules to be the authority, not this document.
- **Removing `Clip.link_group`** — needs v6 and its own protected-surface
  refactor ([§4](#4-migration) point 4).
- **Selection as a group type.** Settled by 35 §3.4: selection is session state
  (`timeline_selection: Vec<ClipId>`). Isolation state is session-only too — not
  in the 39 §1.6 UI sidecar, because it is transient rather than persistent.
- **Multi-track ripple/gap-close for a group** (§5a.5).
- **A "select the middle group" verb / group breadcrumb UI.** Alt+click isolate
  and one-level ungroup cover the workflows; a group navigator is separate scope.
- **`split_av_link` over MCP.** Core has it (`ops.rs:1243`) and the GUI menu uses
  it (`app/timeline/mod.rs:2257`), but there is no MCP tool — a real CAP-019 gap,
  and a K-A13 one, not a K-A5 one.

---

## 9. Clean-room provenance

Per [26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md) and
[23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol):

- **Sources used.** (a) Photonic's own code and specs, cited by `file:line`
  throughout; (b) 26 K-A5's requirement statement, itself derived from Kdenlive's
  `CC-BY-SA-4.0` user documentation as a *requirements source*, cited and never
  pasted; (c) the general NLE practice that groups form a tree and that clicking a
  member selects the unit — a functional idea, not protectable expression.
- **Sources not used.** The Kdenlive source tree, the MLT/`mlt++` source tree,
  frei0r, and any GPL/LGPL derivative were not inspected for this item. No
  identifier, comment, constant, control flow or test case below derives from
  them. The implementer records the 23 §3.4 attestation for the `core-timeline`
  subsystem, and an independent reviewer checks provenance before merge
  (26 §2 point 2).
- **Design origin.** Every concrete decision here is derived from Photonic's own
  constraints, not from a reference product: the plural command variants come
  from `commands.rs:1748`'s per-command `validate` assert; the `GroupTreeDelta`
  shape mirrors `RippleEdit`'s existing before/after-pairs idiom
  (`commands.rs:546`, `:902`); the lock rule comes from `ops.rs:873`'s existing
  "`locked` always wins" contract; the pass ordering comes from 35 §3.6.
- **Photonic-ahead properties preserved** (26 §5, ROADMAP §9). Deltas are `Tick`
  flicks, never floats or frame counts (PA-8). Ranges stay half-open — group span
  is `[min(start), max(end))` (PA-7). Failures are typed
  `EditError::TrackLocked(TrackId)`, never a string (PA-9). No graph or cache key
  changes, so per-node caching is untouched (PA-1). `AvLink` groups keep their
  singleton exemption so a mid-edit A/V pair is never dissolved
  (`sequence.rs:469-472`, `:491`). No reference NLE limitation is ported: nesting is
  unbounded, and groups span video and audio tracks freely.
- **No dependency is contemplated or authorized.** No bundled asset, no codec, no
  patent surface — so none of ROADMAP §7's K/E/X gates apply to this item.

---

## Follow-ups

Changes this document deliberately did **not** make to existing docs
(this item may not edit them; each needs its own change):

1. **`35-model-decisions.md` §3.5, "Trim" row.** The parenthetical "**Exception:**
   `AvLink` groups propagate trims to linked partners, which is today's behaviour
   and must not regress" is **factually wrong about the code**. Trim propagation
   does not exist: `ops_bridge.rs:314-336` and `handlers/video.rs:140-149` both
   state the opposite as a deliberate decision, and `ops::trim_clip`
   (`ops.rs:674`) touches one clip. Suggested correction: "Trims **only** the
   trimmed clip, `AvLink` included — matching the reference-NLE convention 14
   §M-2 records and the shipped behaviour."
2. **`26-kdenlive-mlt-parity.md` K-A5, Files line.** It reads "*a `GroupId` tree
   beside `LinkGroupId`*" and "*`link_group` becomes the A/V-split specialization*"
   as future work; both **already shipped** (`sequence.rs:147`, `migration.rs:212`).
   Suggested correction: point the Files line at the *ops/GUI/MCP* gap and mark
   the model half done, so the effort estimate is not read as including a model
   change that has landed.
3. **`load.rs:183`'s `TODO(39 §2.4)`** — surfacing `LoadReport.dissolved_groups`
   on the 36 diagnostic channel. Group verbs make dissolution user-visible, so
   this TODO becomes worth closing; it is owned by 39, not by K-A5.
4. **`ROADMAP.md` §0 progress table** — add a K-A5 row when the item lands, with
   its commit, per the existing convention.
