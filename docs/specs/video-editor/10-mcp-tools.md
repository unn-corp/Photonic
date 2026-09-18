# 10 — MCP Tools: Video Domain Surface

**Depends on:** all prior docs (01 data model, 02 engine). **Decisions:** D-01, D-03, D-06, D-08, D-09. **Realizes:** CAP-019 (agent parity with GUI, all capabilities), plus every CAP that touches editable state (001–017, 020, 021).

Scope per 00 §5: the MCP tool surface for every video domain (media, sequence, track, clip, effects, keyframes, captions, TTS, grade, node graph, audio, playback, render, export) and the wiring to land it in `crates/photonic-mcp`.

---

## 1. Design rules

1. **Parity by construction.** Each mutating tool calls a pure edit op in `timeline/ops.rs` (01 §10: `fn move_clip(seq: &mut Sequence, ...) -> Result<TimelineCmd, EditError>`) — the same fn the GUI timeline panel calls. A tool never re-derives edit logic; it deserializes args, calls the op, wraps the returned `TimelineCmd` in `Command::Timeline`, and executes via `history.execute_discrete` (pattern already used: `charts.rs:72`, `doc_data.rs:206`). This makes CAP-019 an architectural guarantee, not a parity test post-hoc.
2. **Naming convention** matches the existing 376-tool corpus (`schema_gen.rs` verbs: `create_`, `add_`, `remove_`/`delete_`, `set_`, `get_`, `list_`, `apply_` — confirmed via `create_shape`, `update_node`, `set_layer_mask`, `get_raster_info`, `list_guides`). Video tools use the same verbs; no new verb families except `export_` and `generate_` (already used nowhere else but read naturally: `export_sequence`, `generate_proxies`, `generate_voiceover`).
3. **Ticks + seconds/timecode, always both accepted.** Every time-valued arg exposes three optional fields, precedence **ticks > timecode > seconds** (most-precise-first, matches 01 §1 "ticks only" internally):
   ```json
   { "at_ticks": 352800000, "at_tc": "00:00:00;15", "at_seconds": 0.5 }
   ```
   - `at_ticks` (i64): exact, used as-is.
   - `at_tc` (string, `HH:MM:SS:FF`): parsed via the target sequence's `FrameRate::ticks_per_frame` (01 §1). **Drop-frame is not implemented** — `parse_timecode` currently accepts `;` but treats it identically to `:`, so a `;` separator does *not* select drop-frame numbering and 29.97 timecode drifts ≈3.6 s/hour. Closing this is [26 K-A12](26-kdenlive-mlt-parity.md#k-a12--timecode-as-a-first-class-concept), and it is a behaviour change to a shipped contract; requires a resolvable sequence context (arg validation error `MissingSequenceContext` if the tool has no sequence to interpret against — e.g. a bare asset-level tool).
   - `at_seconds` (f64): converted `round(seconds * TICKS_PER_SECOND)`; documented as "convenience, sub-tick error possible, not authoritative."
   - If none provided and the field is required → standard `serde` "missing field" error. Docs on every arg struct state precedence per repo comment-doc convention (see `AdjustColorsArgs` field-doc style, `protocol/args/c.rs:524`).
4. **One command per mutating call.** No tool issues more than one `execute_discrete`/`Command::Batch` per invocation — batch variants (§3) exist precisely so agents don't need N calls + N undo steps for one logical edit (mirrors `Command::Batch` used today for multi-node edits, `doc_data.rs:206`).
5. **Readonly tools never lock `history`.** They may lock `document` (read-only borrow pattern already used by every `get_*`/`list_*` handler) and, for engine-backed tools, query `EngineSession`/`VideoEngine` state — never `history.lock()`, so a long readonly call (e.g. `render_frame_at` at full quality) can't stall a concurrent mutation's checkpoint scheduling.
6. **Async completion still goes through history.** Long-running jobs (§6) commit their resulting `TimelineCmd` (if any) from the job-completion path using the **same** `Arc<Mutex<CommandHistory>>` handle `AppState` already holds — `execute_discrete`, then `history.schedule_mcp_checkpoint(job_kind)` (`history/coalescing.rs:43`) exactly as `dispatch.rs::post_mutation` does today. This is the one place a tool's mutation happens **outside** `dispatch_tool_inner`'s normal post-mutation hook — call it out in code comments so it isn't missed during checkpoint-debounce debugging.
7. **Lock-ordering invariant (worker-thread commits):** always acquire the **document lock before the history lock** — the same order `dispatch_tool_inner` uses on the synchronous path. The engine snapshot thread also takes the document lock (02 §1); one consistent order across all three acquirers is what prevents deadlock. One line, one rule, no exceptions.

---

## 2. AppState extension

`AppState` (`server.rs:63`) gains one field:

```rust
pub struct AppState {
    pub document: Arc<Mutex<Document>>,
    pub history: Arc<Mutex<CommandHistory>>,
    pub capture_tx: Arc<StdMutex<mpsc::Sender<oneshot::Sender<Vec<u8>>>>>,
    pub config: McpServerConfig,
    pub audit_log: Arc<StdMutex<AuditLog>>,
    pub clipboard_ring: Arc<StdMutex<ClipboardRing>>,
    pub video_engine: Arc<photonic_video::VideoEngine>,   // NEW
    pub video_jobs: Arc<StdMutex<video_jobs::JobRegistry>>, // NEW — §6
}
```

