# 02 — Engine: photonic-video

**New crate** `crates/photonic-video/`. Owns all temporal evaluation and video I/O: media probing, decode, the frame-graph compiler/evaluator, caches, playback clock, audio engine host, proxy generation, export. Depends on `photonic-core` + `photonic-render` (+ wgpu). Never on `photonic-gui`. **Decisions:** D-03, D-08, D-09, D-10.

```
crates/photonic-video/src/
  lib.rs            // VideoEngine facade
  media/            // probe.rs (ffprobe), pool.rs, relink.rs, keyframe_index.rs
  decode/           // sidecar.rs (process mgmt), reader.rs (rawvideo/pcm pipes), scheduler.rs, ring.rs
  graph/            // ir.rs (FrameGraph), compile.rs, eval.rs, cache.rs, ops/ (one file per GraphOp family)
  playback/         // controller.rs (state machine), clock.rs (audio-master), prefetch.rs
  audio/            // engine.rs (cpal host), mixer.rs, dsp/ (eq, compressor, ducking), waveform.rs
  proxy/            // generate.rs, policy.rs
  export/           // render_loop.rs, encoder.rs (ffmpeg sidecar encode), presets.rs, progress.rs
  session.rs        // EngineSession: per-open-document runtime state
```

---

## 1. VideoEngine facade & threading model

```rust
pub struct VideoEngine { /* owns engine thread + audio thread + worker pool */ }

impl VideoEngine {
    pub fn new(gpu: Arc<GpuContext>) -> Self;                  // shares wgpu Device/Queue with renderer
    pub fn open_session(&self, doc: Arc<Mutex<Document>>) -> EngineSession;
}

// GUI/MCP → engine: commands. Engine → GUI: state + frames.
pub enum EngineCmd { Play, Pause, Seek(Tick), Step(i32), SetLoop(Option<(Tick,Tick)>),
                     SetActiveSequence(SequenceId), SetProxyMode(ProxyMode),
                     Export(ExportJob), GenerateProxies(Vec<AssetId>),
                     ScrubSeek(Tick), SetPreviewTarget(PreviewTarget), SetPreviewQuality(PreviewQuality),
                     SeekSource { asset: AssetId, time: Tick }, Shutdown,
                     Probe(AssetId), InvalidateRange(SequenceId, Tick, Tick) }

pub struct EngineFrame {                      // what GUI presents
    pub texture: Arc<wgpu::Texture>,          // Rgba16Float, linear premultiplied (D-09)
    pub time: Tick, pub sequence: SequenceId,
}
```

Threads:
- **Engine thread** — owns playback state machine, graph compile/eval scheduling. Receives `EngineCmd` via crossbeam channel; publishes `EngineFrame` + `EngineStatus` (playhead, dropped frames, cache stats) via `arc_swap` (`ArcSwap`/`ArcSwapOption`) — GUI never blocks on engine.
<!-- spec-assert: dep-present arc_swap -->
<!-- spec-assert: dep-absent triple_buffer -->
<!-- spec-assert: symbol-exists crates/photonic-video/src/session.rs::EngineSession -->
<!-- SD-8 (27 §3): earlier drafts claimed frames publish via `triple_buffer`/watch; the real mechanism is `arc_swap`. These pin the dependency reality so a re-introduction of `triple_buffer` (or removal of `arc_swap`) reds the gate. -->
<!-- SD-6/SD-10 (27 §3): the EngineCmd variant list and the cache-key type are structural claims better carried by a `spec-source` anchored block (40 §3.1); the anchored mechanism is implemented and fixture-tested (tools/spec-extract/tests/cases/anchored/), but wiring a live block into this doc requires the drift gate to run with `--spec-extract`, deferred so the lint-job gate stays a single cheap step. Tracked here rather than faked with per-variant assertions (40 §2). -->

- **Audio thread** — cpal callback; real-time-safe (no locks/allocs in callback); pulls from lock-free ring filled by mixer worker. Owns the **master clock** (§4).
- **Decode workers** — pool (N = cores/2, min 2) driving sidecar processes + pipe reads.
- **GUI thread** — presents latest `EngineFrame` texture in the monitor; sends intents.

Document access: engine snapshots the parts it needs (active sequence + referenced assets/graphs, cheap `Clone` per 01) on change-notification rather than locking mid-playback. The change signal is `CommandHistory::revision` — which today bumps only on checkpoint ops, so P1 extends it to bump on execute/undo/redo with a public accessor (03 §2.1 owns this prerequisite; `doc_generation` in this doc = that extended counter). It triggers re-snapshot + targeted cache invalidation (§5).

