# 05 — Import/Export: Media Pool, Probing, Presets, Reframe, Compression

**Depends on:** 01-data-model.md, 02-engine.md. **Owns:** user-facing surface, formats, presets, policies. Engine mechanics (decode, export render loop) are 02's. **Decisions:** D-03, D-09.

Scope, per 00-overview.md §5: media pool UX + import flows, probing, export preset catalog + encoder integration, aspect-ratio/reframe system, compression options. This doc does not re-specify decode internals, the frame-graph compiler, or the export render loop — those stay in 02; this doc specifies what the user (or an MCP-driven agent, CAP-019) sees, configures, and depends on as a contract.

## 0. Terminology recap (from 01/02, used throughout)

| Term | Meaning | Defined in |
|---|---|---|
| `MediaAsset` | One media pool entry: kind, source, probe, proxy, content hash | 01 §3 |
| `MediaProbe` | Cached ffprobe-derived metadata (duration, streams, container/codec) | 01 §3 |
| `SequenceFormat` | One aspect-ratio variant of a `Sequence` (w×h) | 01 §4 |
| `reframe` | Per-clip, per-format static transform override | 01 §5 |
| `ExportPreset` | This doc's schema (§3.1) — app-level, not document state | 01 §11, §3.1 below |
| `FrameGraph` / render loop | Compiled per-tick DAG; export walks it frame-by-frame | 02 §2, §7 |
| Sidecar cache dir | `<project>.photon.cache/` — proxies, waveforms, thumbnails, keyframe indices | 01 §9 |

---

## 1. Import flows

### 1.1 Entry points
- **File dialog** on media pool ("Import Media…") — native OS picker, multi-select.
- **Drag-drop** onto media pool panel (adds to active bin, no placement) or onto timeline (adds to pool + inserts clip at drop position/track, CAP-002 territory).
- **Batch import**: N files selected at once → one `TimelineCmd::AddAsset` per file, batched under one undo group (`CommandHistory` group, matching existing multi-op batching convention).
- **Paste**: OS clipboard file paths (from file manager) treated as import, same path as drag-drop.

### 1.2 Image-sequence detection
Name-pattern scan on multi-select/folder-drop: files matching `<base>[._-]?(\d{2,})\.<ext>` (same numeric-suffix family, contiguous or near-contiguous frame numbers, same ext, same folder) collapse into **one** `MediaAsset` of kind `Image` with a synthetic `AssetSource::File { path }` pointing at the sequence pattern (`frame_%05d.png` style) + `MediaProbe` synthesizing `duration` from frame count / declared rate (prompt for rate if ambiguous, default sequence frame rate). Non-matching stragglers import as individual stills. Detection runs client-side (no ffprobe needed) before the probe job queues.

### 1.3 Folder watch
**Recommend against for v1.** Rationale: adds a filesystem-watcher subsystem (platform-specific, debounce/coalesce complexity) for a workflow (auto-ingest a growing folder) none of the three acceptance stories need; SPEC non-goals already exclude live capture. Users re-run "Import Media…" on demand. Revisit post-v1 if a story demands it.

### 1.4 On-import pipeline

Normative readiness stages, poster priority (L3 before full thumb strips), and play/scrub gates: [24-preview-media-load.md](24-preview-media-load.md) §2. Summary:

```
drop/select file(s)
      │
      ▼
register MediaAsset (probe=None, content_hash=None) ── media pool row visible, spinner (L0)
      │
      ├──▶ hash job (xxh3 head+tail+len)                                    ── L1
      ├──▶ probe job (ffprobe / RasterImage::from_encoded / import_svg)      ── L2 MediaProbe
      ├──▶ poster-frame job (single still)                                 ── L3 instant monitor/bin paint
      ├──▶ keyframe-index job (video only, ffprobe -skip_frame nokey)        ── L4 scrub-ready
      ├──▶ thumbnail strip job (low priority / visible-range)               ── L6
      └──▶ waveform job (audio only)                                        ── L5 clip waveform ready
                      │
                      ▼
        proxy policy check (02 §6) ──▶ L7 inline "Generate proxy" affordance (never blocking)
```

All five background jobs are independent and idempotent — a project reopened with a warm sidecar cache (same content hash) skips straight to "ready," no re-probe/re-hash. Steps, in prose:

