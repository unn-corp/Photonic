# 16 — Insert / Overwrite / Lift / Extract — 3/4-Point Editing (parity: NLE gap 9)

> **Status: Delivered.** The 3/4-point spine is shipped and protected. Live residuals are tracked in [ROADMAP.md](ROADMAP.md), [19](19-editing-velocity-shot-management.md), and [20](20-pro-workflows.md).

**Why:** the spine of professional editing in Premiere/Resolve. Today Photonic edits by direct clip manipulation only (drag/trim). Reference NLEs let a user set a source range + a timeline point and *insert* (ripple everything right) or *overwrite* (replace in place), and *lift*/*extract* to remove. Wholly absent. Prerequisites: a source range concept, a target track, and a timeline in/out.

**Scope:** the four core edit operations + the minimal supporting model (source in/out on a pending clip, target-track selection, timeline in/out reuse of `work_range`). A full source *monitor* (separate preview of the raw asset) is a related but separable feature (see §6) — this spec delivers the edit operations, driveable from the media pool / keyboard even before a source-monitor UI exists.

## 1. Model additions (`photonic-core/src/timeline`)

- **Source range on the pending edit:** a lightweight `PendingSource { asset: AssetId, src_in: Tick, src_out: Tick }` held in GUI session state (NOT the document) — the currently-armed clip to be edited in. Its duration = `src_out − src_in`.
- **Target track:** GUI session state `target_video_track: Option<TrackId>` (+ audio). The "patch" of which track receives the edit (Premiere's source patching, gap M-3 — minimal version = the selected/first-enabled track).
- **Timeline point:** the playhead (session state) is the insertion point; a timeline in/out is `Sequence.work_range` (already exists, document state).

## 2. The four ops (`photonic-core/src/timeline/ops.rs`, pure fns → `TimelineCmd`)

Each returns a `TimelineCmd` (or `Vec` batched into one undo step) and is invariant-safe (sorted/non-overlapping enforced):

- **`insert_edit(seq, target_track, at: Tick, source) -> Vec<TimelineCmd>`** — split any clip under `at` on the target track, ripple ALL clips at/after `at` (on that track, or all tracks if "ripple all tracks" is on — start with the target track for v1) right by the source duration, and place the new clip in the opened gap. This is Premiere's Insert (`,`).
- **`overwrite_edit(seq, target_track, at, source) -> Vec<TimelineCmd>`** — place the new clip at `at`, trimming/removing whatever it overlaps on the target track (no ripple; timeline duration unchanged unless the clip extends past the end). Premiere's Overwrite (`.`).
- **`lift_edit(seq, track, range: (Tick,Tick)) -> Vec<TimelineCmd>`** — remove clip content in `range` on `track`, leaving a gap (no ripple). Premiere's Lift (`;`).
- **`extract_edit(seq, track, range) -> Vec<TimelineCmd>`** — remove clip content in `range` and ripple everything after left to close the gap. Premiere's Extract (`'`). (This generalizes the existing `ripple_delete`.)

Reuse existing primitives (`split_clip`, `remove_clip`, `ripple_trim`, `insert_clip`, `move_clip`) where possible; these ops compose them into one atomic, undoable batch. Each op gets proptest coverage for the sorted/non-overlap invariant + explicit post-state tests (Insert grows sequence by source-duration; Overwrite keeps duration; Extract shrinks by range).

## 3. Range source: lift/extract from timeline in/out

Lift/Extract operate over `Sequence.work_range` (the timeline in/out set by I/O) on the target track — so the existing I/O keys already provide the range. Insert/Overwrite use the source range from `PendingSource`.

## 4. GUI wiring (`photonic-gui`)

- **Bindings** (`commands.rs` + `command_center.rs`): `,` = Insert, `.` = Overwrite, `;` = Lift, `'` = Extract (Premiere defaults) — plus toolbar buttons on the monitor/timeline.
- **Target track** indicator on track headers (a small "source patch" highlight on the armed track); click to set target.
- **Arming a source:** minimal v1 — selecting a media-pool asset (or a timeline clip) with an in/out sets `PendingSource`. A full source monitor (§6) makes this richer but isn't required to ship the ops.
- All edits route through `ops_bridge` → the new core ops → history (one undo step each).

## 5. Tests

- Core: the four ops' post-state + invariant proptests (above).
- MCP: expose `insert_edit`/`overwrite_edit`/`lift_edit`/`extract_edit` as tools (mirrors the ops; CAP-019 parity) so an acceptance script can drive 3/4-point editing headlessly — add to the parity test surface.
- A headless story: arm a source range, insert at a mid-timeline point, assert the ripple arithmetic and the sequence-duration delta to the tick.

## 6. Related, separable (not required here)

- **Source monitor** (gap L-2): a second preview panel showing the raw armed asset with its own in/out scrub — a UI feature layered on `PendingSource`. Spec separately.
- **Source patching UI** (gap M-3): richer target-track routing (map source A1/A2 → timeline A-tracks). v1 uses the single target-track model above.