## 2. Frame-graph IR

```rust
pub struct FrameGraph { pub nodes: Vec<IrNode>, pub edges: ..., pub output: IrNodeId }  // arena, topo-sorted at build

pub enum IrOp {
    // sources
    DecodeVideo { asset: AssetId, src_time: Tick, proxy: bool },
    DecodeStill { asset: AssetId },
    RasterVector { vref: VectorRef, doc_state: VectorStateKey, w: u32, h: u32 }, // via HeadlessRenderer / GPU path (03)
    SolidColor { color: LinearColor },
    // ops (each = one wgpu render/compute pass, or a CPU fallback for export determinism)
    Transform2D { mat: Mat3, sampling: Sampling },
    Effect { kind: EffectKind, params: ResolvedParams },       // params already keyframe-evaluated;
                                                               // arity 0..N from the EffectKind registry (0-input = generator, e.g. MaskShapeGen — 08 §3)
    Grade { ops: Vec<ResolvedGradeOp> },                        // CDL, curves, HSL, LUT — keyframe-resolved form of 01/07's authoring GradeOp
    Merge { mode: BlendMode, opacity: f32 },                    // 2-input over; uses COMPOSITE_SHADER modes (03)
    CaptionOverlay { cue_batch: CaptionBatch },
    Crop, Resize { w: u32, h: u32, fit: FitMode },
    MatteExtract { model: MatteModel },                         // U²-Net inference via photonic-matte — CPU worker-thread op,
                                                                // NOT a GPU pass; result cached aggressively (08 §3 "slow node")
    TextGen { block: ResolvedTextBlock },                       // styled text raster for graph Text nodes (08 §3)
    ChannelSplit { channel: Channel },                          // Image → single-channel Mask
    ChannelCombine,                                             // 3-4 Mask inputs → Image
    Output { w: u32, h: u32 },
}
```

Properties (normative):
- **Pure function of (document snapshot, sequence, format, tick, quality flags).** Same inputs ⇒ identical graph ⇒ identical pixels. This is what makes caching, export determinism (SS-3), and golden tests (11) possible.
- All keyframe evaluation happens at compile time — the IR carries resolved params. The evaluator is time-ignorant.
- Every node has a **content hash**: `hash(op, resolved params, input hashes)`. Cache key (§5).

### Compilation (`graph::compile`)