1. **Register** `MediaAsset` immediately (kind inferred from extension; `Video`/`Audio`/`Image` via container sniff, `.photon`/`.svg` → `VectorDoc`), `probe: None`, `content_hash: None` — media pool shows the row instantly with a spinner, never blocks the UI thread.
2. **Hash** (background): xxh3 of file head+tail+len (01 §3) — cheap, used for relink identity and proxy/cache keys.
3. **Probe** (background, engine `EngineCmd::Probe(AssetId)` → `photonic-video::media::probe` via `ffprobe`, 02 §3): fills `MediaProbe` (duration, video/audio stream info, container/codec). Stills use existing `RasterImage::from_encoded` metadata (no ffprobe needed — `crates/photonic-mcp/src/handlers/raster.rs:231`), VectorDoc assets probe via existing document/SVG parse (`crates/photonic-core/src/import.rs:26` for `.svg`, native load for `.photon`).
4. **Keyframe index** (background, video only): `ffprobe -skip_frame nokey` scan, cached to `<project>.photon.cache/` (02 §3) — required before scrub-seek is fast; media pool shows "indexing…" until done, playback works meanwhile (just slower first seeks).
5. **Proxy prompt**: if `proxy::policy` (02 §6) flags the asset (source > sequence preview res × 1.5, or long-GOP 4K+), media pool row shows an inline "Generate proxy" affordance rather than a blocking modal — batch imports get one summary toast ("3 of 5 files would benefit from proxies — Generate All / Dismiss"). Never blocks import completion (CAP-014: proxies are optional, always toggle-able later).
6. **Thumbnail + waveform jobs** (background, low priority): first-frame thumbnail (video/image) or waveform peaks (audio, downsampled pyramid) written to sidecar cache, keyed by content hash — reused across projects referencing the same file.

All steps 2–6 run on the engine's worker pool (02 §1), reporting progress via the existing `EngineStatus` channel; media pool subscribes and updates rows reactively. No step is synchronous with the import action.

### 1.5 Supported format matrix