`VideoEngine::new(gpu: Arc<GpuContext>)` (02 §1) needs a `GpuContext` in **both** run modes:
- **Headless** (`main.rs:158`, `args.headless`): build one via `pollster::block_on` exactly like the existing headless-renderer precedent (`repl.rs:56`, `script.rs:33` — both already `pollster::block_on(HeadlessRenderer::new())` with no window). No behavior change to today's screenshot/export_raster limitation (those stay unserviced headless — different subsystem, GUI-render-thread-only); the *video* engine's own GPU path is independent and headless-capable by construction (02 §7: "Headless/MCP export uses the identical path").
- **GUI mode**: share the winit-bound `GpuContext` already constructed for `PhotonicRenderer::new` (`main.rs:442`) — one GPU device, no duplicate adapter.

`AppState::new`/`McpServer::new` (`server.rs:87`) takes `video_engine: Arc<VideoEngine>` as a new constructor param; `main.rs` builds it once (either branch) before constructing `McpServerConfig`, threads it into both `McpServer::new` call sites (headless block + `spawn_mcp_server`).

`EngineSession` (02 §1, per-open-document runtime state) is created once per `AppState` lifetime bound to `document_arc`/`history_arc` — same Arcs, so engine-thread snapshotting (`doc_generation` counter, 02 §1) and MCP mutation both see one document.

**Single-document assumption holds.** `AppState.document` is one `Arc<Mutex<Document>>` today (`server.rs:63`) — one open project per running Photonic process, matching the existing single-window app model. Video introduces no multi-document concept; `NestedSequence` (01 §5) nests *within* one `Document`'s `TimelineProject`, never across processes. `EngineSession` is therefore also a singleton per `AppState`, not a pool — no `session_id` arg anywhere in §3's catalog.