For sequence S, format F, tick t. **Scope order is normative** — rationale and the alternatives considered are in [35 §2](35-model-decisions.md#2-effect-scopes-and-the-adjustment-clip-interaction).

Because clips within a track are non-overlapping, **at tick `t` a track holds at most one clip**: it is empty, carries **content**, or carries an **Adjustment** operator. There is no third case to disambiguate.

1. **Per content clip**, build the chain:
   `asset effects` (`MediaAsset.effects`/`.grade` — inherited by every instance) → source op (`Decode`/`RasterVector`/`Nested`; nested sequences compile recursively with a cycle guard) → speed/trim source-time mapping → default `Transform2D` (evaluated `AnimProps` + the reframe override for F) → `Effect` nodes (enabled, ordered) → `Grade` if set.
2. **If `clip.composition` is set (D-06):** the composition substitutes the clip's **source op only** — instantiate the user `NodeGraph`, bind `ClipIn` to the clip's source op (after trim/speed mapping), and feed the graph's `Output` into the remainder of step 1's chain. Identity transform / empty effects / `None` grade fold away, so a pure comp costs nothing extra. Node `AnimProps` evaluate at t. Type-check ports; on error fall back to the plain source plus default chain and surface a diagnostic — never black-frame silently.
3. **Per track:** take that track's covering content clip (plus transition partner) → `Track.effects` → `Track.grade`. **Track effects apply to that track's own content only, never to the accumulator** — affecting lower tracks is what an Adjustment clip is for. A track whose covering clip is an Adjustment has no own content at t, so its track stack does not apply.
4. **Fold tracks bottom → top:** `acc = Merge(acc, track_result, Track.blend, Track.opacity)`. Then, if this track's covering clip is an **Adjustment**, apply its stack to the accumulator: `acc = adjustment.grade(adjustment.effects(acc))`. An Adjustment therefore affects everything below it and nothing above it.
5. **Master:** `Sequence.master_effects` → `Sequence.master_grade`.
6. `CaptionOverlay` from enabled caption tracks (cues covering t). **Captions composite after the master stack** so a master look never re-grades them — subtitles are burned after grade, per broadcast practice.
7. Splice the project graph (`TimelineProject::project_graph`) between the caption result and `Output`, keeping the node graph as the final-look surface.
8. **TimeOffset expansion:** a graph `TimeOffset { offset }` compiles by duplicating its upstream subgraph re-evaluated at t−offset; duplicates dedup naturally via content hashing. Soft cap: 4 distinct offsets per composition. Generalised by the source-range contract in [32 §1](32-engine-contracts.md#1-source-range--the-one-mechanism-for-temporal-access).
9. Constant-fold + dead-branch-eliminate (invisible clips, opacity 0, disabled nodes, out-of-zone effects).

**Transition handles.** A transition samples its partner past that clip's out point, into remaining source handle. Where the handle is shorter than the transition, **clamp the transition to the available handle and emit a diagnostic**; where it is zero, do not render the transition and warn. Never extend the sequence or move clips to make room ([38 §1.2](38-sequence-semantics.md#12-insufficient-handle)).

**Frame-rate conform.** For a clip whose source rate differs from the sequence rate, map the tick through trim and speed to a source time and select the source frame **covering** it — nearest-source-frame, no blending, identical in preview and export. Emit one `Info` per conformed clip. Blended conform is expressible only once [32 §1](32-engine-contracts.md#1-source-range--the-one-mechanism-for-temporal-access)'s source-range contract exists and must not be built before it ([38 §3](38-sequence-semantics.md#3-frame-rate-conform)).

Effect **applicability** is enforced, not advisory: a manifest declares which scopes it is valid at ([30 §2.3](30-effect-catalogue.md#23-capability-and-applicability)), and the compiler refuses an effect placed at a scope it does not declare.

Compile budget: < 0.5 ms typical (pure CPU, no I/O) — measured in 11.

### Evaluation (`graph::eval`)

Topological execution on wgpu. Each `IrOp` = one pass writing an `Rgba16Float` texture from a pooled allocator (transient textures reused via LRU pool keyed by size). CPU reference path (`eval_cpu`) implements the same ops in f32 for golden tests + the raster/compositor-parity cases (03 §6).

Interactive **preview targets**, Draft/Full quality tiers, import readiness stages, and time-to-paint rules are owned by [24-preview-media-load.md](24-preview-media-load.md). This section remains the decode/proxy mechanics those rules consume.

## 3. Decode: ffmpeg sidecar (D-03)

- **Process model:** one persistent `ffmpeg` process per (asset, quality) actively decoding, spawned via `ffmpeg-sidecar`-style management (own `decode::sidecar` module; we control args): `ffmpeg -ss <keyframe_before(t)> -i <file> -f rawvideo -pix_fmt yuv420p|yuva444p ... pipe:1` + a parallel PCM pipe for audio assets (`-f f32le`). Reader threads parse framed output into `DecodedFrame { pts: Tick, planes }`.
- **Seeking:** input-level `-ss` to the keyframe index entry ≤ target, then decode-forward discarding until pts ≥ target. **Keyframe index** built at import (`ffprobe -skip_frame nokey -show_frames`) and cached in the sidecar dir; makes seek cost = one GOP decode.
- **Ring buffer per active clip source:** decoded frames ±N around playhead (default 16 fwd / 4 back at preview quality). Prefetcher (playback/prefetch.rs) looks ahead along play direction and across upcoming cuts (starts decoders for the next clip early — cut-ahead warmup ≥ 500 ms).
- **Upload:** YUV planes upload to GPU as R8/RG8 textures; `DecodeVideo` eval pass does YUV→linear-RGB (BT.601/709 per probe) + premultiply → `Rgba16Float` (D-09). No CPU colorspace work.
- **Stills/images:** decoded once via existing `RasterImage::from_encoded`, uploaded, cached by asset.
- **Vector frames:** `RasterVector` renders via the existing `HeadlessRenderer::render_rgba_with_opts` (CPU-composited, correct) in P3, migrating to the GPU scene path when 03's texture-target work lands; cached by `VectorStateKey` = hash(referenced nodes' state + evaluated animated props + size).

Failure containment: sidecar crash/EOF → reader reports `DecodeError`; scheduler restarts process (max 3, backoff); frame slot renders diagnostic placeholder. A wedged pipe never blocks the engine thread (all pipe reads on worker threads with deadlines).

## 4. Playback & A/V sync

- **Master clock = audio** (research-confirmed best practice). `clock.rs`: when playing, position = audio samples consumed by cpal callback (sample-accurate, monotonic); when paused/scrubbing, a settable software clock.
- Video presents the frame whose [pts, pts+frame) interval covers clock time; late > 1 frame ⇒ drop (counted in `EngineStatus.dropped`); early ⇒ hold.
- **Scrub:** seek requests coalesce (latest-wins) per engine tick; audio scrub plays short windowed grains at the target (optional, off by default).
- **Step (CAP-004):** pause, clock = snap(t ± 1 frame), evaluate exactly that tick.
- Speed changes (SpeedMap) affect source-time mapping only; the sequence clock always advances 1:1 with audio.

## 5. Caching & invalidation

| Cache | Key | Storage | Evictor |
|---|---|---|---|
| Decoded-frame rings | (asset, quality, pts) | CPU planes | ring position |
| Node results | IR content hash | GPU textures (`TexturePool`, byte-budgeted, default ~1.5 GB) | **LRU over unpinned entries**; the displayed `Output` tick is pinned. A separate 16k-entry *rendered-validity* map is flushed wholesale on overflow — that costs re-renders of still-resident textures, not eviction |
| Vector rasters | VectorStateKey | GPU | LRU |
| Stills / uploads / vector rasters | `(AssetId, Tick, proxy)`, `VectorStateKey` | GPU | clear-on-cap (session caches). **Stills are keyed on `AssetId` alone today** — a defect, since a still then uploads at full resolution regardless of preview scale ([26 K-C8](26-kdenlive-mlt-parity.md#k-c8--key-the-still-image-cache-on-requested-size)) |
| Waveform pyramids, thumbnails, keyframe indices | asset hash | disk sidecar | size cap |

Invalidation is **hash-natural**: edits change resolved params ⇒ different node hashes ⇒ old entries age out; no manual dirty ranges except `InvalidateRange` for asset relink/proxy swap. `doc_generation` only triggers re-snapshot + recompile, which is cheap.

## 6. Proxies

- Policy (`proxy::policy`): offer proxy generation when source > sequence preview resolution × 1.5 or codec is long-GOP 4K+. `ProxyMode = Auto | ForceProxy | ForceOriginal` (session, not document).
- Format: half/quarter-res **all-intra H.264 (openh264-compatible baseline) in MP4**, generated by sidecar ffmpeg at import-time or on demand (background job with progress). Stored in sidecar cache dir, keyed by content hash — survives project moves, rebuildable at any time (never required for correctness: CAP-014 toggle).
- Export always uses originals.

## 7. Export

`export::render_loop`: for frame f in work range → compile graph at tick(f) (quality = full, proxy = false) → eval GPU → readback `Rgba16Float` → convert to encoder pix_fmt (linear→transfer per target, tone-unmapped Rec.709 in v1) → write to encoder sidecar stdin (`-f rawvideo`), audio mixed offline (09) piped as f32le on a second input. Muxing, container, codec flags from `ExportPreset` (05 owns the preset catalog: H.264/openh264, AV1/SVT-AV1 or rav1e, WebM/VP9, alpha-capable outputs for CAP-021, GIF).
- Deterministic: same project + preset ⇒ bit-identical rawvideo stream (SS-3 golden basis); encoder output compared by decode+PSNR in tests (11).
- Runs on worker threads; `ExportProgress { frame, total, fps, eta }` events; cancellable between frames; GUI stays live.
- Headless/MCP export uses the identical render loop — CAP-019. **Status:** `EngineCmd::Export` is currently a NotImplemented stub; the live export path runs from `handlers/video.rs::run_export_job` over a **dedicated** `EngineSession` on a frozen document snapshot, driving `export::render_loop` directly. Wiring `EngineCmd::Export` so the GUI can export is [26 K-0.1](26-kdenlive-mlt-parity.md#8-k-0--foundations); audio muxing is K-0.7.

## 8. Perf budgets (verified in 11)

| Item | Budget |
|---|---|
| Graph compile (10 tracks, 3 active clips) | < 0.5 ms |
| Eval 1080p, 3 layers + grade + captions | < 8 ms GPU |
| Seek-to-photo (cached GOP) | < 50 ms |
| Cold seek (index + 1 GOP decode, proxy) | < 150 ms |
| Cut-ahead warmup | ≥ 500 ms before cut |
| Export overhead vs pure encode | < 25% wall time |