| Kind | Via | Containers/codecs |
|---|---|---|
| Video | FFmpeg sidecar decode (D-03) | MP4/MOV (H.264, H.265/HEVC†, AV1, ProRes read), MKV/WebM (VP9, AV1), AVI (legacy) — whatever the shipped FFmpeg build's decoders support (decode has no licensing constraint the way encode does — reading GPL-adjacent codecs is fine; **only encode/export is constrained**, §3) |
| Audio | FFmpeg sidecar decode | WAV, MP3, AAC/M4A, FLAC, OGG/Opus |
| Image | Existing `RasterImage::from_encoded` (`crates/photonic-mcp/src/handlers/raster.rs:231`) | PNG, JPEG, WebP |
| Image sequence | Same, batched (§1.2) | PNG/JPEG/WebP numbered sequences |
| Vector | Existing `import_svg` (`crates/photonic-core/src/import.rs:26`) for `.svg`; native document load for `.photon` | Becomes `AssetKind::VectorDoc`, `AssetSource::File` for external files or `EmbeddedVector` for artboards/subtrees of the current document (01 §3) |
| 3D LUT | Direct file read, no ffprobe (it's a text/binary grid, not a media stream) | `.cube` — becomes `AssetKind::Lut3d` (01 §3), applied as a `Grade` operator input (07) |

† HEVC: decode/import always supported (reading is unconstrained); flagged in UI as "patent-encumbered format" only when it appears as an **export** target (§3.5).

`AssetKind::Lut3d` (01 §3) gets the same referenced-file/offline/relink handling as any other media asset (§2.2) — a `.cube` file the user points a grade at is a `MediaAsset` in the pool like a video or image, not an embedded blob, so a moved or deleted LUT file shows the same offline badge and goes through the same relink flow.

---

## 2. Media pool panel

### 2.1 Layout
- **Bins** (`MediaPool::bins`, 01 §3): flat list with parent refs, rendered as a tree in a left rail (mirrors existing layer-panel tree affordance). Drag assets between bins; right-click "New Bin."
- **List / grid toggle**: grid = thumbnail-forward (video/image default), list = metadata-forward (audio/large batches default). Persisted as a UI preference (session state, not document — 01 §11).
- **Metadata columns** (list view, sortable): Name, Kind, Duration, Resolution, Frame Rate, Codec, Channels (audio), File Size, Status (Online/Offline/Proxy), Bin.
- **Search**: filter box, matches name + bin path; kind-filter chips (Video/Audio/Image/Vector) alongside.

### 2.2 Offline media
`MediaAsset` with unreachable `AssetSource::File.path` renders a diagonal-stripe placeholder thumbnail (01 §3) and a red "Offline" badge in both grid and list views. Clips referencing an offline asset show the same stripe pattern on the timeline (not a hard error — playback continues past it, export blocks with a pre-flight warning, §3.8 step 7).

**Relink flow**:
1. Auto-relink on project open: for each offline asset, try `rel_path` (project-relative, 01 §9) → absolute `path` → **content-hash match** against any file the user points the relink dialog at.
2. Manual relink: right-click offline asset → "Relink…" → file picker; on match (hash **or** filename fallback per 01 §3), updates `AssetSource` in place — the `TimelineCmd::RelinkAsset { asset, old_path, new_path }` variant defined in [01 §10](01-data-model.md). **`old_path` is required** — without it the command is not invertible, breaking the SPEC's absolute undo constraint and CAP-018. (An earlier draft of this section specified a two-field shape; the shipped command carries three.)
3. Batch relink: point at a folder — matches all offline assets in the project by content hash first, filename second; unmatched assets stay offline with a summary count.

### 2.3 Per-asset actions
Context menu / row actions: **Reveal in Finder/Explorer**, **Generate Proxy** / **Remove Proxy**, **Replace Media** (swap source, keep clip instances + trims — same relink mechanism but user-initiated, not offline-driven), **Duplicate**, **Delete from Pool** (warns if clips reference it; deleting removes referencing clips' visual but not the timeline structure — clip becomes offline-styled, not deleted, so trims/keyframes aren't lost if media returns), **Convert/Compress…** (opens the standalone transcode tool, §5), **Copy Content Hash** (debug/support aid).

### 2.4 Row status states
Each media pool row cycles through a small, explicit state set — always one of: `Importing` (spinner, greyed thumbnail) → `Probing` → `Indexing` (video only) → `Ready` → (optionally) `Proxy Available` / `Proxy Building` → `Offline` (if the file later disappears) → `Relinking` (dialog open). States are derived, not stored (computed from `MediaAsset.probe`/`proxy`/reachability at render time, per 01 §11's rule that engine/session facts aren't undo-tracked document state) — so a project reopened mid-index just recomputes "Indexing" from the sidecar cache's on-disk progress marker, no stale state to reconcile.

### 2.5 Batch selection & sort
Multi-select (shift/ctrl-click, matches existing layer-panel selection convention) drives: batch relink (§2.2), batch proxy generate/remove, batch convert/compress (§5), batch delete, batch bin-move (drag N selected rows onto a bin). Sort (list view) by any metadata column, ascending/descending, persisted per-bin as a session preference. Grid view sorts by name/import-order/duration via a toolbar dropdown (no per-column sort in grid, since there are no columns).

## 3. Export dialog + preset system

### 3.1 ExportPreset schema

```rust
pub struct ExportPreset {
    pub name: String,
    pub container: Container,             // Mp4, Mov, WebM, Gif, ImageSequence
    pub video: Option<VideoEncodeSpec>,    // None for audio-only export
    pub audio: Option<AudioEncodeSpec>,
    pub resolution: ResolutionSpec,        // §3.2
    pub frame_rate: FrameRatePolicy,       // §3.3
    pub alpha: bool,                       // requires alpha-capable container+codec (§3.4)
    pub faststart: bool,                   // MP4/MOV: moov atom at front, web-streamable
    pub loudness_target: Option<LoudnessTarget>,  // LUFS target, per 09-audio-mixer.md §normalization
}

pub struct VideoEncodeSpec {
    pub codec: VideoCodec,                 // H264, Av1, Vp9, ProResLikeMezzanine, Gif, Png (sequence)
    pub quality: QualityMode,              // Crf(f32) | Bitrate { target_kbps, max_kbps } | Lossless
}

pub struct AudioEncodeSpec { pub codec: AudioCodec /* Aac, Opus, Pcm */, pub bitrate_kbps: Option<u32> }

pub enum ResolutionSpec { SourceFormat, Explicit { w: u32, h: u32 }, Scale(f32) }   // "SourceFormat" = active SequenceFormat's w/h (01 §4)
pub enum FrameRatePolicy { MatchSequence, Explicit(FrameRate) }
```

Worked instance (the "Social 9:16" built-in, §3.5, serialized shape — illustrative, not a wire-format commitment):

```json
{
  "name": "Social 9:16",
  "container": "Mp4",
  "video": { "codec": "H264", "quality": { "Crf": 20.0 } },
  "audio": { "codec": "Aac", "bitrate_kbps": 128 },
  "resolution": "SourceFormat",
  "frame_rate": "MatchSequence",
  "alpha": false,
  "faststart": true,
  "loudness_target": { "integrated_lufs": -14.0, "true_peak_dbtp": -1.0 }
}
```

`ExportPreset` is **app-level config**, not document state (01 §11 explicitly excludes render/export presets from the data model). This is a deliberate departure from the existing raster `ExportProfile` (`crates/photonic-core/src/document.rs:115`, stored in `Document.export_profiles`, applied via `run_export_profile`) — that mechanism suits small per-document SVG/PNG export recipes; video presets are heavier (encoder params, licensing-gated codec choices) and users expect them to follow *them*, not the file, matching every NLE's preset-browser convention. **Per-SequenceFormat export is still document-aware**: the export dialog offers "export all formats" which loops the render loop (02 §7) once per `Sequence.formats` entry using that entry's dimensions as `ResolutionSpec::SourceFormat`, producing one output file per aspect ratio in a single job (progress reports as N sub-jobs).

### 3.2 Resolution & fit
`ResolutionSpec::SourceFormat` is the default and covers CAP-012 (aspect switch reflects in export automatically — no separate resolution step needed for the common case). `Explicit`/`Scale` exist for "half-res proxy-quality delivery" and custom social crops the format list doesn't cover.

### 3.3 Frame rate policy
`MatchSequence` (default) passes the sequence's declared `FrameRate` straight to the encoder — no retiming. `Explicit` triggers CFR conform at export time (02 §4's frame-selection logic, run once over the full range rather than live) — this is the **only** point in the pipeline that forces CFR (see §6.2, VFR policy).