> **Correction ([39 §3](39-document-lifecycle.md#3-document-identity-a-4)).** [04 §1](04-ui-mode-timeline.md) makes timeline state **per-tab**, and a user may have a vector-only tab and a video-project tab open at once. So the assumption above is not that there is one document — it is that MCP binds to **the active tab**, and that was never stated. With two tabs open, every video tool silently targets whichever document `AppState` holds, and CAP-019 parity becomes unverifiable.
>
> Normative resolution: **MCP binds to the active tab.** Add read-only **`get_active_document`** (id, name, path) so an agent can check what it is about to edit, and **`set_active_document { id }`** so multi-project automation is possible without threading an id through 110 tools. Every tool accepts an **optional** `document_id`; when present it must match the active document, else `DocumentMismatch`. `EngineSession` follows the active tab and playback stops on the outgoing one. CAP-019 parity tests must bind explicitly and assert `get_active_document` before acting.

---

## 3. Tool catalog

> **The catalogue below is illustrative, not authoritative.** The shipped surface is **110** video tool handlers, and this document has historically carried three different counts. Per [27 A-10](27-spec-audit.md#a-10--p2--the-mcp-tool-count-is-stated-four-ways-and-matches-nothing), the fix is to **generate** these tables from `schema_gen.rs::tool_list()` under the existing doc-drift CI gate rather than hand-maintain a count. Until that lands, treat `docs/mcp-api.md` as the source of truth.

Mutating tools always: validate → call `timeline/ops.rs` fn → `execute_discrete` → `ToolOutput::mutating`. Readonly tools: `ToolOutput::readonly`. "Job" = async pattern, §6.

### 3.1 Media (7)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `import_media` | Add file(s) to media pool; triggers async probe | `paths: [string]`, `bin: string?` | mutating |
| `probe_media` | Force re-probe (ffprobe) an asset, refresh `MediaProbe` | `asset_id` | job |
| `relink_media` | Repoint offline asset to a new path; matches by `content_hash` then filename (01 §3) | `asset_id`, `new_path` | mutating |
| `generate_proxies` | Batch-generate proxies (02 §6) | `asset_ids: [string]`, `force: bool?` | job |
| `remove_proxy` | Delete generated proxy file(s), revert asset to original-only (05 §2.3) | `asset_ids: [string]` | mutating |
| `transcode_media` | General-purpose transcode to an editing-friendly intermediate (distinct from proxy — user-picked codec/container, not the fixed proxy profile) | `asset_id`, `preset` | job |
| `list_media` | List pool assets + probe/proxy status, optional bin filter | `bin: string?` | readonly |

### 3.2 Sequence (10)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `create_sequence` | New sequence; ≥1 `SequenceFormat` required (01 §4) | `name`, `frame_rate`, `formats: [{name,width,height}]` | mutating |
| `delete_sequence` | Remove; fails if referenced by a `NestedSequence` clip elsewhere (cycle/dangling-ref guard, 01 §5) | `sequence_id` | mutating |
| `list_sequences` | | — | readonly |
| `set_active_sequence` | Sets `TimelineProject::active_sequence` via `TimelineCmd::SetActiveSequence` (01 §10 — document state, undoable) | `sequence_id` | mutating |
| `set_sequence_format` | Add/update/remove a `SequenceFormat` variant (CAP-012); `op` picks the mode — maps to `TimelineCmd::SetSequenceFormat` (01 §10, covers all three ops) | `sequence_id`, `op: add\|update\|remove`, `format?` | mutating |
| `set_active_format` | Switch active aspect-ratio variant via `TimelineCmd::SetActiveFormat` (01 §10 — undoable) | `sequence_id`, `format_index` | mutating |
| `set_work_range` | In/out for preview + export (01 §4) | `sequence_id`, `start_*`, `end_*` (or `null` to clear) | mutating |
| `add_marker` / `remove_marker` | Sequence markers | `sequence_id`, `at_*`, `name?`, `color?` / `marker_id` | mutating |
| `list_markers` | | `sequence_id` | readonly |

### 3.3 Track (4)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `add_track` | Append video/audio track | `sequence_id`, `kind: video\|audio`, `name?` | mutating |
| `remove_track` | | `track_id` | mutating |
| `set_track_prop` | name/enabled/locked/height (universal setter, mirrors `SetClipProp`) | `track_id`, `prop`, `value` | mutating |
| `reorder_track` | Change z/mix order (Track vectors ARE the order, 01 §4) — same shape as existing `reorder_layers`/`ReorderLayers` (`history/mod.rs:1822`) | `sequence_id`, `kind`, `old_order: [track_id]`, `new_order: [track_id]` | mutating |

### 3.4 Clip edit ops (9) — one tool per `TimelineCmd` variant, 1:1 with `ops.rs` (design rule 1)

| Tool | `TimelineCmd` | Key args |
|---|---|---|
| `insert_clip` | `InsertClip` | `track_id`, `start_*`, `source` (`ClipSource`, 01 §5), `source_in_*?`, `duration_*` |
| `move_clip` | `MoveClip` | `clip_id`, `new_start_*`, `new_track_id?` |
| `trim_clip` | `TrimClip` | `clip_id`, `edge: in\|out`, `new_*` |
| `split_clip` | `SplitClip` | `clip_id`, `at_*` |
| `remove_clip` | `RemoveClip` | `clip_id`, `ripple: bool?` |
| `ripple_edit` | `RippleEdit` | `clip_id`, `edge`, `delta_*` |
| `roll_edit` | `RollEdit` | `clip_id_a`, `clip_id_b` (shared edge), `delta_*` |
| `slip_clip` | `SlipClip` | `clip_id`, `delta_*` (shifts `source_in` only) |
| `slide_clip` | `SlideClip` | `clip_id`, `delta_*` (moves clip, trims neighbors) |

All nine are single-command mutating calls; snapping (to clip edges/playhead/markers, CAP-002) is a **GUI input-layer** concern per 01/02 — MCP callers pass exact ticks, no implicit snap (agents are expected to compute exact target ticks; documented explicitly so an agent doesn't assume snap-to-marker happens server-side).

### 3.5 Clip properties (5)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `set_clip_prop` | Universal property setter — name, `transform` (pos/scale/rotation/anchor/opacity), `reframe` override for a format index, `enabled`, `speed` (see below) — mirrors `SetClipProp{old,new}` (01 §10), same shape as existing `update_node` | `clip_id`, `path` (PropPath, 01 §6.2), `value` | mutating |
| `set_clip_speed` | Accepts `SpeedMap::Constant` **and** `SpeedMap::Keyframed` ramps (01 §5.1) — the handler builds keyed ramps and is covered by `mcp_parity_round2.rs`. An earlier restriction to constants was never shipped | `clip_id`, `ratio: {num,den}` \| `keys` | mutating |
| `set_transition` | Add/replace/remove (`null`) `transition_in`/`transition_out` | `clip_id`, `edge: in\|out`, `transition: {kind,duration_*,params}\|null` | mutating |
| `list_clips` | List clips on a track or whole sequence, filterable by time range | `track_id?`, `sequence_id?`, `range?` | readonly |
| `get_clip` | Full clip dump incl. `AnimProps` tracks, effects, grade ref, composition ref | `clip_id` | readonly |

### 3.6 Effects (5)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `add_effect` | Push onto `Clip::effects` stack (01 §6.3), seeded param defaults from `EffectKind` registry | `clip_id`, `kind`, `index?` | mutating |
| `remove_effect` | | `clip_id`, `effect_index` | mutating |
| `reorder_effects` | | `clip_id`, `new_order: [effect_index]` | mutating |
| `set_effect_param` | Sets one `PropPath` under `effects[i].params` (includes `enabled` as a boolean path — no separate toggle tool) | `clip_id`, `effect_index`, `path`, `value` | mutating |
| `list_effect_kinds` | Registry introspection: available `EffectKind`s + their `PropPath`/range table (01 §6.2) — lets an agent discover params without guessing | — | readonly |

### 3.7 Keyframes (4) — generic `AnimProps` system, works for clip transform, effect params, audio automation, vector-node props (01 §6)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `set_keyframe` | Upsert one keyframe on a `PropertyTrack` (creates the track if absent) | `target` (`clip_id`+`path`, or `graph_node_id`+`path`), `at_*`, `value`, `interp` (`Hold\|Linear\|Bezier{...}`) | mutating |
| `remove_keyframe` | | `target`, `at_*` | mutating |
| `batch_set_keyframes` | N keyframes on the same or different `PropertyTrack`s as **one** undo step (design rule 4) | `ops: [{target, at_*, value, interp}]` | mutating |
| `get_keyframes` | Full `PropertyTrack` list for a target (folds "list" into "get" — the target is small, no pagination need) | `target` | readonly |

### 3.8 Captions (11)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `auto_caption` | Hosted transcription (D-04) → word-level `CaptionCue`s on a new/existing track (CAP-009) | `sequence_id\|clip_id`, `track_id?`, `provider?` (default = configured hosted service) | job |
| `add_caption_track` / `remove_caption_track` | | `sequence_id`, `name?` / `track_id` | mutating |
| `get_caption_track` | Full cue+word dump for one track (folds "list_caption_cues") | `track_id` | readonly |
| `set_caption_cue` | Text/timing/position for one cue; creates if `cue_id` omitted | `track_id`, `cue_id?`, `start_*`, `end_*`, `words?`, `position_override?` | mutating |
| `split_caption_cue` / `merge_caption_cues` | | `cue_id`, `at_*` / `cue_id_a`, `cue_id_b` | mutating |
| `set_caption_word` | Per-word text/timing edit (CAP-010) | `cue_id`, `word_index`, `text?`, `start_*?`, `end_*?` | mutating |
| `set_caption_style` | Track-default or cue-override style (font/size/color/background/karaoke `highlight`/`animation`, 01 §7) | `track_id\|cue_id`, `style` | mutating |
| `import_captions` / `export_captions` | SRT/VTT/ASS interchange | `track_id`, `path`, `format?` / `track_id`, `path`, `format` | mutating / readonly |

### 3.9 TTS (1)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `generate_voiceover` | Submit text to configured TTS provider (D-04); on completion inserts an audio clip sized to returned audio duration (CAP-011) | `text`, `track_id`, `start_*`, `voice?`, `provider?` | job |

### 3.10 Grade (5)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `set_grade` | Full `Grade` replace/patch — exposure/contrast/temp, lift-gamma-gain, tone curve, HSL (07 owns operator catalog); `null` clears the grade | `clip_id`, `grade\|null` | mutating |
| `apply_lut` | Attach a 3D LUT as part of the grade stack; `null` removes | `clip_id`, `lut_path\|null` | mutating |
| `copy_grade` | Copy one clip's grade (incl. LUT ref) onto N others | `source_clip_id`, `target_clip_ids: [string]` | mutating (one `Command::Batch`) |
| `grade_preset` | Save current grade as a named preset, apply a preset, or list presets — one tool, `op` field, avoids 3 near-identical tools | `op: save\|apply\|list`, `clip_id?`, `name?` | mutating (save/apply) / readonly (list) |
| `get_scopes` | Waveform/vectorscope/histogram data for a clip at a tick (compute-shader output, 07) — data, not an image; UI/agent renders it | `clip_id`, `at_*` | readonly |

### 3.11 Node graph (8)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `create_clip_composition` | Instantiate a per-clip `NodeGraph` (D-06), binds `ClipIn` to the clip's source; `graph_id: null` on an existing composition detaches it (folds "remove") | `clip_id`, `graph_id?\|null` | mutating |
| `add_graph_node` / `remove_graph_node` | | `graph_id`, `op` (`GraphOp`, 01 §8) / `graph_id`, `node_id` | mutating |
| `add_graph_edge` / `remove_graph_edge` | Cycle-checked at edit time (01 §8); fails clean, never panics | `graph_id`, `from: {node_id,port}`, `to: {node_id,port}` / `graph_id`, `edge_index` | mutating |
| `set_graph_node_param` | One `PropPath` under a node's `AnimProps<EffectParams>` | `graph_id`, `node_id`, `path`, `value` | mutating |
| `set_project_graph` | Sets/clears `TimelineProject::project_graph` (splices after active-sequence output, 01 §2) | `graph_id\|null` | mutating |
| `get_graph` | Full node/edge/param dump, incl. type-check diagnostics if the graph currently fails to compile (02 §2 step 3: falls back to default chain + surfaces a diagnostic — this tool is how an agent reads that diagnostic) | `graph_id` | readonly |

### 3.12 Audio (6)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `set_clip_audio` | Gain/fades/channel map (01 §5, `ClipAudio`) | `clip_id`, `gain_db?`, `fade_in_*?`, `fade_out_*?`, `channel_map?` | mutating |
| `set_track_audio` | Volume/pan/mute/solo (`TrackAudio`, 09) | `track_id`, `volume?`, `pan?`, `muted?`, `solo?` | mutating |
| `audio_fx` | Add/remove/reorder an EQ/compressor/ducking node in a track's fx chain — one tool, `op` field (mirrors `grade_preset` pattern) | `track_id`, `op: add\|remove\|reorder`, ... | mutating |
| `set_master_bus` | Master bus level/limiter | `sequence_id`, `volume?`, `limiter?` | mutating |
| `get_audio_meters` | Current/peak levels per track + master (session state, not document — engine-backed) | `sequence_id` | readonly |
| `get_waveform` | Decoded waveform pyramid summary for an asset/clip region (09, sidecar-cached per 01 §9) — returned as sampled peak arrays, not an image | `asset_id\|clip_id`, `range?`, `resolution?` | readonly |

### 3.13 Playback (7) — see §7 headless matrix

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `play` / `pause` | Engine transport (`EngineCmd::Play`/`Pause`, 02 §1) | `sequence_id` | mutating* |
| `seek` | `EngineCmd::Seek` | `sequence_id`, `at_*` | mutating* |
| `step` | Single-frame step (CAP-004) | `sequence_id`, `frames: i32` (±) | mutating* |
| `set_loop_range` | `EngineCmd::SetLoop` | `sequence_id`, `range\|null` | mutating* |
| `set_proxy_mode` | `Auto\|ForceProxy\|ForceOriginal` (02 §6, session not document) | `mode` | mutating* |
| `get_engine_status` | Playhead, dropped-frame count, cache stats (`EngineStatus`, 02 §1) | `sequence_id?` | readonly |

`mutating*` — these mutate **engine/session state**, not the `Document`; they do not call `history.execute_discrete` and produce no undo step (01 §11: playhead/selection are session state, never document state). Classified `ToolOutput::mutating` only in the loose sense of "has a side effect"; **do not wrap in `Command`** — this is the one place design rule 4 doesn't apply, called out explicitly to prevent an implementer from inventing a spurious `Command::Timeline(SetPlayhead)` variant that 01 explicitly says must not exist.

### 3.14 Render (1) — see §4

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `render_frame_at` | Compile + evaluate the frame graph at one tick, headlessly; returns an image | see §4 | readonly |

### 3.15 Export (6)

| Tool | Purpose | Key args | M/R |
|---|---|---|---|
| `export_sequence` | Start an export job (02 §7); preset name or inline `ExportPreset` fields | `sequence_id`, `out_path`, `preset?`, `overrides?` (codec/resolution/frame-rate/bitrate/container) | job |
| `get_job_status` | Generic poll — covers export, proxy-gen, transcode, auto-caption, TTS (§6, one job registry) | `job_id` | readonly |
| `cancel_job` | | `job_id` | readonly (no document mutation; engine-state side effect only, same footing as `set_proxy_mode`) |
| `list_export_presets` | Catalog from 05's preset table + custom presets | — | readonly |
| `save_export_preset` | Create/overwrite a custom preset (app-level config, 05 §3.6 — no document mutation, no undo step) | `name`, `preset` | readonly (config side effect) |
| `delete_export_preset` | Remove a custom preset; built-ins are read-only, `NotSupportedV1`-style refusal | `name` | readonly (config side effect) |

**Count:** 110 video tools ship today; the figure below (89) was this document's design-time estimate and is retained only for its rationale. The count is a design outcome, not a target: CAP-019 requires literal parity with 21 GUI capabilities across 4 structurally distinct subsystems (edit/grade/caption/graph), and consolidation already folds ~15 would-be singleton tools into `op`-field variants (`set_sequence_format`, `grade_preset`, `audio_fx`) and read-folds (`get_clip` absorbs `list_keyframes`, `get_graph` absorbs kind-listing where sensible). Recommendation: accept 89; do not force further merges — each remaining tool maps to exactly one `TimelineCmd`/`GraphCmd`/`CaptionCmd`/`AudioCmd` variant (01 §10), and collapsing further would require multiplexed `op` fields on structurally unrelated operations (anti-pattern: `set_clip_prop`-style generic setters only work because the target IS a flat property bag; edit ops are not).

---

## 4. `render_frame_at` (the visual-feedback-loop tool)

The one tool agents lean on hardest — equivalent to `screenshot`/`export_raster` for the vector canvas (`canvas.rs`, `doc_export.rs:312`), but engine-owned and headless-capable where those two are GUI-render-thread-only (`server.rs:62` `capture_tx` is unserviced in `--headless`, `main.rs:158`).

```
render_frame_at(
  sequence_id: string,
  at_ticks? / at_tc? / at_seconds?,   // design rule 3
  format_index?: number,               // which SequenceFormat (aspect ratio); default = active_format
  quality: "preview" | "full",         // preview = proxy-eligible sources, lower internal res; full = originals, full res
  scale?: number,                      // 0 < scale <= 1, downscale output (matches existing screenshot `scale` arg, canvas.rs)
  output_format?: "png" | "raw_rgba16f", // default png (8-bit sRGB-encoded for display); raw = linear f16 planes, base64, for pixel-exact golden-frame comparison (SS-3, 11)
) -> ToolResult { text: "rendered WxH frame at tick T (compile Xms, eval Yms)", image: base64_png (if png), data: { width, height, tick, compile_ms, eval_ms, dropped_cache_entries } }
```

Path: `graph::compile(sequence, format, tick, quality_flags)` (02 §2) → `graph::eval` on wgpu (02 §2, "Evaluation") → readback `Rgba16Float` texture → for `png`: linear→sRGB transfer + premultiply-undo + PNG-encode (reuses existing `downscale_png`/encode helpers, `canvas.rs`); for `raw_rgba16f`: raw plane bytes, base64, no transfer applied (deterministic — this IS the golden-frame basis, 02 §7).

Example request/response (illustrative — final field names follow §1 rule 3 exactly):

```json
// request
{"method":"tools/call","params":{"name":"render_frame_at","arguments":{
  "sequence_id":"5e2a...","at_tc":"00:00:03:12","quality":"preview","scale":0.5
}}}

// response (JSON-RPC success envelope; tool-level status in content[0])
{"result":{"content":[
  {"type":"text","text":"rendered 960x540 frame at tick 2540160000 (compile 0.3ms, eval 4.1ms)"},
  {"type":"image","data":"<base64 png>","mimeType":"image/png"},
  {"type":"text","text":"{\"width\":960,\"height\":540,\"tick\":2540160000,\"compile_ms\":0.3,\"eval_ms\":4.1,\"dropped_cache_entries\":0}"}
]}}
```

**Cost warnings (document in tool description, not just this spec):**
- `quality: "full"` on an uncached 4K composite can hit the eval budget ceiling (02 §8: <8ms GPU for 1080p/3-layer — 4K scales roughly with pixel count, no budget guarantee given). Recommend agents default `quality: "preview"` for iterative feedback loops, `"full"` only for final verification frames.
- Cold-seek cost is a **decode** cost, not a render cost, and applies even to `render_frame_at`: an uncached GOP behind the target tick costs up to the "cold seek" budget (02 §8: <150ms for proxy) before the frame graph can even evaluate — a tight agent loop scrubbing many far-apart ticks should expect per-call latency dominated by decode, not GPU eval.
- Every call is independent (no held playback state) — repeated calls at nearby ticks benefit from the node-result cache (02 §5) since `IrOp` content-hashing is tick-independent for unchanged inputs; the compiler still re-runs (<0.5ms, 02 §8) but eval mostly hits cache.

---

## 5. Wiring plan

| Layer | Location | Action |
|---|---|---|
| Handlers | `crates/photonic-mcp/src/handlers/video.rs` (new, flat file — see below) | One `pub async fn` per tool, domain order matching §3 |
| Domain registration | `handlers/mod.rs:1-28` | Add `pub mod video;` (alphabetical slot, after `utility` per existing near-alphabetical order — `typography` then `utility` then `video`) |
| Args | `crates/photonic-mcp/src/protocol/args/video.rs` (new) | See below |
| Schema | `schema_gen.rs::tool_list()` (`:7`) | Append 89 entries to the `json!([...])` array, same shape as existing (`name`, `description`, `inputSchema`) |
| Dispatch | `dispatch.rs::dispatch_tool_inner` (`:67`) | 89 match arms, `ToolOutput::mutating`/`::readonly`/job-start-is-mutating-but-returns-immediately (§6) |
| Doc regen | `docs/mcp-api.md` | `cargo run -p photonic-mcp --bin dump_tools \| python3 tools/gen-mcp-docs.py > docs/mcp-api.md` — run and commit as part of DoD for every phase that adds tools (byte-identical gate, CI) |

**Handlers file: flat, not split.** Evidence against a size-triggered submodule split: the largest existing domain handler is `handlers/shapes.rs` at 3,478 lines (`transform.rs` 2,602, `utility.rs` 2,348, `styling.rs` 2,284) — all flat single files, **no** domain in the repo is split into a subdirectory regardless of size. `handlers/video.rs` at 89 tools will likely land in the 2,000–3,000 line range (comparable to `shapes.rs`); follow the established precedent and keep it one file, organized with `// ─── <subdomain> ───` section-comment banners (matching the banner style already used inside `protocol/args/c.rs`, e.g. `// ─── Tool result type ───`). Do not create `handlers/video/{media,clip,...}.rs` — that would be a new convention with zero precedent, adding navigation cost (import wiring across files) for no size benefit the repo currently values.

**Args file: new domain-named file, breaking the a/b/c/d size-bucket convention on purpose.** `protocol/args/{a,b,c,d}.rs` are pure size-balanced partitions of one undifferentiated 376-tool corpus (confirmed: `mod.rs` just does `mod a; mod b; mod c; mod d; pub use {a::*, b::*, c::*, d::*};` — no semantic grouping, alphabetical-ish only incidentally). Recommendation: add `protocol/args/video.rs` as a fifth module (`mod video;` + `pub use video::*;` in `args/mod.rs`) instead of folding ~1,500–1,800 lines of new structs into a/b/c/d. Reasons: (1) video lands as one cohesive phased body of work (12-agent-execution-plan waves) — a dedicated file is reviewable/revertable as a unit; (2) it doesn't perturb the existing size balance of a-d for an unrelated reason; (3) if a/b/c/d's balancing is ever automated, video's separateness costs nothing (add `video::*` to whatever regenerates the split). Alternative considered: append to `d.rs` (currently smallest headroom) — rejected, would make the largest args file also the semantically muddiest.

**Representative args struct** (`protocol/args/video.rs`, matches the doc-comment-per-field style of `AdjustColorsArgs`, `protocol/args/c.rs:524`):

```rust
#[derive(Debug, Deserialize)]
pub struct InsertClipArgs {
    pub track_id: Uuid,
    /// Position in the sequence. Precedence: at_ticks > at_tc > at_seconds (§1 rule 3).
    #[serde(default)] pub start_ticks: Option<i64>,
    #[serde(default)] pub start_tc: Option<String>,
    #[serde(default)] pub start_seconds: Option<f64>,
    pub source: ClipSourceArg,           // {"kind":"asset","asset_id":...} | {"kind":"vector",...} | {"kind":"nested_sequence",...} | {"kind":"solid_color","color":...} | {"kind":"adjustment"}
    #[serde(default)] pub source_in_ticks: Option<i64>,
    pub duration_ticks: i64,             // duration always exact ticks — no dual-unit ambiguity for a length agents typically compute from probe data
}
```

Handler resolves the three `start_*` fields once via a shared `resolve_tick(ticks, tc, seconds, sequence_ctx) -> Result<Tick, ToolError>` helper (one fn, reused by every time-valued arg — avoids reimplementing precedence logic per tool).

**Audit log + clipboard-ring:** `dispatch_tool` (`dispatch.rs:17`) wraps every call generically (records `AuditEntry` incl. args/result/duration regardless of domain, `audit.rs:6`) — video tools get audit logging for free, no wiring needed. `clipboard_ring` (copy/paste of `SceneNode`s) has no video analog in v1 — video tools never touch `state.clipboard_ring`.

---

## 6. Async / long-running job pattern (new — no existing precedent)

Searched for an existing async-job/poll pattern (`job_id`, `JobId`, polling): **none found** — every current MCP tool is synchronous request/response (screenshot's `capture_tx`/`oneshot` round-trip, `canvas.rs`, is the closest analog: fire-and-await-one-response, but still synchronous from the caller's view). Video introduces the first genuinely long jobs (export, proxy gen, transcode, transcription, TTS) — pattern defined here, to be reused by all five, not export-specific.

```rust
// handlers/video.rs (or a shared video_jobs.rs helper module within it)
pub struct JobRegistry { jobs: HashMap<JobId, JobHandle> }

pub enum JobStatus {
    Queued,
    Running { progress: f32, message: String },   // e.g. ExportProgress{frame,total,fps,eta} mapped in (02 §7)
    Done { result: serde_json::Value },             // e.g. {"clip_id": "...", "duration_ticks": ...} for generate_voiceover
    Failed { error: JobError },                      // §8 taxonomy
    Cancelled,
}
```

- **Start tool** (`export_sequence`, `generate_proxies`, `transcode_media`, `auto_caption`, `generate_voiceover`) validates args synchronously, spawns the work on a `photonic-video` worker thread (02's export/proxy/decode worker pools — reuse, don't add new thread pools), inserts a `JobHandle` into `video_jobs`, returns immediately: `ToolResult::text("job started").with_data({"job_id": "..."})`. Classified `mutating` only if it touches the document synchronously (e.g. `import_media` inserting the `MediaAsset` stub is synchronous+mutating even though probing is a job; `export_sequence` touches no document state at start, so it is **readonly at call time** — the export itself never mutates the timeline).
- **`get_job_status(job_id)`** — readonly, reads `video_jobs` (a `StdMutex`, same pattern as `audit_log`/`clipboard_ring` — sync lock, no async needed since it's just a `HashMap` read).
- **`cancel_job(job_id)`** — readonly (engine-state side effect, no document mutation, same footing as `set_proxy_mode` in §3.13); maps to `EngineCmd::CancelExport`/analogous per-domain cancel signal.
- **Completion callback** (runs on the worker thread, NOT inside `dispatch_tool_inner`): if the job's result implies a document mutation (`auto_caption` → `TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues)`; `generate_voiceover` → `TimelineCmd::TtsEdit(TtsCmd::GenerateAndPlace)` — 06 §6's exact command names, both in 01 §10; `probe_media`/`generate_proxies`/`transcode_media` → asset-field updates), the worker builds the `TimelineCmd`, then — observing design rule 7's lock order, document before history — calls `history.execute_discrete` on the shared `Arc<Mutex<CommandHistory>>`, then `history.schedule_mcp_checkpoint("<job_kind>")` (design rule 6). `export_sequence` completion mutates no document state (export is read-only over the timeline) — only updates `JobHandle` status.
- Progress: worker updates `JobHandle.status` under the same `StdMutex` on each tick (export already has `ExportProgress{frame,total,fps,eta}`, 02 §7 — map directly to `JobStatus::Running`); no separate streaming channel needed since MCP here is poll-based (no server-push transport in the current axum setup, confirmed: `server.rs` is plain HTTP POST per call, no SSE/WS).
- **Job lifetime/GC:** completed/failed/cancelled jobs retained 10 minutes (matches nothing existing exactly; chosen to comfortably exceed the debounced-checkpoint window, `server.rs` background flush is 10s/60s) then evicted by the same background `tokio::spawn` interval task pattern already in `server.rs::run` (the one that flushes checkpoints) — add a second timer arm in the same loop rather than a new task.

---

## 7. Headless matrix

Headless (`--headless`, `main.rs:158`) runs the MCP server only — no winit loop, no GUI render thread, `capture_tx` unserviced (existing limitation, unrelated to video). Video's own engine/GPU path is independent (§2) and headless-capable by construction (02 §7).

| Domain | Headless | Notes |
|---|---|---|
| Media, Sequence, Track, Clip edits, Effects, Keyframes, Captions (except live-preview), Grade, Graph, Export, `render_frame_at` | **Full** | Pure data ops (core, no GPU) or engine ops that construct their own headless GPU context (§2) — same as `export::render_loop`'s headless path (02 §7) |
| `get_scopes`, `get_waveform`, `get_audio_meters` | **Full** | Compute-shader/DSP reads, no display surface needed |
| `auto_caption`, `generate_voiceover` | **Full** | Network calls to hosted providers, no local GPU/audio dependency |
| `play`, `pause`, `step` (audio-synced playback) | **Degraded** | Requires the audio thread (cpal host, 02 §1/`audio/engine.rs`) — a sandboxed/CI headless box may have no default audio device. `cpal` init failure → clear structured error `AudioDeviceUnavailable` (§8), not a panic, not a silent no-op. Recommendation: attempt init lazily on first `play` call (not at startup) so `--headless` boxes with zero audio devices still serve every other tool normally. |
| `seek`, `set_loop_range`, `set_proxy_mode`, `get_engine_status` | **Full** | No audio device dependency (`seek` when paused is a pure clock set + one-frame eval, no cpal callback involved per 02 §4) |

Position: **all tools work headless except live audio-synced transport (`play`/`pause`/`step`)**, and even those fail with a clear error rather than blocking the rest of the surface.

---

## 8. Error taxonomy

Existing convention: tool errors are `ToolResult::error(msg)` + `is_error: Some(true)` inside a normal JSON-RPC **success** response (`protocol/args/c.rs:568`) — JSON-RPC-level `error{code,message}` (`protocol/mod.rs:35`) is reserved for transport/protocol failures (bad method name, `server.rs:199` `"Unknown method"`), not domain errors. Video tools follow the same envelope but **add a structured error code in `with_data`** (new for this domain — existing handlers only return free text on error; recommended here because agents scripting the three acceptance stories (CAP-019 test, 00 §2) need to branch on error kind, not string-match text):

```rust
ToolResult::error(format!("asset {asset_id} is offline"))
    .with_data(json!({ "error_code": "AssetOffline", "asset_id": asset_id }))
```

| `error_code` | Trigger | Example tools |
|---|---|---|
| `AssetOffline` | `MediaAsset` source unreachable (01 §3) | any tool resolving a `ClipSource::Asset` |
| `TickOutOfRange` | tick outside clip/sequence/work-range bounds; payload includes `{min_ticks, max_ticks}` | `trim_clip`, `split_clip`, `render_frame_at`, `set_work_range` |
| `GraphTypeMismatch` | node-graph edge port-type check fails (01 §8: "Type-check ports; on error... surface a diagnostic") | `add_graph_edge`, `create_clip_composition` |
| `ProviderAuthError` | hosted transcription/TTS auth failure (D-04) | `auto_caption`, `generate_voiceover` |
| `AudioDeviceUnavailable` | cpal init fails (§7) | `play`, `pause`, `step` |
| `NotSupportedV1` | feature flagged non-goal/post-v1 (e.g. keyframed speed ramps, 01 §5.1) | `set_clip_speed` |
| `CycleDetected` | nested-sequence or graph-edge cycle guard (01 §5, §8) | `insert_clip` (NestedSequence), `add_graph_edge` |
| `MissingSequenceContext` | `at_tc` given without a resolvable sequence (design rule 3) | any time-valued arg on a sequence-less tool |
| `JobNotFound` | `job_id` unknown/evicted (§6 GC) | `get_job_status`, `cancel_job` |

---

## 9. Test hooks (feeds 11-testing-phasing.md)

1. **Acceptance-story scripts.** Each of AS-1/AS-2/AS-3 (00 §2) gets one MCP-only script (no GUI) exercising the exact tool sequence a user's pointer/keyboard actions would produce — e.g. AS-1: `import_media` ×2 → `insert_clip` ×N → `auto_caption` (job, poll `get_job_status`) → `set_caption_style` → `set_sequence_format` → `create_sequence`... → `set_grade` → `export_sequence` (job, poll). Output compared against a GUI-produced run of the same story on the golden-frame corpus (11) — this pair (script + GUI run, same story, output diff) **is** the CAP-019 test.
2. **Doc-drift gate** (already CI-gated for the existing 376 tools) already covers the video surface — `tool_list()` carries all 110 entries, so no new CI wiring is needed. **Outstanding:** generate §3's tables *from* `tool_list()`, and the error-code table in §8 from `DiagCode` ([36 §5](36-error-model.md#5-mcp-mapping)), so neither can drift again.
<!-- spec-assert: ci-step-contains gen-mcp-docs.py -->
<!-- SD-17 (27 §3): the doc-drift gate the prose relies on is real and CI-wired; pinned so removing the gen-mcp-docs.py step reds the drift gate. -->

3. **Schema/args/dispatch consistency test.** New: a test that for every entry in `tool_list()` under the video domain, (a) a corresponding `*Args` struct exists in `protocol/args/video.rs` deserializable from the schema's `example`/required-fields shape, and (b) a `dispatch_tool_inner` match arm exists calling it. Recommend a `#[test]` in `crates/photonic-mcp/tests/` iterating `tool_list()` names against a static registry macro (avoids relying on the schema JSON and dispatch match staying manually in sync — the existing 376-tool surface has no such test today; introducing one here is scoped to prevent the video domain's much larger single-PR landing from silently drifting mid-implementation across phases P3–P8).
4. **Job-registry tests.** `get_job_status`/`cancel_job` against a fake instant-completing job (no real ffmpeg/network) to verify the registry/GC/checkpoint-on-completion wiring (§6) independent of engine correctness — these are MCP-layer tests, not engine tests (02/11 own engine correctness).
5. **`render_frame_at` determinism check.** Two calls, same args, `output_format: raw_rgba16f` → byte-identical (02 §2 "pure function" property) — cheap regression guard runnable in every CI pass, independent of the full golden-frame corpus (11 owns the corpus; this is a fast smoke test for the property the corpus assumes).