### 3.4 Alpha-capable outputs (CAP-021)
Enabling `alpha: true` restricts `container`+`codec` to a validated set; dialog greys out incompatible combinations rather than allowing an invalid preset to be built:
- **WebM + VP9** with alpha (libvpx alpha side-channel — decoder support is broad: Chrome, Firefox, Photonic's own preview).
- **MOV + ProRes 4444** (FFmpeg's `prores_ks` encoder — LGPL-fine per D-03 research, no GPL/patent entanglement) — best fidelity, large files, "mezzanine" use case (compositing round-trips into other NLEs).
- **APNG** — universal alpha support, no video compression efficiency; fine for short loops/stickers.
- **PNG sequence** — always available fallback, one file per frame, alpha guaranteed, used when a downstream tool needs raw frames.
- H.264/AV1/GIF: alpha toggle disabled (H.264/AV1 in mainstream containers don't carry alpha in a broadly-compatible way; not worth the encoder-specific hacks for v1).

### 3.5 Built-in preset catalog

| Preset | Container | Video | Audio | Notes |
|---|---|---|---|---|
| Social 9:16 | MP4 | H.264 (openh264), CRF ~20 | AAC 128k | faststart on; `ResolutionSpec::SourceFormat` assumes a 9:16 `SequenceFormat` is active |
| Social 1:1 | MP4 | H.264, CRF ~20 | AAC 128k | same family, square format |
| Social 16:9 | MP4 | H.264, CRF ~20 | AAC 128k | |
| Master AV1 High | MKV | AV1 (SVT-AV1, preset speed 4, CRF ~20) | Opus 192k | mezzanine/archival master; default "best quality" pick per licensing research (D-03: AV1 has no patent pool, SVT-AV1 best speed/quality among the unencumbered options) |
| Web H.264 | MP4 | H.264, target-bitrate ladder (1080p ~6 Mbps) | AAC 128k | faststart on; broad-compatibility web delivery |
| WebM VP9 Alpha | WebM | VP9 + alpha, CRF ~24 | Opus 128k | motion-graphics overlay delivery (CAP-021) |
| ProRes Mezzanine | MOV | ProRes 4444 (`prores_ks`) | PCM | round-trip / handoff to other tools; alpha on by default |
| GIF | GIF | paletted, dithered | — | short loops; frame-rate capped 15–24fps UI hint (file-size guardrail) |
| PNG Sequence | — (folder) | PNG per frame | — | alpha always on; numbered output matching §1.2's detection pattern (round-trips as an image-sequence import) |

HEVC is **deliberately absent** from the built-in catalog (patent-pool risk flagged in constraints research) — not blocked outright (a user with a paid HEVC license path could add a custom preset if their FFmpeg build has one), but the shipped LGPL FFmpeg build has no HEVC encoder anyway, so it's moot for v1 unless the user brings their own binary (§3.7).

### 3.6 Custom presets
User-defined presets persist in app-level config (`~/.config/photonic/export_presets.json` or platform-equivalent — same directory family as other app-level prefs, not the project file). CRUD via the export dialog's "Save as preset…" / preset manager list; built-ins are read-only (shown with a lock icon, "Duplicate to edit").

### 3.7 Bring-your-own FFmpeg
Advanced/settings-level escape hatch: point `photonic-video`'s sidecar at a user-supplied FFmpeg binary path (e.g., one with x264/HEVC/fdk-aac built in) instead of the shipped LGPL build. This keeps Photonic's **shipped** binary clean of copyleft/patent-encumbered code (SPEC constraint: "external codec tooling runs only as separate subprocesses") while not preventing power users who've separately licensed/built those codecs from using them. Surfaced as a warning-gated preference, not a default-visible setting — out of scope for the built-in preset catalog and CAP-013's test (which only exercises shipped presets).

### 3.8 Export dialog walkthrough
1. **Trigger**: "Export…" from the file menu, timeline toolbar, or per-sequence context menu. Opens with the last-used preset for that sequence pre-selected (session-level memory, not document state).
2. **Preset picker** (left column): built-ins (§3.5, locked icon) then custom (§3.6), grouped; search/filter box for large custom libraries. Selecting a preset populates all fields below — every field stays editable after selection (picking a preset is a starting point, not a lock; editing fields marks the preset selector as "Custom (based on X)").
3. **Fields** (right column, driven by `ExportPreset` schema §3.1): container, video codec + quality mode (CRF slider ⟷ target-bitrate field, mutually exclusive UI, matches `QualityMode` enum), audio codec + bitrate, resolution (`SourceFormat` default, radio to `Explicit`/`Scale`), frame rate (`MatchSequence` default), alpha toggle (disabled unless container/codec combination in §3.4's allow-list), faststart checkbox (MP4/MOV only), loudness target dropdown (Off / -14 LUFS streaming / -23 LUFS broadcast / custom, per 09).
4. **Format loop**: if `Sequence.formats.len() > 1`, a checklist of formats appears ("Export: ☑ 16:9 ☑ 9:16 ☐ 1:1") — each checked format renders as one sub-job in the same export job (§3.1); unchecked ones are skipped, not queued.
5. **Range**: work range (`Sequence.work_range`, 01 §4) pre-fills in/out; "Entire sequence" override available.
6. **Estimate strip**: read-only line showing approximate output size (duration × target bitrate, or CRF-based historical-average heuristic per codec — same estimator used for the disk-space guardrail, §6.5) and estimated render time (from the perf-budget table, 02 §8, scaled by clip count/effect count) — set expectations before the user commits, not a hard promise.
7. **Pre-flight** (risk mitigation, §8): offline-media check runs here, before the "Export" button becomes enabled; failures list the specific offline assets with a "Relink…" shortcut inline.
8. **Progress**: non-modal panel (`ExportProgress`, 02 §7) — frame/total, fps, ETA, cancel button; GUI stays interactive (D-02-consistent — export never freezes the app). Multi-format jobs show N progress bars or one aggregate with a sub-job label.
9. **Completion**: toast + "Reveal" action; batch/multi-format completion lists each output path.

---

## 4. Aspect-ratio system UX

### 4.1 Adding/switching SequenceFormats
`Sequence.formats: Vec<SequenceFormat>` (01 §4) — the UI equivalent is a format-tab strip above the program monitor (mirrors D-02: canvas stays the monitor, timeline docks below). "Add Format" offers the CAP-012 list (16:9, 9:16, 1:1, 4:5, Custom w×h) plus removal (guarded: can't remove the last format, can't remove `active_format` without switching first — matches existing guarded-delete UX patterns e.g. `delete_layer`, `crates/photonic-mcp/src/handlers/doc_export.rs:64`). Switching `active_format` is instant (no recompile cost beyond normal graph recompile, 02 §2) and is itself a `TimelineCmd::SetActiveFormat` (01 §10), undoable — distinct from `SetSequenceFormat`, which adds/updates/removes format *entries*.

### 4.2 Per-clip reframe
`Clip.reframe: HashMap<usize, ClipTransform>` (01 §5) keyed by format index. Editing surface: with a format tab active and a clip selected, the monitor shows format-safe-area overlay + on-canvas transform handles (pos/scale/rotation) exactly like the existing vector-transform gizmo pattern; dragging writes/updates the `reframe` entry for the *current* format index only — other formats' reframes are untouched. "Reset reframe for this format" clears the entry (falls back to `clip.transform` base, auto-fit per §4.3). Reframe edits route through the same `SetClipProp{old,new}` command as any other clip property (01 §10) — no new undo plumbing.

### 4.3 Smart initial reframe heuristic
When a `SequenceFormat` is added or a clip is first viewed under a format with no `reframe` entry yet, compute a **center-weighted auto-fit**: scale clip source to cover the target aspect (crop-to-fill, not letterbox — matches social-editing convention), centered. This is a pure function of source dimensions + target dimensions (no ML, no per-frame cost) — computed once and written as the initial `reframe` entry on first view (so it becomes editable state, not a recomputed default that silently drifts).

**Auto-subject-detect reframe (post-v1, explicitly deferred):** `photonic-matte::remove_background` (`crates/photonic-matte/src/lib.rs`) already provides an on-device salient-object matte (U²-Net-p) that could locate a subject bounding box per frame/keyframe to bias the center-weight toward the subject rather than the geometric center. Deferred because: (a) it's a *per-frame* video cost, not the one-shot still-image cost `photonic-matte` is built for — needs a sampling strategy (first frame? scene-representative frame? tracked across cuts?) that's genuinely a design problem, not a wiring problem; (b) none of the three acceptance stories require it (AS-1's reframe is manual per CAP-012's test wording); (c) keeps this phase's scope to the frame-graph/UI work already load-bearing for P4. Ship center-weighted heuristic + full manual override in v1; revisit with `photonic-matte` once the sampling-strategy question has an owner.

### 4.4 Mobile preview toggle
D-02 locks the existing layout (timeline bottom-docked, canvas-as-monitor). Mobile preview is a monitor-level toggle (not a mode switch): renders the active format at a phone-frame chrome (notch/safe-area guide overlay) inside the same canvas area, using the already-compiled frame graph — zero extra engine cost, purely a monitor-decoration change in `photonic-gui`. Toggle lives in the monitor's toolbar next to the format tabs, independent of them — a user can preview any format (16:9 included) inside the phone chrome to sanity-check how a horizontal format looks pillarboxed on a handset, not just to check the 9:16 format itself.

### 4.5 Safe-area guide overlay
Overlay draws two guide rectangles (toggleable independently, on by default when a non-square format is active): an **action-safe** box (~90% of frame, keeps subject motion clear of edge crops on some players/thumbnails) and a **title-safe** box (~80%, where captions/lower-thirds should stay clear of platform UI chrome — profile picture, like/share buttons overlaying the bottom-right on Reels/TikTok/Shorts style layouts). Guides are visual-only (never affect render output, never persisted per-clip) — same category as the existing canvas rulers/grid, a viewing aid drawn by `photonic-gui`, not a document property.

---

## 4b. Starter title-template library (D-11, ships P6)

The v1 answer to "CapCut has templates" (PM review): a small shipped set of **vector title/lower-third templates** — Photonic's native strength — with stock music explicitly out (SPEC Non-goals).

- **What a template is:** a `.photon` snippet (a `VectorDoc`-shaped document fragment: styled text + shapes) with pre-authored `PropertyTrack` keyframes for entrance/exit (01 §6 — e.g., slide-up + fade lower third, typewriter title, scale-pop caption card). No new file format: templates are ordinary documents with animation tracks.
- **Shipped set (v1, ~8–10):** lower third (name/role), centered title + subtitle, corner bug/watermark, end card (CTA + handle), caption card, quote card, chapter marker, subscribe/like reminder. Each in light/dark variants driven by swatch slots, not baked colors, so DESIGN.md-consistent recoloring is one click.
- **Where they live:** built-ins ship in the app bundle (read-only); user templates in an app-level library dir (same family as export presets, 05 §3.6). "Save selection as title template" from the vector canvas exports the selected group + its keyframe tracks.
- **Browsing/insertion:** a "Titles" group in the Effects browser drawer (04 §4.1; interior owned here) shows thumbnail previews (rendered via `HeadlessRenderer` at first browse, cached in the sidecar dir). Drag-drop onto the timeline creates a `VectorDoc` asset (embedded, 01 §3 `AssetSource::EmbeddedVector`) + a clip with the template's `PropertyTrack`s copied in — after insertion it's plain editable vector content + keyframes, no live template link (deliberate: no template-versioning complexity in v1).
- **Editing:** double-click the clip → the vector document opens for editing in Vector mode (mode switch per 04 §1.2), exactly like any embedded vector asset; text is retargeted by normal text editing.
- **MCP:** `list_title_templates` + `insert_title_template { template, track_id, start_* , text_overrides? }` — 2 tools added to 10's catalog at P6 (10's count updates then; flagged there by the doc-drift gate automatically).

## 5. Convert/Compress tool (standalone transcode)

Addresses the user requirement to "change video types" / compress independent of building a timeline.

- **Entry points**: media pool context action ("Convert/Compress…") on any video/audio asset; MCP tool `transcode_media` for headless/automation parity (CAP-019).
- **Mechanism**: reuses the export engine (02 §7) on a **single-clip synthetic sequence** — internally: wrap the source asset in a throwaway `Sequence` with one clip spanning its full duration at native resolution/rate, run the identical render loop against a chosen `ExportPreset` (built-in or custom, §3), discard the synthetic sequence after the job (never persisted to `TimelineProject`). This guarantees the transcode tool and the "real" export path can never drift (one encoder integration, one code path — CAP-019 parity by construction, same principle 02 applies to playback/export).
- **Options exposed** (a reduced export dialog): target preset (any from §3.5/3.6, defaulting to a smart suggestion — e.g. source is HEVC/4K → suggest "Web H.264"; source is a PNG sequence → suggest "Master AV1 High"), output location (defaults to source's folder, `<name>_converted.<ext>`, never overwrites source), and a **compression-only** shortcut: "Reduce file size" preset picker for users who don't want to think about codecs at all:

  | Shortcut | H.264 CRF | AV1 CRF | Typical result vs. source |
  |---|---|---|---|
  | Small | 28 | 34 | Aggressive — messaging-app-safe file size, visible softening |
  | Medium | 23 | 28 | Balanced — default suggestion, visually near-lossless at normal viewing distance |
  | Large | 18 | 22 | Light — archival-adjacent, modest size reduction only |

  Codec choice (H.264 vs AV1) for the shortcut follows the same smart-suggestion logic as the preset picker above; the CRF just adjusts within whichever codec is chosen.
- **Result handling**: on completion, offers "Add to Media Pool" (imports the output as a new `MediaAsset`, running the normal import pipeline §1.4) — so a converted file immediately becomes usable in the current project without a manual re-import round-trip.
- **Batch**: multi-select in media pool → "Convert/Compress…" applies one preset to all selected, queued as sequential jobs (shares the export engine's single render-loop worker budget — see 02 §7 threading; not run in parallel against the same GPU/encoder resources).

---

## 6. Policies

### 6.1 Color metadata on export
Per 03 (linear-light Rec.709 working space, D-09): export always tags output container/stream metadata with the correct transfer/primaries (Rec.709 for SDR delivery; sRGB tagging equivalence noted for web-consumed still/PNG-sequence outputs since most consumers treat them interchangeably at SDR). Colour-space *conversion* choice follows the sequence's working state (D-09): `LinearRec709Sdr` by default, `LinearRec2020Hdr` where an approved HLG/PQ workflow selects it. The former "HDR is a non-goal" statement was repealed by the S3 amendment; HDR export tagging and the encoder matrix are owned by [22 §7](22-dji-advanced-workflows.md#7-d-13--hdrhlg-10-bit-color-pipeline). Mistagged/absent metadata is treated as a bug, not a preset option — every built-in preset in §3.5 tags correctly by construction (encoder args set the tag, not user input).

### 6.2 VFR input policy

> **Frame-rate conform (the far more common case).** A clip whose source rate differs from the sequence rate — 30 fps material on a 24 fps timeline — selects the source frame **covering** the mapped source time. Nearest-source-frame, no blending, deterministic, and identical in preview and export. 30 → 24 drops every fifth frame; 24 → 30 repeats one; both judder on motion, and that is the correct v1 trade because it is exact and cheap. Emit `Media::FrameRateConformed` (`Info`) per affected clip, surfaced through the import triage. Owned by [38 §3](38-sequence-semantics.md#3-frame-rate-conform). Pulldown/inverse-telecine is a **different problem** and is out of scope — see [32 §6.3](32-engine-contracts.md#63-relationship-to-pulldown).
**Recommendation: pts-true playback, CFR only at export (matches 02 §4's stated approach).** Import never conforms variable-frame-rate source to constant rate — the frame graph evaluates at the exact requested tick regardless of source cadence (02 §2's "pure function of tick" property holds either way). Two consequences spelled out for this doc's ownership:
- **Proxies** (02 §6) are generated all-intra at the *source's native* rate — proxy generation is a quality/seek-speed aid, not a conform step; it must not silently change effective frame rate the editor sees.
- **Export** is the only conform point (§3.3, `FrameRatePolicy::Explicit`): when the target preset's frame rate differs from source, the render loop resamples by evaluating the frame graph at each *output* tick (nearest-source-frame or interpolated — v1 ships nearest-frame only; motion-interpolated retiming is a SpeedMap-adjacent post-v1 feature, not built here). This keeps VFR handling out of playback's critical path entirely, where it would risk A/V sync (CAP-004, SS-3).

### 6.3 Max resolutions
No hard cap enforced in the data model or import pipeline (a `MediaProbe` records whatever the source is, including 8K) — but the export dialog surfaces a soft warning above 4K output ("high resolution — export time and file size will be substantial") rather than silently accepting it, since SS-1's perf budget targets are defined at 1080p/4K-via-proxy, not native 8K playback.

### 6.4 Crop-to-fill vs letterbox default
§4.3's auto-fit defaults to crop-to-fill (matches social-editing convention and avoids dead pillarbox/letterbox bars nobody asked for). This is a policy choice worth stating explicitly rather than leaving implicit in the heuristic description: a user who *wants* letterboxing (e.g. preserving a full 16:9 frame inside a 1:1 export for a "cinematic" look) gets it by manually scaling down via the reframe handles (§4.2) — there is no separate "fit mode" toggle in v1, because CAP-012's normative behavior is reframe as a per-clip transform override, not a discrete fit-mode enum per clip. If user feedback post-v1 shows letterbox is a common enough want, promote it to a one-click toggle that pre-computes the scaled-down `reframe` entry — additive, not a data-model change (it would still just write a `ClipTransform` into the same `HashMap`).

### 6.5 Disk-space guardrails
Sidecar cache dir (`<project>.photon.cache/`, 01 §9) holds proxies, waveforms, thumbnails, keyframe indices — all rebuildable, none required for correctness (01 §9, 02 §6). Guardrails:
- **Size cap** (`ProjectVideoSettings`, 01 §2) — default cap (e.g. 20 GB, user-adjustable) on the sidecar dir; LRU eviction by content-hash access recency once exceeded, matching the cache-table eviction policy already defined for engine caches (02 §5) — same eviction philosophy, disk instead of GPU/CPU memory.
- **Low-disk warning**: before starting a proxy-generation batch or an export job, check free space against a conservative estimate (proxy: ~source duration × codec bitrate; export: preset bitrate × duration) and warn (not block — user may know better) if the estimate exceeds available free space.
- **Cache clear action**: media pool / project settings exposes "Clear cache" (with a "proxies will regenerate on demand" caveat) — deletes the sidecar dir; next access rebuilds lazily, never a correctness issue, only a one-time perf hit.

---

## 7. MCP tool surface (CAP-019 parity)

10-mcp-tools.md is the normative tool catalog; names here defer to it. Every tool below routes through the same `photonic-video` engine calls and `timeline/ops.rs` pure functions the GUI uses (01 §10, "GUI and MCP both call them") — no parallel logic path, matching the existing MCP handler convention (`crates/photonic-mcp/src/handlers/doc_export.rs` already does this for SVG/PDF/raster export).

| Tool | Maps to |
|---|---|
| `import_media` | §1.4 pipeline, given one or more paths; returns `AssetId`s + initial probe status |
| `list_media` | §2.1 — bins, assets, metadata columns, offline status |
| `relink_media` | §2.2 manual relink; matches by `content_hash` then filename |
| `generate_proxies` / `remove_proxy` | §2.3, batch or single-asset, wraps `EngineCmd::GenerateProxies` |
| `list_export_presets` / `save_export_preset` / `delete_export_preset` | §3.6 CRUD over app-level preset store |
| `export_sequence` | §3 — takes `ExportPreset` (by name or inline), sequence id, format selection, range; returns a job id, polled via `get_job_status` (02 §7) |
| `set_sequence_format` | §4.1, `op: add\|update\|remove` field picks the mode against a `Sequence`'s `formats` list — one tool, not three |
| `set_clip_prop` | §4.2 reframe edits go through this universal property setter (`path` = the format-indexed `reframe` entry), same as any other clip property — no dedicated reframe tool |
| `transcode_media` | §5 standalone convert/compress tool |

Every tool call is a normal document/engine mutation or query — no MCP-only side channel, so a scripted CAP-019 story (e.g. AS-1's full pipeline) produces byte-identical output to the same actions performed in the GUI, verified by the parity test in 11.

---

## 8. Risks + test hooks (feeds 11-testing-phasing.md)

| Risk | Mitigation / test hook |
|---|---|
| Image-sequence name-pattern detection false-positives/negatives (mixed batches, non-contiguous numbering) | Probe-matrix corpus (11) includes: contiguous sequence, gapped sequence, two interleaved sequences in one folder, single stray file matching the regex — verify correct collapse/non-collapse per case |
| Preset catalog drifts from actual encoder capability (e.g. shipped FFmpeg lacks an encoder a preset assumes) | Golden-probe test per built-in preset (CAP-013): export a fixed test sequence with every §3.5 preset, `ffprobe`-verify container/codec/dimensions/duration/alpha-presence match the preset's declared spec — run in CI against the exact shipped LGPL FFmpeg build, not a dev machine's system FFmpeg |
| Alpha round-trip silently drops (encoder ignores alpha flag, or player doesn't render straight/premultiplied correctly) | Golden-frame corpus (11) includes a checkerboard-alpha test asset; export each alpha-capable preset, decode + sample known transparent/opaque regions, assert exact channel values |
| Export blocked by offline media discovered only mid-job (bad UX: user waits, then fails at frame N) | Pre-flight check before render loop starts: walk the compiled export's referenced assets, fail fast with a clear list of offline assets if any are unreachable — never starts encoding partial/placeholder frames into a real export |
| Relink-by-hash false match (two different files, same head/tail/len by coincidence) | Acceptable residual risk (documented, not solved): xxh3 head+tail+len is a heuristic, not cryptographic; filename fallback + user confirmation dialog on batch relink mitigates blind auto-accept; noted for 11 as a "won't test exhaustively, accept as known trade-off" |
| Disk-space guardrail estimate wildly wrong for exotic codecs/resolutions | Estimate function unit-tested against the golden-probe corpus's known output sizes (§8 row above) rather than a formula nobody validates |
| Convert/Compress tool's synthetic-sequence wrapper diverges from real export path over time (someone "optimizes" one path only) | Single code path by construction (§5) is the primary mitigation; CAP-019 parity test additionally exercises the `transcode_media` MCP tool and asserts byte-identical output to an equivalent manual single-clip sequence export |

---

## 9. Open choices resolved (no TBDs)

- Folder watch: **not in v1** (§1.3) — recommendation, not a gap.
- HEVC in built-in catalog: **excluded by default, available via bring-your-own-FFmpeg** (§3.5, §3.7) — resolves the "flag HEVC" instruction without simply banning power users.
- Preset storage location: **app-level JSON config, not document** (§3.1, §3.6) — explicit departure from existing `ExportProfile` documented with rationale.
- Auto-subject-detect reframe: **post-v1, named dependency on `photonic-matte`** (§4.3) — not a vague "future work," a specific unresolved design question (frame-sampling strategy) blocking it.
- VFR handling: **pts-true playback, CFR at export only, nearest-frame resample** (§6.2) — matches 02's engine-level recommendation, spelled out at the export-preset level here.
