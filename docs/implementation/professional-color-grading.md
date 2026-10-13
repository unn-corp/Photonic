# Professional color grading implementation

Status: Milestone 1A implemented; Milestone 1B, the Color workspace, and parts
of Milestones 3 and 5 are in progress. The full roadmap remains in progress. This is a staged implementation of the approved roadmap,
not a claim of Resolve parity, complete managed color, HDR mastering, or facility readiness.
Native-managed video-track SDR preview is now available under explicit limits;
qualified full-resolution ProRes MOV delivery is available. Other managed
timeline stages and export formats remain gated.

## Publication checkpoint — 2026-10-13

The cloud continuation through checkpoint 137 was published as
`d98ee3585a2a4977f2d8df588e35ae07910d0b41` on
`feat/professional-color-foundation` at the owner's request. The restored
source matches the saved checkpoint; the tracked-file patch also passes a
reverse-application check against its original base `30f221c`.

The final October 9 qualification recorded 3,187 library tests and 246
integration tests passing, with 14 tests ignored, plus Clippy, native build,
formatting and native scope UI checks. These are prior-run results for the
restored source, not a claim that the full suite was rerun after the cloud
workspace reset. The checkpoint archive SHA-256 is
`dcfab2bf0b9c1cf8fa7b66462430bc3a7d590753dac8b66ec1f271b623576861`.
The complete roadmap and hardware-performance qualification remain unfinished.
Earlier statements that continuation work is uncommitted/unpushed describe
historical checkpoints; this publication entry supersedes that delivery status.

## Post-publication acceptance fixes — 2026-10-13

The initial GitHub run `38339955008` passed lint (formatting, Clippy,
generated MCP docs, spec drift and dependency checks) and PDF preflight,
but exposed two integration checks missing from the earlier targeted run:

- MCP discovery still expected only the original two keyframe targets. Its
  assertion now includes local/stage grading and checks that batch discovery
  preserves the same operator/stage address schema as individual keyframes.
- The keyboard-focus source scanner combined production focus checks with
  key events inside inline test fixtures. Those 16 tests now live in explicit
  test-only modules; production widget tokens are unchanged and the scanner
  is unchanged. Repeated-arrow coverage establishes focus on a separate frame
  before key events, matching egui's filter-installation lifecycle.

All 11 MCP discovery contracts, all 16 numeric-control tests and the original
keyboard-focus integration check pass locally. The full workspace all-feature
run is still in progress. Its preset test initially used the cloud's read-only
inherited config directory; the exact test passes with isolated writable XDG
configuration. This is a setup correction, not a relaxed assertion.

The acceptance-index generator also now ignores fenced Markdown examples and
its own negative-test corpus during whole-repository scans, while preserving
inline code in criterion descriptions. All seven isolated pass/fail corpora
and the real workspace check pass. A full-workspace regression is added so
example pollution cannot silently return. This index currently covers eight
explicitly numbered criteria; it is not a complete video/color acceptance list.
The full professional roadmap remains in progress.

### Full-workspace regression follow-through

The first complete local all-feature run on October 13 finished with 3,733
passing tests, four failures and 24 ignored tests. This was not a clean pass.
In addition to the isolated XDG preset configuration issue above, the broader
run exposed two stale export expectations and a real checker throughput gap:

- The late-frame atomic-export test now uses an out-of-range exposure instead
  of treating an ordinary solid source as unsupported. It also exports and
  decodes the supported two-frame sequence separately, and still verifies that
  invalid delivery preserves the existing output bytes. All three export-engine
  tests pass after the correction.
- The managed contract now accepts bounded Deflicker and uses an explicit
  unknown-effect sentinel for refusal coverage. All six contract tests pass.
- Structural extraction consumed roughly 13.25 seconds before checking. A
  bounded pool of at most four independent parsers now consumes results and
  errors in original source order. Serial/parallel byte equality and first-error
  ordering are tested; the original ten-second acceptance budget is unchanged.
  All 14 spec-extract unit/integration tests pass, including that budget.

GitHub run `38343529423` on `6ca80f57` passed lint, PDF preflight and strict
video/render/export acceptance, but its full workspace job stopped at the stale
late-frame export fixture. The follow-through fixes were published as `3fa0adf`.
The complete local all-feature rerun then passed: 3,738 tests, zero failures,
24 ignored. GitHub run `38346178519` passed lint, PDF and strict video acceptance,
but its full job hit a single 67 ms scrub-coalescing timing sample against the
unchanged 5 ms budget. That sample passed on retry; the retry then exposed the
spec timing test competing with a second full extraction in the same parallel
test executable (11.68 seconds). Only those two extraction sites now share a
mutex; the timer starts after acquisition and the ten-second budget is unchanged.
The independent fixture/structural checks remain parallel. All eleven checks
pass locally with four test threads, including the unchanged timing gate;
GitHub run `38352089510` on checkpoint `21096975` is green: Ubuntu build/all-feature
tests, lint, PDF and strict video acceptance all passed. The scheduled nightly
job was correctly skipped on push. No full-roadmap completion is implied.

## Cloud continuation — mixed caption metrics (checkpoint 138, 2026-10-13)

Native-managed caption words now honor their authored font-size overrides
(1–512 pixels), using the existing word → cue → track style cascade. The
resolved rich-text metrics are shared by glyph rendering, word bounds and
cue backgrounds; preview scaling applies to both cue defaults and word
metrics. Size changes participate in the content hash, and malformed metrics
are refused before shaping. Underline fallback width follows the effective
word size. Cue position, wrapping, animation and shared decorations retain
their existing bounds; independent per-word layout/decorations remain gated.

Legacy caption resolution emits no metric override. Its default span attributes
and content-hash stream remain unchanged. No document-schema or MCP-tool change
is needed: existing word-level style edits now reach the managed renderer.

Focused qualification currently passes 112 video tests (compiler/scaling/cache,
caption behavior and actual GPU text geometry), six renderer span tests and
four MCP caption workflows. The new MCP workflow authors a mixed-size cue,
checks undo/redo and document save/reload, renders a preview, exports three
high-depth ProRes frames, verifies the manifest and compares decoded coverage
and color with preview. The full product rerun then passed 3,742 tests with
zero failures and 24 ignored; the spec harness was additionally checked with
four concurrent test threads after its extraction isolation change. Formatting
and full-workspace Clippy also pass (existing warnings remain). The 3,738
all-feature passes above belong to the preceding published checkpoint. GitHub CI passed on `21096975`. Separate explicit qualification also passed:

- SS-3: all 18,000 frames of a ten-minute 320×180 Legacy synthetic beep/flash
  timeline were exported and decoded; worst audio/video offset remained below
  one 30 fps frame. This took 1,032 seconds on the cloud computer and makes no
  high-resolution realtime or native HDR claim.
- Two native source-end fixture tests and the explicit MCP recovery test passed.
  Invalid end-of-source previews fail without an image; export publishes neither
  output nor manifest, and valid seeks recover without document mutation.
- The explicit missing-subject-model MCP test passed. Preview/export refuse
  clearly without a provisioned model; no model was downloaded for this test.

These extra targeted runs are separate from the aggregate count above. Broader
footage, high-resolution soak, external hardware and the remaining software
roadmap are still unfinished.

## Cloud continuation — source-pixel pointwise processing (checkpoint 139, 2026-10-13)

Differently sized native MediaIn sources can now pass through bounded Invert,
ChromaKey and LumaKey operations before an explicit Resize. These operations
retain source-pixel logical dimensions and source-resolution scope taps, including
Full → Draft → Full cache reuse. Resize remains mandatory before sequence output
or other processing; this does not enable arbitrary variable-extent graph geometry.

Every retained native source and pointwise intermediate is charged against the
existing 16,777,216 padded-texel bound. Duplicate hashes do not reduce that charge.
Source dimensions remain bounded to 8192 per axis. Native source and intermediate
extents are validated before rendering, including topological input availability.

Focused compiler, actual GPU/CPU resize and source-tap comparisons, and the new
intermediate-allocation budget test pass. The numerical matrix includes native
stills and video, Fit/Fill/Stretch, intermediate sizes and repeated quality changes.
The new end-to-end MCP workflow also passes for still/video sources and all
three fit modes with an intermediate 21×7 Resize: authoring, undo/redo,
save/reload, preview, ProRes decode, manifest validation and relink/refusal
recovery. Its independent binary64 reference applies bounded straight-color
Invert before premultiplied resampling. The first full run passed 3,744 tests
and failed one exact diagnostic-compatibility assertion: early extent validation
had changed the existing forward-edge refusal text. Production now preserves
that diagnostic without weakening validation. The complete corrected all-feature
rerun passed 3,745 tests, zero failures and 24 intentional ignores across 84 test
executables; full-workspace Clippy and formatting pass with existing warnings.
Publication and remote CI verification are pending for this checkpoint.

## Current roadmap checkpoint — 2026-10-13

This checkpoint takes precedence over earlier dated status, pending lists and
qualification counts below. The chronological entries remain evidence for their
individual increments, not a claim that the full roadmap is finished.

- Branch: `feat/professional-color-foundation`.
- Published implementation checkpoint: `21096975`, committed and pushed to the
  same branch on `origin`, with green GitHub CI. Checkpoint 139 above is local
  continuation work until its publication and qualification are recorded.
- Goal: **resumed at the user's request on 2026-10-05** in a cloud checkout of
  documentation checkpoint `30f221c`. The full end-to-end roadmap remains
  incomplete; no completion percentage is asserted. Work is being performed
  directly, without delegated coding agents.
- Cloud continuation has completed raw matte-output inspection and qualified
  native Gaussian blur/unsharp/glow, bounded Invert and AP1 luma/chroma keys, sixteen additional extended-channel blend modes and nine explicitly bounded artistic blend modes, a bounded
  clip/project node subset, authored-pixel Draft scaling, and all six catalog native input
  transition kinds for adjacent qualified media/solid-color sources, plus native solid-color clips and solid/ellipse/rectangle/static-polygon graph generators and compile-time Switch routing. Later increments qualify bounded vectors, decorated titles/captions and title/vector/nested transition partners; see the current results below. Catalog-created Gaussian effects are covered through actual
  MCP authoring and ProRes export. Exact incremental results and remaining limits
  are recorded in the Cloud continuation section below.
  Historical checkpoint totals do not imply current-tree release acceptance.

### Implemented and qualified within explicit boundaries

1. **Trust foundation:** read-only panel viewing, gesture/structural undo,
   atomic lock-aware edits, dependency diagnostics and export refusal, Legacy
   SDR preservation, asynchronous scopes and the shared MCP scope-job interface.
2. **Managed color:** Photonic-owned AP1/ACEScct transforms; qualified sRGB
   stills and selected high-depth video inputs; explicit PQ-display and HLG-scene
   source interpretation; scene-linear extended-range/alpha handling; supported
   asset/clip/group/shared-look/track/master ordering and nested sequences;
   native creative LUT contracts and pinned dependencies; qualified SDR preview
   and Full/original ProRes delivery. Unsupported stages still fail visibly.
3. **Color workspace:** monitor, shot strip, gallery and verified references,
   versions/copying, scope selection/settings and persistent scope dock sizing,
   windows/handles, native curve and qualifier picking, input-node selector,
   matte/image-node output preview and curve/qualifier input picking for clip/local/linked shared look stages, independent look editing, clip/local/shared/group scalar and primitive-window keyframe authoring, checked track
   compositing drafts, and MCP save acknowledgement in GUI history state.
4. **Advanced grading:** qualified native exposure, offset, printer lights,
   roll-off, saturation/vibrance, contrast, CDL/log wheels, normalized artistic
   white balance, explicit Kelvin white adaptation, linked wheel means and
   three-range tonal exposure, master/RGB and five secondary curve families, HSL qualifiers;
   ellipse/rectangle/gradient windows with clip-local scalar animation, static and fixed-topology animated closed Bézier graph keys, and graph-based soft Boolean combinations; typed image/matte graph, key
   mixers, matte application, median/grow/erode/Gaussian/levels refinement.
   GUI/MCP exact input inspection covers unambiguous and explicitly addressed
   native curve/HSL correctors and qualifier-key source nodes.
5. **Assistance/finishing foundations:** editable bounded Legacy SDR shot-match
   proposals, initial verified asset archives, render/dependency manifests,
   actual high-bit SDR ProRes decode/reimport checks, and limited reference-GPU
   playback/scope measurements. These do not establish complete restoration,
   HDR mastering, facility readiness or portable reproducibility.

### Remaining work, in roadmap order

| Area | Work still required |
| --- | --- |
| Managed pipeline | General effects beyond Gaussian/box/directional/catalog Surface and Lens blur/Median/Dust & Scratches, bounded measured Deflicker across asset/clip/track/master and multiple instances, native detail/unsharp/glow/vignette/mosaic and fixed-canvas Pinch/Spherize/Ripple/Perspective, Emboss/Find Edges/Chromatic Aberration and Drop Shadow, bounded transfer-space Posterize/Threshold and extended Desaturate/Channel Mixer/Black & White/Photo Filter/Color Balance/catalog Curves/Levels and Alpha View/Unpremultiply diagnostics/coverage Outline/linear Hue-Saturation/Vibrance/transfer Invert, spatial bilateral denoise, deterministic uniform grain, bounded Invert and AP1 luma/chroma keys, general variable-extent graph geometry beyond the qualified source-pixel Invert/chroma/luma-key chains, explicit MediaIn intermediate Resize and fixed-canvas normalization path, high-resolution and long-soak multi-offset playback and broader subject-mask footage qualification, plus broader geometry beyond the qualified fixed-canvas nodes and static polygon generator, unqualified nonadjacent incoming transition edges (all six known outgoing kinds now follow the existing eased opacity-fade contract), remaining vector/logo features beyond the bounded whole-document solid-path source, title-style animation beyond the qualified whole-title 200ms SlideUp entrance, and per-word decorations and independent layout/animation beyond the qualified mixed font-size/family/weight/color caption spans and bounded decorated title/caption paths; mixed-source/conversion and proxy/cache precision qualification; full stage-order and independent-reference acceptance. |
| Color workspace/scopes | Broader workspace/view arrangements beyond the qualified local Color presets; live shared reference ownership beyond qualified immutable cross-sequence gallery copies, and local/group version arrangements beyond qualified explicit project-shared-look named versions; broader cross-sequence inspection arrangements beyond qualified selected-member group/compound and local/shared workflows; HDR absolute scopes/scales and broader high-resolution qualification beyond bounded continuous AP1 relative-stop waveform/RGB parade/RGB waveform/histogram, paired MCP measurement and live AP1/SDR statistics; safe asset/cache cleanup and portable color dependencies. |
| Primaries/secondaries | Further white-balance models beyond explicit Planckian source/target Kelvin adaptation; additional tonal-range workflows beyond the qualified linked three-range exposure and RGB wheel mean; broader multiple-window UX beyond qualified static and fixed-topology animated Bézier graph keys and primitive/Bézier Boolean combinations; broader compound/stabilization coordinate contracts beyond qualified clip-relative, post-transform primitive animation. |
| Tracking/graph | Qualify broader footage, occlusion recovery, scale/rotation/perspective and Bézier/compound/stabilization tracking beyond the bounded ordinary-clip fixed-reference and opt-in bidirectional short-term translation proposals; extend the qualified synthetic and short real-footage static-target balance/match/tracked-version/export/reimport/archive regressions to broader representative footage and grading workflows; broader cross-sequence tracking and structural/non-scalar look/group animation beyond the qualified scalar, primitive-window and fixed-topology Bézier lanes (selected-member group/compound image, matte and input inspection, local/shared outputs, native eyedroppers and checked static edits are implemented). |
| Assistance/restoration/performance | Chart balance, scene grouping, stronger editable subject/skin/depth assistance; temporal and broader noise restoration beyond the qualified AP1 bilateral spatial filter, detail/sharpening, film/sensor-model grain beyond qualified deterministic uniform AP1 grain, halation/bloom, gamut compression/legalization; explicit preview/final quality, resource/cache optimization and longer representative benchmarks. |
| HDR/professional finishing | HDR scopes/mastering, output tone/gamut transforms, actual HDR high-bit export metadata/sample verification; calibrated/native/external monitoring, panels and optional vendor RAW adapters; conform/relink, annotations, complete dependency reports and portable archives; exact shipped-artifact commercial audit. |
| End-to-end acceptance | Balance → match → tracked isolation → alternate version → export/reimport with save/undo/locks and GUI/MCP parity; independent numerical references, unchanged Legacy goldens, interaction/throughput/memory/cache/soak checks. Clean build artifacts only after complete acceptance. |

**Matte-output continuation:** the earlier committed tap plumbing has now been
completed with shared raw-weight preview/statistics, a dedicated read-only MCP
tool, GUI output selection, stale-result rejection and rendered/native-window
checks. See the Cloud continuation entry for exact scope and evidence. Broader cross-sequence arrangements and structural animation remain separate work; clip/local/linked look output inspection, native eyedroppers and checked look edits are implemented as recorded below.

**External qualification:** calibrated HDR/external-output hardware, grading
panels, vendor SDK/RAW access and broader hardware soak evidence remain necessary
for the corresponding advertised capabilities. Their absence does not justify
claiming software work complete or silently substituting Legacy SDR behavior.

**Resume point:** inspect the checkpoint and this current status first; qualify each new increment on the current tree, then continue through
the remaining rows. Preserve qualification artifacts and existing implementation.
The historical handoff is superseded by
[`handoff-2026-10-05.md`](../../_arcwright-output/handoffs/handoff-2026-10-05.md).

## 1A — Trustworthy existing grading

Implemented on `feat/professional-color-foundation`:

- Color-panel and tool-tab viewing do not insert correctors. Quick controls and
  empty tool editors create a corrector only on adjustment; add/remove/reorder
  remain explicit operations. Viewing controls does not clamp stored parameters.
  Empty curve channels now use virtual neutral control points; opening an
  H-H/H-S tab cannot materialize a non-neutral hue correction.
- `SetGrade` supports gesture coalescing; GUI parameter adjustments use normal
  history execution, while stack operations remain discrete. Undo restores the
  original absence of a grade when a drag creates its first corrector.
- Shared clip/track grade operations reject locked tracks. MCP multi-target copy
  constructs and validates every command before applying its batch; empty,
  duplicate, and source-as-target batches leave document and history unchanged.
- Paste Attributes now validates each target through the same clip-property
  operation. A locked target rejects the entire multi-shot paste before any
  command reaches history.
- Missing LUTs, unsupported operators, and unresolved roto masks bypass the
  entire affected corrector and produce structured operator-specific diagnostics.
  Disabled correctors and globally bypassed grades do not block export.
- Timeline grading diagnostics identify the asset, clip, track, or master grade
  that owns the affected corrector. This disambiguates reused operator IDs in
  GUI warnings and MCP frame/status payloads; standalone graph grades still
  have no timeline owner.
- Diagnostics from an independent or shared clip-look stage also identify that
  stage, so a missing LUT in the look is distinguishable from a missing LUT in
  the clip's ordinary grade. The additive stage field preserves older payloads.
- Compile diagnostics follow the exact published frame. The Color Controls and
  scopes show warnings; MCP `get_engine_status.grading` includes provenance and
  structured diagnostics. Final export refuses a frame carrying grading errors.
- RGB parade accumulates separate per-column channel waveforms. Downsampling the
  displayed parade/waveform includes all source columns, including narrow peaks.
  Histogram analysis also counts straight linear RGB values below zero and above
  reference white before display clamping; GUI and MCP show per-channel counts.
  The scopes panel offers a spatial RGB overlay alongside the side-by-side parade.
- Waveform, parade and RGB overlay can plot full-range signal or remap video-legal
  8-bit code levels 16–235 to the display height. Measured full-range bins are
  preserved; excursions collect at the top/bottom edge and are counted explicitly
  in the panel and MCP scope payload. This is SDR scope scaling, not HDR scopes.
- The vectorscope now offers 75% or 100% R/Y/G/C/B/M reference targets whose
  plotted positions use the same selected BT.709/BT.601 Cb/Cr bin calculation
  as the measurement. The I/Q and approximate skin-axis guides remain; target
  markers are signal references, not a calibrated gamut/legalization claim.
- GUI scopes submit asynchronous GPU readbacks and poll without waiting. One
  bounded pending measurement and one completed result are retained. Document,
  snapshot, sequence, requested tap, actual tap, dimensions, scope type, and
  matrix changes invalidate incompatible results. MCP scopes include actual
  measured tick/revision, interpretation and compact spatial RGB counts; texture
  readback runs on a blocking worker rather than the async runtime. Playback can display a previous
  measured frame with its explicit time/revision label while the next arrives.
- Legacy grading arithmetic is unchanged for supported correctors.

Validation includes core gesture/lock tests, headless egui document-integrity
checks (including all empty tool tabs and out-of-range existing parameters), MCP
atomic rejection, compiler missing-LUT diagnostics, and CPU/GPU scope patterns.
A real GPU/FFmpeg export regression confirms a missing LUT fails without
publishing output. It also confirms an incomplete 1D shaper blocks export;
explicitly disabling that corrector permits export. A complete combined 1D+3D
LUT renders through final export.
Broader checks are recorded below when completed.

Remaining foundation qualification:

- Interactive playback measurements on reference hardware.
- Further professional scope scales/targets. The shared MCP scope-job interface is now
  implemented as `measure_scopes` (see the later increment). MCP `get_scopes` now accepts `vectorscope_matrix`
  (`bt709` by default or `bt601`) and returns the selected matrix with its
  vectorscope grid; this changes only the measurement, not source interpretation.
- Verify nested-sequence provenance in MCP status and export presentation.
  Grading diagnostics now carry an additive root-to-leaf `sequence_path` and
  display it when a failing corrector is inside a nest; standalone diagnostic
  payloads and older serialized results remain compatible.

## 1B — Managed color (in progress)

Implemented contracts and development qualification:

- Persist versioned sequence configuration, OCIO/configuration/transform hashes,
  independent display/export choices, explicit unknown-input policy, and asset
  interpretation with clip overrides. Legacy fields serialize exactly as before.
- Shared undoable operations author input interpretation and create a managed
  sequence copy. The original stays intact and active; locked clip edits fail.
  MCP `set_input_color` exposes asset and clip overrides with the same undo,
  validation, and lock behavior; passing null clears an override for inheritance.
  The Color workspace now offers explicit asset and clip input-interpretation
  editors for managed sequences. They use the sequence's pinned configuration
  digest, offer range and matrix choices, and keep panel viewing read-only.
- Input resolution rejects mismatched configuration pins instead of ignoring an
  override. Unknown configuration versions round-trip but cannot execute.
  Read-only source preflight reports each visible clip as explicit, assumed or
  unresolved; an explicit interpretation that requests range or matrix from
  absent probe metadata is unresolved until the tag or an explicit value is
  supplied. A sequence-level color-space assumption also requires range and
  matrix metadata for video; still images do not require those video tags.
  Clip overrides take precedence over asset interpretation. MCP
  engine status includes the findings and the selected clip shows its
  interpretation in Color Controls, with unresolved diagnostics shown first.
- The unrestricted managed timeline compiler remains unavailable. A qualified
  native-managed video-track subset now renders through the sequence viewer
  and exports through a narrow ProRes MOV delivery path described below.
  Unsupported effects, grades, nested-sequence configurations, and other timeline stages
  produce diagnostics and fail closed. GUI warnings and MCP status expose the
  limit; the full managed conversion path is not advertised as ready.
- A historical OCIO 2.5.2 probe measured ACES transforms. Its executable
  sources and generated artifacts have been removed. Commercial use appears
  permitted by the published licenses, but the owner selected Photonic-owned
  transforms. OCIO runtime integration is deferred.
- A self-built Rust ACEScct scalar reference now encodes and decodes extended
  AP1 values using the published piecewise equations, including the negative
  toe and the specified half-float ceiling on decode. This is a reference
  building block, not an enabled managed render path. The equations are from
  the [Academy's ACEScct specification](https://docs.acescentral.com/encodings/acescct/).
- A Photonic-owned colorimetric reference now derives linear sRGB/D65 to AP1
  primaries from published chromaticities and white adaptation, and decodes
  signed extended sRGB without clamping. Matrix/white/round-trip tests pass.
  Its inverse display-referred conversion is also checked across negative and
  above-white samples; this is not an ACES scene output rendering transform.
  Display-referred sRGB remains display-referred after this conversion; it is
  not an ACES camera input or inverse rendering transform. The managed
  renderer and output transform remain gated.
- Explicitly tagged ACEScct values now have Photonic-owned scalar conversion
  to ACEScg, and exposure is defined on the scene-linear result. Tests cover
  negative, middle-gray and above-white channels. This reference is not wired
  into the video graph until input interpretation, grading, display and export
  stages can be qualified as one pipeline.
- A Photonic-owned Y′CbCr scalar reference now decodes full or limited 8–16-bit
  codes for BT.601, BT.709 and BT.2020 non-constant-luminance matrices without
  clipping code excursions. Neutral, saturated-red, below-black, above-white
  and invalid-code tests pass. It returns nonlinear RGB; source transfer and
  chroma reconstruction remain separate qualification steps, so this is not
  yet a managed decoder path.
- The native reference also includes the inverse BT.709 source OETF with a
  signed extension for excursions and a continuous-coefficient inverse
  BT.2020 source OETF. Linear BT.2020 primaries can be converted into AP1 using
  the same independently derived white-adapted matrix method. These are
  distinct from sRGB display decoding and do not stand in for camera-specific
  input transforms.
- The explicit BT.709 and BT.2020 non-constant-luminance scalar input references
  now compose code range, matrix, source transfer and gamut conversion into
  scene-linear AP1. BT.601 is deliberately excluded from this composed path:
  its matrix tag does not identify source primaries. The renderer still rejects
  managed sequences until chroma reconstruction, operator domains and output
  transforms are qualified together.
- Configuration cache identity includes all pins, but is not yet connected to a
  managed render cache because managed rendering remains disabled.
- A separate `native_managed` sequence draft now records Photonic-owned
  transform and grading-semantics revisions with independent SDR display and
  export selections. Its serialized identity cannot be confused with preserved
  OCIO-backed `managed` drafts. Creating a native copy leaves the Legacy SDR
  source unchanged; compiler preview and export both fail closed until its
  source interpretation, render stages and output transform are qualified.
  Native source preflight marks media inputs unresolved until a distinct
  BT.709-scene or BT.2020-scene interpretation is authored with explicit range
  and matching matrix; it explicitly declines to reuse an OCIO interpretation.
  Asset and clip native input fields persist separately and have undoable shared
  edit operations with track-lock and validation checks. The Color panel shows
  the mode-specific diagnostic and offers separate native asset/clip editors.
  MCP `set_native_input_color` uses the same shared operations and reports
  rejected edits without changing history. MCP `create_native_color_draft`
  creates an undoable copy while leaving the source active; converting a
  managed draft again is rejected. Color Controls offers the same explicit
  draft-copy action for a Legacy SDR sequence. The native renderer remains gated.
  Asset-level OCIO and native input changes also reject when a directly or
  nested referencing clip sits on a locked track, so shared source
  interpretation cannot alter that locked shot through the media pool.
- LUT declarations can now name Photonic-owned ACEScg, ACEScct or encoded sRGB
  with a pinned native transform revision. Legacy SDR rejects those LUTs with
  its existing grading diagnostic; managed LUT execution remains gated.
- Full-resolution decode now requests planar 16-bit YUV from FFmpeg for probed
  9/10/12/14/16-bit YUV sources, including big-endian source formats. A
  4:2:0 source retains subsampled 16-bit chroma planes instead of asking
  FFmpeg to upsample to 4:4:4; odd-width plane offsets are validated. A
  lossless FFV1 round trip verifies probe, sidecar decode, exact 16-bit plane
  codes and native GPU conversion. Its plane
  storage reaches `R16Float` GPU upload
  and the floating working texture without an 8-bit intermediate. A real
  ProRes export/redecode and a GPU gradient test each retain over 256 levels;
  the GPU test also checks partial alpha. This is a Legacy SDR precision path,
  not managed source color interpretation.
- Native 16-bit grayscale/RGB/RGBA still images now decode and resample through
  RGBA16 samples directly into the `Rgba16Float` working texture. A real 16-bit
  PNG gradient retains over 256 levels, and a premultiplied-alpha resample test
  prevents transparent color bleed. Other still formats and embedded vectors
  retain their existing decode paths; Legacy stills still assume sRGB transfer.
  The bounded raster-job estimate now allows a nominal full-resolution 4K
  RGBA16 still while accounting for decoded, converted and packed buffers;
  4K throughput and peak-process-memory qualification remain outstanding.
- An isolated native GPU YUV source converter now has BT.709-scene and
  BT.2020-scene entry points, with full/limited code-range decoding, signed
  source transfer decoding, AP1 gamut conversion and premultiplied alpha.
  Its separate shader leaves Legacy SDR conversion unchanged. GPU/scalar
  tests cover 8-bit extended codes, partial alpha and 16-bit samples with
  half-float output tolerance. Native 16-bit YUV upload packs each source code
  losslessly into two `Rg8Unorm` channels and reconstructs it before transfer
  decoding, without requiring an optional wgpu texture feature. A GPU test
  distinguishes adjacent 16-bit luma codes near black and confirms that
  switching between native and Legacy SDR conversion leaves Legacy output
  unchanged. A checked
  bridge maps persisted native input
  intent to this converter, rejects inherited range/mismatched matrix/unknown
  versions, and supports padded graph-pool output. A decoded-frame bridge
  derives subsampling from the actual frame planes and rejects a missing siting
  declaration before GPU submission; its padded variant keeps out-of-frame
  pixels transparent. Native input intent now
  records explicit 4:2:0 chroma siting (left, center, top-left, top,
  bottom-left or bottom); preflight reports missing siting for probed 4:2:0
  assets, and the isolated GPU converter offsets chroma sampling according
  to that choice. Spatial 4:2:0 patterns exercise horizontal and vertical
  placement, including odd-width 8-bit and 16-bit frames checked against scalar
  reconstructed chroma values; the 16-bit pattern covers all six authored
  locations. A separate 16-bit 4:2:0 pattern verifies below-black and
  above-white luma against the scalar reference. Packed 16-bit chroma is decoded per texel before
  bilinear interpolation so a byte carry cannot corrupt the reconstructed code.
  ffprobe's chroma-location tag is retained as advisory metadata
  and included in the unresolved-input diagnostic; it never silently supplies
  the native interpretation. The location names follow [FFmpeg's documented chroma locations](https://www.ffmpeg.org/doxygen/8.1/pixfmt_8h_source.html).
  An isolated GPU ACEScg/AP1 linear ↔ ACEScct pass now matches the scalar
  reference on negative, above-white, partial-alpha and transparent samples.
  A separate scene-linear exposure pass matches the scalar reference at
  negative, zero and positive stops, preserves premultiplied alpha, and rejects
  non-finite or out-of-range settings. A composed source → linear exposure →
  ACEScct encoding test checks operation order against the scalar reference.
  Neither pass changes Legacy SDR.
  These are component references only: final working-texture precision,
  display/export transforms and the complete managed graph remain unqualified,
  so native sequences still fail closed for preview and export.
- A scalar ACES 2 output tone-scale component now matches the Academy's
  published luminance table at 100, 500, 1,000, 2,000 and 4,000 nit targets.
  Isolated AP0/AP1↔JMh scalar appearance models now round-trip neutral and
  saturated colors, and their lightness/luminance conversions agree on neutral
  reference values. The Academy output stage uses the AP0 model after its
  input clamp; the AP1 model alone is not an output transform. A neutral
  AP1 → clamped AP0 → JMh → tone-scale test reaches the published SDR gray
  luminance without clipping the scene-linear grade beforehand. Both components
  are attributed under Apache-2.0 in
  `THIRD_PARTY.md`; no CTL runtime or upstream transform asset is bundled.
  A separate Rec.709/D65 100-nit sRGB-piecewise display-encoding component
  matches IEC sRGB sample values and rejects non-finite input. A narrow
  neutral-only scalar reference composes the AP1 input, AP0 appearance,
  tone-scale, Rec.709 limiting appearance and display encoding stages and
  refuses chromatic input. An isolated in-gamut chroma shaper now derives its
  360-hue AP1 reach table and keeps neutral colorfulness and hue stable while
  shaping saturation. Independent JMh and chroma vectors, a qualified output
  gamut policy, white limiting, complete stage composition,
  full-output GPU parity are still required before an SDR output
  transform is available.
- A separate Photonic SDR v1 **scalar candidate** now composes these stages for
  straight AP1 RGB and uses a fixed-lightness/hue Rec.709 boundary search with
  a continuous colorfulness shoulder. It also accepts premultiplied RGBA by
  unpremultiplying before nonlinear mapping and restoring coverage afterward;
  transparent pixels with nonzero RGB are rejected. A small RGB grid, extended
  inputs and a neutral comparison remain finite and bounded. This is Photonic's gamut policy,
  not the Academy ACES 2 Output Transform. It has no independent chromatic
  reference images, GPU implementation or output integration, so it does not
  lift the native managed render/export gate.
  A cross-component test runs native YUV conversion and scene-linear GPU
  exposure into the scalar candidate, checking neutral output, below/above
  reference values and partial/zero alpha. An isolated GPU Rec.709-to-sRGB
  display-encoding pass agrees with the scalar stage on those premultiplied
  values, including signal excursions. Neither test implements or qualifies
  the full GPU output transform.
  An independent chromatic comparison now uses the Academy's ACES 2.0 CG
  config v4.0.0 through the system OCIO 2.5.1 command as a **test tool only**;
  its config SHA-256, twelve focused vectors and a 64-sample RGB grid are
  pinned in the qualification tests. The candidate fails the focused gate on a warm highlight:
  `[4, 0.3, 0.1]` produces approximately `[1.0, 0.680, 0.637]` against the
  reference `[1.0, 0.499, 0.433]`. On the grid, the worst channel error is
  approximately **0.362** for `[0.18, 4, 0]`: red is 0.362 instead of zero.
  A trial adaptive lightness search improved the small set but still failed
  the grid, so it was discarded. The fixed-lightness gamut boundary needs a
  proper gamut and lightness mapping. Both qualification tests remain
  explicitly ignored and managed preview/export remain gated. The reference
  config and OCIO executable are not bundled with Photonic; only the numeric
  test fixture and its BSD-3-Clause notice are retained.
  A separate ACES 2-style scalar output candidate now finds the limiting
  display-cube cusp, builds a corner-aware hue table, fits the upper hull,
  estimates the lower hull and reach shell, and maps along focus lines. The
  assembled `Aces2SdrOutput` API applies tone scale, chroma shaping, gamut
  mapping and sRGB display encoding, including correct alpha handling. Its
  independent 64-sample grid, 32 edge/highlight/negative samples, and twelve
  chromatic vectors
  all pass at under **0.002 per encoded channel**. An isolated WGSL output pass
  now consumes the same prepared matrices and corner-aware hue table. It
  matches the scalar API on floating AP1 highlights, negative inputs and
  partial alpha, and its 96-sample independent corpus stays within **0.005 per
  encoded channel** after half-float output. This is sampled component
  qualification; image-level comparisons, dense hue-boundary tests, frame
  performance, managed graph ordering and export/reimport acceptance still
  precede preview/export integration. A GPU-native source → exposure → scalar
  output smoke test also preserves premultiplied coverage. The older
  Photonic SDR v1 fixed-lightness candidate remains distinct and its two
  explicitly ignored tests continue to document its failure.
- FFmpeg decode now preserves 8-bit and high-bit-depth 4:2:2 as planar
  `yuv422p` and `yuv422p16le`, and non-alpha 8-bit 4:4:4 as `yuv444p`,
  including odd-width plane boundaries. The GPU upload keeps complete 16-bit
  codes and reconstructs horizontal chroma siting
  while leaving full-height chroma on its own scanline. Lossless FFV1
  encode/probe/sidecar decode checks the exact planes; GPU tests check both
  bit depths, siting and full-resolution 4:4:4 samples without an unnecessary
  4:2:0 intermediate. Format selection also retains 4:2:2/4:4:4 sampling
  for FFmpeg's NV16/NV24 and P210/P212/P216/P410/P412/P416 families; these
  aliases have unit coverage but not yet source-file round trips.
  Lossless 10-bit 4:2:0, 4:2:2 and 4:4:4 source round trips confirm that
  requesting the matching 16-bit planar layout expands each code by six bits
  without 8-bit quantization. Each decoded first pixel also matches the native
  GPU converter's scalar BT.709/AP1 reference. Semi-planar 10-bit aliases
  still need source-file coverage.
  Native source preflight now names probed unsupported pixel formats (for
  example planar GBR) instead of treating an authored BT.709/BT.2020 YUV
  interpretation as sufficient. It now checks exact supported YUV layout and
  depth names; an unhandled format such as `yuv422p18le` is unresolved rather
  than silently taking the decoder's 8-bit fallback. Missing pixel-format probe
  data is unresolved for the same reason. The managed render gate remains global. These
  paths do not yet qualify every camera format, proxy, or source interpretation.
- Tagged PQ, HLG and common log transfers, plus known BT.2020/DCI-P3
  primaries and matrices unsupported by the Legacy converter, now emit an
  export-blocking color diagnostic in clip and asset peeks instead of being
  interpreted through its BT.709/601 path. Untagged and unknown-tag sources
  retain legacy behavior and still require managed-color qualification.

Remaining implementation:

1. Qualify a versioned Photonic-owned Rust/WGSL transform runtime against
   independent CPU vectors and GPU execution. Preserve existing OCIO-named
   metadata and require an explicit migration to native transform identities.
   The deferred OCIO proposal is recorded in `ocio-runtime-decision.md`.
   Never silently approximate an unsupported transform with a different one.
2. Connect the persisted color contracts and shared authoring operations to the
   qualified runtime and GUI/MCP workflows, including source interpretation UI.
3. Qualify conversion rendering against legacy goldens and mixed-source fixtures;
   only then expose the existing copy operation as an end-user conversion action.
4. Define source → input transform → asset/clip/group/effects/composite →
   track/master → display/export stage ordering, including adjustment layers,
   transitions, titles, logos and captions. ACEScg is managed compositing space;
   ACEScct is the logarithmic grading domain; exposure operates scene-linearly.
5. Complete precision qualification across high-bit-depth decode, chroma
   reconstruction, range/matrix interpretation, proxies, caches, monitoring and
   export. Preview proxies and unrecognized input formats still use 8-bit
   decode, and the Legacy SDR transfer path does not interpret PQ/HLG.
6. Distinguish creative LUTs from technical transforms; persist input/output
   spaces, hashes, interpolation and domain/shaper contracts.
7. Validate CPU references independently, then GPU transforms, including negative
   values, highlights above white, partial alpha, log and mixed-source timelines.
8. Qualify managed SDR first. Display and export transforms remain separate.

The Legacy SDR `.cube` parser now evaluates a declared 1D shaper before the 3D
table on CPU and GPU. It also rejects incomplete shapers, non-finite samples,
malformed triples, duplicate 3D size declarations and reversed/zero-width input
domains. MCP `apply_lut` reports malformed input and leaves document/history
unchanged. Engine compile diagnostics retain the parse error, while final export
rejects an unavailable grade dependency. Managed LUT transform execution and
its reference qualification remain part of this milestone.

MCP `apply_lut`, MCP media import, and GUI media import now persist a full-byte LUT hash separately from
the sampled media relink hash. The render session checks this pin when it warms
LUTs; changed bytes at the same path bypass the affected correction with a
diagnostic, and final export fails. The export regression exercises a complete
pinned shaper LUT and a subsequent same-path change. The media-pool context menu
offers Replace / repin LUT, validating the selected `.cube` and applying path,
sampled hash and full pin as one undo step. MCP `relink_media` uses its existing
`allow_hash_mismatch` consent to repin a changed LUT even at the same path; it
rejects malformed replacements without changing history. Legacy LUT assets
without a pin remain readable. LUT assets now carry an optional versioned
creative/technical purpose and input/output declaration: `legacy_srgb_encoded`
or an OCIO space with a configuration digest. GUI and MCP imports declare
creative Legacy SDR by default. The media-pool menu can mark a LUT technical;
MCP `set_lut_interpretation` can author named spaces. The Legacy SDR renderer
rejects incompatible declarations with a diagnostic instead of silently
applying the LUT. Managed transform execution is still gated. Shaper samples
participate in the render-cache key alongside the 3D table.

## 2 — Color workspace (in progress)

Clip-grade versions now save alternate looks with stable IDs and names. The
active version follows ordinary grade edits, including undo/redo and whole-clip
attribute edits. Color Controls can save, select, rename and remove versions;
the MCP `grade_version` tool uses the same core operations. Legacy clips omit the
new fields on serialization. Named versions remain clip scope only. Focused core and MCP tests cover
switching, undo, persistence, duplicate-name and lock rejection.

The project model now has reusable shared looks as a distinct stage after each
clip's own grade and before group post-grade. Core and MCP operations create,
edit, link, remove, and make a linked look independent while preserving its
rendered stage. Editing a shared look checks every linked track for locks.
Missing references produce a grading diagnostic that blocks final export.
Color Controls can create empty project looks, link a selected shot, detach it,
or move the selected clip grade into its linked look in one undo step. The panel
shows how many shots share that look and can edit linked-look correctors directly
through the existing grading widgets. Edits on any locked linked track are
disabled. Unused looks can be removed from the panel.

Reference-still storage now has a sequence gallery record with stable ID,
image asset, source clip/time, captured document revision, color configuration,
and format index. Add/remove operations are undoable; serialization and sequence
duplication preserve the image reference while remapping local clip identity.
The remove-unused-media scan retains gallery images. In the Color workspace,
Capture Still requests the current sequence frame at full resolution from
original media. It accepts only the requested time/revision, rejects cached
previews and grading/color errors, writes a project-relative PNG, hashes it,
and adds the image asset and gallery record in one undo step. Capture requires
a saved project. A left gallery shows thumbnails, capture metadata and offline
warnings; still removal is undoable.
Selecting a gallery still can now compare its full PNG with the live program
using a movable vertical wipe or an aspect-preserving side-by-side split.
Comparison verifies the image hash, color configuration and format before
opening; switching sequence or losing the reference stops it. A changed image
is rejected rather than silently substituted. Save As verifies and copies the
captured PNGs into the new project directory before saving; conflicting files
are left untouched. Duplicated sequences can share the same captured image
without breaking the copy; only validated `reference-stills/<uuid>.png` paths
are written. The Save As copy now publishes a verified image without replacing a
file concurrently created at the destination. The File menu also offers Archive
Project: it copies every file-backed pool asset (including LUTs and stills),
verifies the copies and still hashes, writes a history-free project, and refuses
offline assets without publishing a partial archive. Reopening a moved archive
resolves its project-relative paths before creating the video engine. Cleanup of
unreferenced still files, proxy/cache collection, and portable managed-color
configuration assets remain pending.
The GUI runs collection on a worker and reports completion in the File status;
large source copies do not block the editor frame loop.
MCP `archive_project` runs the same collector off the async runtime and leaves
the open document and its history unchanged.
MCP `get_project_dependencies` reports file-backed pool assets, missing gallery
references, and full-byte integrity results for pinned LUTs and reference
stills before archive or conform. Unpinned media is marked available by file
presence, with `integrity_checked: false`.
The reference stores an immutable full-file xxh3-128 hash independently from
the asset's faster sampled relink hash. Capture, comparison, and Save As use the
full hash, so an edit in the middle of a large PNG is detected.
MCP `list_reference_stills` exposes the gallery records for an explicit or
active sequence with verified/changed/offline image status and comparison
eligibility. It is read-only and leaves history unchanged. MCP
`remove_reference_still` uses the same undoable core operation as the GUI.
The focused test covers valid, changed, offline and format-mismatched stills,
removal, rejection without history changes and undo. MCP
`capture_reference_still` now renders an exact full-resolution frame from
original media, refuses stale/cached/downscaled frames and grading errors,
and adds the PNG asset plus gallery record in one undo step. The GPU-backed
capture regression checks image dimensions, hash and undo; a missing-LUT
regression proves diagnostic rejection.

The GUI reference-still path now performs frame readback, PNG encoding, and
hashing on a background worker. It validates the sequence, project path, and
document revision again before adding the asset and gallery record as one
undoable command. Canceled or stale captures remove uncommitted image files;
the GPU-backed capture and stale-file cleanup tests cover both outcomes.
Until managed output transforms and reference encoding are qualified, GUI and
MCP capture reject managed sequences, the GUI refuses to open managed
comparisons, and MCP listing marks existing managed stills non-comparable.
These rejection paths leave history unchanged.

MCP `compare_reference_still` verifies
the saved image and renders a full-quality current frame, then returns RGB
MAE/RMSE, channel mean deltas, a 95th-percentile absolute difference and alpha
MAE. It rejects changed references, incompatible color/format, grading errors
and stale frames. The GPU regression checks an identical frame, an exposure
change and corrupt reference bytes without changing history. Metrics use
display-referred Legacy SDR RGBA8 composited over black; they are inspection
measurements, not a professional color-match algorithm. Managed-color
reference encoding and comparison need the Milestone 1B runtime.
Comparison hashes the exact PNG bytes loaded into the viewer and checks the
file's path, size and modification time while it remains open. An altered or
replaced reference stops the comparison instead of leaving a stale texture
presented as current. The per-frame check is metadata-based; a same-size edit
that deliberately restores the prior modification time requires a fresh open
to detect via the full hash.

Core selective grade copy can replace a target grade or append chosen source
correctors while preserving the target's bypass state. Each copied corrector
receives a fresh ID. MCP `copy_grade` exposes `op_ids` and `append`, validates
all targets before its single history batch, and rejects duplicate corrector
selections. Color Controls offers a source-shot selector, whole-grade replace,
and per-corrector append actions through the same core operation.
Previous/next clip controls navigate chronologically on the selected video
track. They select the adjacent clip and seek to its start as session state,
without adding document history; stale navigation requests are ignored.

The Video toolbar now has a Color layout toggle. It opens Color Controls and
replaces the full timeline with a compact, horizontally scrollable shot strip,
giving the existing program monitor more height. Cards span visual tracks in
timeline order and reuse the same history-free clip selection/seek operation.
File-backed shot cards use the timeline's asynchronous thumbnail cache and
bounded egui texture registry; offline or undecodable sources retain a usable
flat card. The layout is local UI state and does not mutate the document.
Its open/closed choice is saved in local app preferences and restored on the
next launch; older preference files default to the ordinary timeline layout.
The shared texture registry now checks a weak reference to the decoded frame
before accepting a pointer-key hit, preventing a different shot from inheriting
an evicted thumbnail's texture when allocator addresses are reused.

Color Controls exposes existing ellipse/rectangle power windows for each
corrector, including normalized center/size, rotation, feather and inversion.
Structural changes use discrete undo; numeric gestures use grade coalescing.
Existing out-of-range values remain unchanged when the panel is merely viewed.
The curve editor can now place an anchor from its sample-colour swatch on the
selected RGB, hue, luma, or saturation axis. It seeds the anchor at the
current curve output and ignores duplicate or endpoint samples; changing the
swatch alone does not edit the grade. The program-viewer eyedropper now samples
the displayed frame through a background GPU readback for both curve anchors
and HSL qualifier seeding. It maps zoomed/cropped monitor coordinates to the
rendered pixel, uses unpremultiplied working RGB, rejects stale results, and
commits a successful edit in one undo step. The swatch's display RGB is decoded
to the same working domain before seeding either control.

Qualifier matte evaluation now has matching CPU and GPU paths. Both evaluate
earlier correctors before the qualifier, include its power window and source
alpha, and exclude the qualifier's CDL and later correctors. A GPU parity test
covers a preceding exposure, soft key, window and partial alpha. The video
compiler and engine now expose each clip's pre-grade node as a tap, after its
source/clip effects and group pre-grade. A tap regression proves it differs
from both the post-grade clip and the program output, and falls back honestly
when the clip is absent. The compiler now carries resolved corrector IDs and
operations to the displayed frame. The Color panel's Highlight matte control
uses that exact stack and pre-grade tap to render the selected qualifier's matte
over a held frame. Readback runs off the UI thread; frame, revision, tap, and
selection checks reject stale results. Playback skips matte readback to avoid
competing with the renderer. A GUI integration test verifies that inspection
does not edit the document or history and clears when selection changes.
The Color workspace program monitor now draws the selected clip/track/master
power window over the actual sequence canvas. Handles move its center, resize
each local axis and rotate it; an inner contour shows the feather width, and a
dedicated handle adjusts ellipse and rectangle feather. The overlay is read-only
for locked clip/track grades and commits drags through
the shared grade command/history path. Source-asset windows use source-space
coordinates before the clip transform and therefore do not show these program
handles. A headless egui pointer-drag test confirms a center move changes the
correct grade and one undo restores it. Multiple masks, Boolean combinations,
animatable feathering, and interactive visual acceptance are still pending.

Color Controls now has an explicit clip/source asset/track/sequence master scope
selector. Each scope reads and writes its own grade through the shared core
operation; clip-only named versions and shot-copy controls appear only at clip
scope. The qualifier eyedropper is disabled at shared scopes until its target
can identify those owners. Opening track and master views leaves document and
history unchanged; edits affect only the selected grade and undo cleanly.

Color Controls now offers Creator and Advanced views over the same grade. Creator
keeps the quick exposure, contrast and saturation controls visible and reports
the number of existing correctors; Advanced retains the stack, individual
editors, versions, shared looks and graph. Switching views is read-only and
the choice persists as a local preference, defaulting to Advanced for existing
installations. A graph grade's quick sliders stay disabled in Creator, with a
prompt to switch to Advanced. The view choice is not part of the project
document. The reference gallery width and shot-strip height are now resizable
and saved locally after a resize; shot cards grow with the strip. Older preferences retain their original
178×104 logical-pixel defaults. The floating Scopes panel now saves its last
position, size, scope type, tap, and SDR scale in local preferences after an
interaction; older preferences default to the original window layout. Colorists
can open up to three additional floating scope windows alongside the primary
one. Each has an independent scope type, SDR scale, saved rectangle, and
asynchronous analysis state. Vectorscope matrix and target percentage also
persist per view; older preferences default to Rec.709 and 75% targets. All
views share the selected measurement tap. Saved
rectangles are clamped to the current screen when a monitor is disconnected.

Advanced Color Controls now supports Ctrl/⌘ multi-selection and Shift range
selection in the corrector stack. Enable and Bypass apply to all selected
correctors; ordered stacks can remove the selected set in one undo step.
Graph-grade removal stays in the graph editor to preserve its connections.
Selection is local UI state keyed to the grade scope, and a batch regression
checks one-step undo and locked-track rejection.

Named local Color arrangements and the primary scope dock are now qualified below. Broader workspace/view arrangements remain open. Establish ownership/identity before
shared versions and references.

## 3 — Advanced corrections and graph (in progress)

The curve stack now includes optional hue-vs-luma, luma-vs-saturation and
saturation-vs-saturation controls, with CPU/GPU implementations and GUI tabs.
Qualifier samples near red now seed an unwrapped hue interval that extends
below zero or above one. The existing circular CPU/GPU hue gates then include
nearby colors on both sides of the seam; Color Controls permits those bounds
when adjusting hue. Saturation and luma bounds remain in `[0,1]`.
The qualifier now persists additional disjoint **add** and **subtract** HSL
regions. Color Controls offers **+ Pick** and **− Pick** from the monitor,
editable per-region ranges/softness and removal. A fresh full-range qualifier's
first add sample becomes its base key. Set/seed replaces the whole key; a
duplicate sample leaves history untouched. CPU and GPU combine the maximum
inclusion with the maximum exclusion, then apply the existing window/alpha
weight. Legacy grades omit the new field and keep their output. The GPU supports
16 additional regions; malformed or excess loaded keys bypass that corrector
with an export-blocking diagnostic rather than being silently truncated.
Each qualifier also has neutral-by-default Clean black and Clean white matte
thresholds. They remap the combined key before applying its window and alpha,
so existing grades render unchanged. Color Controls exposes both thresholds;
shared edits reject non-finite or out-of-range values, and loaded invalid
values bypass the corrector with an export-blocking diagnostic. CPU and GPU
soft-edge tests cover the same threshold behavior. Spatial matte denoise, blur, and grow/shrink are now qualified through the
typed graph refinement utility; direct ordered-qualifier controls remain pending.
Legacy curve documents omit the new fields and preserve their prior output;
empty tabs use virtual neutral curves. These controls currently use the Legacy
SDR domain and do not establish managed-color grading semantics. Full core,
render and GUI library suites passed (**848 + 162 + 425 tests**), as did the
video and MCP libraries (**749 + 223 tests**); the video golden-frame corpus
passed without reblessing. New CPU known vectors and GPU
opaque/partial-alpha parity tests cover the curves. Cache-key and MCP
round-trip regressions cover authoring and invalidation.

A distinct Linear Offset primary adds animatable RGB values in straight
scene-linear space, preserving negative channels and highlights above reference
white until the later display/output stage. It is a new serialized operator,
so old CDL and wheel grades retain their previous arithmetic. The Color Controls
catalog exposes neutral RGB-zero sliders, and MCP whole-grade edits round-trip
the same state. Known CPU vectors, partial-alpha GPU parity and cache-key tests
cover the new operation. Full core/render/video/GUI/MCP library suites and the
Legacy SDR golden-frame corpus passed without reblessing. This is still a Legacy SDR grading control; ACEScct
grading semantics require Milestone 1B.

Printer Lights is a separate animatable primary: each RGB channel uses twelve
points per stop in scene-linear light. Zero points is identity, and the
operator preserves negative and above-white values with partial alpha. It has
its own serialized kind and render-cache identity, plus Color Controls sliders.
CPU known values, animation resolution, serialization and GPU parity are tested;
it does not change the math of existing grades.

The power-window catalog now includes a directional gradient. Its center,
falloff distance, rotation and inversion use the existing undoable grade path;
on-viewer handles expose the transition and skip the unused horizontal size.
The CPU evaluator and GPU shader use the same smooth transition in window-local
Y coordinates. Existing ellipse and rectangle masks retain their math, and
the shape is included in render-cache keys. Gradient width is controlled by
falloff; the ellipse/rectangle Feather control is hidden for this shape.
The serialized mask round-trips through MCP `set_grade` and `get_clip`.

Sequence groups now persist separate pre- and post-grades. Each member clip
renders group pre → clip grade → group post after its clip effects; nested
groups apply pre from root to leaf and post from leaf to root. The Color scope
selector exposes these shared grades, and MCP `group_grade` uses the same
undoable grade operation. Edits are rejected if a member track is locked.
Group-grade keyframes evaluate in each member clip's local time, so the same
shared look follows the same relative beat in every shot.
Group LUTs participate in remove-unused dependency scanning, and load-time
property-track repair walks both group stages. Missing group LUT diagnostics
identify their pre/post owner, and the comparison-clean path bypasses those
shared looks. This establishes stage order;
an independent group graph remains pending.

An embedded grade-stage image graph now supports serial correctors and parallel
branches joined by a layer mixer. Ordered stacks can be converted to a serial
graph without changing corrector identities or their evaluation order; the
graph sits at each grade scope's existing stage. Invalid references and cycles
emit export-blocking diagnostics. Whole-grade copy remaps corrector IDs in the
graph; selective/append graph copies are rejected until their topology semantics
are defined. The Color panel and MCP expose one-step stack conversion and undo.
The panel can add serial/parallel correctors, label nodes, reroute image inputs,
adjust a layer mix, and remove nodes; connection changes are validated before
commit. Removing a corrector reconnects its input, while removing a mixer keeps
its bottom branch and prunes only newly orphaned nodes. Parked, already
disconnected alternatives are preserved. Node IDs use a monotonic cursor so
deleted IDs are not immediately reused. MCP uses the same add/remove operations
with one undo step apiece. The ordered-stack
controls stay disabled while a graph is active. `apply_lut` rejects graph grades rather than
silently changing an unconnected op. Shared grade edits reject invalid graph
topology before producing a history command; loaded invalid topology still emits
an export-blocking compile diagnostic. The graph data model and compiler are
available. The Color panel now shows a scrollable node canvas with serial and
parallel branches. Clicking a corrector selects it; clicking an output dot then
an input dot routes that image connection through the same graph validation and
undo path as the detailed node controls. Typed matte ports, qualifier key sources,
key mixers and masked image application are now qualified in the increment below.
Shared/compound nodes remain pending.
An unresolved corrector inside the grading graph now carries its node ID in
the structured diagnostic, alongside operator and scope owner. Older diagnostic
payloads still deserialize without this optional field. The compiler regression
checks the node, clip owner, missing LUT, and visible message together.

Remaining: additional managed log primaries beyond the qualified v1 controls;
shared nodes and compound grades; editable Bézier windows
with Boolean masks and direct ordered-qualifier refinement controls. Graph
matte refinement is qualified in the later increment. Define mask
coordinates across retiming, transforms and stabilization before implementing
cancellable deterministic tracking with confidence, manual keys and stale-result
rejection.

The primary catalog now includes a distinct scene-linear Highlight Roll-off
corrector. It compresses the positive RGB peak above an adjustable knee and
scales all channels together, preserving hue ratios, negative values and alpha.
Zero strength is identity, so adding the corrector does not alter a look until
adjusted. Both controls are animatable and included in cache identity; CPU/GPU
tests cover values above reference white and partial alpha. This is a creative
legacy-SDR operator, not a managed HDR output transform or mastering tone map.

The advanced primary catalog also includes a distinct Saturation & Vibrance
corrector. It adjusts Rec.709 luma/chroma in scene-linear RGB without clamping
negative or above-white values; vibrance favors less saturated colors. Its
neutral settings are identity, both controls are animatable, and GUI and MCP
edits share the same grade state. CPU known values, GPU partial-alpha parity,
serialization and cache-key tests cover the operator. Existing CDL saturation
retains its legacy behavior.

## 4 — Matching, restoration and performance (in progress)

The shared video engine now provides an explainable global RGB balance
calculation for GUI and MCP callers. MCP reference comparison includes its
proposal expressed as editable Printer Lights points. It uses the central 80%
of each channel's opaque, non-clipped Legacy SDR code-value distribution for a
trimmed mean in log2 light, requires adequate sample coverage,
and caps proposals at two stops per channel. `apply_shot_match` recomputes the
verified comparison, checks the expected document revision and exact frame,
requires the target to be the sole visible video clip, and appends one undoable
corrector. Locked tracks, bypassed grades, stale previews, changed references
and insufficient samples fail without changing history. This is a conservative
global estimate, not a scene-aware color-match algorithm; users must inspect
the previewed proposal and resulting look.

The Color gallery also exposes the shared proposal for a selected, sole visible
clip. It requests a full-quality original frame, verifies the still image hash,
shows per-channel Printer Lights points, and applies through the shared grade
operation with document-revision, playhead, visibility, and reference checks.
The gallery runs reference decoding, GPU readback, and matching in a background
worker, then discards a result if the selection, playhead, sequence, or revision
changed before delivery.
A GPU-backed gallery integration test covers proposal generation, one-step
apply/undo, stale-preview rejection, changed-reference rejection, and locked
track rejection. A separate test verifies stale background results are dropped.

Resolved HSL qualifier keys now live out of line in the render IR. A size guard
keeps the common `ResolvedGradePayload` at or below 64 bytes rather than making
every primary operator carry the qualifier's fixed 16-key array. CPU/GPU
qualifier parity and scope-tap integration continue to exercise the same
rendered behavior.

Chart-based balancing, scene grouping and stronger explainable
assistance; spatial/temporal restoration with explicit preview/final quality;
resource reuse, pass optimization and unchanged-branch caching; recorded 1080p/4K
reference-hardware interaction/scope/playback/memory/cache benchmarks. No universal
real-time promise.

The opt-in playback benchmark now has a `grading` workload with four active
correctors and asynchronous waveform readbacks. It records completed scope
measurements, p95 readback latency, playback cadence and cache memory at 1080p
and 4K. Longer Legacy grading release-build runs and GUI interaction timing
remain pending.
The benchmark also has a `native_grading` workload with a freshly probed YUV
source, four native correctors, SDR display output, and a signal-aware scope
readback. On RTX 4090 / Vulkan / driver 610.57.04, eight-second release-build
Full/original runs observed 29.87 unique fps at 1080p and 29.85 fps at 4K;
scope p95 latency was 4.85 ms and 4.91 ms respectively, with no readback errors
or incomplete evaluations. The 4K run reported a 68.30 ms longest held frame,
45.52 ms p95 frame interval, 419.05 ms p95 exact seek and 1,069,547,520
bytes of managed GPU cache. GUI interaction timing and other hardware remain
unqualified; these results are not universal throughput claims.

An initial two-second debug-build run on RTX 4090 / Vulkan / driver 610.57.04
with generated H.264 source and full-quality originals measured 13.48 unique
published fps at 1080p and 13.49 at 4K. Async waveform readback completed 28
times in each run, with p95 latency 5.04 ms and 6.89 ms respectively and no
readback errors. Managed GPU cache occupancy reported 1,336,934,400 bytes.
These short debug measurements establish a reproducible baseline, not a release
throughput target or interactive GUI qualification.

## 5 — HDR and professional finishing (in progress)

The ProRes 4444 SDR alpha export path now quantizes directly from the floating
working frame to `yuva444p12le` input. The export test probes the encoded
12-bit format, decodes a synthetic gradient, and confirms more than 256
distinct luma codes. This removes the former 8-bit rawvideo bottleneck on this
delivery path. Opaque ProRes now uses the HQ 4:2:2 profile with direct
`yuv422p10le` input rather than feeding the 4444 encoder 8-bit 4:2:0. A
decode/reimport regression confirms the encoded 10-bit format and more than
256 distinct luma values; odd-width 4:2:2 chroma and legal-range endpoints
have unit coverage. Selected high-depth YUV sources now have a 16-bit decode/upload
route, but the wider managed pipeline and mixed-source qualification remain
pending; this result does not establish HDR mastering.

Wider HDR interpretation beyond the qualified explicit PQ-display and HLG-scene inputs; HDR scopes and mastering controls; tone/gamut mapping;
verified high-bit-depth encoded samples and metadata through decode/reimport;
calibration/external output, grading panels, vendor RAW adapters; conform/relink,
review annotations, further finishing qualification of render manifests, missing dependencies and portable archives.
Proprietary SDK/hardware support stays optional. Each facility capability needs
its own hardware qualification before being advertised.

## Acceptance gates across all milestones

- Panel viewing is read-only, gesture undo is one step, rejected edits preserve
  history, and save/reload preserves all authored grades/versions/references.
- Independent numerical references supplement CPU/GPU agreement.
- Legacy SDR goldens remain unchanged, except explicitly unsafe unresolved
  dependencies now fail visibly and cannot silently alter final output.
- Scope patterns verify spatial position, channel separation, full/legal scales
  and measurement before/after output transforms.
- Tracking tests cuts, occlusion, motion, cancellation, stale results and manual
  corrections. Every assisted correction remains editable and undoable.
- End-to-end workflow: balance, match, isolate with tracked window, alternate look,
  export and reimport.
- GUI/MCP share edit operations, persisted state and rendering. MCP parity ships
  with each area, including inspection, validation, comparisons and job status.

Historical full library verification (2026-10-04; superseded by the current checkpoint): core **873**, render **195**,
video **853** (two ignored), GUI **460**, and MCP **257** tests passed.
Scope-job regressions and direct/nested native scope/export/manifest
parity checks also passed in focused runs. Workspace documentation tests passed. The Legacy SDR golden-frame
corpus passed without reblessing; workspace Clippy, formatting and
`git diff --check` passed. Earlier test totals below describe their respective
implementation snapshots.

Grade-owner diagnostic verification: render **163**, video **750**, GUI **427**,
and MCP **223** library tests passed. A compile regression checks identical
corrector IDs in clip, track, and master stacks; a wire-format regression checks
that older diagnostics without an owner still deserialize and serialize unchanged.
Reference-still storage verification: core **849** library tests passed,
including undo/redo, serialization, duplicate-sequence identity, and media
retention. GUI **433** and video **751** library tests passed, including a headless GPU capture
that verifies the 16×16 PNG, asset hash, revision and undo behavior; comparison
loading rejects a changed PNG or mismatched format. Split geometry preserves
image aspect ratio. Interactive visual grading acceptance is still pending.
Save As tests confirm copied stills resolve after the old image is removed and
conflicting destination files remain unchanged.
Untitled projects saved from close-tab and quit dialogs also copy validated
reference images before writing the project file.
The full-file hash regression changes only the middle of a 256 KB fixture and
detects it where the sampled relink hash does not.

## Automated validation

Validated on this Linux workspace:

| Check | Result |
| --- | --- |
| `cargo test -p photonic-core -p photonic-mcp --lib --locked` | 838 core + 219 MCP passed |
| `cargo test -p photonic-render --lib --locked` | 157 passed, GPU scope tests exercised |
| `cargo test -p photonic-gui --lib --locked panels::video::` | 135 passed |
| `cargo test -p photonic-video --lib --locked grade` | 7 passed |
| `cargo test -p photonic-video --test export_engine_cmd --locked -- --nocapture` | 2 passed, GPU/FFmpeg exercised |
| Clippy, all targets of the five affected crates | Passed; existing unrelated warnings remain |
| `cargo fmt --all --check`, `git diff --check` | Passed |

Total: 1,358 passing tests in these non-overlapping runs. This does not qualify
professional color accuracy or hardware playback latency; the renderer retains
Legacy SDR semantics and those gates remain explicit below and in Milestone 1B.

### Managed-color contract increment

- Core + MCP libraries: **844 + 219 passed** after the new persisted fields and
  shared edit commands.
- Managed root/nested sequence render/export gate and input preflight: **3 passed**.
- Historical native OCIO comparison: **11 reference samples passed**;
  maximum absolute error 3.18e-7. The probe has since been removed.
- CPU/GPU clipping-count patterns: **2 passed**.
- Historical OCIO GPU qualification: **1 test passed**, covering three processors
  with four RGBA samples each, on NVIDIA RTX 4090 / Vulkan / driver 610.57.04.
  The test and generated artifacts have since been removed.
- All-target Clippy passed for the five affected crates (existing warnings remain);
  formatting and diff checks passed. GUI/MCP all-target compilation passed.
  At that time, development dependency license, advisory and source checks
  passed under the earlier license policy; commercial release qualification
  is not established.

These are incremental qualification results, not managed-pipeline acceptance.

### Native source and grading-domain increment

- Render library: **193 tests passed** after the 16-bit 4:2:0 Legacy parity
  regression; video library: **796 tests passed** after decoder and output
  reference additions.
- Native GPU source: **6 tests passed**, including odd-width 8-bit and 16-bit
  chroma siting and signal excursions; native grading-domain/output-component
  GPU: **4 tests passed** for ACEScct transfer, scene-linear exposure, the
  scalar output bridge and sRGB display encoding. Managed-color contract:
  **6 passed**.
- A real FFmpeg lossless 16-bit 4:2:0 encode/probe/sidecar-decode/GPU test
  retained every source plane code. GUI and MCP compilation, all-target
  render/video Clippy, formatting and diff checks passed; existing unrelated
  Clippy warnings remain.
- Managed sequences still fail closed; these component checks do not establish
  output-transform accuracy or production readiness.
- Output-reference increment: **12 focused scalar tests passed** for published
  ACES 2 tone-scale targets, AP0/AP1/Rec.709 appearance-model round trips,
  neutral stage composition, display encoding, premultiplied alpha and invalid
  numeric inputs.
- After the isolated display pass and alpha API, the settled render/video
  library suites passed **193 + 799 tests**. The four native transfer GPU tests,
  all-target render/video Clippy, Cargo license gate, formatting and diff checks
  passed; existing unrelated Clippy warnings remain.
- After the 16-bit 4:2:2 decode increment, **10** synthetic export/decode
  integration tests and **7** native source GPU tests passed. After the 8-bit
  increment, **11** synthetic export/decode tests and **8** native source GPU
  tests passed. After 4:4:4 support, those suites passed **13 + 9**.
  The settled render/video library suites passed **193 + 799** tests, managed
  contract and native transfer GPU suites passed **6 + 4**, GUI/MCP compilation
  passed, and all-target Clippy, formatting, diff and license checks passed.
  Existing unrelated Clippy warnings remain.
- After the independent output grid, cusp primitive and broader YUV format
  selection, the video library has **800 passing tests and 2 explicitly
  ignored output-qualification tests**. The synthetic export/decode,
  managed-color contract and native-source GPU suites passed **13 + 6 + 9**;
  those ignored tests fail when explicitly run and therefore keep managed
  output gated. GUI/MCP compilation, formatting, diff and license checks
  passed.
  These tests do not yet qualify chromatic output or the GPU output path.
- After the focus-line qualification experiment and exact native-source format
  preflight, the video library has **801 passing tests and 3 explicitly ignored
  output-qualification tests**. The managed-color contract's **6 tests** pass,
  including rejection of missing probe data and unsupported bit depths. The
  focus-line experiment remains test-only and does not open the managed gate.
- Real 10-bit 4:2:0, 4:2:2 and 4:4:4 source/decode/GPU tests pass; the synthetic
  export integration suite now contains **16 passing tests**.
- The workspace library suite passes after these changes: core **870**, GUI
  **458**, MCP **247**, render **193**, and video **801 passed / 3 ignored**.
  GUI/MCP compilation, the Cargo license gate, formatting and diff checks pass.
- After the corner-aware gamut table and isolated `Aces2SdrOutput` scalar API,
  the video library suite passes **807 tests / 2 ignored**. The two ignored tests
  belong to the older Photonic SDR v1 candidate. The new output API passes
  64 grid, 32 edge and twelve focused independent vectors with a strict
  0.002 encoded-channel bound. All-target video Clippy and GUI/MCP checks pass;
  managed preview and export remain gated pending full-pipeline qualification.
  The full workspace library suites pass (**870 core, 458 GUI, 247 MCP, 193
  render, 807 video; two older output-candidate tests ignored**). The four
  native-transfer GPU tests, six managed-color contract tests, Cargo license
  gate, formatting and diff checks also pass.
- The isolated native ACES 2 SDR GPU output pass now passes **5 tests**:
  native YUV-derived extended-range pixels, direct float AP1/partial-alpha
  vectors, all 96 independent reference samples, and a 37×19 two-dimensional
  image with transparent pixels and padded readback. The image test exercises
  a caller-owned output target and existing command encoder for graph texture
  pooling and batched GPU submission. A 17×9 16-bit YUV 4:4:4 image also
  passes through native scene-linear exposure and the output pass with
  positional GPU/scalar agreement. This qualifies the
  output operation in isolation, not managed timeline preview or export.
- The frame graph now records whether working textures are Legacy linear
  Rec.709 or scene-linear ACEScg. Legacy graph construction keeps its default.
  CPU/GPU evaluation refuses an ACEScg graph containing the legacy `Grade`
  operation, and the evaluator salts managed cache identities by domain.
  This is a safety contract, not activation of managed graph compilation:
  dedicated managed grade operations, source interpretation, and full graph
  ordering still need implementation and qualification.
- An isolated `NativeExposure` graph operation now evaluates scene-linear
  ACEScg exposure on CPU and GPU with partial alpha, uses caller-owned pooled
  GPU output, and rejects Legacy-domain or out-of-range use before evaluation.
  Content hashes include stops and the distinct operation tag. It is not yet
  emitted by managed sequence compilation, and the high-exposure precision
  limit of RGBA16F still needs qualification before activation.
- Isolated ACEScct encode/decode graph nodes now make the scene-linear ↔ log
  boundary explicit. CPU/GPU graph evaluation round-trips a partially
  transparent image after exposure, while graph validation rejects a decode
  before encode, exposure inside the log domain, and displaying encoded log
  pixels as an output. Transfer direction has a
  distinct cache identity. Sequence compilation still does not emit these
  nodes or enable managed playback/export.
  The settled video library suite passes **812 tests / 2 ignored**; the
  four focused managed graph tests, four native transfer GPU tests, twelve
  CPU/GPU parity tests, six managed contract tests, GUI/MCP compilation,
  license gate, formatting and diff checks also pass.
- An isolated `NativeSdrOutput` IR operation now completes a small managed
  graph from scene-linear exposure through ACEScct encode/decode to ACES 2
  100-nit SDR output. CPU/GPU pixels are compared with the scalar transform.
  Graph validation requires scene-linear input, makes the transform the graph
  output, and rejects further operations on display-encoded pixels. ACEScct
  intermediate pixels may only enter the explicit decode node until other
  log-space operators are qualified. Its GPU
  pipeline initializes only when a graph contains this operation, so Legacy
  evaluator startup does not build it. Managed sequence compilation and
  export remain gated pending full operator, source, and workflow qualification.
- Frame output interpretation is now explicit (`LegacyLinearRec709`,
  `SceneLinearAcescg`, `Acescct`, or `SrgbDisplay`) on published engine frames. The GUI
  presenter decodes managed sRGB code values before writing its sRGB target,
  avoiding a second OETF; a GPU present test checks half-alpha pixels. Native
  export rejects scene-linear, log, and display-encoded pixels without a
  compatible encoder path, so future managed graph activation cannot silently
  double-encode.
  The Color workspace also suppresses scope plots for managed frames until
  each tap carries a qualified color interpretation; scope revision identity
  includes the frame output encoding so an old Legacy result cannot survive a
  mode change.
  After this handoff change, the render library suite passes **194**, the
  video library suite **813 / 2 ignored**, and the GUI library suite **458**.
  The encoded-present GPU test, managed graph tests, existing native export
  integration tests, GUI/MCP compilation, license gate, formatting and diff
  checks pass.
- Frame graphs now derive color interpretation at each intermediate node,
  including ACEScct grading coordinates. Published frames carry the resolved
  scope tap's encoding separately from the final output encoding. Scope labels
  identify that tap, and result identity includes both encodings; managed tap
  plots remain gated pending calibrated color-specific scales. The focused
  graph-domain and GUI scope-invalidation tests pass. This does not enable
  managed sequence compilation or managed export.
- MCP `get_scopes` now checks the measured tap's interpretation before GPU
  readback and rejects ACEScg, ACEScct, sRGB, or unresolved taps rather than
  returning them as Legacy BT.709 scope data. Legacy taps retain the existing
  payload. The video library suite passes **814 / 2 ignored** and GUI **458**;
  the focused MCP guard test is included in the MCP library regression suite
  (**248 passed**). Formatting and diff checks pass.
- Native SDR drafts now author separate `srgb_sdr` display and
  `bt709_video_sdr` export intents. The latter has an isolated scalar BT.709
  video-signal encoding reference for already-rendered Rec.709 pixels; it
  rejects sRGB display intent and non-finite input. Viewer validation rejects
  the video export target as a display transform. Older native drafts retain
  their serialized sRGB export intent and continue to round-trip. Managed
  export still fails closed: the reference is not yet wired to a qualified
  render, matrix/range packer, encoder metadata, and export/reimport test.
  The scalar ACES 2 SDR renderer now exposes the shared linear Rec.709 result
  before either transfer function; focused tests check both encodings branch
  from that result and preserve the independent display reference grid. The
  technical branch preserves partial-alpha coverage and rejects invalid
  transparent pixels before any Y′CbCr packing.
- The isolated frame graph now has a distinct `NativeSdrVideoOutput` operation
  and `Bt709Video` pixel encoding. It uses the same ACES 2 SDR appearance and
  gamut stages as the display operation, with a separate WGSL BT.709 transfer
  branch and cache identity. CPU/GPU parity is tested on a managed graph. The
  GUI refuses to present video-encoded pixels, and the current export encoder
  still refuses this graph output until a dedicated encoded-signal packing
  path and metadata/reimport qualification are finished.
- The export converter now has an isolated BT.709 signal-to-Y′CbCr pixel
  packing reference that unpremultiplies encoded RGB and skips the legacy
  OETF. A focused regression compares it to the old linear-input path and
  proves that routing the same encoded pixel through that path would encode
  twice. Separate encoded-signal plane packers now cover 8-bit 4:2:0/4:4:4,
  10-bit 4:2:2, and 12-bit 4:4:4:4; an sRGB display packer covers RGBA8.
  Odd-dimension synthetic frames compare their bytes with equivalent legacy
  linear inputs. Managed export dispatch and metadata/reimport qualification
  are still gated.
- A neutral ACEScg sample now traverses the scalar SDR rendering transform,
  BT.709 video transfer, 10-bit 4:2:2 limited-range packing, Y′CbCr decode,
  and inverse BT.709 transfer; its reconstructed linear Rec.709 values agree
  with the renderer's pre-transfer output within 0.005 per channel.
- Export-loop frames now carry their pixel encoding. Conversion dispatches
  Legacy linear frames through the existing path, BT.709 video frames through
  the no-second-OETF YUV packers, and sRGB display frames through straight
  RGBA8 packing. Mismatched pixel encoding and delivery layout fail rather
  than silently encoding again. The production job still preflights managed
  sequences until source, grading, metadata, and reimport qualification are
  complete. Its frame guard now accepts the qualified BT.709 video signal;
  conversion rejects a BT.601 target matrix for those pixels.
- A real FFmpeg-backed isolated export now accepts a BT.709-encoded SDR frame
  through that render loop, writes H.264 with BT.709 primaries/transfer/matrix
  tags, decodes the result, and checks the code values against the single-
  transfer scalar reference. A mismatch test rejects BT.709 video pixels for
  RGBA8 image delivery and sRGB display pixels for YUV delivery. This proves
  the transport/encoder branch, not the full managed sequence pipeline.
  A real encoder rejection regression confirms that a mismatched encoded frame
  preserves a previous export file and removes its staged output.
- A typed `NativeDecodeVideo` graph source now carries a validated input
  interpretation and hashes it into node identity; Legacy decode cannot enter
  a managed graph and native decode cannot enter a Legacy graph. The GPU
  evaluator calls a separate provider method. The session's existing decode
  ring converts retained YUV planes to ACEScg through the native converter,
  bypassing Legacy converted-upload aliases. A real small-video regression
  confirms that two explicit interpretations of the same frame yield different
  pixels without reusing the Legacy upload cache. It also evaluates real
  decoded video through `NativeDecodeVideo` and both GPU SDR display/video
  outputs, comparing each to its scalar transform. The video result is also
  packed into 10-bit 4:2:2 and decoded back to the renderer's linear result.
  Normal managed sequence
  compilation remains gated until its entire operator chain is domain-safe.
- Native clip-source lowering now resolves the clip's explicit interpretation
  before the asset's, emits `NativeDecodeVideo` for supported video sources,
  and diagnoses missing or invalid interpretations. Interlaced native sources
  remain gated until deinterlacing is qualified before the input transform.
  The top-level managed compiler gate still prevents an incomplete sequence
  pipeline from reaching preview or export.
- The graph builder now records its working color domain. When lowering a
  managed grade, unmasked exposure uses the scene-linear `NativeExposure`
  operation; unsupported correctors and masks produce a coded error and are
  bypassed. The top-level managed sequence gate remains in place while the
  other timeline stages are made domain-safe. A focused compiler regression
  checks operator selection, graph domain, and the error path.
- Managed graph validation now refuses legacy effects and display-authored
  still/vector/text sources and display-authored captions until their input or
  output transforms are implemented. This prevents a graph assembled outside
  the top-level compiler from silently treating those values as ACEScg.
  This increment passed the full video library suite (828 passed, 2 ignored),
  the managed graph (6), native source GPU (9), and synthetic export (16)
  integration suites, GUI (458), MCP (248), workspace compile, formatting, and
  diff checks. The subsequent caption rejection has a focused passing test.
- Explicitly interpreted video assets now use native decode and the ACES SDR
  display transform in the isolated source-peek graph. Invalid interpretations
  and interlaced sources fail closed with a color diagnostic. A real FFmpeg
  source-peek regression compares the compiled graph's pixels with the
  independently assembled native source/display graph. The session permits
  this isolated asset peek while a native-managed sequence is selected;
  managed sequence playback remains gated. Verification after this increment:
  video library 830 passed/2 ignored, GUI 458, MCP 248, workspace check,
  all-target video Clippy (existing warnings), formatting, and diff checks.
  The real-video test now waits with a bounded deadline for the asynchronous
  decoder to warm; two successive runs passed after this race was exposed.
- Managed graph validation permits scene-linear Normal compositing and rejects
  other blend modes until their math and GPU output are independently qualified.
  A focused graph test pins both outcomes; an isolated CPU/GPU compositing test
  confirms that partial-alpha Normal blending retains values above reference
  white instead of clipping them to 1.0. After this change the video library
  suite passed 831 tests (2 ignored), along with the managed graph (7), native
  source GPU (9), and synthetic export (16) integration suites.
- Managed grading now lowers an unmasked Linear Offset corrector to a dedicated
  scene-linear operation after exposure. The GPU shader applies the offset to
  straight RGB through premultiplied alpha; CPU/GPU tests cover negative values,
  highlights above white, partial alpha, invalid parameters, and cache identity.
  Legacy grade math and the managed timeline gate remain unchanged.
- Unmasked Printer Lights now lowers through a separate native scene-linear
  operation (12 points per stop per channel). CPU/GPU reference vectors check
  channel-specific gain and preserved alpha, while validation rejects nonfinite
  points and a distinct content hash prevents cache aliasing. The GPU evaluator
  now initializes native primary and ACEScct pipelines only when a graph uses
  them, avoiding extra startup pipelines for Legacy SDR projects.
- Unmasked Highlight Roll-off now lowers through an unclamped native operation.
  It scales all channels by the same mapped-positive-peak ratio, retaining hue
  ratios, signed channel detail, and premultiplied alpha. An analytic reference
  vector and CPU/GPU test cover these cases; invalid parameters fail graph
  validation. Managed timeline playback/export remain gated.
- An authored serial grading graph containing scene exposure and linear offset
  now lowers to those native operators in order, with no legacy `Grade` node.
  The compiler regression validates the resulting graph domain and diagnostics.
  The broader run after native roll-off passed render library 194, video library
  832 (2 ignored), managed graph 10, and workspace compile, format, and diff
  checks; the subsequent serial-graph regression passed separately.
- The export packer now rejects any non-finite RGBA component before codec
  conversion. A real encoder regression confirms a NaN frame fails and leaves
  an existing destination untouched with no staged file remaining; Infinity is
  rejected by the direct conversion test. This catches half-float overflow
  instead of silently publishing altered pixels. After this change the video
  library passed 833 tests (2 ignored) and the managed graph suite passed 10;
  formatting and diff checks passed.
- Managed grade lowering now validates each native primary before emitting an
  IR node. An invalid exposure produces a coded color diagnostic and bypasses
  only that corrector; a following valid offset still lowers to a domain-valid
  graph. This prevents a bad authored value from becoming an evaluator miss
  without an actionable preview/export error. The video library passed 834
  tests (2 ignored) and the managed graph suite passed 10 after this change;
  GUI 458, MCP 248, and the Cargo license gate also passed.

### Clip grade version increment

- Core and MCP library suites: **846 + 220 passed**.
- After selective copy, core and MCP library suites: **847 + 222 passed**;
  the focused GUI document-integrity test also passed with another graded
  source shot present.
- Core timeline integration: **38 passed**.
- All-target Clippy passed for the affected core, GUI and MCP crates;
  pre-existing warnings remain.
- The focused GUI document-integrity test includes an existing named version.
- After shot navigation, the full GUI library suite passed (**424 tests**) and
  all-target GUI Clippy passed; navigation tests cover chronological order,
  seek/selection state, stale requests and unchanged document state.
- After the empty-curve fix, the full GUI library suite passed (**425 tests**)
  and all-target GUI Clippy passed; the regression covers all six curve tabs.
- MCP copy-grade validation regressions pass for locked, duplicate, empty, and
  source-as-target batches.
- The full render scope test set passed (**12 tests**), including GPU/CPU
  agreement and legal-scale endpoint/excursion patterns. Focused MCP and GUI
  tests and all-target Clippy passed for render, GUI and MCP.

## Native working-range qualification increment — 2026-10-04

An extreme but valid native exposure reproduced a CPU/GPU reference mismatch:
the GPU stored signed finite half-float limits while the CPU retained values
above a billion. Native CPU graph nodes now mirror the premultiplied RGB
storage boundary `[-65504,65504]`; native transfer shaders and native LUT output
also clamp that boundary explicitly. Alpha and values within the representable
range keep their semantics. Legacy CPU/render arithmetic is unchanged.
LUT and shaper texels outside the finite half-float range are rejected before
upload rather than converted to infinities in their half-float textures.

The failing signed/partial-alpha exposure regression now passes with exact
finite endpoints. The 21-test native managed graph suite, including valid LUTs
and invalid oversized LUT/shaper fixtures, passed; Legacy golden frames also
passed without reblessing. This establishes the working-storage limit rather
than promising unlimited floating-point radiance.

## Reproducible render manifests increment — 2026-10-04

GUI exports offer **Write render manifest**; MCP `export_sequence` and each
batch output accept `write_manifest`. Both use the shared export worker and
publish `<output>.photonic-render.json` after successful encoding and any stems.
The record includes the frozen timeline and embedded vector document where
needed, canonical full-byte snapshot hashes, revision, color/transform intent,
resolved frame range/rates/dimensions, preset/options, FFmpeg build/binary identity,
a full-byte before/after inventory of the whole project pool, and final output
hash. Unused/offline pool entries remain explicitly identified. This adds hashing
time and records source paths/project state; it is opt-in.

`inspect_render_manifest` checks read permissions for both files, validates the
snapshot identities and adjacent output hash on a worker, and returns the
record without changing document/history. It does not re-render, validate
current source files, assert calibrated output, or provide cryptographic signatures.
Changed pool sources prevent new manifest publication; cancellation and foreign
sidecar content also preserve existing records. Writes are atomic. Video/stems
and manifest are separate publications: a manifest failure can leave the encoded
output present, and a stale record then fails output-hash verification.
Before/after hashes do not lock source files against transient concurrent writes.

A live authenticated HTTP export also encoded 60 Native SDR frames at 640×360
and verified its emitted manifest through `inspect_render_manifest`.
Direct and nested Native SDR MCP export/reimport tests now enable manifests,
verify the emitted snapshot/options/source inventory and inspect through the MCP
route. Unit tests cover full-output tampering, changed-source and foreign-sidecar
preservation, object-order-independent snapshot hashing and cancellation.
The whole library sweep passed: **873 core, 460 GUI, 257 MCP, 195 render,
853 video** (two video tests ignored), followed by workspace documentation tests.

## Asynchronous MCP scope measurement increment — 2026-10-04

`measure_scopes` queues the same exact-frame/tap/signal measurement as
`get_scopes`, returning a job ID and admission revision immediately. Rendering,
readback and CPU binning execute on a blocking worker. The shared bounded job
registry supplies status, terminal results and cooperative cancellation at the
frame boundary. A changed snapshot before measurement fails with
`RevisionConflict`; results retain exact time, revision, generation, dimensions,
actual tap, diagnostics and signal interpretation. Measuring never edits history.

Focused tests cover cancellation, capacity, invalid targets and a changed
snapshot held behind the real engine transport lock. Native direct and nested
PNG/ProRes workflows compare every scope distribution and interpretation with
the synchronous tool. A live authenticated HTTP call against the native editor
also completed a 640×360 sRGB program measurement at document revision zero.

## Scope workspace and native UI increment — 2026-10-04

The primary scope can dock below the monitor or float. Its open state,
placement and dock height persist alongside scope type/tap/scale and the
existing additional floating windows. Older preferences receive compatible
floating/closed defaults. The dock uses a compact wrapping toolbar; waveform
and parade plots use the available width while vectorscopes stay square.
Native SDR status uses a concise label with detailed provenance on hover.
The configured MCP listener port now feeds both the status bar and endpoint
modal instead of a hard-coded default.

Native editor launches under an isolated Xvfb display exposed an egui texture
lifetime error: textures scheduled for release were destroyed before the
render encoder was submitted. Releases now occur after `finish_frame`.
Repeated real application redraws, clip selection and resizing remained stable
following this correction. Screenshots at 1920×1080 and 1280×800 verified the
compact scope dock with a selected explicitly interpreted sRGB still. These
checks qualify this layout and texture-lifetime fix, not calibrated monitoring,
grading panels or the full workflow acceptance gate.

Shared asset interpretation/grade edits now also protect composition `MediaIn`
dependencies on locked direct and nested shots, including parked graph nodes.
A regression checks rejection without a command and successful edits after
unlocking.

## Native nested sequence increment — 2026-10-04

Native nests with matching color configuration, frame rate and selected canvas
now compile their inner program in scene light. The inner display transform is
removed at the import boundary; its source, grades, groups/looks and master
stages feed the outer clip's stages before one outer output transform. Trim and
speed map the inner sampling time. A shortened inner sequence holds its final
frame and warns. Active cycles, missing nests, incompatible boundaries and more
than 32 levels fail closed. Different-rate/canvas native conform remains gated.

Compiler tests check scene-stage order and numerical extended/partial-alpha
results for preview and delivery, single output rendering, held tails and
rejections. The real nested MCP workflow passes: grade authoring, PNG, scopes,
gallery capture/comparison, 12-bit ProRes and decoded spatial-window pixels.

The preceding still/lock snapshot passed all workspace library tests (core
872, GUI 460, MCP 254, render 195, video 849 with 2 ignored) and doc tests.
After native nests, 49 native video library tests pass with 2 ignored.

## Native sRGB stills and dependency locks increment — 2026-10-04

Native-managed timelines now accept explicitly declared display-referred sRGB
stills. The persisted source standard is `srgb_display`, with full range, RGB
matrix and no chroma siting; both asset and clip authoring validate source kind.
PNG (8/16-bit) and JPEG (8-bit) are the initial decoder qualification. Magic-byte
inspection gates other formats, including float HDR/EXR, before a Legacy decode
could discard precision. Still decoding retains the existing linear-light
resampling; a separate native operation converts premultiplied Rec.709/D65 to
AP1/ACES white. This gamut conversion does not invert an output transform or
recover scene radiometry. Source peek and the Color interpretation editor expose
this path. Legacy still rendering remains unchanged.

Compiler preview/delivery and GPU tests pass for explicit interpretation,
logical canvas sizing, primary gamut vectors and non-square images. A real MCP
16-bit PNG test passes through `set_native_input_color` and PNG preview, checked
against the scalar output transform with partial alpha. Broader workspace library and doc checks
pass on this snapshot.

Shared LUT interpretation and content-pin mutations now reject dependencies in
locked shots, including bypassed clip/local/shared looks, group, track, master,
asset grades and nested sequences. Asset-grade edits likewise protect locked
source users. The regression covers every grade stage plus nested outer locks;
composition graph LUT/grade references are included in the dependency walk.

## Native windows and log contrast increment — 2026-10-04

Qualified native primary corrections and creative LUTs now support ellipse,
rectangle and gradient windows with rotation, feathering and inversion. Window
coordinates follow the logical sequence canvas rather than pooled GPU texture
size. Correction and original pixels mix in the operation's declared coordinates
(ACEScg for scene operators, ACEScct for log operators), preserving alpha.
Invalid geometry fails closed with corrector provenance. Independent geometry
references and CPU/GPU tests cover a non-square logical frame; real MCP authoring,
PNG and ProRes redecode verify spatial isolation of a masked exposure.

Native Contrast now uses an explicit ACEScct bridge. Pivot is an ACEScct code
value in 0–1; Amount is the log2 slope in −4–4, so +1 doubles slope and −1 halves
it. This Photonic creative contract differs from Legacy Contrast. Neutral Amount
is skipped before transfer to preserve exact scene identity. Masks blend in log
coordinates before decoding. Numerical/GPU qualification passes, including signed/extended input, partial
alpha, invalid parameters/domains, cache identity and exact neutral lowering.

Current workspace library and doc tests pass after the window/LUT increments.
Typed mattes, Bézier/Boolean windows, tracking, additional primaries, sources,
HDR and professional hardware qualification remain outstanding.

## Native creative LUT increment — 2026-10-04

The managed SDR compiler and live preview/delivery engine now execute creative
LUTs declared with equal ACEScg or ACEScct input/output spaces and native revision
1. A full-file content pin is required and independently verified by the LUT
cache. ACEScct LUTs have explicit encode → LUT → decode graph stages. LUT
sampling uses the declared coordinates directly, avoiding the Legacy sRGB
encode/decode kernel. Technical, cross-space, display-authored, unknown-version,
unpinned, changed, invalid and unresolved LUT dependencies remain refused.
Intensity blends in the declared grading coordinates; the authored LUT domain
and shaper determine behavior outside its grid.

Native LUTs carry distinct cache identity including table, shaper, domain,
interpolation and intensity. Pure compiler and cache regressions cover both
spaces, source pins and fail-closed behavior. CPU/GPU tests cover trilinear and
tetrahedral interpolation, a 1D shaper, explicit extended domains, partial
alpha, invalid data, and a logarithmic round trip against piecewise equations.
Export preflight warms the same validated dependency provider used by live
rendering. The media-pool interpretation menu exposes creative ACEScg/ACEScct
choices for pinned LUTs; older assets use the existing Replace/repin action.
The native quick saturation control uses the qualified AP1 operation. Native
Color-panel read-only qualification, workspace library tests and doc tests pass.

## Native saturation and diagnostic provenance increment — 2026-10-04

Native-managed Saturation & Vibrance has a separate scene-linear AP1 operation.
Its luminance coefficients use the AP1-to-XYZ Y row from the
[Khronos Data Format specification](https://registry.khronos.org/DataFormat/specs/1.4/dataformat.1.4.html).
This is a Photonic creative operator, not an ACES output transform. Saturation
accepts finite 0–4 and vibrance finite −1–1; signed and above-white RGB and
premultiplied alpha survive. Neutral settings are identity. Legacy Rec.709
arithmetic remains unchanged. The operation has distinct cache identity and
executes through the same persisted grade authoring used by GUI/MCP.

Native lowering now retains resolved corrector IDs. Its structured failures
include operator, owner, sequence path, graph node, and independent/shared look
stage, plus the reason the correction cannot execute. Enabled unsupported
corrections still refuse preview/delivery rather than publishing a partial look.

Independent f64 AP1 vectors supplement CPU/GPU parity. Tests cover invalid
parameters, Legacy-domain rejection, signed/extended values, transparent and
partial alpha, cache identity, graph-node/shared-look provenance, and diagnostic
wire round trips. The real MCP authoring, PNG, gallery, ProRes/redecode flow passes.
This does not qualify other managed operators, masks, LUTs, HDR, or the entire
roadmap.

## Native group and look increment — 2026-10-04

Native-managed SDR preview and delivery now lower supported group pre-grades
(root to leaf), clip grading, independent/shared looks, and group post-grades
(leaf to root), followed by track/master grading. These are per-shot corrections,
not corrections on a group composite. Adjustment clips retain their previous
restricted contract. Unsupported operators or masks still fail closed; missing
shared looks retain structured owner/stage/sequence provenance. Missing group
ancestors, cycles, and unknown group kinds refuse rendering instead of silently
omitting an authored stage. Unreferenced groups no longer block unrelated shots.

Compiler regressions exercise preview/delivery order, malformed ancestry,
missing and unsupported shared looks, and a noncommutative numerical reference:
source 0.1 → group offset +0.2 → clip exposure +1 stop → group offset +0.3 = 0.9.
The existing shared-look resolver is reused by Legacy SDR and Native Managed.
Verification on 2026-10-04: video library **844 passed, 2 ignored**; native managed
graph **11 passed**; native source GPU **9 passed**; workspace compile, formatting
and diff checks passed.

This extends the supported SDR boundary; mixed-source, effects, nested sequences,
managed LUT/mask execution, HDR, and full roadmap acceptance remain incomplete.

## Tracker synchronization

### Native sequence preview increment (2026-09-24)

The interactive sequence viewer now compiles a limited native-managed path for
enabled video tracks with one active clip per track, explicit native input
interpretation, and supported serial asset, clip, track, and master primary
corrections. It decodes into scene-linear ACEScg and ends with the sRGB SDR
display transform. Finite, invertible clip transforms and per-format reframing
use a dedicated transparent-border graph operation; finite clip/track opacity in 0–1 uses the
Normal composite. It refuses effects, unsupported correctors, looks, captions,
project/composition graphs, non-Normal composites, and other unsupported stages with a visible
grading diagnostic and blank frame.
Supported serial grade graphs lower through the same scene-linear operators as
ordered stacks. A native sequence compiler regression checks a clip exposure
graph; other graph topology still needs workflow qualification.
Grade-only adjustment clips can correct the existing scene composite in stack
order; adjustment effects and transforms remain unavailable.
Native-managed clip transforms now sample transparent black outside source
bounds, including partial-alpha bilinear edge coverage; Legacy SDR transforms
retain their established edge-clamp behavior. CPU known-pixel and GPU parity
tests cover nearest and bilinear translation, and the native preview compiler
regression checks that it selects the distinct transform operation. A cache-key
test prevents aliasing between Legacy and native border semantics. A CPU/GPU
composite test confirms that moving the upper shot reveals the lower track at
the transparent edge. The full
workspace test sweep, workspace compile, formatting, and diff checks pass
after this change.
Missing or invalid source interpretation also fails closed. The full
managed timeline compiler remains gated. Compiler regressions validate the
graph domain, grade-scope order, and refusal paths. A real FFmpeg-decoded video
frame also renders through the preview compiler and scene exposure, matching
the independent scalar display reference within 0.01 per channel.
The same generated source publishes an sRGB-encoded, diagnostic-free frame
through the actual engine session sequence target. Replacing that snapshot with
an unsupported blend publishes a new fully transparent frame with a color
error; the earlier valid frame is not left on screen as the current result.
The same qualified sequence can compile an isolated BT.709 video-signal
delivery graph when its output configuration explicitly requests that
transform. A real decoded source with scene exposure matches the scalar video
reference within 0.01 per channel. Export jobs now admit only full-resolution,
original-media ProRes MOV at the sequence frame rate and reject proxies,
hardware encoding, raw encoder overrides, and other output formats. Each
rendered frame must retain the BT.709 delivery encoding and have no color or
grading errors before it can reach the encoder.
Job resolution checks the first sequence frame and rejects an unsupported
source or grade immediately. It also detects an offline first-frame video
asset before starting the encoder; later-frame failures abort the staged file.
A dedicated engine-session command switches between sRGB display and BT.709
delivery output without changing the document; the real-source session test
verifies both published encodings and a later fail-closed edit.
The frozen export worker requests delivery output for native sequences. A real
source exports and decodes as 10-bit ProRes HQ or 12-bit ProRes 4444; invalid
exposure and missing input interpretation fail without replacing an existing
destination. A two-frame run with an unsupported second frame also aborts
after the first frame without publishing its staged output. Decoded ProRes
metadata reports BT.709 primaries, transfer, and matrix.
Delivery rechecks visible native source files at every frame, including clips
that start after the first frame. A later offline source aborts the job without
replacing an existing destination; the real ProRes regression covers this.
The MCP `export_sequence` job path also exports a native-managed video source
with the built-in ProRes preset and verifies the 12-bit encoded result by
ffprobe; it uses the same render and encoder worker as the GUI.
MCP `render_frame_at` now packs native sRGB display frames directly as PNG
instead of applying the Legacy linear-to-sRGB transfer twice; it reports the
published frame encoding for PNG and raw readback. The real native-source MCP
test renders a full-quality PNG before starting the ProRes export.
A byte-level PNG regression proves a 0.25 sRGB code value stays at 64/255,
while the same numeric value in Legacy linear encodes at 137/255.
Reference capture now accepts the qualified Native Managed preview in GUI and
MCP only after checking the published sRGB encoding and full-quality original
frame. The PNG writer uses that encoding without a second transfer. Gallery
comparison checks the stored color configuration and image hash; MCP comparison
returns image metrics but leaves the Legacy-only Printer Lights suggestion
absent. The real-source MCP flow captures, lists, compares, and exports the
same native sequence. Unqualified managed configurations remain rejected.
The program-monitor Extract Frame command also uses the published encoding
and refuses frames with grading or color errors, so an sRGB native preview is
not silently saved with a doubled transfer.
A GUI integration test generates a real native-managed video, captures its
reference PNG at full quality, verifies the red display pixel, and opens the
comparison view against the saved still.
Normal multi-track composites lower to scene-linear merges; a
non-Normal track blend produces a diagnostic and a blank frame.
The color status API exposes `preview_mode` and `delivery_mode` separately
from `available`; the latter continues to mean full timeline and export
support.
Native program scopes now tap the sRGB display output of the qualified preview
graph. CPU and GPU scope analysis use an explicit signal mode, so they measure
sRGB component code values without a second BT.709 transfer. GUI scopes label
that interpretation and force full-range plotting; MCP reports it and omits
video-legal excursion claims. Chroma uses selected luma coefficients and is
labelled as derived display chroma, not a calibrated BT.709 video signal.
Native scene-linear clip taps still fail closed pending scene-referred scales.
A 0.25-gray CPU/GPU scope vector and real native MCP program/clip-tap tests
cover the distinction.
Interactive native sequence preview now checks the active video source files
before publishing a frame. An offline source produces an explicit color error
and transparent fallback, so gallery capture refuses it instead of saving a
silent blank. The pure graph compiler remains filesystem-independent; its live
variant is used by the engine. Compiler and real engine-session regressions
cover offline and restored source paths.
Live native preview and each delivery frame also enforce native input preflight.
A probed unsupported layout such as planar GBR, or an asset without a probed
pixel format, cannot enter the decoder's legacy 8-bit fallback with a Native
Managed grade. Valid sources need stored probe metadata before these paths run.
Delivery checks the first frame during job resolution and later frames during
rendering, leaving an existing destination untouched on failure.
The decoder's fresh probe is compared with the stored pixel format. A mismatch
produces a visible native color error and a transparent fallback; replacing a
file behind an already open decoder is also detected by file size, timestamp
and, on Unix, device/inode identity before another native frame is served.
The source must be re-probed or relinked before grading resumes. This is not a
cryptographic file-identity check, and a changed file with indistinguishable
filesystem metadata remains a qualification gap.
Native frames bypass the playback preview cache so each presented source passes
the live identity check. A real 4:4:4-to-4:2:2 replacement regression covers
preview and export. A corrupt-but-present file also produces a visible native
source error. Export aborts both failures while preserving its destination.
Changing an asset's probed pixel format invalidates its cached decoder, so a
corrected probe can recover a native preview without reopening the project.
The GUI gallery and MCP native export fixtures now carry the same required
probe metadata as imported assets; both workflows pass their focused tests.
The full `cargo test --workspace` sweep passes after updating its timeline
command exhaustiveness guard and spec-extractor command inventory for the new
roadmap commands. The opt-in hardware soak and fixture-generator tests remain
ignored by the normal sweep.
MCP background probe and proxy-generation commits now use core history
commands. Their revisions refresh the MCP engine snapshot and their metadata
transitions can be undone. `remove_proxy` detaches a batch in one undo step and
retains generated cache files so undo can restore a usable proxy; cache
eviction remains a separate concern. Focused MCP regressions exercise proxy
revision and undo, including detach. This does not qualify the entire proxy
cache lifecycle or complete the remaining grading roadmap.
Probe jobs now hash the entire source before and after probing and refuse to commit
if the bytes changed. Their commit also confirms that the asset still points
to the probed path, so a relinked asset cannot receive stale metadata. Proxy
jobs likewise check the source path and current proxy at each history
transition, preserving user-attached replacements or detachments made while
the worker runs. Proxy cache selection uses a fresh source hash, and a changed
source after transcode is marked failed rather than ready. The persisted relink
and proxy-cache keys retain the existing head/tail/length format for project
compatibility; the worker's before/after integrity checks use the existing
full-file hash helper. External writes that race with the final check and
history commit remain a qualification gap.
Proxy transcode now validates the source before atomically publishing its
staging file. A failed validation removes only the staged file and preserves
any previous cache entry; a real-FFmpeg regression covers this failure path.
After the initial history change, `cargo test -q --workspace`, MCP's library tests,
`cargo fmt --all -- --check`, and `git diff --check` pass. Long hardware soak
and fixture-generation tests remain intentionally ignored by the standard run.

Unnamed Development MCP was unavailable in this session. Pending tracker payload:

- Project: Photonic
- Work: Professional Color Grading Roadmap / Milestone 1B managed color
- Intended status: In progress; Milestone 1A implemented and automated checks
  passing; 1B contracts and development qualification implemented, runtime
  integration pending; Milestone 2 clip versions in progress; Milestones 3–5
  remain pending.


## Native PQ source interpretation qualification — 2026-10-04

Native source intent now has a distinct `bt2100_pq_display` standard. It requires an explicit `reference_white_nits` integer (1–10000) and BT.2020 nonconstant-luminance matrix. Code-range expansion and chroma reconstruction precede the BT.2100 PQ EOTF; encoded RGB excursions clip to the defined [0,1] PQ domain. Absolute light in nits is divided by the authored reference white, converted from Rec.2020/D65 into AP1/ACES white, and premultiplied by alpha. This is display-referred source interpretation, not camera rendering inversion. Existing SDR/still serialization omits the new optional field and rejects inappropriate normalization.

The GUI offers an explicit PQ choice and a reference-white draft initialized to 203 nits; nothing is persisted until Apply. MCP exposes the same contract. Existing SDR display and SDR ProRes delivery remain the output paths; this source increment does not qualify HDR output, HDR scopes, HLG, HDR monitors, or camera-specific IDTs.

The isolated native-source GPU suite passes all 10 tests, including independent PQ 100/1000/10000-nit anchors, packed 16-bit full/limited Y′CbCr, three reference whites, colored samples, alpha, rejected normalization and serialization compatibility. Normative EOTF: [ITU-R BT.2100-3](https://www.itu.int/dms_pubrec/itu-r/rec/bt/R-REC-BT.2100-3-202502-I!!PDF-E.pdf). The end-to-end MCP PQ-source test also passes: ten-bit source decode, graded PNG preview, window isolation, synchronous/queued scopes, SDR ProRes encode/readback parity and manifest verification. Workspace all-target compilation passes.

The MCP audit panel now uses full-width result cards with status, duration, expandable/copyable arguments and character-safe summary truncation. A multibyte-result regression passes, and the isolated 1280×800 native editor screenshot shows a readable live manifest inspection result (`/tmp/photonic-color-ui/vfl-audit-fixed.png`). Export job option labels now describe user actions without internal specification identifiers.


## Native CDL and log wheels — 2026-10-04

Native `Cdl` and `Wheels` now lower into a distinct ACEScct SOP/saturation operation, surrounded by explicit encode/decode stages. Wheels retain the authored slope = gain − lift, offset = lift, power = 1/gamma mapping. These are log wheels, not scene-linear lift/gamma/gain. Neutral operations skip both transfers for exact identity; static windows mix in ACEScct before decoding. Native hashing includes every CDL parameter.

This contract uses no-clamp CDL: negative SOP passes through the power stage, and saturation uses fixed Rec.709 CDL coefficients. SOP/power intermediates are bounded to ±65504 to avoid undefined arithmetic at the working storage boundary. Loaded invalid parameters fail closed (finite slope 0–8, offset −4–4, power 0.1–10, saturation 0–4). Existing Legacy SDR CDL/wheels retain their arithmetic. [OpenColorIO's CDL documentation](https://opencolorio.readthedocs.io/en/v2.4.2/api/transforms.html#cdltransform) describes the no-clamp convention; this implementation does not load or depend on OCIO.

Independent scalar-formula and GPU tests pass for signed scene values, alpha, ACEScct transfer, SOP/power/saturation and domain refusal. A reproduced extreme-coordinate overflow is fixed; compiler/cache and end-to-end MCP preview/scopes/ProRes export/manifest qualification also pass.


## Native master/RGB curves — 2026-10-04

Native master and per-channel RGB curves now run in ACEScct, with endpoint tangent extrapolation outside [0,1] so negative scene values and above-white headroom are not implicitly clipped. Explicit encode/curve/window-mix/decode stages share preview/delivery compilation. Neutral curves skip the stage for exact identity, and cache identities include every sampled table. Invalid samples and unqualified hue/luma/saturation families fail closed. The GUI labels logarithmic coordinates and disables unqualified curve families and viewer sampling; Legacy curve behavior and sampling remain unchanged.

An independent affine-curve CPU/GPU test passes for signed scene input, highlights, alpha and domain refusal. Compiler/cache and all five native MCP preview/scopes/export/manifest checks pass. The full workspace library suite passes: core 873, GUI 462, MCP 260, render 195 and video 855 (two video tests intentionally ignored). Documentation tests and the all-target Clippy performance gate pass; other Clippy warnings remain. All 24 native managed GPU tests, 10 native source GPU tests and the Legacy golden-frame check pass without reblessing. The native export dialog now seeds ProRes Mezzanine and checks shared settings validation before submission. The interactive check does not warm LUTs or compile media graphs; final submission retains complete source qualification. Its regression passes. All 195 render tests and 23 native managed GPU tests passed after correcting a misplaced LUT-only uniform reference in the Legacy curves shader.


The rebuilt native editor verifies these controls at 1280×800. Export opens on ProRes Mezzanine; selecting Web H.264 displays the real settings error and disables submission. A live curve/CDL grade renders, and scrolling now reaches both editors: the prior right drawer clipped them below the viewport because it had no outer scroll area. Evidence is retained in `_arcwright-output/qualification/color-2026-10-04/vfl-native-{export-preflight,export-rejected,curves-scroll,cdl-scroll}.png`. Inspection of the earlier live manifest still verifies after the optional color-schema additions. This does not finish the overall roadmap.


## Native HLG scene input — 2026-10-04

`bt2100_hlg_scene` explicitly requires reference white and a nominal HLG reference peak (400–2000 nits, white between 1 and peak). The inverse HLG OETF recovers scene light, retaining positive headroom and using an odd signed extension below black. The neutral-white exposure scale is `(white / peak)^(-1 / gamma)`, where `gamma = 1.2 + 0.42 log10(peak / 1000)`; this anchors scene white to the reference display's zero-black neutral response. It does not apply the luminance-coupled display OOTF or a per-channel display gamma. Rec.2020/D65 conversion produces scene-referred ACEScg. PQ remains a separate absolute display-referred interpretation.

The GUI drafts white at 203 and peak at 1000 nits, displays both before Apply, and wraps source controls within narrow drawers. Neither value is inferred into document state. MCP exposes the same metadata and validation. Non-HDR sources omit both optional fields; inappropriate peak/white metadata is rejected. SDR display/delivery remain the qualified outputs; HDR output and HDR scopes are still pending. Reference equations and the production-monitor peak range follow [ITU-R BT.2100-3, Table 5 and Note 5f](https://www.itu.int/dms_pubrec/itu-r/rec/bt/R-REC-BT.2100-3-202502-I!!PDF-E.pdf); the white-anchor normalization and odd extension are explicit Photonic source semantics.

All 11 native source GPU tests pass, including HLG nominal anchors, neutral white, signed excursions, three white/peak choices, full/limited packed 16-bit Y′CbCr, color, alpha, validation and serialization. The initial test caught an omitted HLG reference-white upload parameter; corrected parity passes. The end-to-end MCP HLG-source preview, scope-job parity, ProRes readback and manifest test passes, as does workspace all-target compilation. The full libraries pass: core 873, GUI 462, MCP 261, render 195, video 855 (two ignored). Documentation tests and the all-target Clippy performance gate pass. The native 1280×800 input editor now displays all three source dropdowns, reference white, HLG peak and Apply/Clear buttons within the drawer. Live probing and rendering pass; evidence is retained in `_arcwright-output/qualification/color-2026-10-04/vfl-hlg-source-controls.png`.

## Native normalized white balance — 2026-10-04

The existing temperature/tint corrector now runs in native scene-linear ACEScg. Its v1 artistic gains are `[1 + 0.4 temp, 1 - 0.2 tint, 1 - 0.4 temp]`, lowered to the qualified native printer-light pass using `12 log2(gain)`. Neutral bypass is exact; authored normalized parameters remain unchanged, including animation and undo. Invalid loaded values produce owner-aware export-blocking diagnostics. Static windows mix in scene light. Equivalent channel gains intentionally share the render-cache identity of printer lights. Legacy arithmetic is unchanged. This does not establish physical Kelvin/CCT white balance.

Compiler tests cover neutral identity, boundary gains, parameter cache invalidation and invalid values. Independent GPU vectors cover color, signed excursions, highlights and partial/zero alpha. The MCP preview, synchronous/queued scopes, ProRes readback and manifest round trip passes with a non-neutral green trim. Workspace all-target compilation and diff whitespace checks pass; the full video library passes 856 tests (two ignored), GUI passes 462, and native live-control inspection passes at 1280×800. Evidence is retained in `_arcwright-output/qualification/color-2026-10-04/vfl-native-white-balance.png`.

## Native logarithmic HSL secondary — 2026-10-04

Native qualifiers now select HSL computed from ACEScct/AP1 channels bounded to `[0,1]` for selection only. The correction uses the original extended logarithmic values with the qualified no-clamp CDL, mixes by the key, applies any static window in the same log domain, then returns to scene-linear ACEScg. Wrapped hue, disjoint add/subtract keys and matte thresholds retain their existing authoring model. Neutral CDL bypass is exact. This is an explicit Photonic log-grading key, not display HSL or a calibrated colorimetric selector. Invalid key ranges, counts, correction values and matte thresholds fail qualification. Native monitor picking, swatch seeding and Highlight matte are disabled until their pre-correction tap implementation uses this domain; typed matte graph ports and spatial refinement remain pending.

Independent CPU/GPU vectors pass for red-seam selection, green inclusion, subtract keys, half-weight soft edges, extended channels and opaque/partial/zero alpha. Graph checks enforce log-domain input. Compiler checks cover neutral identity, static-window routing and key-cache changes. The first cache check exposed a shared bug: matte thresholds and sampled regions were missing from qualifier hashes. They now invalidate both Legacy and native render caches, while default Legacy keys preserve their original identity. The MCP qualifier preview, synchronous/queued scopes, ProRes readback and manifest round trip passes. All 195 render tests, 857 video tests (two ignored), 462 GUI tests and workspace all-target compilation pass. The Legacy golden corpus passes without reblessing. The native qualifier controls render and scroll at 1280×800; evidence is retained in `_arcwright-output/qualification/color-2026-10-04/vfl-native-qualifier.png`.

## Native advanced logarithmic curves — 2026-10-05

All five secondary curve families now run in native ACEScct: hue versus hue/saturation/luma, luma versus saturation and saturation versus saturation. HSL selection uses bounded AP1 log channels. The luma-coordinate families use AP1 coefficients on those log coordinates, not calibrated scene or display luminance. Corrections retain the difference between the original extended channels and their bounded key values, so secondary curves do not clip signed values or positive headroom. Master/RGB endpoint extrapolation remains unchanged. Empty families are disabled and explicit neutral midlines bypass exactly when the entire operation is neutral. Non-finite authored points and invalid resolved tables fail qualification. GUI tabs expose these controls and label AP1 log semantics; native monitor/swatch sampling remains pending.

Independent CPU/GPU vectors pass for every family, AP1-weighted luma sampling, extended-channel residuals, combined neutral curves and opaque/partial/zero alpha. Compiler tests pass for neutral secondary curves, active secondary-only curves, invalid loaded points and cache identity for every table. MCP authoring, preview, synchronous/queued scope measurement, ProRes readback and manifests pass with all five families authored. Workspace all-target compilation passes. All 195 render tests, 26 native managed graph tests, 462 GUI tests and the Legacy golden corpus pass. The rebuilt native 1280×800 editor exposes all nine tabs, and the authored hue-versus-luma curve renders. Evidence is retained in `_arcwright-output/qualification/color-2026-10-04/vfl-native-secondary-curves.png`.


## Native qualifier input inspection — 2026-10-05

Ordered Native Managed clip qualifiers now expose a dedicated scene-linear input tap before their own correction. The picker encodes that tap through the same ACEScct half-float boundary as the corrector and selects bounded AP1 logarithmic HSL. Highlight matte evaluates the same key, sampled add/subtract regions, thresholds, static window and alpha without applying the qualifier’s CDL or later correctors. Unavailable inputs fail rather than sampling program output. Neutral correction bypass retains inspection metadata. Graph correctors, shared grades and native curve sampling still require separate input-tap integration.

MCP `inspect_qualifier` returns the same opaque grayscale coverage image for Native or Legacy ordered clip qualifiers, with coverage statistics, exact frame tick, document revision, snapshot generation and key-coordinate interpretation. It does not edit the document or history. Missing or inactive qualifiers are rejected. GUI picker results additionally reject changed frame hashes or snapshot generations before applying one undoable edit.

The compiler input-order regression, independent GPU matte vectors and Native/Legacy MCP inspection checks pass. The Native MCP test also exercises preview, scope measurement, ProRes readback and manifest verification. The GUI integration verifies input-domain sampling despite earlier/later exposure, matte preview without edits, stale-result rejection, one-step apply and exact undo. Its first run passed assertions but crashed during GPU process teardown; a repeat and five additional isolated runs exited successfully. This intermittent teardown event remains recorded pending broader and live qualification.


Broader qualification passes all 463 GUI tests with a clean process exit. The MCP run passes 265 existing/behavior tests; its new schema assertion initially selected the common result wrapper incorrectly and passes after correction. Workspace all-target compilation and diff whitespace checks pass. The rebuilt 1280×800 editor verifies live picking while the isolation matte is visible, key updates and window coverage; the completion status now reports the applied sample and the matte label occupies the viewer’s upper right without overlapping readiness text. Live MCP inspection reports the same updated key’s coverage and ACEScct/AP1 interpretation. Evidence is retained in `_arcwright-output/qualification/color-2026-10-04/vfl-native-qualifier-input.png` and `mcp-native-qualifier-input.png`. Visual scores for this control/preview target: layout 8, color 9, typography 8, components 8. The isolated GPU teardown event has not reproduced in the repeat series, full GUI library or live editor; it is not claimed to have a confirmed cause or fix.


## Native curve input sampling and shutdown diagnosis — 2026-10-05

Native ordered clip curves now retain an exact scene input tap even when neutral. Monitor picking encodes that input at the same ACEScct half-float boundary as the curve, uses bounded AP1 logarithmic coordinates, and excludes the selected corrector and later grades. Native master/luma-versus-saturation anchors use AP1 coefficients; Legacy anchors keep their prior Rec.709 coefficients. Native swatch seeding stays hidden because a display swatch has no authored log-source interpretation. Shared and graph correctors remain outside this input-tap increment.

Compiler input-order checks and the GPU-backed GUI picker integration pass, including an independent colored-source calculation of AP1 log luma, earlier/later exposures, neutral curves, stale result rejection, one-step apply and exact undo.

The intermittent process-exit crash recurred in the curve test and was captured under GDB. The faulting `photonic-embed` thread was inside ONNX session initialization while the main thread destroyed ONNX’s global operator-schema registry during `exit`. The previously detached semantic-search worker could therefore outlive the app. Semantic search now starts model initialization only for a nonempty query, disconnects its request channel on drop and joins any active worker before exit. Native library initialization cannot be interrupted safely; shutdown waits for it when a search is already loading. Lifecycle tests verify empty searches do not initialize a model and drop disconnects/joins an initializing worker. Both qualifier and curve picker tests then pass with clean exits. Debugger evidence: `/tmp/photonic-native-curve-gdb-2.log`.


The post-fix qualification series exits cleanly in ten alternating qualifier/curve picker runs. All 466 GUI tests and 859 video tests pass (two video tests intentionally ignored), and workspace all-target compilation passes.

MCP now exposes `sample_grade_input` for enabled Native Managed ordered clip curves and qualifiers. Normalized canvas coordinates select a source-covered pixel from the exact pre-correction scene tap; the tool uses the same ACEScct half-float encoding and bounded straight AP1 log coordinates as the GUI, and returns RGB, AP1 log luma, alpha, pixel location and frame provenance without editing history. It rejects zero coverage, invalid coordinates, unsupported correctors/scopes and absent inputs. A real PNG-backed MCP regression independently verifies sRGB decode, AP1 conversion, earlier exposure, ACEScct sampling, partial alpha and exclusion of a non-neutral selected curve/later exposure. The qualifier MCP preview/scopes/ProRes/manifest test passes with input sampling included.


All 267 MCP library tests pass after the sampler addition. In the rebuilt editor, picking a yellow source region for the native luma-versus-saturation curve creates the neutral anchor at `0.5420207381248474`, exactly matching the live MCP sampler’s AP1 log-luma result. The source-input sampler and completion status render at both 1280×800 and 1920×1080. Evidence is retained in `_arcwright-output/qualification/color-2026-10-04/vfl-native-curve-input.png` and `vfl-native-curve-input-desktop.png`; target scores are layout 8, color 9, typography 8, components 8. A live nonempty semantic search starts real `photonic-embed` workers, and the editor then closes with exit status zero. MCP reference documentation is regenerated from the current tool catalog.


## Typed grading mattes and key mixing — 2026-10-05

The embedded grade graph now distinguishes image and matte ports. A
`qualifier_matte` node resolves the selected qualifier's key and static window
from its explicitly routed image input, excluding that qualifier's CDL.
Native inputs are explicitly encoded into bounded ACEScct/AP1 key coordinates;
Legacy inputs retain their existing key coordinates. Its output is an
unassociated 0–1 weight, stored as opaque grayscale. Zero source coverage
produces zero key weight. Disabled or unresolved key sources produce a black
matte; unresolved dependencies retain export-blocking diagnostics.

`key_mixer` supports max union, min intersection, first-minus-second subtraction,
and multiplication. `matte_apply` blends corrected straight color into the
original image through that weight and preserves the original alpha exactly
once. Native grading layer mixers now interpolate grade branches while
preserving the bottom branch's coverage, instead of accumulating alpha through
source-over compositing. Existing Legacy layer mixer arithmetic is unchanged.
Matte outputs have their own render-IR domain and cannot enter an image,
display, or export operation. All authored nodes, including parked branches,
are checked for missing references, cycles and incompatible port types before
an edit commits. Whole-grade copies remap both correction and key-source IDs.

Color Controls exposes key utilities, typed input choices, labels and mix modes.
White image pins and green matte pins identify routing types. **Expand graph**
opens a wider canvas that displays the seven-node qualification graph at both
1920×1080 and 1280×800. The live click test found and fixed an existing pending
connection bug: the canvas stored `Option<u32>` but retrieved `u32`, losing the
selected output between frames. A multi-frame pointer regression now exercises
selection, an intervening frame, and a later input click. Live GUI routing
created exactly one history revision; one undo restored the original topology.
Opening either canvas is read-only. Incremental visual scores: layout 8,
color 9, typography 8, components 8; this is not whole-workspace final signoff.

MCP `effect_stack` adds `add_grade_graph_utility` with a tagged
`grade_graph_node` payload. Valid utility edits are one undo step; invalid
image-to-key connections leave the document and history unchanged. Existing
whole-grade serialization carries the same graph. Generated MCP documentation
was refreshed from the running schema.

Independent CPU/GPU known values cover partial alpha, negative channels,
above-white values, all four key modes, transparent pixels and key weights
independent of alpha. The complete GPU evaluator is tested on a 17×9 logical
frame in pooled textures. Compiler tests cover native key-domain lowering,
native layer mixing, exclusion of the qualifier's CDL from the key cache,
and continued image-cache dependence on that CDL. The five affected library
suites passed at this increment: core 874, render 196, video 861 (two existing
ignored), GUI 467 and MCP 268. The later routing and save regressions add GUI
tests. All 27 managed graph integration cases and Legacy goldens passed without
reblessing; the workspace all-target check and performance lint gate passed
before the subsequent save parity fix.

Live MCP authoring rendered a full-quality preview and exported 60 frames of
640×360 ProRes 4444 with `yuva444p12le`, BT.709 metadata and a render manifest.
Manifest inspection verified the output hash. A decoded-frame comparison,
accounting for sRGB-preview versus BT.709-video transfer, measured median
absolute normalized linear display-light error 0.00115, p95 0.00885 and maximum
0.02277; this is lossy-codec qualification, not exact pixel identity. Artifacts:
`_arcwright-output/qualification/color-2026-10-04/mcp-native-typed-matte.png`,
`vfl-native-typed-matte.png`, `vfl-native-typed-matte-desktop.png`,
`live-typed-matte.mov` and its manifest, plus `native-typed-matte.photon`.
The fixture's source references are local qualification dependencies, not a
portable archive. Graph-specific input sampling/inspection, matte refinement,
Bézier/Boolean windows, animation/tracking, shared/compound nodes and the other
roadmap milestones remain pending.

## MCP save and GUI saved-history parity — 2026-10-05

MCP saves now publish the document and exact history node actually written to
disk. The host acknowledges the receipt after drawing, updates the matching
active or parked tab's saved marker/path, and leaves newer edits dirty. Failed
writes publish no receipt. Notifications are bounded for headless sessions;
MCP saves are serialized, and a save finishing after a document switch does
not replace the active document's path. The save notification state survives
MCP-server restarts.

GUI regressions cover exact saved-state acknowledgement, a newer edit remaining
dirty and a save completing after a tab switch. MCP save tests cover native
round-trip, the successful receipt, pathless save and failure preserving prior
bytes/path without a receipt. Live GUI routing persisted through pathless MCP
save; undo restored the qualification graph, another save persisted it, and
the editor closed with exit code 0 and released its MCP port. The earlier
routing smoke process reported exit 143; that termination's cause was not
established, and it is not described as an ONNX or GPU crash.


## Spatial matte refinement qualification — 2026-10-05

A typed `MatteRefine` graph utility now applies a 3×3 median, square grow/shrink,
separable Gaussian blur, and clean black/white thresholds in that order. Grow
and Gaussian sigma use fractions of the logical picture's shorter dimension,
limited to ±2% and 0–2% respectively; median neighborhoods use processing
pixels. Preview resolution therefore affects median filtering, while full-quality
export uses the final processing grid. These controls are graph utilities;
ordered qualifiers do not yet expose the spatial stages directly.

CPU and GPU known-value tests passed for isolated impulses, dilation, erosion,
Gaussian samples, tiny sigma, constant keys, and logical picture edges with
nonzero texture-pool padding. Invalid settings are rejected before mutation.
The complete native evaluator passed a partial-alpha, extended scene-linear
fixture through refinement and matte application. Core serialization/removal,
MCP one-edit/invalid-setting/undo, GUI typed routing, and compiler neutral
passthrough/cache-invalidation regressions passed. Live MCP full-quality PNG produced no grading diagnostics. A 60-frame
640×360 ProRes export completed and its render manifest verifies. Comparing
sRGB PNG to decoded BT.709 ProRes in linear display light measured median
absolute error 0.00115, p95 0.00885, maximum 0.02277; this lossy codec comparison
is not an exact numerical identity claim. The saved fixture is
`_arcwright-output/qualification/color-2026-10-04/native-matte-refinement.photon`
and retains local source references rather than being portable.

The complete core/GUI/MCP/render/video library suites passed
**875 / 470 / 268 / 199 / 861** tests (two existing ignored video tests).
All **27 native graph integration cases** and the **Legacy golden-frame corpus**
passed without reblessing. `cargo clippy --workspace --all-targets -- -D
clippy::perf` passed with existing warnings. Controls were inspected at
1280×800 and 1920×1080; their layout, colour, typography and components score
8/9/8/8 against DESIGN.md. The expanded graph initially clipped the output
column; its default width now follows graph depth within viewport bounds and
that final layout passed live verification at both desktop sizes, with all eight
nodes and the output port visible. The app loaded the saved graph and closed
cleanly after qualification. Large spatial radii add
work and carry no real-time performance promise.


## Native graph correction input inspection — 2026-10-05

Native Managed clip graphs now retain the upstream scene image before an
unambiguous curve or HSL image corrector, including neutral correctors. Sampling
and qualifier inspection use that input; own and downstream corrections are
excluded. Rewiring a corrector changes the sampled branch. Operator-only
requests reject repeated image-corrector references rather than selecting one
silently. Key-source utilities are not duplicate image correctors, and this
inspection does not represent downstream key mixing/refinement. Explicit
node-addressed inspection and shared/compound grade scopes remain pending.

Compiler branch-rewiring and duplicate-reference regressions passed for curves
and qualifiers. Native GUI graph picker/matte tests passed with one-step undo
and stale snapshot rejection. MCP graph qualifier inspection/preview/ProRes and
independent partially transparent sRGB graph curve input checks passed. The full core/GUI/MCP/render/video suites passed **875 / 472 / 270 / 199 /
861** tests, with two existing ignored video tests. The workspace all-target
performance gate passed with existing warnings after final control and action
handler fixes. Live MCP graph sampling and matte inspection returned exact
frame provenance. Live GUI Highlight matte worked at 1280×800 and 1920×1080;
picking while highlighted changed only the key, and one undo restored the
original grade and topology exactly. The app closed cleanly. Visual scores
for the control/matte target are 8/9/8/8 against DESIGN.md.

Live verification caught an additional ordered-only gate in the panel action
handler after the controls were enabled. The handler now uses a shared toggle
method, and the GUI graph regression exercises that toggle before readback.
Evidence: `vfl-native-graph-qualifier-input.png`, its desktop counterpart,
`vfl-native-graph-qualifier-picked.png`, `mcp-native-graph-qualifier-input.png`,
and `native-graph-input-sample.json` in the color qualification directory.


## Explicit native graph input inspection — 2026-10-05

`sample_grade_input` and `inspect_qualifier` now accept optional
`graph_node_id`. Requests identify both graph node and operator, so a node
whose operator changed cannot satisfy a stale request. Native curve/HSL image
correctors and qualifier key sources retain their exact scene inputs separately.
Operator-only requests retain their existing unambiguous-input rule. These
inputs must participate in the currently rendered clip graph; inactive/missing
inputs fail rather than falling back to program pixels. Key-source inspection
is coverage from the source key and window, excluding downstream key mixing
or spatial refinement; generic graph-node output inspection remains separate.

The Color editor exposes an input selector for native graph operators. Changing
it cancels an armed/pending sample and switches an active matte preview to the
new target without editing the document. Pick application still edits the
operator referenced by the nodes, so multiple instances continue sharing that
operator's parameters.

Compiler tests passed for independent repeated-operator branches and mismatched
operator IDs. Three GUI tests passed for explicit curve, HSL image-corrector,
and qualifier key-source sampling/matte/undo paths. MCP repeated-curve branch
sampling passed independent partial-alpha sRGB/AP1/ACEScct values and atomic
invalid/missing/ambiguous input rejection. Full affected core/GUI/MCP/render/video libraries passed **875 / 476 / 270 /
199 / 861** tests (two existing ignored video tests). Explicit MCP key-source
coverage/input matched the corresponding image corrector in the shared fixture.
The workspace all-target performance gate passed with existing warnings.

The live fixture routes the key source after +1 stop while its image corrector
reads the original scene. MCP node samples differ by the expected ACEScct stop
step within GPU tolerance. Selecting the key source in the GUI makes no document
edit. A live pick from that source matched MCP's key-luma centre within 8×10⁻⁹;
topology was preserved and one undo restored the grade exactly. The input
selector and matte display passed at 1280×800 and 1920×1080 (8/9/8/8 visual
scores against DESIGN.md), and the app closed cleanly.

Evidence is in the color qualification directory: `native-node-inputs.photon`
(local-source fixture), `native-node-input-samples.json`, both explicit MCP
matte PNGs, and `vfl-native-node-input.png`, its desktop counterpart, and
`vfl-native-node-input-picked.png`. Generic mixed/refined matte outputs and
shared/compound grade scopes remain pending.

## Cloud continuation — 2026-10-05 (in progress)

Resumed from `30f221c` on `feat/professional-color-foundation`. The tracked
October 5 handoff is available in this checkout. No additional GitHub CLI login
was needed to clone this public repository.

### Raw matte-output inspection

The shared raw-weight preview/statistics helper feeds GUI and MCP. The new
`inspect_grade_matte` tool addresses reachable clip graph matte nodes, preserves
float statistics and returns an opaque grayscale PNG without display encoding
or alpha division. Invalid/unreachable/image nodes fail closed. Read-only GUI
selection works on locked tracks; asynchronous previews reject old revisions,
presentation state, selection and undo results. Inspection cancels incompatible
qualifier sampling without changing the document or command history.

Affected library qualification passed core **875**, GUI **479**, MCP **272**,
render **202**, video **861** with two pre-existing ignored tests. The actual
cloud desktop app displayed the mixed/refined matte on a locked clip and Exit
matte restored the program picture. Visual inspection found an overlapping
preview-status label and exit control; the control was moved down and the rebuilt native window confirmed
that both labels and the Exit matte button are readable. This does not qualify the remaining professional workspace or all window
sizes. Native UI uses the supported cloud desktop; a shell-only Xvfb attempt
could not create its listening socket and is not the active UI route.

### First native general effect: Gaussian blur

Gaussian blur now runs directly on premultiplied scene-linear AP1 RGBA at asset,
clip, track, master and untransformed adjustment-clip effect scopes, before each
scope's grade. It preserves negative channels, highlights and alpha without a
transfer conversion. Enabled unsupported effect kinds and invalid radii still
fail closed. Other general effects, compositions, transitions, titles, captions
and non-Normal blends are not qualified by this increment.

The native GPU path follows the CPU bounded large-radius convolution plan,
including planned sample spacing; the historical Legacy path keeps unit sample
spacing. Independent impulse/extended-alpha/logical-edge/large-radius tests
pass. The first full post-effect video library run passed **864** tests with two
pre-existing ignored tests. Six integration targets passed **56** tests, including
the unchanged Legacy golden corpus. A real MCP preview/synchronous and queued
scope/reference/ProRes export/redecode/manifest round trip with Gaussian effects
at all four ordinary scopes also passed. Additional invalid-domain/arity/radius and animated half-open zone checks pass.
The final affected-library rerun passed GUI **479**, MCP **273**, and video
**865** tests (two existing ignored video tests). Rebuilt native UI layout confirmation passed. These are per-increment results; no current-tree
full release acceptance or whole-roadmap completion is claimed.

Cloud build setup uses Rust 1.99 and official Debian development-library headers
in a workspace-local prefix. The default ONNX download endpoint failed TLS
certificate validation; verification was not disabled. The build instead uses
the matching Microsoft ONNX Runtime 1.20.0 official release via its supported
local-library configuration. No project dependency versions were changed.

### Extended-safe scene-linear blend subset — cloud continuation

Normal plus Multiply, Screen, Darken, Lighten, Difference and Exclusion now
compile for native-managed video tracks. They operate on straight AP1 channels
inside premultiplied source-over compositing, without clamping negative or
above-white color. The separable equations and alpha composition are checked
against the [W3C compositing equations](https://www.w3.org/TR/compositing-1/#blending).
Their extension to signed/highlight AP1 coordinates is a Photonic contract;
it is not a claim of display-referred CSS or Photoshop matching. Modes with
bounded-color, luminance, division or other unqualified semantics remain gated.
Legacy blend behavior is unchanged.

Independent f64 oracle checks cover all seven modes, signed/highlight color,
opaque/partial/zero source and backdrop alpha, and zero/half/full track opacity
against actual CPU and GPU results (105 cases). Compiler checks verify the
selected mode and opacity reach the merge stage. IR checks reject invalid
opacity and unqualified modes. The session's existing rejection fixture now
uses the still-unqualified Color mode instead of newly supported Multiply.
The full video library passes **865** tests (two ignored), and a real MCP Screen
blend preview/scopes/reference/ProRes export/redecode/manifest test passes.
The subsequent full MCP run passed **275** tests, native graph integration
passed **28**, the Legacy golden harness passed, and all-target Clippy completed
with warnings.

### Bounded native composition/project graphs — cloud continuation

The first native graph subset supports ClipIn (clip compositions only), Output,
qualified Grade/LUT nodes, Gaussian Blur and the qualified AP1 Merge modes.
Clip graphs replace the raw source before asset effects/grade; project graphs
process the outer program after master effects/grade and before output encoding.
Nested sequences do not reapply the project graph. Empty timelines still
validate an authored project graph instead of silently bypassing it.

Reachable graphs are checked before lowering: missing nodes, inconsistent node
IDs, cycles, invalid/multiply connected ports, unsupported operations and invalid
resolved Gaussian/opacity values fail closed. Bounded validation allows at most
4,096 nodes, 8,192 edges and 128 levels. Disconnected branches are not rendered.
Legacy graph lowering remains separate and unchanged. Other native source, temporal, matte-extraction and utility node kinds remain
unqualified. Crop and transparent affine nodes are covered by the later geometry
continuation below; Resize remains unqualified.

The compiler stage-order, single source, nesting and malformed-graph tests pass.
A real MCP clip-grade-graph plus project-Gaussian-graph preview, scopes, reference,
ProRes export/redecode and manifest test passes. The video library passes **866**
tests with two existing ignored tests. The subsequent GUI/MCP runs passed **479 / 275** tests; all **28** native graph
integration checks and the unchanged Legacy golden corpus pass. All-target
Clippy passed with warnings. A final strict numeric-opacity regression
also passes. This is a bounded subset,
not completion of the full graph or managed-pipeline roadmap.

### Native graph geometry and Draft basis — cloud continuation

Native composition/project graphs now accept finite invertible Transform2D
nodes and finite normalized Crop margins. Transforms use transparent borders;
RGBA remains premultiplied AP1, including signed/highlight color. Wrong scalar
types, singular matrices and invalid crop margins fail closed. Independent CPU
and GPU pixel checks verify retained color/alpha and the exact transparent edge.

Qualification uncovered an existing native preview issue: authored-pixel scaling
was derived only from a Legacy `Output` terminal, while native sequences terminate
in a color output transform. A new identity `NativeCanvas` boundary carries
selected-format dimensions without an extra GPU render or full-size allocation.
The evaluator derives the Draft basis through the native output transform; affine
translation/pivots and native Gaussian sigma now scale to the processing canvas.
The isotropic Gaussian uses the smaller axis ratio for aspect-preserving Draft
rounding. Native output, source and cache identities retain their declared domains.

A new independent CPU/GPU regression compares authored 16px output against an
explicitly scaled 8px reference and tests 8→16→8 cache reuse. Logical output stays
at the requested processing size. Current-tree affected libraries pass GUI **479**, MCP **275**, and video **866**
with two existing ignored video tests. Native graph integration passes **30**
tests; both scaling targets pass **3** each, and the Legacy golden corpus passes
without reblessing. All-target Clippy passes with warnings.

### Native cross-dissolves and gap/end fades — cloud continuation

The native clip-chain compiler is shared between ordinary shots and borrowed
transition partners, so asset/clip/group/shared-look processing stays identical.
Adjacent media clips can now use CrossDissolve with a duration within the incoming
clip span. The outgoing source is sampled past its cut using the documented
handle model; a shortened handle emits Info and exhaustion emits Warning. Track
and master grading run once after the mix. Clip inspection taps remain the
individual graded shots, not the combined transition. CrossDissolve on an end/gap
`transition_out` fades toward transparent. Other transition kinds, transitions
across gaps and nested-source transition handles remain unqualified.

`NativeDissolve` interpolates all four premultiplied AP1 channels, including
coverage, rather than using source-over as a substitute for interpolation.
Independent CPU/GPU vectors cover signed/highlight color, partial/zero alpha and
five blend positions. The existing Legacy merge arithmetic is unchanged.
Compiler checks cover source times, per-clip opacity, one track/master grade,
shortened/exhausted handles and fade-out. A real MCP set-transition → undo → redo
→ preview → five-frame ProRes export → RGBA64 re-decode test passes, including
independent ACES preview values and changing alpha on every output frame. Preview
and export leave the document/history unchanged. Full affected libraries pass
GUI **479**, MCP **276**, video **867** (two existing ignored video tests).
Native graph integration passes **31** tests, the unchanged Legacy golden corpus
passes, and both scaling suites pass **3** tests each. All-target Clippy passes
with warnings.

### Native spatial transitions — cloud continuation

Adjacent media cuts also qualify DipToBlack, directional Wipe/Push and analytic
LumaWipe. They share the same borrowed-handle, per-clip processing and once-only
track/master path. DipToBlack uses two premultiplied interpolations through opaque
AP1 black. Native luma wipes have a distinct progress mapping that sweeps the full
softened map and guarantees exact endpoints, including zero/one map pixels.
Softness is finite in 0..0.5. DipToColor, nonadjacent/nested transition partners,
and non-CrossDissolve end fades remain gated.

Independent f64 map/alpha references cover 96 CPU/GPU parameter cases across
all directions, five maps, inversion, hard/soft edges, signed/highlight color and
odd-sized canvases. This exposed a GPU Clock-map center mismatch; the center is
now explicitly 0.5 in both CPU and WGSL, matching the CPU reference. The Legacy
CPU golden corpus remains unchanged; the GPU center correction is recorded
rather than claimed byte-identical to its prior center behavior.

Real MCP author/undo/redo, preview, five-frame ProRes export and RGBA64 re-decode
checks pass for all four additional kinds, including spatially varying alpha at
three sample positions and every exported frame. The full affected GUI/MCP/video
libraries pass **479 / 277 / 868** tests (two existing ignored video tests), native
graph integration passes **32**, the scaling suites pass **3 + 3**, Legacy goldens
pass without reblessing, and all-target Clippy passes with warnings.

### Catalog-created Gaussian routing regression

An end-to-end catalog check found that the Legacy raster bridge projects the
`blur.gaussian` identifier to `Unknown`, which incorrectly prevented the native
compiler from accepting Gaussian effects created by the actual catalog. Native
routing now checks the declared effect kind and qualified identifier before the
Legacy projection. The compiler regression covers both catalog-created and
historical identifier-less effects. The initial Gaussian export fixture constructed catalog effects directly. The
subsequent unsharp continuation corrected the fixture to use real MCP `effect_stack` add/set-parameter commands at all four scopes
(asset, clip, track and master), rather than constructing test-only effect values.
That authoring-to-export regression passes, including preview, encoded alpha and
re-decoded output checks. This correction adds no new effect qualification.

After the catalog correction, full MCP/video libraries pass **277 / 868** tests
(two existing ignored video tests). All-target Clippy and the native application
build pass with warnings. No publication or release claim is implied.

### Native dip-to-color — cloud continuation

The sixth catalog input transition, DipToColor, now decodes the document swatch's
encoded sRGB through the pinned D65-to-AP1 transform and premultiplies once by its
original coverage. Both halves use native premultiplied interpolation; partially
transparent through-colors are supported. Missing color retains opaque-black
compatibility. Non-finite or out-of-range swatches fail closed instead of being
clamped or interpreted as AP1. Legacy dip arithmetic is unchanged.

Independent midpoint/coverage and rounded red-primary references pass, as do
invalid-channel rejection, compiler handle/stage-order cases, and actual MCP
create/undo/redo, preview and five-frame ProRes RGBA64 re-decode with a colored,
partially transparent through-swatch. Full MCP/video libraries pass **277 / 869**
(two existing ignored video tests); native integration **32**, scaling **3 + 3**
and unchanged Legacy goldens pass. Adjacent media-only partner limits and
CrossDissolve-only end fades remain in force. This is another bounded increment,
not full-roadmap completion.

The current dip-to-color tree also passes all-target Clippy and the native
application build with warnings.

### Native solid-color generators — cloud continuation

Solid-color clips now share the strict encoded-sRGB-to-premultiplied-AP1 swatch
contract, then participate in clip composition, transform, effects,
group/clip/shared-look/track/master stages and the configured output transform.
They do not invent an asset grade or source interpretation. Generator transition
partners remain gated; qualifying a solid does not qualify title/vector sources.

A compiler regression verifies the eight remaining grade stages in order and
rejects malformed swatches. Actual MCP insert/undo/redo, full preview and
three-frame ProRes export/re-decode verify all RGB channels and partial alpha;
preview/export preserve the document and history. Full affected libraries pass
MCP **278**, video **870** (two existing ignored tests); native graph integration
**32**, scaling **3 + 3** and unchanged Legacy goldens pass. All-target Clippy and
the native application build pass with warnings. Broader roadmap work remains.

### Native unsharp detail enhancement — cloud continuation

Native `sharpen.unsharp` and the corresponding composition node now operate in
scene-linear AP1 with original coverage retained. The low-pass estimate is
computed from premultiplied RGBA, normalized by blurred coverage, then the
straight-color detail is applied under the source alpha. Signed/highlight RGB
is not clamped. Amount is finite in 0..10, sigma in 0..100 authored pixels;
zero amount/subpixel identity radii preserve input, and Draft scales sigma using
the same authored-canvas basis as Gaussian. Legacy sharpening is unchanged.

Independent f64 spatial references exercise 28 amount/radius combinations,
including signed/highlight color, partial and transparent coverage, multi-pass
large radii and logical texture dimensions. CPU RGB tolerance is 0.0003; GPU
half-float accumulated RGB tolerance is 0.035 times max(1, |reference|). Coverage
is exact in those fixtures. Draft 17→34→17 cache/radius reuse also passes. These
bounded tolerances do not establish a full professional precision/performance
budget for arbitrary long effect stacks.

Catalog and historical identifier-less stacks pass scope order, keyframe timing,
half-open effect zone and fail-closed cases. Composition-node defaults match the
catalog's zero amount/radius; malformed values and cache-key distinctions pass.
Both Gaussian and unsharp export fixtures now use actual MCP `effect_stack`
add/set_param commands at asset, clip, track and master scopes. Their real
preview/ProRes export/re-decode checks pass. Earlier source checkpoints used
catalog construction for Gaussian despite the handoff describing command-path
coverage; that description is corrected here and the current test explicitly
exercises the command path.

Current affected qualification: GUI **479**, MCP **279**, video **872** tests
(two existing ignored video tests), native graph integration **34**, scaling
**3 + 3**, unchanged Legacy goldens, all-target Clippy and native app build pass
with warnings. One full-suite run exposed an existing asynchronous test-readiness
race after decoder invalidation: a single source-peek evaluation could occur
before reopening completed. The test now reuses its existing bounded decoder
readiness helper before asserting canvas fit; production scheduling and the
pixel/dimension assertions are unchanged. Two subsequent complete video-library
runs passed. This result is not treated as a waived failure.

A fresh `cargo test --workspace --lib` on the current unsharp tree passes
**2,707 tests**: core875, GUI479, MCP279, render202, video872; embed has no
library tests and matte's one library test is ignored. Together with the two
existing ignored video tests, three tests remain explicitly ignored. This is a
workspace-library pass, not all integration, hardware or full-roadmap acceptance.

### Exact grading graph image-output inspection — cloud continuation

Reachable image outputs in an ordinary Native Managed clip grade can now be
inspected through the Color panel and read-only `inspect_grade_image` MCP tool.
The returned PNG uses the qualified ACES SDR display transform and retains alpha;
separate straight AP1 extrema over covered pixels retain negative/highlight
values. Image and matte taps are typed and cannot substitute for each other.
Missing, inactive, unreachable or unresolved taps fail instead of returning the
program. Shared/independent-look graph IDs cannot leak into ordinary clip output
inspection; those broader ownership scopes remain unqualified.

Inspection is read-only on locked clips and excludes later correctors/looks from
the requested intermediate image. GUI results are keyed by node/type, time,
revision, snapshot, quality and proxy mode. Stale results are discarded and one
in-flight worker is retained/drained across target changes, avoiding an unbounded
readback/tone-map queue. MCP tone mapping runs on a blocking worker and revision
is checked again after presentation encoding.

Independent helper tests, actual GPU-backed MCP inspection and GUI tests pass,
including alpha, scene extrema, exact intermediate vs output values, colliding
look node IDs, wrong-type/inactive rejection, undo/selection/snapshot invalidation
and single-worker behavior. The cloud native window visibly selected image node0
before the later exposure, displayed the darker exact input, and Exit image
restored the brighter program. Both views were saved as screenshot evidence.
Current workspace libraries pass **2,715 tests** (core875, GUI482, MCP280,
render202, video876), with three existing ignored tests. Native integration34,
scaling3+3 and unchanged Legacy goldens pass; current MCP/library and all-target
lint were rerun after the asynchronous presentation change and pass with warnings.

**One-time storage exception:** on 2026-10-05 EDT, the user explicitly approved
clearing only Photonic's rebuildable Rust target cache after this pass is saved.
Source, test logs, assets and checkpoints must be retained. This permission is
for this cleanup, not a relaxation of future artifact-retention requirements.

### Native bounded Invert — cloud continuation (2026-10-06)

`NativeInvert` is qualified for asset, clip, track and master effect stacks and
clip/project composition nodes. Its explicit authored contract is
`(1 - clamp(straight_ap1_rgb, 0, 1)) * alpha`; source alpha is retained. This is
a bounded creative inversion, not an unclipped HDR or film-negative transform.
Native AP1 image-domain and unary-arity checks reject malformed graphs. Catalog
`color.invert` and identifier-less declared Invert effects are supported; unknown
identifiers remain fail-closed. Legacy behavior is unchanged.

Twelve independent signed/highlight/alpha CPU/GPU cases, four-scope compiler
ordering, half-open zones, graph routing, cache identity and actual MCP effect
authoring plus ProRes preview/export/re-decode pass. Current full verification:
2,717 workspace library tests passed (3 ignored), 35 native graph integration
tests, 6 scaling tests and the unchanged Legacy golden harness passed.
All-target Clippy and the native application build pass with warnings. These
checks do not close the remaining roadmap or establish external hardware
qualification. Work remains local and uncommitted.

### Native AP1 luma key — cloud continuation (2026-10-06)

`NativeLumaKey` supports declared `key.luma` and identifier-less LumaKey effects
in all four effect scopes, plus clip/project graph nodes. It measures straight
AP1 luminance using AP1's Y coefficients, then multiplies premultiplied RGBA by
`smoothstep(threshold, threshold + max(softness, 0.0001), luminance)`, optionally
inverted. Surviving straight RGB is neither clipped nor transferred. Threshold
and softness are finite in 0..=1; invert is boolean. Invalid image domains, arity,
parameters and unknown identifiers fail closed. Legacy kernels are unchanged.

72 independent CPU/GPU signed/highlight/alpha/key cases at non-bucket dimensions
pass. Compiler tests cover four scopes, half-open zones, graph/default routing,
identifier-less compatibility, parameter rejection and distinct cache hashes.
Actual MCP authoring confirms four compiled native key stages before LUT
dependencies are added; preview/scopes/ProRes re-decode parity passes. Program
PNG coverage is not used as evidence for raw key alpha; that is tested directly
in the CPU/GPU cases. An attempted PNG fractional-alpha assertion was invalid
for composited presentation and was replaced with the explicit routing check.

Final current-tree verification: 2,719 workspace library tests passed, 3 ignored;
36 native graph integration tests, 6 scaling tests and the unchanged Legacy
golden harness passed. All-target Clippy and the application build pass with
warnings. Full roadmap and external hardware qualification remain incomplete.
All continuation source remains local and uncommitted.

### Native AP1 chroma key — cloud continuation (2026-10-06)

`NativeChromaKey` supports `key.chroma`, identifier-less declared ChromaKey
effects and clip/project graph nodes. Authored key swatches are interpreted as
encoded sRGB and converted to straight AP1 (swatch alpha is deliberately not a
distance axis). The existing linear RGB-distance/smoothstep key contract and
dominant-key-channel spill suppression now operate in AP1. Ties select the
lowest channel index. Tolerance, edge softness and spill controls are
finite in 0..=1; swatches are finite in 0..=1. Extended source RGB is retained
apart from the requested spill adjustment; resulting coverage scales original
alpha. GPU texel reads respect logical dimensions. Legacy behavior is unchanged.
This is the catalog's RGB-distance key, not a new perceptual keyer.

135 independent CPU/GPU vectors cover exact-key removal, softened distance,
spill0/0.5/1, dominant-channel ties, signed/highlight RGB and transparent/partial
alpha. Compiler tests verify swatch conversion, all four scopes, half-open
zones, graph routing, parameter rejection and cache identity. Real MCP command
authoring verifies four converted native stages; preview/scopes/ProRes
re-decode parity passes. Final current-tree workspace library tests: 2721passed,
3ignored; native integration37, scaling6 and Legacy golden harness pass.
All-target Clippy and native application build pass with warnings. Full roadmap,
external hardware qualification and remaining effects stay open; no commit/push.

### Native AP1 glow — cloud continuation (2026-10-06)

`NativeGlow` qualifies catalog `stylize.glow`, identifier-less declared Glow,
and clip/project graph nodes. Straight AP1 luminance drives bright extraction;
encoded-sRGB tint is converted to AP1 (tint alpha is ignored like the existing
catalog contract). A native planned Gaussian convolves premultiplied RGBA,
then the catalog's screen-add formula combines source and tinted glow:
`src.rgb + glow.rgb * tint * intensity * max(1-src.rgb, 0)`, with alpha-over
using clamped glow coverage. This is not an energy-linear bloom redesign;
existing signed/highlight source channels are not forcibly clamped. Sigma
0..500, threshold0..1, intensity0..10, finite swatches and image-domain/arity
checks are explicit. Draft scales sigma with the authored canvas; logical
texel dimensions are honored in extraction and combination. Legacy stays on
its original kernels and unit-spacing behavior.

36 independent f64 spatial-reference cases cover sigma0/0.4/1/12/48/500,
intensity0/0.5/2, two thresholds, signed/highlight colors and partial alpha.
CPU tolerance0.0003 and GPU half-float accumulation tolerance3.5% (minimum
absolute0.035) are explicit in the test. Separate Draft/full/Draft cache checks
pass. Compiler tests cover scopes/zones, graph defaults, tint conversion,
invalid controls and parameter hashes. Actual MCP authoring confirms all four
native stages and preview/scopes/ProRes re-decode parity.

The expanded async integration fixture exceeded the default test-thread stack
when stored inline. Boxing its 18 callers fixes that test-harness resource
issue; final tests run on the default stack, without a stack-size override.
Current full workspace library verification: 2723passed, 3ignored; native graph
integration39, scaling6 and Legacy golden harness pass. Clippy/all-targets and
application build pass with warnings. Remaining general effects, graph stages,
tracking, workspace, finishing and hardware work remain open. No commit/push.

### Native solid-color graph generators — cloud continuation (2026-10-06)

Clip composition and project graphs now accept `SolidColor` as a zero-input
generator. `params.color` is sampled at graph time, strictly type/range checked,
and converted from encoded sRGB to premultiplied AP1. Missing color retains the
existing opaque-black default. Animated swatches interpolate in their authored
space before conversion. Invalid colors and attempted generator input wiring
fail closed. Other unsupported generators remain gated.

Compiler tests cover animated color/alpha, default behavior, invalid values and
zero-input arity. A direct graph fixture overrides a deliberately red underlying
clip with the blue partial-alpha generator at both clip and project scope; real
MCP rendering/export and three-frame ProRes RGBA64 re-decode preserve expected
color and alpha. Graph fixture construction is direct, not claimed as MCP graph
authoring. Existing MCP solid insertion undo/redo checks still pass.

Current full workspace library tests: 2725passed, 3ignored; native integration39,
scaling6 and unchanged Legacy golden harness pass. All-target Clippy and native
app build pass with warnings. Full roadmap is incomplete; no commit/push.

### Native ellipse/rectangle generators — cloud continuation (2026-10-06)

`NativeMaskShape` produces an AP1-white premultiplied image with ellipse or
rectangle coverage. It has zero inputs: it replaces its source, not a unary
mask applied to that source. Catalog `util.mask_shape` remains an ellipse and
is qualified independently at asset/clip/track/master scope; graph nodes carry
the ellipse/rectangle kind. Later generators correctly prune earlier image
chains. Polygon generators remain unsupported and fail closed. Center0..1,
half-size0..2, finite radian rotation and feather0..1 are validated. Geometry is
normalized to the logical canvas, so Draft does not change its footprint.

108 geometry combinations across 17x9/full34x18/17x9 reuse (324 evaluations)
match an independent f64 coverage reference. The native CPU computes geometry
in f64 to avoid narrow-edge amplification; Legacy is unchanged. GPU coverage
tolerance is0.002, widened to0.004 only for the implicit0.0001 near-hard feather
band. Invalid input arity is rejected. Compiler tests cover all four scopes,
half-open clip zones, generator pruning, invalid geometry and cache hashes.
Direct clip-ellipse and project-rectangle fixtures verify feathered partial
alpha through actual MCP preview, three-frame ProRes and RGBA64 re-decode.

Current workspace library tests:2727passed/3ignored; native integration40,
scaling6 and Legacy golden harness pass. All-target Clippy and native app build
pass with warnings. Remaining roadmap work is still open; no commit/push.

### Named local Color layouts — cloud continuation (2026-10-06)

The Video toolbar now has a Layouts menu. Up to 16 named local Color arrangements
can be saved, replaced, restored or removed. Names are trimmed, bounded to 64
characters and checked for control characters; replacement is case-insensitive.
Snapshots contain Color visibility, Creator/Advanced view, reference-gallery
width, shot-strip height, right drawer/width and primary/additional scope
settings, including docking, tap and floating placement. Project data, grades,
selection, media and unrelated preferences are not restored. Save failures are
reported rather than silently claimed as persisted. Removal requires a separate
confirmation window; Keep, Escape and closing that window cancel it.

Restoration clears only the relevant retained panel dimensions. Floating scopes
store their measured client-body size separately from their outer rectangle,
avoiding title/frame-margin growth on repeated reload. Invalid geometry is
normalized, old preferences default to no presets, and monitor clamping remains
in place. Both startup paths restore scope visibility and type.

Nine regression tests cover serialization, compatibility, replacement/capacity,
normalization, unrelated-state preservation, retained-panel reset, repeated
client-size restoration and confirmation survival across native pointer events.
Native cloud-window checks verified save/load, Color visibility restoration,
dock-to-float recall, restart persistence, removal confirmation and Keep. A
repeated float reload retained the exact saved rectangle `[240,52,332,414.636]`
and client size `[320,366.636]`. Gallery drag gestures were not established in
this UI pass; width recall is covered by retained-panel-state tests.

Current final source passed 2,736 workspace library tests (3 ignored), 40 native
graph integration tests, 6 scaling tests and the unchanged Legacy golden
harness. All-target Clippy and the native app build pass with warnings. Native
restart and confirmation screenshots are retained in the source checkpoint.
This qualifies local Color arrangements, not every possible workspace view,
shared version/reference ownership or the full roadmap. No commit or push.

### Native graph Switch — cloud continuation (2026-10-06)

Clip and project graphs now resolve the editor's four-input Switch directly to
its selected producer at compile time. The enum selection is sampled at the
graph tick with stepped keyframes. A missing selected input falls back to the
lowest connected input index, independent of serialized edge order; no inputs
produce transparent black, never the underlying program. Switch adds no image
processing operation. Dormant branches are not lowered, so an unsupported node
or cycle on an unselected branch does not invalidate the selected image. A
selected cycle, missing producer or unsupported operation still fails closed.
Duplicate or out-of-range input ports and non-primary source ports are rejected,
including malformed dormant connections. This qualification is bounded to the
four inputs exposed by the current editor, not arbitrary variadic inputs.

Focused compiler tests cover stepped selection, cache hashes, missing-input
fallback, empty output, dormant/selected cycles and malformed controls/wiring.
Direct clip/project graph fixtures choose blue, green, blue over three frames,
with different partial alpha values. Actual MCP preview selects the middle green
frame, and ProRes RGBA64 re-decode preserves all three colors and alpha values.
Fixture graph construction is direct; MCP graph authoring is not asserted.
Current final source passed 2,739 workspace library tests (3 ignored), 40 native
graph integration tests, 6 scaling tests and the unchanged Legacy golden
harness. All-target Clippy and the native application build passed with
warnings. Formatting and diff checks passed. The full roadmap remains
incomplete; no commit or push.

### Native piecewise blends — cloud continuation (2026-10-06)

Overlay, Hard Light and Pin Light now join the qualified scene-linear AP1
separable blends. Their existing per-channel piecewise equations operate on
straight AP1 and then use premultiplied source-over composition. The 0.5 branch
pivot is a creative scene-channel value, not perceptual middle gray. Signed
channels and highlights are not silently clamped into display range. Modes
requiring bounded color, divisions or unqualified luminance conversion remain
gated; Legacy rendering is unchanged.

An independent f64 reference covers all ten qualified modes across four color
pairs, five alpha pairs and three opacities: 600 CPU/GPU comparisons. Inputs
include negative/highlight values, zero/full/partial alpha and values below,
exactly at and above the 0.5 pivot. Track compilation and graph Merge routing
cover the new modes, sampled opacity and distinct mode/opacity hashes. Actual
MCP preview and ProRes re-decode pass for each new mode; blend-track fixture
construction is direct, not claimed as MCP authoring. Current final source passed 2,741 workspace library tests (3 ignored), 40 native
graph integration tests, 6 scaling tests and the unchanged Legacy golden
harness. All-target Clippy and the native application build passed with
warnings. No commit or push; the full roadmap remains open.

### Composition graph track-lock repair — cloud continuation (2026-10-06)

A regression test reproduced an indirect lock bypass: MCP `add_graph_node`
could change a composition belonging to a locked track, even though creating,
detaching or pasting the composition was already protected. Shared graph edit
builders now check every direct clip owner across all sequences before creating
a command. If any owner is locked, adding/removing nodes or edges, changing node
parameters and moving persisted node positions return `TrackLocked`. Unknown
graphs and missing removed edges are rejected. Project-only and unreferenced
graphs remain editable, and graph reads remain available. Undo/redo applies its
already-recorded commands normally; the checks guard new editing operations.

The native editor uses the same guard for palette, inline parameters, wiring,
context actions and canvas movement. An already-open composition becomes visibly
read-only when its owning track is locked, while selection, navigation and viewer
pinning remain available. A native cloud-window check attempted add, move and
Delete actions while locked: the graph stayed at three nodes/two edges and the
history retained only the two intentional track-lock toggles. Unlocking restored
the controls; adding a Merge made one history entry, and undo removed that node
and restored the locked state. No save roundtrip is claimed for this UI check.

Final qualification: 2,743 workspace library tests passed (3 ignored), plus 39
core timeline integration tests, 40 native graph tests, 6 scaling tests and the
unchanged Legacy golden harness. All-target Clippy and native app build passed
with warnings. The regression covers all five MCP graph mutation tools, shared
ownership including a locked owner in another sequence, and GUI command helpers.
Screenshot evidence is retained. Full roadmap work remains incomplete; no
commit or push.

### Native graph authoring and truthful controls — cloud continuation (2026-10-06)

Clip/project solid generators, clip ellipses and project rectangles are now
constructed through the actual MCP graph tools in their export fixtures.
Composition/project creation, seeded-edge removal, node insertion, parameter
edits and output connection each round-trip through undo/redo with exact
before/after document comparisons. The resulting graph survives JSON reload;
actual preview and three-frame ProRes RGBA64 checks retain the expected color
and alpha. Sequence color configuration is still test setup. Switch animation
fixtures still construct their keyframe lanes directly; this does not claim MCP
authoring of graph-node keyframes.

Native UI review exposed a misleading unstored Merge opacity of zero although
the compiler uses one. Node controls now resolve implicit values from their
operator: Merge opacity is one, SolidColor is opaque black, affine scales are
one, and Resize dimensions follow the active format. Effect defaults come from
the existing manifest, without allocating seeded parameter bags per display.
Stored values, including zero, always take precedence; opening controls does not
materialize a parameter or change history. The inert graph Transform opacity
control is no longer advertised: its compiler currently applies only the
affine, so opacity belongs on a Merge. This does not qualify native Resize.

Native cloud-window review showed unstored Merge opacity 1, Transform scales 1
and a black SolidColor swatch while history remained at zero edits. Two display
regressions check implicit defaults, manifest parity, stored-zero precedence
and non-mutating inspection. Final source passed 2,745 workspace library tests
(3 ignored), core timeline integration39, native graph40, scaling6 and unchanged
Legacy golden harness. All-target Clippy/app build passed with warnings; no
commit or push. The node-control range and gesture follow-up is qualified below.

### Non-destructive numeric node editing — cloud continuation (2026-10-06)

An egui regression reproduced passive clamping: displaying Transform X=-240 in
the former fallback slider changed it to zero before any user edit. Unbounded
position/anchor/rotation controls now use unconstrained numeric entry, with
rotation explicitly labeled in radians. Bounded sliders clamp only deliberate
edits, and numeric/enum widgets preserve pre-existing out-of-range values during
inspection. This preserves data and diagnostics rather than silently repairing
an invalid loaded parameter. A bounded slider's text field also defers changes
until confirmation or focus loss; Escape cancels text entry.

Numeric drags keep a local editor draft across frames and produce one document
command on release. They do not write intermediate preview values into the
document. Escape, hidden controls and concurrent value changes discard stale
drafts. Six added regressions cover non-mutating display, inline/enum values,
bounded and unbounded drag/release, Escape/concurrent-edit cancellation, hidden
controls and typed Enter/Escape flows.

Native cloud UI review retained X=-240, Y=500, rotation=pi and dormant Merge
opacity=1.25 with zero history edits. Cancelling a typed value kept that state.
Entering X=-120 created exactly one history entry; a longer native drag to -8
created exactly one more. Two separate undo actions restored -120 and then -240.
Short initial drag attempts did not establish a change; the longer drag after
restoring canvas/inspector focus did. Saving the restored test fixture produced
a graph identical to the initial graph, and the app closed normally.

Final qualification: 2,751 workspace library tests passed (3 ignored), plus
39 core timeline tests, 40 native graph tests, 6 scaling tests and the unchanged
Legacy golden harness. All-target Clippy and native app build passed with
warnings. Source remains local/uncommitted; the full roadmap is incomplete.

### Structural graph connections and history — cloud continuation (2026-10-06)

A focused MCP test reproduced acceptance of a nonexistent source port. Shared
edit-time validation now checks catalog port counts as well as graph ownership
locks and cycles. GUI and MCP input reconnection use one atomic edge replacement:
there can be only one producer on the target input, reconnecting the same wire
is a no-op, and undo restores the exact prior edge ordering. Explicit edge
removal likewise preserves order. Node removal captures the full original edge
order and whether the node had a saved canvas position, so undo does not create
layout metadata that was absent before removal. These structural checks do not
qualify previously unsupported rendering operations.

Command roundtrip tests also exposed an existing serialization defect: both
TimelineCmd and its nested subcommand emitted a `cmd` discriminator at the same
JSON level. Graph, caption, TTS and audio subcommands now serialize inside a
`command` envelope. Core timeline fixtures validate both standalone timeline
commands and their history wrappers through JSON. Historical standalone graph
AddNode payloads with a position and no restoration metadata still load. This
is not a claim to migrate previously malformed duplicate-key history files.

Final source passed 2,754 workspace library tests (3 ignored), 42 core timeline
integration tests, 40 native graph tests, 6 scaling tests and the unchanged
Legacy golden harness. All-target Clippy and native app build passed with
warnings. Native UI palette reconnection produced one edit; repeating it
produced none. The saved fixture showed ClipIn feeding Output instead of Blur,
with one producer on that input. One undo and another save restored the exact
original graph. Input-socket drag cancellation is a separate remaining
interaction check; no claim is made for that flow here. No commit or push.

### Deferred wire gestures — cloud continuation (2026-10-06)

Dragging an existing input connection now leaves its original edge in place
until a drop is committed. A valid target removes the origin and replaces the
destination in one reversible command. Dropping outside the canvas or on an
invalid surface cancels; dropping deliberately on blank canvas disconnects.
Stale origins, cycles and locked owners cannot produce a partial edit. Pointer
classification uses the original press position, so crossing the drag threshold
does not miss a small port. A plain node click now selects that node.

The focused Escape path cancels the transient wire before clearing selection
or closing the canvas. Hidden/switched graph views discard transient gestures.
Wire and numeric drafts are scoped to document identity as well as graph/node
identity. Focused tests cover atomic rewiring and undo, stale/locked refusal,
outside/blank drops, hidden views, Escape and independent document scopes.
Final qualification: 2,758 library tests passed, 3 ignored; timeline 43;
native graph 40; scaling 6; Legacy golden 1; Clippy and build passed with
warnings. Native cloud window verified plain-click selection with zero edits,
outside-canvas cancellation preserving both wires, atomic input rewiring as
one edit, and deliberate blank-canvas disconnection as one additional edit.
Each operation undid in one step. Saved rewired graph had only ClipIn→Output;
final undo/save restored the exact initial graph. Native app closed normally.
Escape during a held drag is covered by the focused synthetic UI path, not a
native held-button demonstration. Source remains local and uncommitted.

### Direct qualifier spatial cleanup — bounded Native Managed qualification

Static denoise, grow/shrink and blur now have an optional authoring field and
direct Native Managed qualifier controls. Native ordered and graph correctors
lower the combined unassociated key/window through spatial refinement before
log-domain correction; inspection applies source coverage afterward. Absent
and neutral values preserve the original path. Legacy non-neutral settings
produce a diagnostic. Core validation/serialization/undo, focused compiler,
GPU inspection and CPU/GPU/cache-scale numeric tests have passed. Actual MCP
ordered and graph authoring/undo/redo, inspection and ProRes parity passed on
the default test-thread stack after boxing tool-call futures in the broad test
fixture. No production stack setting changed. Full aggregate and native-window verification are recorded below.

Spatial-cleanup numerical follow-up: an extended-range, partial-alpha fixture
exposed accumulated RGBA16F render-target rounding. Intermediate GPU taps match
an independent truncating binary16-store calculation (one blue sample was
0.751953125 versus ideal f64 0.75707483). No production arithmetic or tolerance
was silently substituted: the tests now separately compare the CPU to an f64
mathematical reference, the GPU to explicit nearest/truncating binary16-store
references, and the result to a disclosed ideal-format bound of
0.016 × max(1, abs(reference)). The fixture covers two source colors (including
negative and above-white channels), four coverage levels, and 50→25→50 cache
sizes. This is 24 bounded cases, not unrestricted HDR precision qualification.
Key inspection also checks five spatial settings over 2,500 partial/transparent
pixels each. Wider HDR/intermediate-precision acceptance remains open.

Fresh aggregate: 2,765 library tests passed, 3 ignored; timeline 43; native graph
41; scaling 6; unchanged Legacy golden 1. Clippy and the application build also passed with warnings. Native cloud-window
inspection displayed the loaded cleanup and its partial-coverage matte. Reset
created one edit and saved without the optional field; one undo restored the
original. Typing 1.00% saved blur=0.01 as one additional edit; undo/save restored
the exact initial grade. The application closed normally. Static Native Managed
controls are qualified within the precision/ownership limits above; no Legacy
spatial evaluation, animated cleanup, or broader HDR precision claim is made.

### Fused spatial-qualifier correction — qualified bounded precision improvement

The precision follow-up replaces three post-key image passes (log CDL, matte
mix, and decode) with one `NativeMatteCdl` pass, hash discriminator 54. It reads
the original scene-linear image and refined unassociated key, performing
encode → CDL → key mix → decode in f32 within one shader invocation. The
half-float log texture remains only for the existing key-selection semantics;
it no longer quantizes the image correction path. Zero-key and identity-CDL
pixels return the original stored image exactly. The absent/neutral spatial
setting still uses the unchanged original qualifier evaluator.

The same 24 signed/HDR, partial-alpha and 50→25→50 cases now pass an ideal GPU
bound of 0.0015 × max(1, abs(reference)), compared with 0.016 for the retained
unfused compatibility test. Independent CPU and explicit binary16 source/output
store checks remain. Another 240 independent combinations of color, alpha, key
weight and CDL controls pass CPU/GPU checks; zero-key and identity cases also
assert exact stored-source equality. Domain/arity/invalid-control refusal and
hash invalidation tests pass. Actual ordered/graph MCP preview, inspection and
ProRes parity also passed. Wider pipeline/HDR mastering precision remains open.

Fresh aggregate: 2,766 library tests passed, 3 ignored; timeline 43; native graph
43; scaling 6; unchanged Legacy golden 1. Clippy and the application build passed with warnings. Native-window smoke
verification showed the fused program image, loaded spatial controls and
partial-coverage matte. The initial history HEAD and two existing review
branches were retained, the saved grade was unchanged, and the app closed
normally. No commit or push.

Fused-CDL robustness follow-up: the shader now bounds its positive power input
using the same guard as the existing native CDL implementation. All 36 extreme
valid-control/source/coverage/key-weight cases preserve finite RGBA16F storage,
alpha and transparent black. These are stability checks, not tight-precision
claims at extreme settings. The 240 moderate numerical cases still pass.
Final: 2,766 library tests passed, 3 ignored; timeline 43; native graph 44;
scaling 6; Legacy golden 1; Clippy/build passed with warnings. No GUI changes;
the last native-window smoke was the prior fused-pass build. No commit/push.

## Cloud continuation — 2026-10-06: explicit Kelvin white adaptation

`white_balance_cct` adds source/target Kelvin values within 1667–25000 K.
It uses an approximate Planckian-locus xy model and Bradford adaptation,
converted to linear Rec.709/D65 for Legacy SDR or scene AP1/ACES-white for
Native Managed. Both defaults are 6500 K; equal values bypass exactly.
The existing normalized artistic temperature/tint operator is unchanged.
6500 K on this locus is not exactly D65 or ACES white. This is not a RAW
camera setting, illuminant estimator, daylight-white or Duv/tint model.

Both fields are registered animatable properties; valid source/target values
are checked before shared grade edits and invalid loaded values diagnose.
CPU/GPU paths preserve alpha and accept signed/extended RGB. Native RGB matrix
IR uses hash tag 55; resolved payload uses tag 11. Matrix coefficients and
hash inputs are bounded/finite. Actual MCP full-grade authoring, exact undo/redo,
JSON reload, Native preview/ProRes and three-frame animated Legacy preview/ProRes
pass. Animation was authored in the full-grade payload, not via a separate
keyframe-tool interaction. MCP schema accepts complete grade objects; no
hardcoded grade-kind catalog prevents this new operator.

Independent numerical evidence includes published xy examples, NumPy matrix
references, source-white mapping, inverse/identity/bounds, and 120 CPU/GPU cases
across both RGB domains, signed/extended samples and four alpha values. CPU
bound is 1e-5*max(1,abs(reference)); GPU bound is 0.003*max(1,abs(reference)).
These are bounded tests, not exhaustive spectral/colorimetric qualification.
Source: https://colour.readthedocs.io/en/latest/generated/colour.temperature.CCT_to_xy_Kang2002.html
Polynomial reference: https://colour.readthedocs.io/en/latest/_modules/colour/temperature/kang2002.html

Current-tree verification: 2,775 library tests passed, 3 ignored; core timeline
43; native graph 45; scaling 6; unchanged Legacy golden 1. Clippy and application
build passed with warnings. Broader roadmap and external HDR qualification
remain incomplete. Work remains local and uncommitted; no push or merge.

Native cloud-window qualification also passed: loaded 6500/6500 values and
Escape-cancelled typing kept the initial history HEAD (two pre-existing branches
were present). Typing source 3200 and Enter created exactly one new entry,
changed the rendered preview, and saved source=3200/target=6500. One undo and
explicit Save restored the exact initial grade. Screenshots and saved JSON
checks are preserved in the checkpoint evidence.

## Cloud continuation — 2026-10-06: linked wheel editing and passive-value safety

A regression reproduced loaded wheel values changing merely on viewing the
controls: lift [-0.7,0.15,0.8] became [-0.5,0.15,0.5]. Wheel RGB numeric fields
now clamp only actual edits, preserve stored values on view, and defer typed
changes until completion. Each wheel also has an All control for its arithmetic
RGB mean. It applies a common shift preserving channel differences, bounded by
the intersection of all three channels' feasible edit intervals. Impossible
spreads disable only the linked control; the individual fields remain editable.
This is a linked authoring convenience, not measured scene luminance or a change
to the existing Legacy/ACEScct wheel renderer. Separate scene-linear luminance
and remaining tonal-range controls are still open.

Tests cover 28 common-shift cases, bounds/chroma/identity/reversal, invalid and
impossible values, passive native/Legacy panel views, document/history equality,
exact undo/redo/JSON reload, and locked-track rejection. The full current tree
passes 2,779 library tests with 3 ignored, timeline 43, native graph 45, scaling
6 and the unchanged Legacy golden. Clippy and application build pass with
warnings. No new rendering, schema or dependency behavior was introduced.

Native cloud-window verification: loaded lift [-0.7,0.15,0.8] and gain
[1,1,2.7] remained unchanged; passive viewing and cancelled typing left zero
history edits. Gamma All=1.2 saved approximately [1,1.2,1.4] without changing
other channels. One continuous drag added one further edit and displayed
[1.07,1.27,1.47]. Two individual undos and explicit Save restored the exact
initial grade. The extreme-value fixture rendered white, so this review
establishes editing/preservation/history, not perceptual grading quality.

## Cloud continuation — 2026-10-06: linked tonal exposure

New `tonal_exposure` adds shadows/midtones/highlights stop adjustments with
ordered stop-relative luminance boundaries and a shared soft transition width.
The three weights are a nonnegative partition of unity; one resulting gain
scales all RGB channels and preserves alpha. Native uses scene AP1 luminance;
Legacy uses linear Rec.709. Signed/nonpositive luminance is assigned to Shadows.
Zero stops bypass exactly, all-equal stops reduce to exposure, and the original
operators retain their look. This is a creative primary, not spatial restoration
or a display/HDR tone mapper; extreme opposing adjustments can reverse tonal
ordering. The precise contract is in specification 07.

Evidence: known weights and 1,443 partition checks; 256 independent f64-reference
CPU/GPU cases across both working domains, four parameter sets, eight signed/
extended/neutral samples and four alpha values. CPU tolerance is
1e-5*max(1,abs(reference)); GPU tolerance is 0.004*max(1,abs(reference)). Six
ellipse/rectangle/gradient plus inverse window cases retain logical coordinates
and alpha with independent window arithmetic. Scoped/graph compiler order,
neutral identity, invalid-domain/parameter rejection, cache hash changes,
parameter animation and passive GUI preservation pass.

Actual MCP full-grade authoring, exact undo/redo and JSON reload pass for Native
preview/ProRes and a three-frame animated Legacy preview/ProRes test. The
animated lane is authored inside the grade payload; separate keyframe-tool
interactions and a Native animated export are not claimed by those tests.
Current test results: 2,788 libraries passed, 3 ignored; timeline 43; native graph
47; scaling 6; unchanged Legacy golden 1. All-target Clippy passes with warnings.
Native app build and interactive control review are pending at this entry.
Full roadmap remains incomplete; no commit or push.

Tonal qualification follow-through: a second three-frame animation test now
creates a Native Managed draft through the actual MCP conversion tool, authors
the grade with exact undo/redo, and verifies neutral/brighter/neutral previews
and ProRes re-decode. Both Legacy and Native animated exports pass. Keyframes
remain authored inside the complete grade payload, not separate keyframe tools.
Current totals supersede the prior entry: 2,789 library tests passed, 3 ignored;
timeline 43; native graph 47; scaling 6; Legacy golden 1. Current all-target
Clippy and application build pass with warnings. Only test code changed after
the initial app build used for the following interactive review.

Native cloud-window review passed: neutral controls and Escape-cancelled typing
left zero history edits. Midtones +1 stop changed the reference-step preview
and created one edit. Attempting shadow boundary +5 with highlight boundary +2
clamped the edited field to +1.99 and created one further edit. Saved JSON
contained exactly those settings. Two individual undos and Save restored the
exact initial grade. This establishes the tested control/history path; no
calibrated-monitor or artistic-quality approval is inferred.
Full roadmap remains incomplete. Local source only; no commit, push or merge.

## Cloud continuation — 2026-10-06: direct grade-keyframe authoring

Generic keyframe tools now accept local clip GradeOp targets (`grade_op`,
`clip_id`, `op_id`) for registered scalar paths. Set/batch/get/copy/paste/remove
use the existing animation commands. Checked authoring rejects locked owners
and invalid grade paths/value types/ranges before a batch is committed; reads
and copy remain available. The core scalar mapping is shared with rendering.
The Clip Inspector's embedded/floating editor lists grade correctors by name,
including friendly tonal/Kelvin/channel labels, and timeline markers include
grade keyframes. Group/shared/track/master/asset and structural-window/curve
animation are outside this clip-local editor increment.

Actual MCP authoring/read/copy/paste/remove, exact undo/redo and failed-batch/lock
checks pass. The Native and Legacy three-frame tonal export fixtures now author
keys via the real `batch_set_keyframes` tool rather than embedding animation
inside `set_grade`. This supersedes that earlier verification limitation.

Native review exposed and corrected additional interaction defects: the
launcher could be clipped in a narrow inspector; small keyframe drags used the
post-threshold pointer instead of press origin; local time was divided by an
obsolete 30000 rate rather than the timeline tick rate. Synthetic drag/Escape,
property identity, launcher bounds and time readout tests pass. Native add,
drag and Ease In-Out operations were exercised. Escape-during-drag remains
synthetic evidence, not a native held-button demonstration.

A generated test fixture also exposed floating keyframe edits not marking the
tab dirty, allowing exit without saving. Timeline history revision changes now
feed tab dirtiness/autosave/quit protection. The focused regression passes and
the real app displayed the unsaved-changes prompt. Save all & quit persisted
three edits, a middle key at 423360000 ticks/value 2.962963104248047, and the
Ease In-Out handles [0.42,0]/[0.58,1]. The second neutral Kelvin operator was
unchanged. The earlier lost changes were test-fixture-only; no user project
was affected. Follow-through at 11:00 UTC: the saved file reopened with all
three history entries; the native editor showed 0.60s at frame 18/30fps.
Three individual undos returned to Initial state, and Save all & quit
restored the entire grade semantically. The only serialization difference
from the seed was omission of default orphaned:false; all other grade fields
matched exactly. The app closed. Native evidence and the verification receipt
are included in the next checkpoint.

Current libraries: 2,800 passed, 3 ignored (core879, GUI521, MCP299, render211,
video890). Timeline43, native47, scaling6, Legacy golden 1 previously passed on
the same engine/core tree; later changes were GUI-only. Current Clippy/build
pass with warnings. The API reference was regenerated from `dump_tools`;
manual contract notes are preserved in `mcp-color-grading.md`.
Full roadmap incomplete. Local source only; no commit or push.


## Cloud continuation — Native Soft Light qualification (2026-10-06 11:08 UTC)

Qualified the existing Soft Light equation for scene-linear AP1 track and
composition-graph merges. The shader and Legacy equation are unchanged;
only the Native support gate and tests changed. The signed-channel cubic
extension and RGBA16F limits are explicit in the color specification.
This is creative scene-linear math, not CSS/Photoshop appearance matching.

The independent f64 blend oracle now covers 990 cases: 11 modes × 6 RGB
pairs × 5 alpha pairs × 3 opacities, including both sides of the Soft Light
source 0.5 and backdrop 0.25 boundaries, negative color and highlights.
CPU tolerance is 1e-6 and GPU 0.01 for these declared samples. Domain refusal,
track compilation, graph opacity interpolation and mode/hash separation pass.
A new actual MCP preview/scopes/ProRes test passes with a directly constructed
Soft Light track fixture; it does not establish MCP blend-setting authoring.

Current workspace libraries: 2,801 passed, 3 ignored (core879, GUI521, MCP300,
render211, video890). Native graph47, scaling6 and Legacy golden 1 passed;
Clippy and native application build pass with warnings. An initial aggregate
integration command used nonexistent target names and was corrected; the
correctly named integration command passed. Core timeline43 remains from the
unchanged core tree. No UI controls changed in this increment.
Broader roadmap incomplete; work remains local, uncommitted and unpushed.


## Cloud continuation — track compositing authoring (2026-10-06 11:29 UTC)

Closed an authoring gap: tracks already rendered blend/opacity, but general
track edits did not capture them. Optional composite snapshots now extend
TrackSettings without resetting composite values when old serialized commands
lack those fields. Core shared edits validate finite 0..1 opacity, reject
changed image compositing on audio/locked tracks, and prevent an unlock+edit
request from bypassing the lock. MCP set_track_prop supports blend/opacity,
atomic combined settings, and no-op history preservation. Unsupported Native
modes retain renderer refusal. The generated API reference was regenerated.

The timeline track-name menu now offers Compositing with a scoped Apply/Cancel
draft, Native mode availability, no passive numeric clamp, stale-document/
sequence/composite protection, and one combined undo step. Native testing:
typed opacity0.20 then Escape left preview/history unchanged; SoftLight plus
opacity0.375 applied in one edit and changed preview. Locking added one edit;
the reopened dialog was read-only with Apply disabled, and Cancel added none.
Save all & quit persisted both settings and the lock. Relaunch restored both
history entries. Undo lock then composite edit restored the original preview;
saving restored the entire video_tracks array exactly to the seed JSON.

Actual MCP Screen/Overlay/HardLight/PinLight/SoftLight export fixtures now
author track settings through the real tool with exact undo/redo before
preview/scopes/ProRes checks, superseding the earlier direct-fixture limitation.
Core tests cover old command compatibility, invalid values, lock bypass and
audio rejection; GUI tests cover passive view/Escape, stale drafts and atomic
undo; MCP tests cover invalid-batch atomicity, no-op, locks and JSON reload.

Current libraries: 2,805 passed, 3 ignored (core879, GUI524, MCP301, render211,
video890); timeline45, native graph47, scaling6, Legacy golden 1. Clippy and app
build pass with warnings. Native screenshots and verification receipt are in
the checkpoint. Static video/text editing is exposed; Native title rendering
and unqualified blend modes remain separate limitations. Full roadmap is
incomplete. No commit, push or merge.


## Cloud continuation — solid-color transition partners (2026-10-06 11:40 UTC)

Native transitions now accept the already-qualified solid-color source as an
incoming or outgoing partner alongside qualified media assets. All six catalog
kinds retain their existing AP1 transition math and source-over/alpha behavior.
Solid outgoing handles are unbounded per the existing generator contract;
asset handles still clamp/suppress using the original logic. Ordinary nested
clips do not become qualified transition partners. Nonadjacent partners,
other generator kinds, and non-dissolve end fades remain gated. CrossDissolve
end fades now also accept solid-color clips. No new shader or UI control.

Qualification: 90 compiler samples (three media/solid combinations × six kinds
× five times) check domains, outgoing/incoming taps and exactly-once track/
master grades; nested rejection remains. Six CPU end-fade samples verify
unchanged/half/near-zero/zero alpha. An actual MCP set_transition/undo/redo/
preview/ProRes matrix covers 18 partner/kind cases with three preview positions
and five-frame alpha checks; export manifests verify. A separate solid end-fade
case creates the clip and transition through real tools, checks three PNG alpha
frames, then decodes five ProRes alpha frames. Both fixtures require engine and
FFmpeg availability rather than silently returning. Rendering/export leaves
document/history unchanged.

Current libraries: 2,809 passed, 3 ignored (core879, GUI524, MCP303, render211,
video892); native graph47, scaling6, unchanged Legacy golden 1. Core timeline45
remains from the unchanged core tree. Clippy and app build pass with warnings.
This increment changes no UI controls, so no new native-window interaction
claim is made. Full roadmap remains incomplete, local and uncommitted.

## Cloud continuation — primitive window animation qualified (2026-10-06 12:26 UTC)

Six scalar paths animate an existing power window through the GradeOp's current
params.tracks array: center X/Y, half-size X/Y, rotation in radians and feather.
No serialized field was added. Gradient omits its inapplicable width/feather
lanes. Checked authoring rejects absent/inapplicable masks, nonfinite or
unrepresentable values, nonpositive sizes and out-of-range feather. Resolved
animation also validates bounds and diagnoses the corrector instead of silently
clamping. Static documents keep their original behavior; dormant lanes are
retained. Older builds are not qualified to evaluate these new paths.

Ordinary local clip-grade keyframes use clip-relative timeline ticks regardless
of source speed. Windows remain normalized to the post-transform sequence
canvas, not attached to a tracked subject. The existing Float/Bezier editor and
MCP grade_op target expose the same checked paths. Animated base controls are
labelled and disabled; on-viewer handles display sampled geometry read-only so
static drags cannot fight keys. Shape/invert remain static. Optical tracking,
Bézier/multiple/Boolean masks and stabilized/compound coordinate contracts
remain open work.

Qualification includes core sampling/serialization/invalid/dormant tests,
checked authoring and GUI rows, sampled read-only handle geometry, retime-local
sampling, resolved hash separation/repeat identity, and matching raw qualifier
inspection masks. The independent f64 oracle covers 96 CPU/GPU cases (three
shapes, inverted/noninverted, four sampled times, two logical canvases and two
alpha values), including signed/highlight color. CPU tolerance 1e-5 and GPU 0.005
are per-channel bounds for those samples. Actual MCP Native and Legacy tests
create a grade and feather keys, verify undo/redo and JSON reload, preview three
frames, and export/decode three-frame ProRes.

Native desktop review showed movement and feathering; adding a midpoint feather
key and Ease In-Out made two history entries. Autosave persisted them at 12:03:20
UTC. The final current3 app reopened the saved curve/history, with 0.50s/value 0.747
visible. Two undos restored the original preview; Save all & quit restored the
entire Grade JSON exactly. A clipped height label and unreadable arrow glyph in
guidance were corrected; the shortened height label was rechecked in the final
app. Sampled read-only viewer-handle behavior is additionally synthetic evidence.
The app is closed and verification screenshots/receipt are saved.

Final current3 libraries: 2,818 passed, 3 ignored (core 882, GUI 526, MCP 305,
render 212, video 893); timeline 45, native graph 48, scaling 6, unchanged
Legacy golden 1. Clippy and app build pass with warnings. Remote branch was
read at 12:09 UTC and still matched 30f221cdccb8bb87fdb7251bb5f8aed987a3081c.
Full roadmap remains incomplete. Local uncommitted source; no push or merge.

## Cloud continuation — geometric window keys and Boolean combinations (2026-10-06)

WindowMatte is an additive typed graph source referencing an existing operator's
primitive mask and manual scalar keys. It resolves geometry independently of RGB
parameters and source alpha, supporting signed/highlight and transparent inputs.
No mask gives a full key; disabled operators give zero. Invalid/unresolved geometry
produces a source-specific diagnostic and blocks qualified export. Separate
MatteRefine nodes supply cleanup. Neutral masked owners plus a separate unmasked
correction allow multiple independent windows without applying color twice.

GUI Add key utility / Window from creates a key; MCP uses the existing atomic
add_grade_graph_utility operation. KeyMixer supports max union, min intersection,
clamped subtraction and multiplication of continuous feathered weights. MatteApply
preserves source coverage. Deletion retains still-referenced operators and whole
grade copying remaps window references. The new graph variant requires a compatible
reader. Bézier paths, tracking and broader coordinate/inspection scopes remain open.

Qualification covers typed/atomic edits, copy/removal/serialization, invalid static
and animated geometry, clip-local retime-independent sampling, all geometry hash
fields and color-parameter independence. An independent f64 oracle checks 432
CPU/GPU cases over two working domains, nine shape pairs, four key operations,
three alpha values and two canvases. Raw keys meet CPU 1e-5/GPU .003 per-channel
bounds; the corresponding signed/highlight matte applications meet CPU .0001/GPU
.03 bounds with exact tested alpha. These are bounded sample tolerances, not an
unqualified perceptual/HDR claim. Eight actual MCP Native/Legacy mode fixtures
exercise authoring, undo/redo, file history reload, animated preview and three-frame
ProRes decoding; Native fixtures also inspect the raw combined matte read-only.

Native desktop review verified a parked source insertion and undo without pixel
changes, union-to-subtract shape change, raw grayscale matte inspection and exit,
and saved subtraction reload. That review exposed a broader grade-graph history
bug: Serde's internally tagged command buffering changed numeric map keys into
strings, so the file loader silently omitted otherwise-saved undo history. A
bounded numeric/string key decoder fixes this without changing serialized keys;
duplicate/out-of-range/malformed IDs are rejected. Core .photon history undo/redo
and actual MCP saved-history tests cover it. The same pre-fix saved fixture then
reopened with both edit-tree branches in the final build; one undo restored the
initial union. Save all & quit restored the full Grade semantically (empty track
arrays are omitted by serialization). This cannot restore history already erased
by a subsequent old-build save. Screenshots include the original failure and the
final recovery, labelled in the verification receipt. The app is closed.

Current libraries: 2,826 passed, 3 ignored (core 885, GUI526, MCP307, render213,
video895); timeline45, native graph49, scaling6, unchanged Legacy golden1. Current
Clippy and app build pass with warnings. An earlier integration invocation used
incorrect target names; the corrected complete suite and the post-history-fix
current suite both passed. Full roadmap remains incomplete. Source is local and
uncommitted; no commit, push or merge.

## Cloud continuation — paired native program/output measurements (2026-10-06)

The read-only measure_native_output MCP tool captures the program's unclipped AP1
working texture immediately before the SDR transform and the actual sRGB output
from the same Full/original EngineFrame. It reports separate straight RGB extrema,
equal-covered-pixel means, signed/above-one counts, coverage and a 128-bin relative
AP1-Y stop histogram over [-12,12), with explicit nonpositive/under/overflow counts.
Fully transparent frames have null color extrema/means. The PNG is made from the
actual output readback, not a second CPU tone-map prediction. Document ID, sequence,
frame-start tick, revision, snapshot generation and content hash bind the result.

Legacy, unresolved stages, tap fallback, lossy cached playback, nonfinite pixels,
changed alpha, stale document identity/revision and invalid format/time requests
fail explicitly. Both authored and rendered pair sizes are bounded to 16,777,216
pixels. No silent downsampling occurs. The added NativeSceneProgram tap resolves
the child of the qualified preview/delivery output-transform node; the MCP tool
exposes the sRGB preview pair only. Existing SDR scopes keep their original signal
interpretation. This adds an MCP measurement surface, not an interactive HDR
waveform/vectorscope UI, absolute-nit/IRE scale or calibrated monitoring capability.
Display-referred inputs do not become reconstructed scene radiometry by conversion
to AP1; the reported stops describe the engine's working values relative to 0.18.

Six pure reference tests cover AP1 Y derived independently from the published
chromaticities/white, signed/highlight values, partial-alpha weighting, transparent
null statistics, bin/outlier accounting, dimensions/resource bounds and malformed
pairs. The compiler verifies both preview and delivery scene-tap placement and
Legacy refusal. An actual MCP test authors a Native solid/grade/half-opacity locked
track, checks unclipped working values within .003, compares the returned PNG to
render_frame_at within one code value with exact tested alpha, and proves document
and history are unchanged. Invalid time/format/sequence calls fail. A controlled
transport-blocked document replacement at the same revision is rejected rather
than returning stale measurements. Another actual MCP test rejects an oversized
Native format without changing document/history.

Current libraries: 2,835 passed, 3 ignored (core 885, GUI526, MCP309, render219,
video896); timeline45, native graph49, scaling6, unchanged Legacy golden1. All-target
Clippy and native app build passed with warnings. Generated tool documentation is
refreshed. No new native-window interaction is claimed for this API-only increment;
the preceding Boolean-window/history native review remains its own evidence.
Interactive measurement presentation, broader scene/HDR scopes, representative
performance/hardware qualification and the rest of the roadmap remain unfinished.
Local uncommitted source only; no commit, push or merge.


## Cloud continuation — interactive native output measurements (2026-10-06)

Color Controls now exposes “Measure program / output” for the active Native
sequence, including with no clip selected and on locked tracks. The read-only
floating window captures the currently presented paused preview, retaining its
actual Draft/Full resolution and requested proxy mode. It shows the actual SDR
thumbnail, AP1 and sRGB minimum/mean/maximum values, signed/above-one channel
counts, alpha coverage, and the 128-bin AP1 luminance histogram in stops from
0.18. It shares the qualified paired-measurement implementation with MCP rather
than recalculating the display transform on the CPU. Use Full/original for final
checks; this UI does not silently replace Draft with a Full render.

The capture identity includes document, active sequence, tick, revision,
generation, content hash, dimensions, quality and proxy request. Seeking or
playing clears old results; playing disables Capture, and pausing asks for a new
capture. Inspection-only/cached-playback or unqualified frames are refused. A
single bounded worker reads both retained textures, then normal scope routing is
restored. Close/reopen epochs reject old completions without starting an unbounded
queue; Escape and X close the window. Mode/document/sequence changes close it.

Five GUI regressions cover scientific formatting, stale/closed/reopened captures,
active-Native/no-edit guards and an actual locked half-opacity Native engine
capture, including the subsequent normal program tap, playback/pause and document
replacement. Current libraries: 2,840 passed, 3 ignored (core 885, GUI531, MCP309,
render219, video896); timeline45, native graph49, scaling6 and unchanged Legacy
golden1 passed. All-target Clippy and native app build pass with warnings.

Native cloud-desktop review verified Draft 960×540/518400 covered pixels and
Full 1280×720/921600, half alpha, negative AP1 red and above-one green/blue with
bounded SDR output. Escape, seek invalidation, recapture, playback disabling and
close were exercised. An initially stale pause instruction was repaired and the
final build visibly shows “Playback paused. Capture the current preview.” Full
recapture succeeds afterward. History remains zero edits; the fixture file is
byte-identical after both runs and the app exits without a save prompt.
Screenshots and verification receipts are included in the local checkpoint.

This qualifies a bounded, one-frame software measurement window. It does not
qualify continuous HDR waveform/vectorscope displays, absolute nits/IRE,
calibrated external monitoring, recovered scene radiometry from SDR sources,
representative performance/soak tests or the remaining roadmap. Source is local
and uncommitted; no commit, push or merge.


## Cloud continuation — native arithmetic compositing (2026-10-06)

Native Managed track and clip/project Merge authoring now qualifies Linear Dodge,
Linear Burn, Subtract and Linear Light, using explicit unclamped AP1 equations
B+S, B+S-1, B-S and B+2S-1 respectively. Source-over alpha and opacity are unchanged.
Working RGB remains signed/extended and is bounded only to finite RGBA16F storage.
The native track dialog states this behavior. Legacy names retain their original
bounded equations; no serialized blend identifiers changed. These are creative
numeric operations, not calibrated/perceptual contrast models.

NativeArithmeticMerge has an independent IR/hash identity (tag58). Domain,
qualified-mode, binary arity and finite opacity guards reject malformed or
Legacy/log/matte/display-domain use. Native authoring lowers centrally through
the builder; track and graph paths are both covered, including animated graph
opacity and distinct cache hashes. A Legacy Merge IR cannot silently substitute
its bounded equation in a native arithmetic graph.

The independent f64 affine/source-over oracle checks 360 CPU/GPU combinations of
four modes, six signed/highlight/boundary vectors, five alpha pairs and three
opacities (CPU <1e-6, GPU <.01). Eight additional opaque/partial-alpha extreme
cases verify finite half-storage saturation (GPU absolute tolerance32 at extreme
magnitudes); explicit Legacy comparisons preserve the old bounded equations.
Four actual MCP fixtures author track mode/opacity through undo/redo checks and
verify native PNG/scopes/ProRes output. GUI draft tests cover all four modes,
JSON roundtrip and exact undo.

Native cloud-desktop review verified new enabled choices and the explanation,
Subtract draft cancellation with zero edits, one Apply/one history entry,
retained negative AP1 channels in the measurement window, autosave, app restart
and restored history. Visible History Undo restored the initial orange preview;
the final saved video_tracks array equals the initial fixture exactly. One
injected Ctrl+Z did not change the UI, so keyboard-shortcut behavior is not
claimed by this test; the visible Undo control was verified.

Current libraries: 2,844 passed, 3 ignored (core 885, GUI532, MCP310, render219,
video898); timeline45, native graph51, scaling6 and unchanged Legacy golden1
passed. All-target Clippy and native application build passed with warnings.
An earlier aggregate invocation named nonexistent scaling targets; the corrected
current aggregate completed successfully. No calibration/hardware, continuous
HDR monitoring, representative performance/soak or full-roadmap acceptance is
claimed. Local uncommitted source only; no commit, push or merge.


## Cloud continuation — AP1 whole-color comparisons (2026-10-06)

Native Managed Darker Color and Lighter Color now select a whole straight RGB by
AP1 scene-linear luminance, then composite with the existing source-over alpha
and opacity contract. They preserve signed/highlight RGB instead of choosing each
channel independently. Equal computed Y selects the backdrop. The selector uses
the AP1-to-XYZ Y row, not Legacy's 0.30/0.59/0.11 artistic luma. Near a tie,
half-float input rounding can change which whole color wins; this is a hard
creative selection, not smooth matching or calibrated/perceptual contrast.

NativeLuminanceMerge has a separate IR/hash identity (tag59), binary AP1 input and
mode/opacity validation, and central native track/graph lowering. Legacy
whole-color math remains unchanged. Animated graph opacity and cache separation
are covered. The native track dialog identifies the AP1 comparison semantics.

The independent high-precision AP1-Y/source-over oracle passes 240 CPU/GPU cases:
two modes, eight signed/highlight/tie/order-reversal vectors, five alpha pairs and
three opacity values (CPU <1e-6, GPU <.01). A separate regression explicitly
contrasts native selection with Legacy luma and verifies an exact f32 CPU tie.
Two actual MCP fixtures author the modes with undo/redo and qualify PNG, scopes
and ProRes output. GUI draft, JSON reload and exact undo tests cover both choices.

Native cloud-desktop review exercised both modes in sequence: Darker Color chose
the blue backdrop, Lighter Color the orange source, each creating one history
entry. Autosave and restart preserved the lighter state and both entries. The
first visible Undo restored blue; the second restored the original Normal state.
Explicit File Save restored the entire video_tracks array exactly to the initial
synthetic fixture. Screenshots and before-close test-state backups were retained;
no user media was involved.

Current libraries: 2,849 passed, 3 ignored (core 885, GUI533, MCP311, render219,
video901); timeline45, native graph52, scaling6 and unchanged Legacy golden1
passed. All-target Clippy and native application build passed with warnings.
One initial isolated GPU invocation aborted in glibc's
__pthread_tpp_change_priority before reporting a test result. The unchanged
retry, five subsequent isolated repetitions and full native graph suite passed.
Its underlying cause is unresolved; these repeats do not establish that the
runtime issue was fixed or close the longer stability/hardware qualification.
No commit, push or merge; the broader roadmap remains unfinished.


## Cloud continuation — expanded playback/source acceptance (2026-10-06)

Ran the broader managed-color contract, native source/transfer/ACES2 output,
preview/import, scope-tap, seek-budget and session-playback integration targets
on the current tree. The first pass identified an outdated delivery assertion:
the test still expected a solid-color Native sequence to be unavailable even
though solid sources and ProRes delivery are qualified. The corrected contract
now asserts successful 32×32/30-frame full/original ProRes resolution and a native
BT.709 output stage, while continuing to reject H.264, proxy/preview delivery,
unqualified Deflicker and the wrong export transform. The generic Legacy
compiler remains explicitly gated for native intent.

The parallel playback target also initially failed to observe frames on both
sides of the strict ±3-frame cut window; it did not report a transparent pixel.
An unchanged isolated run passed. The test was synchronously mapping/readbacking
every presented GPU texture inside that narrow real-time observation loop.
Capture now retains bounded immutable EngineFrame references during playback
and reads them after shutdown, removing that observer stall. Both-side coverage,
the original ±3-frame window and opacity assertions are unchanged. Three full
session-playback target repetitions passed after the harness change, followed
by the complete expanded run. No production playback code or drop allowance was
changed, and these checks do not prove reference-machine throughput.

Expanded current integration results: 66 passed, 1 intentionally ignored
sidecar-kill test (not run). Breakdown: managed contract6, native ACES2 output5,
native source11, native transfer6, preview/import8, scopes15, seek8, playback7.
No missing-GPU/tool skip message occurred. The current workspace library rerun
passed 2,849 with 3 ignored; all-target Clippy passed with warnings. The previous
native graph52, scaling6, Legacy golden1 and application build qualifications
remain applicable because this increment changes only tests and documentation.

The earlier isolated GPU/glibc abort remains unresolved despite its passing
repeats. Long representative soak, hardware performance/calibration, the full
balance/match/tracking/version/export workflow and remaining implementation rows
are still open. No new native-window feature is claimed here. Source remains
local and uncommitted; no commit/push/merge.


## Cloud continuation — bounded translation tracking (2026-10-06)

Implemented a shared, snapshot-isolated translation analyzer used by two MCP
tools and a native Color Controls dialog. This is a bounded ellipse/rectangle
center tracker, not a planar, scale, rotation, perspective or Bézier tracker.
Ordinary enabled clips with enabled ordered grades are supported; graph,
nested/compound and adjustment owners are explicitly refused. The sampled
input is the actual Full/original ClipPreGrade canvas after source fitting and
clip transforms/effects. Tracking writes clip-local center keys only.

The numerical kernel uses direct spatial, mean-subtracted normalized
cross-correlation of straight RGB channel-average texture, with a fixed
reference patch, deterministic tie order, runner-up separation and bounded
quadratic subpixel refinement. This texture signal is not physical luminance.
The mathematical reference is J. P. Lewis, [Fast Normalized Cross-Correlation]
(https://scribblethink.org/Work/nvisionInterface/nip.html); this implementation
does not use the paper's FFT acceleration. Positive gain/offset tolerance does
not confer scale, rotation or perspective invariance.

Input is bounded to 16,777,216 pixels and downsampled to a maximum 256-pixel
analysis side. Template half-size is 4–16 analysis pixels, search radius 2–24
(default 12), default minimum NCC .75 and margin .05. Missing texture/coverage,
ambiguous peaks, cuts/low confidence and search-boundary hits stop instead of
inventing motion. Forward/backward intervals contain at most 121 samples and
have a 120-second analysis bound. Cancellation is cooperative; an in-flight GPU
readback can finish before the worker releases its resources.

Analysis clones the document and uses its own engine session, preserving the
user's transport and history. Apply is a separate explicit action producing one
normal undoable set-grade command. Document ID/revision/fingerprint and source
file metadata are checked; metadata is not a full media-content hash. Locked or
stale targets refuse. Existing center animation needs explicit replacement
permission; outside keys remain but adjacent interpolation may change, and
endpoints hold outside the sampled interval. Partial results need separate
acceptance and at least two samples. Size, rotation and softness remain intact.

Fifteen focused tests pass: five numerical kernel tests, four pure apply/plan
checks, one actual isolated-engine forward/backward translation test in both
Legacy and Native Managed, one actual asynchronous MCP analysis/apply/undo test,
and four dialog lifecycle/context tests. Native cloud UI analysis produced 31
samples from frame 0 through 30, moving center X from 0.4 to 0.5333 with minimum
correlation 0.838. Analysis/discard left history at zero and playhead unchanged;
repeat analysis and Apply created one edit. Frame 29 visibly kept the window on
the moving patch. Explicit Save, restart and visible Undo preserved history and
restored the entire video_tracks array semantically (the only JSON difference
was omission of empty params.tracks). Two 31-key center lanes were verified in
the saved file. Final readable wording was rebuilt and rechecked natively.

Full workspace libraries passed 2,864 with 3 ignored (core 885, GUI 537, MCP 312,
render 224, video 906). All-target Clippy and application build passed with
warnings; the 15 focused tests passed again after the cosmetic label correction.
MCP reference generation now lists 519 tools. Screenshots and native saved-state
checks are retained. No commit, push or merge. Real-footage accuracy, full
tracked-isolation export/reimport, longer soak and hardware qualification remain
open; the earlier isolated GPU/glibc abort has not been declared fixed.


## Cloud continuation — tracked version and delivery regression (2026-10-06)

Added a full in-process MCP dispatch regression for a moving Native Managed
window. It creates untracked/tracked grade versions, analyzes and applies a
four-frame translation, switches both versions to prove the original stays
untracked, serializes/reloads the active version, previews all four frames,
exports ProRes with a verified manifest, and redecodes the delivered frames.
The moving center samples match the qualified Native output transform within
16-bit codec tolerance and preserve source alpha. Export leaves the source
document/history unchanged. The delivered file is imported back through MCP
and probed as 96×72 at 30 fps with the expected four-frame duration.

This tests delivered-media reimport/metadata, not an inverse of the SDR output
transform or an exact scene-referred reconstruction. It does not close the full
balance/match/real-footage tracking/version/delivery acceptance row. Fixture
construction is direct; tracking, version operations, preview, export, import
and probe use the actual JSON-dispatch path.

The first fixture attempted a /tmp export that the normal application path
policy correctly refused. The fixture now writes beneath its already-permitted
working directory; production containment was not loosened. FFprobe decimal
seconds differed from exact frame ticks by one third of a microsecond, so only
the metadata-duration assertion permits one microsecond. Independently decoded
frame count remains exactly four. The focused test passes, as does the current
workspace library suite: 2,865 passed, 3 ignored (MCP 313; other counts unchanged).
Production code is unchanged from the bounded-tracking checkpoint. All-target Clippy also passed with warnings. No commit, push or merge.


## Cloud continuation — managed graph Resize (2026-10-07)

Resumed root-only cloud work at the user's explicit request. The disposable
workspace had been reset; all 64 source files from Library checkpoint 44 were
restored against exact base `30f221c` and verified against their saved SHA-256
hashes. Rust 1.99, workspace-local Debian development/runtime dependencies and
Microsoft ONNX Runtime 1.20 were restored without altering the user's PC.

Native clip/project composition graphs now support bounded Resize with Stretch,
Contain and Cover. Dimensions must evaluate to finite integers in 1..8192 with
at most 16,777,216 intermediate pixels. Invalid types, fractional values,
nonfinite values and excessive allocations fail visibly. No color transfer or
clamping is inserted: bilinear sampling retains premultiplied signed AP1 RGB
and alpha. Contain creates transparent letterboxing; Cover crops symmetrically.

The native graph has a fixed sequence canvas: Resize first samples the requested
intermediate raster, then normalizes its logical extent back to that canvas for
subsequent processing. It does not change sequence/export dimensions. Draft
scales both intermediate dimensions with the processing canvas, rounding to at
least one pixel. Legacy explicit resize behavior is unchanged. This operation
can reduce detail and requires two resampling passes; no new throughput claim is
made. Wider graph node coverage remains unfinished.

Focused compiler/domain checks pass, including dimension/arity/matte rejection,
fit-mode cache identity, twelve independently calculated f64 Fit/Fill/Stretch
CPU/GPU image cases (negative/above-white RGB, partial alpha, letterbox edges,
odd dimensions and singleton sources), and repeated Draft/Full/Draft cache
checks. An actual MCP preview and ProRes encode/decode regression passes using a
native clip composition. Initial integration returned a blank center because a
smaller logical texture reached downstream canvas-sized processing; explicit
normalization fixed the regression. The focused tests require a real GPU
adapter (software Vulkan on this cloud host), not a mocked evaluator.

Current-tree library suites pass **2,868 tests / 3 ignored** (core 885, GUI 537,
MCP 314, render 224, video 908 / 2 ignored, plus one ignored matte test).
All **54 native graph integration tests**, both three-case scaling suites and
the unchanged Legacy golden corpus pass. The current app builds with warnings.
All-target Clippy passes with warnings after a scoped `result_large_err` allowance on
the unchanged public JSON-RPC transport API: its success and protocol-error
branches carry the same response structure; this avoids an API change or new
error-only heap allocation. Other warnings remain visible.
Native desktop interaction for this newly enabled node is not yet inspected.
The full roadmap remains incomplete; no commit/push or hardware acceptance is
implied by this increment.


## Cloud continuation — radial AP1 vignette (2026-10-07)

The catalog `stylize.vignette` now lowers to a dedicated native operation at
asset, clip, track and master scopes, with normal zone and keyframe evaluation.
It multiplies premultiplied scene-linear RGB by a smooth radial gain while
retaining alpha and signed/above-white channels. Amount is finite -1..1;
feather is finite 0..1. Native zero feather has an explicit corner-limit band
of 0.0001; singleton pictures retain their center. Legacy raster behavior is
unchanged. This is a creative image-space vignette, not optical lens correction
or a calibrated scene-illumination model.

A separate IR/cache identity (tag 60) prevents aliasing with Legacy raster
processing. Invalid domains, arity and parameters fail closed. Focused tests
qualify four-scope routing, half-open zones, clip-local interpolated keys and
parameter cache identity. Independent f64 references cover 240 CPU/GPU cases:
four picture geometries, five signed amounts, four feather settings and three
RGB/coverage vectors. An actual MCP four-scope catalog authoring, preview and
ProRes decode regression also passes. Current-tree aggregate verification passes: **2,871 library tests / 3 ignored**,
**55 native graph tests**, six scaling cases, unchanged Legacy goldens, all-target
Clippy and the native app build (warnings retained). No new desktop control layout is introduced; the existing catalog
controls are reused. General restoration and the remaining roadmap are open.


## Cloud continuation — deterministic AP1 grain (2026-10-07)

Catalog `stylize.grain` now has a separate native scene-linear implementation,
with explicit integer-hash noise rather than the Legacy floating hash. RGB
receives uniform signed noise multiplied by amount and source alpha; alpha and
signed/above-white color are retained. Monochrome mode shares one draw across
RGB; color mode uses independent channel salts. This is creative uniform grain,
not measured film/sensor noise, temporal denoising or an automatic flicker model.
A fixed seed produces a fixed pattern. Animate the existing seed property to
change it over time; the engine never changes it implicitly.

Amount and monochrome are finite 0..1, with monochrome selected above 0.5.
Seeds are finite 0..1,000,000,000, rounded to the nearest integer at compilation.
The seed is passed to WGSL as integer bits, retaining distinct values above
2^24. IR/cache tag 61 includes controls, seed and authored pixel scale. Draft
samples the same authored pixel grid at its pixel centers; it is a subsampled
preview of grain, not a promise of identical noise variance at reduced sizes.
Legacy behavior remains unchanged.

An independent Python integer-modulo/binary64 fixture supplies noise references
without calling production kernels. Numerical CPU/GPU checks cover 288 base
combinations across four geometries, four seeds, monochrome/color, three amounts
and three color/coverage inputs, plus repeated Draft/Full/Draft checks for the
8x4 cases. Compiler tests cover four scopes, clip-relative animated seed and
zone, strict controls, and cache changes; domain/arity/scale tests reject malformed
IR. Actual MCP catalog authoring at four scopes, preview and ProRes encode/decode
passes. Current-tree qualification passes: **2,874 library tests / 3 ignored**,
**56 native graph tests**, six scaling cases, unchanged Legacy goldens,
all-target Clippy and the native app build (warnings retained). No new GUI layout is
introduced; native desktop interaction for this effect remains uninspected.


## Cloud continuation — AP1 spatial noise reduction (2026-10-07)

Catalog `noise.reduce` now supports Native Managed asset, clip, track and master
stacks, including zones and keyframes. It is a bounded spatial bilateral filter:
range distances use straight AP1 RGB, accumulation is coverage weighted, and the
center alpha is retained. Transparent neighbors cannot darken an opaque edge.
Signed/above-white channels remain unclamped except for the shared finite
RGBA16F storage boundary. Strength is finite 0..1; zero is an exact bypass.

At Full quality the radius is two pixels (25 samples), spatial sigma is
1.5 + 1.5×strength, and range sigma is (10 + 70×strength)/255 in linear AP1
units. Draft scales the spatial neighborhood and sigma with its canvas, with
integer-radius rounding; oversampled neighborhoods are capped at radius eight.
This is not temporal denoising, motion-compensated restoration, a perceptual HDR
metric or a measured camera-noise profile. No real-time throughput guarantee is
made. Legacy raster behavior is untouched; native IR/cache identity is tag 62.

Independent binary64 bilateral references qualify 96 base CPU/GPU combinations
across four geometries, four strengths, three alpha values and flat/hard-color
edge cases, plus repeated Draft/Full/Draft cases. Inputs combine independently
specified grain with transparent edges and partial-coverage compositing. A
separate noisy-flat-region regression confirms over 50% variance reduction in
all RGB channels while preserving alpha, negative red and above-white green.
Compiler/domain tests cover four scopes, clip-relative keys, invalid parameters,
arity and cache identity. Actual MCP catalog authoring/preview/ProRes passes.
The new Resize/Vignette/Grain/Denoise export qualifications now explicitly require
GPU and FFmpeg rather than silently returning when unavailable; all 30 matching
native-export regressions passed with those requirements.

Current libraries pass **2,877 tests / 3 ignored** (core 885, GUI 537, MCP 317,
render 224, video 914 / 2 ignored, matte 1 ignored). All **58 native graph tests**,
six scaling cases, unchanged Legacy goldens, all-target Clippy and app build
pass with warnings. The full roadmap remains open; native desktop interaction
for these reused catalog controls has not been newly inspected.


## Cloud continuation — plain managed title clips (2026-10-07)

Native Managed timelines now render bounded plain `ClipSource::Text` titles.
Glyphon first rasterizes its existing Accurate-mode sRGB fill into a transparent
premultiplied linear-Rec.709 texture; an explicit Bradford-adapted linear matrix
then converts it into AP1 before the normal clip grade, transform, composite and
output stages. This follows the existing native solid-swatch contract: it is a
scene-pipeline graphic, not output-referred broadcast-white text. Glyph colors
retain the existing 8-bit sRGB authoring quantization. Legacy text is unchanged.

Native title IR/cache tag 63 includes text, font, color and layout. Bounds:
4096 UTF-8 bytes, up to 128 runs, font size 1..512, weight 1..1000, nonempty font
family up to 128 bytes, normalized position and max width. Outline, background,
karaoke and caption-style animation remain rejected rather than silently omitted.
Clip transforms/grades use the normal native chain. Fonts may fall back according
to the installed font set; this does not qualify font packaging or cross-platform
pixel reproducibility. Glyph rasterization remains GPU-only; no CPU glyph parity
is claimed. Graph Text and caption tracks are still separate unqualified stages.

`CaptionCompositor::composite_checked` now reports invalid targets and atlas/
render errors. Managed title failure aborts the frame and does not mark its cache
entry valid, avoiding a blank successful frame. Legacy callers retain the logging
wrapper. Checked-target rejection is tested; GPU-loss/atlas-exhaustion fault
injection has not been performed.

MCP's optional `style.clear_stroke` explicitly removes the inherited outline.
The native Titles panel offers a one-step “Use plain fill (remove decorations)”
edit. The command preserves text, color and timing, respects track locks, has
exact undo and JSON round trips, and is absent for an already plain style.
Passive title controls no longer clamp stored out-of-range values merely by
being viewed. Tool documentation was regenerated (519 tools).

Independent NumPy binary64 primary matrices/Bradford adaptation and sRGB decoding
qualify five glyph colors/alphas at repeated Draft/Full sizes, including logical
texture padding. Actual MCP insert, undo/redo, save/reload, preview and three-frame
ProRes color/alpha decode passes. A fixture export was correctly denied outside
the app's permitted roots; the retained fixture was rerun under an already
permitted repository directory without relaxing path policy.

Native cloud-window verification at 1180x812 showed the plain-fill control,
correct red title output and exactly one history entry. Explicit save, close,
restart and the visible History undo restored the original decorated clip
exactly on disk. Keyboard undo did not establish a successful action; the visible
undo control did. Both Draft/Full options were inspected. Screenshots and checks
are retained; the app was closed after the test.

Final libraries pass **2,881 tests / 3 ignored** (core 885, GUI 539, MCP 318,
render 224, video 915 / 2 ignored, matte 1 ignored). All **60 native graph tests**,
six scaling cases, unchanged Legacy goldens, all-target Clippy and the final app
build pass with warnings. The complete professional roadmap remains unfinished.

## Cloud continuation — display-authored plain captions (2026-10-07)

Native top-level SDR preview and ProRes delivery now accept enabled caption
tracks whose active cues use bounded plain fill. Caption glyphs are composited
**after** the managed output transform: decode the premultiplied sRGB viewer or
BT.709 delivery signal to display-linear Rec.709, apply glyphon alpha-over, then
re-encode. Caption white and colors therefore bypass scene grading/tone mapping.
The source is never mistakenly treated as AP1. Program and paired scene/display
scope taps remain pre-caption, and Draft uses the original authored canvas.

The explicit `NativeCaptionOutput` boundary (hash tag 64) must immediately follow
its matching native SDR output transform. The batch preserves enabled track/cue
order, uses half-open cue timing, and is bounded to 16 active cues, 128 word runs
and 4,096 UTF-8 bytes. Per-cue metrics share the plain-title bounds; complete
cue/word overrides are validated at their effective scope. Word font family,
weight and fill may differ; unsupported per-word layout changes fail visibly.
Disabled tracks and inactive cues do not block a valid frame. Checked shaping or
transfer failure does not publish/cache a successful blank frame.

Qualification on the current source tree:

- Actual GPU glyph colors match independent binary64 sRGB/BT.709 references
  for both encodings, translucent colors, odd dimensions, padded targets and
  Draft→Full→Draft cache reuse
- 48 standalone GPU transfer cases cover both directions, signed/extended
  values, alpha and piecewise thresholds against an independent f64 reference
- Translucent glyphs over a colored translucent graded image match independent
  display-linear alpha-over; the sampled program tap remains unchanged
- Actual MCP add-track/cue/style edits, one-step undo/redo, JSON save/reload,
  preview, three-frame ProRes decode/color/alpha and dependency-manifest checks
  pass; read-only preview/export preserve the document/history revision
- Full libraries: **2,883 passed / 3 ignored** (core 885, GUI 539, MCP 319,
  render 224, video 916; the ignored tests are unchanged)
- All **63 native graph integration tests** pass

Limits: outlines, backgrounds, karaoke and caption animation remain gated;
active display-referred captions inside scene-linear nested sequences are
explicitly rejected. Glyph rasterization is GPU-only; the CPU reference does
not pretend to rasterize fonts. Font fallback and 8-bit glyph color remain the
plain-title constraints. No new native-window caption inspection, full platform
font parity, HDR captions or hardware qualification is claimed by these tests.
This is another bounded increment, not full roadmap completion.

## Cloud continuation — timed caption fills (2026-10-07)

The qualified native caption boundary now also accepts the existing authored
SlideUp, FadeWords and grapheme-safe Typewriter timing, plus exact WordPop
highlighting. Cue/word times and the **unrevealed authored text** are validated
before resolution, so animation cannot hide oversized or invalid input. SlideUp
may transiently place the top anchor up to 5% below the normalized frame edge;
this is intentional clipping, not a relaxation of authored position bounds.
Per-word font/color overrides remain supported; layout and animation are
cue-level, and incompatible per-word overrides fail visibly.

`set_caption_style` exposes explicit `animation`, `word_pop` active/inactive
colors, and `clear_highlight`. Contradictory clear/set requests and invalid
colors/enums fail without a document/history mutation. The shared style schema
and regenerated 519-tool MCP reference describe these fields. Native title
clips still reject title animations and karaoke; accepting the vocabulary does
not silently enable an unqualified title path.

Qualification includes exact timestamp/cue-edge/Unicode/hash compiler checks
and four actual MCP-authored 18-frame ProRes workflows. SlideUp, FadeWords,
Typewriter and WordPop each pass one-step authoring undo/redo, save/reload,
preview checks at frames 0/3/12, decoded export color/alpha comparisons,
manifest verification and read-only history checks. Typewriter retains the
existing floor-of-grapheme-count timing contract: a full-word hold requires
cue time after that word's end. No alternate reveal timing was silently added.

The rebuilt native cloud window also showed red HI / blue OK at frame 0 and
blue HI / red OK after eight single-frame seeks in Draft; the recorded frame-8
image is retained. Native playback and Home/arrow navigation responded. A
Full-quality dropdown selection was not established by that UI interaction;
Full qualification comes from the actual MCP/export and GPU checks, not an
invented native-window result. The app was closed after inspection.

Outlines, backgrounds, underline and true intra-glyph FillSweep remain open;
Legacy's approximated FillSweep is not substituted into the managed path.

Final timed-caption tree: **2,886 library tests passed / 3 ignored** (core 885,
GUI 539, MCP 321, render 224, video 917), all **63 native graph integrations**,
all-target Clippy and the native application build pass with warnings. A schema
contract test initially assumed the manifest was an object rather than the
actual array; it was corrected, then the complete library suite rerun. No test
was disabled to obtain this result. Changes remain local and uncommitted.

## Cloud continuation — editable native graph text (2026-10-07)

Static `GraphOp::Text` payloads now lower to the already-qualified AP1 title
raster in both clip compositions and the outer project graph. The native path
validates the actual string/font/fill/position, rejects stray/animated parameter
bags, and hashes the resolved glyph payload. It does not route through Legacy's
transparent graph-text placeholder. The current payload uses default caption
weight/wrap metrics (700 / 0.8); it has no hidden outline or background.

A typed `GraphCmd::SetText`, shared `graph_ops::set_text` constructor and
`set_graph_text` MCP tool replace only the text payload. Node identity, params,
edges and canvas layout are preserved. Graphs referenced by locked clips are
protected; invalid payloads and exact repeats do not add history. Older text-free
histories remain readable. The generated MCP reference now contains 520 tools.

The native node inspector has explicit text/font/size/fill/position drafts,
Apply and Revert. Viewing stored out-of-range values does not clamp them;
concurrent payload changes invalidate stale drafts. Pure GUI tests cover
single-command undo, locks, stale drafts, no-op repeats and passive viewing.
Actual MCP creation/wiring/editing, undo/redo, JSON save/reload, preview and
three-frame ProRes decoded color/alpha checks pass for clip and project graphs.

Native cloud QA selected the Text node, edited and reverted a draft, then applied
one text edit. Returning to the program viewer showed the changed text. File →
Save persisted it; after a full app restart, one visible History undo restored
HI from bye. One separate node-position edit was also made for inspection, so
the history correctly contained two edits. The native graph-canvas inset remains
an existing placeholder; the actual program viewer supplied the visual proof.
No live inset preview is claimed. Ctrl+S did not establish a save in this test;
the explicit File menu did, verified in the saved document.

This inspection also exposed an older graph layout undo defect: moving a node
with no saved layout entry restored a synthetic (0,0) entry instead of absence.
A failing round-trip regression reproduced it. Move commands now preserve
optional old/new layout entries; older object-valued commands remain readable
and round-trip unchanged. Missing-node and non-finite moves are refused. All
48 timeline integration tests pass, including serialization, lossless inverse,
old-command compatibility and the new text edit. The fix cannot infer missing
layout provenance that an already-recorded older move command never stored.

Limits: static bounded plain text only (4,096 UTF-8 bytes, 128-byte font family,
size 1..512, finite normalized fill/position); no text-node keyframes, outline,
background, vendor font packaging or cross-platform raster guarantee. Legacy
text-node behavior is preserved and explicitly labeled unqualified in the UI.
All changes remain local, with the broader roadmap still open.

Final graph-text/layout tree: **2,891 library tests passed / 3 ignored**
(core 885, GUI 541, MCP 323, render 224, video 918), all **63 native graph
integrations**, **48 timeline integrations**, all-target Clippy and native app
build pass with warnings. The final binary reopened the saved history containing
older object-valued move records, replayed the saved text edit and displayed bye;
returning to the original history state restored HI. No release push or complete
roadmap acceptance is implied.

## Cloud continuation — native title and caption paint (2026-10-07)

Static title clips and top-level caption cues now render bounded outer outlines
and rounded backgrounds. An opaque glyph-coverage mask keeps outline geometry
independent of authored fill alpha: a transparent fill can still have a visible
hollow outline. The outline is a specified sampled radial maximum minus the
original coverage, not an advertised font-native vector stroke. Width is 0..8
authored pixels. Backgrounds follow the actual shaped typographic line rectangle
with padding and corner radius each in 0..128 pixels, using a rounded-rectangle
coverage function. Font/painter measurements and geometry use the same glyphon
shaping/wrapping inputs; Draft scales font and decoration pixels together.

Paint colors retain the existing 8-bit sRGB authoring quantization and composite
in linear Rec.709. Title pixels then convert explicitly to AP1 before scene
grading/output. Captions remain after the matching SDR output transform, with
pre-caption grading scopes. SlideUp fades the complete decorated cue once as a
group; independently fading overlapping box/outline/fill layers would be wrong.
FadeWords uses animation-only per-word mask alpha, independent of fill alpha.
Typewriter shapes the revealed text and its box; WordPop changes its word fills.
A reusable coverage mask and, only when required, one reusable fade-group target
avoid allocating a full-frame texture for every word/cue. The prior 16-cue /
128-run / 4,096-byte admission limits remain enforced.

The native title inspector now offers document-scoped outline/background drafts
with Apply/Revert and a single discrete history command. It preserves unrelated
text/font/fill/position, drops stale paint baselines, respects track locks and
does not clamp stored values merely by viewing them. Shared MCP style arguments
expose `stroke`, `background`, `clear_stroke` and `clear_background`; contradictory
clear/set requests, invalid bounds/colors and unknown style keys fail atomically.
The generated reference still has 520 tools. No document migration is required.

Qualification on this tree:

- 48 actual GPU decoration cases against independent binary64 outline/rounded-box
  references, including fractional widths, alpha, singleton/odd dimensions and
  unchanged target padding
- Actual glyph rendering against independently derived AP1 matrix values,
  including hollow transparent-fill outlines, rounded boxes, combined paint,
  Draft→Full→Draft cache reuse and zero logical-frame padding
- 12 group-opacity cases against independent premultiplied alpha-over; compiler
  tests verify separate group/word opacity, paint hashes and invalid admission
- Actual MCP author/undo/save/reload/preview/ProRes checks for a decorated title
  and all four decorated caption animation modes; decoded color/alpha and
  dependency manifests verified, with the plain-caption regressions retained
- Native cloud window: inspected paint values, changed/reverted a local draft,
  applied background removal, verified exactly one history edit, saved/restarted,
  then restored the background with one undo. Original paint and embedded
  history are verified on disk; the application was closed
- **2,896 library tests passed / 3 ignored** (core 885, GUI 543, MCP 325,
  render 224, video 919), all **66 native graph integrations**, unchanged
  Legacy golden corpus and both three-test scaling suites. All-target Clippy
  and the native app build pass with warnings; diff whitespace checks pass

Limits: this qualifies Full/Draft authored-pixel rendering, not arbitrary
supersampling, font portability, large-project real-time performance or HDR
captions. Underline and true intra-glyph FillSweep, differing per-word metrics /
decorative styles, title-style animation and active nested captions remain
explicitly gated. The native GUI evidence is one 1180×812 cloud window in Draft;
Full and delivery evidence comes from the GPU/MCP/export checks. The wider
roadmap is still incomplete and these changes remain uncommitted.

### Cloud continuation — 2026-10-07: explicit managed vector/logo source

The first native vector boundary now supports whole-document embedded vector
clips and static `VectorIn` nodes at clip/project scope. It qualifies bounded
solid-filled/solid-stroked paths and plain structural groups. Native vector
inputs use a separate IR/source/cache contract (hash tag 66), convert authored
sRGB swatches before linear-light source-over, decode the premultiplied scene
readback without multiplying alpha again, then explicitly convert Rec.709 to
AP1 before asset/clip/group/look/track/master grading. Legacy raster paths and
conversion remain unchanged. The vector raster is an 8-bit sRGB scene readback
into RGBA16F, not a claim of high-bit vector paint precision.

Native framing contains the document on the logical canvas, centered with no
interactive-view 10% inset. Draft rasterizes directly at its logical size;
source keys include size and a separate native namespace. Immutable drawing
revision changes invalidate the raster and graph caches. Graph-only vector
sources now carry their document snapshot even with an empty media pool;
export manifests retain and verify that embedded source document.

Qualification rejects external vector files, node/artboard subreferences,
non-solid paint, dashed/variable/painted strokes, live vector text (outline it
first), symbols, group transforms/clipping/isolation, raster/mixed documents,
vector effects and non-Normal blending. Unsupported document content binds a
ColorPipelineUnavailable diagnostic to a new frame instead of returning the
previous successful picture. Extra graph parameters and animated reference
payloads are rejected. Bounds include 4,096 nodes, 256 layers, 64 hierarchy
levels, 1 MiB total path strings, 65,536 bytes/8,192 elements per path, finite
paint/geometry, and at most 16,777,216 output pixels with each side at most
8,192. These are safety limits, not a real-time performance guarantee.

Focused checks passed: real GPU solid/overlap/partial-alpha samples against
independent binary64 source-over and sRGB equations; Draft→Full cache behavior,
AP1 matrix references, drawing revision changes and zero pool padding; compiler
asset-stage ordering, domain/parameter/refusal cases; actual MCP path/graph
authoring with undo/redo, save/reload, Full PNG, three-frame alpha ProRes decode,
manifest verification, and refusal after an authored unsupported-gradient edit.
The first refusal fixture incorrectly mutated the document without its history
revision; it was corrected to use the actual MCP edit path and then passed.
Final current-tree aggregate: 2,901 library tests passed / 3 ignored; 66 native
graph tests, 48 timeline tests, the unchanged Legacy golden corpus and both
three-test scaling suites passed. All-target Clippy and native app build passed
with existing warnings. Native QA on the cloud desktop verified Draft and Full
preview, a +10px source-path edit, the changed managed picture, explicit Save,
restart with one retained history edit, visible-history Undo, and the restored
picture and saved identity transform. The native app is closed. The illustrative
fixture has a legacy 100x100 artboard alongside a 128x64 document canvas; this
whole-document source deliberately uses document dimensions. It does not claim
artboard-subreference qualification. Final focused tests repeated the numerical,
MCP/export and refusal cases after the last production edits.
This is a bounded source qualification, not completion of all vector features
or the broader roadmap. No commit or push has been performed.

### Cloud continuation — 2026-10-07: native title/vector/nest transitions

Native input transitions now admit already-qualified title, embedded vector and
nested-sequence sources, in either participant position, in addition to media
and solids. Source admission does not bypass the original full native
style/color/graph/vector/nest checks. Matching color configuration, frame rate
and selected canvas remain required for nests; active nested output captions
and recursive/missing/unqualified sources still fail visibly.

The six existing native kinds retain their existing scene-linear operators and
parameter semantics: CrossDissolve, DipToBlack, DipToColor, Wipe, Push and
LumaWipe. Each participant is processed through its own source/clip chain before
the transition; outer track/master/output processing applies once. Titles and
vectors have unbounded generator handles. Nests borrow only their remaining
speed-adjusted source duration: shortening emits TransitionHandleClipped Info;
zero handle suppresses the mix with a Warning. No last-frame freeze substitutes
for missing transition handles. CrossDissolve end fades also admit these source
classes. Nonadjacent input transitions and non-dissolve end fades remain gated.

A failing real MCP preview first reproduced the prior refusal for an outgoing
title. After the source admission change, 48 actual MCP transition preview,
undo/redo and five-frame alpha-ProRes workflows pass across the six kinds and
participant directions, including nested/nested and title/title combinations.
Three further title/vector/nest end-fade workflows pass. The title fixtures use
transparent glyphs and a full-frame decorated background for independent color
and coverage expectations; glyph rasterization itself is qualified separately.
Vector fixtures share one document, so two differently colored whole-document
vector sources are not implied by that fixture. Positive compiler matrices
cover all three source classes, participant masks and five transition ticks.
Nested tests verify 2x-speed handle shortening, exact fractional source tick,
signed/highlight scene math, one output boundary, zero-handle suppression, and
continued refusal of active nested captions. A first test assertion incorrectly
rounded the requested source tick; the documented continuous tick mapping was
confirmed and the assertion corrected to 9.5 frames (decoder frame selection is
a separate stage). Initial aggregate passed; the final aggregate after the source-inspection fix is recorded below.

The cloud native app also displayed the nested red source at frames0/1, the
purple dissolve midpoint at3, blue incoming source at4, and the restored midpoint
when stepping backward, at Draft and Full quality. App closed without document
edits. A separate source-inspection regression found that a qualified vector
clip still received the old missing-video-input warning. Shared vector-asset
validation now reports intrinsic sRGB paint, rejects incompatible explicit
video/still tags, and keeps Color inspection read-only; its failing regression
now passes. The final native build also visibly reports “Input color: intrinsic · sRGB vector
paint” without a missing-interpretation warning; viewing left the grade empty.
Final current-tree libraries: 2,906 passed / 3 ignored (MCP328, video924,
render226, GUI543, core 885). The transition tree also passed 66 native graph,
48 timeline, unchanged Legacy golden and 6 scaling integration tests; the later
input-status fix was covered by the final full libraries and focused vector
suite. Final all-target Clippy and native app build passed with warnings. All
native QA apps are closed. No commit or push; the broader roadmap is unfinished.

### Cloud continuation — 2026-10-07: native monitor logical-edge accuracy

Native-window QA exposed an unwanted bottom/right fade when a small managed
picture was magnified. A real two-stage GPU regression reproduced the problem:
the program was presented into the full pool bucket, then linear sampling of a
cropped UV rectangle blended the picture's last texels with transparent padding.
The first failing sample was red149 instead of153; this was a display artifact,
not altered export data or intended vector transparency.

Native SDR monitor presentation now texel-copies the logical region into an
exact logical-size sRGB target. Its real texture edge clamps the existing
bilinear display sampler, without changing source pixels, grading, export, the
Legacy presentation path, or its established alpha-view convention. Invalid
sizes, source/target formats, usage and sampling layouts refuse the new checked
present API. Unsupported encodings/invalid frame metadata clear the monitor.
Frame/quality changes reuse targets of the same size; alpha/color toggles do not
allocate another target. Native precision-trim stills copy the logical target.

The previously failing GPU magnification test passes at every pixel. Additional
real GPU cases verify nonuniform source orientation/color/alpha and invalid
requests leaving targets unchanged. A real EngineBridge/egui integration checks
Full→Draft→Full, exact dimensions, unchanged-frame target reuse, alpha toggling
and no document/history mutation; the existing Legacy bridge test still passes.
The native cloud window visibly has uniform picture boundaries at Draft, Full
and alpha inspection on the same16x8 vector fixture. Color viewing was restored
and the app closed. This does not establish calibrated display or general GPU
throughput qualifications.

Final aggregate: 2,909 library tests passed / 3 ignored (GUI544/render 228;
MCP328/video924/core 885 unchanged). Both EngineBridge integrations,66 native
graph tests, the unchanged Legacy golden corpus and 6 scaling integrations pass.
All-target Clippy and native app build pass with existing warnings. No commit or
push; remaining roadmap work continues.

### Native static polygon generator — cloud continuation, checkpoint 57

Native clip/project `MaskShape::Polygon` now has a dedicated AP1 white/coverage
IR path, cache tag 67, f64 scalar reference and bounded GPU shader. Static
vertices use normalized [0,1] local-box coordinates; center/size use the existing
shape center/half-extent convention, rotation is radians, and feather is inward
local-space edge distance. Even-odd fill handles concavity and crossings;
repeated/degenerate edges are finite and zero-area shapes are transparent.
The payload is limited to 3–128 finite vertices. Center, size, rotation and
feather lanes are sampled; per-vertex animation is explicitly refused and remains
post-v1. Invalid payload/geometry/ports refuse rather than borrowing Legacy.
Logical pixel coordinates are independent of pool padding and preview quality.

Typed `GraphCmd::SetMaskShape` preserves identity, edges, layout and parameter
lanes through undo/redo and serialized commands. Shared graph edits protect
locked clips and exact repeats are no-ops. Native inspector shape/vertex drafts
use explicit Apply/Revert with document-scoped state and stale-base protection;
passive widgets do not clamp imported vertices. MCP `set_graph_mask_shape` uses
the same constructor. Legacy polygon rendering remains unqualified; this does
not claim general typed-mask coercions or a point-handle canvas editor.

The native 512×256 polygon window verified a vertex draft (third x 0.5→0.25),
explicit one-step Apply, file Save, app restart and History Undo. The restored
Document exactly equals the original after separating the normal saved
`photon_history`/`photon_format` envelope; redo history remains persisted.
The graph inset still is a placeholder, so the program monitor was inspected.

**Native monitor alpha finding:** that real-window inspection exposed bright
soft edges. The exact-size native present pass inherited straight-alpha output
from the older presenter, but egui's registered-texture pipeline blends with
source factor One / destination OneMinusSrcAlpha. An actual egui renderer test
reproduced red 127 instead of 32 for encoded red 0.5 / alpha 0.25 on black. The native
logical present path now retains encoded premultiplication before the sRGB
round trip. Both the graph/export pixels and Legacy presenter are unchanged.
The actual egui renderer now passes all four alpha values (0/.25/.5/1) for
color and alpha inspection over black and gray, using the real registered-texture
pipeline. Native Draft/Full color and alpha windows confirm the fringe is gone;
Color viewing was restored and the app was closed.


Final checkpoint 57 results: **2,914 library tests passed / 3 ignored**
(core 885, GUI 546, MCP 330, render 228, video 925). All 68 native graph integrations,
49 timeline command tests,3 EngineBridge/egui tests, the unchanged Legacy golden
corpus and 6 scaling integrations pass. Polygon qualification includes 84 base
GPU/f64 cases over three resolution/cache evaluations, the 128-vertex boundary,
concavity/crossings/degenerate geometry and transparent pool padding, plus
animated geometry/hash/invalid-payload compiler tests. Actual MCP clip/project
shape edits, undo/redo, unchanged-repeat/invalid refusal, save/JSON round trip,
preview pixels, three-frame ProRes RGBA64 redecode and manifests pass.
All-target Clippy and the native app build pass with existing warnings. The MCP
reference was regenerated at 521 tools. Source remains local and uncommitted;
remote branch still equals 30f221c at 11:57 UTC. Full roadmap work continues.

### Native active-word underlining — cloud continuation, checkpoint 58

Native caption Underline now paints beneath the active word for its half-open
[start,end) window, keeping glyph fill unchanged. Stroke color/width takes
precedence; without a stroke, active_color and 5% of the cue font size (bounded
to 1–8 authored pixels) supply the underline. Placement follows actual shaped
word advances and each wrapped line's typographic bottom, excluding separator
spaces. Word metadata is used only for this layout query; Legacy glyph rendering
and its span attributes remain unchanged. Empty/revealing words, zero-width or
transparent paint, SlideUp group opacity, FadeWords and Draft scaling retain
explicit behavior. Payload validation and hashing include word, color and width.
Background-only decoration draws are scissored to their padded rectangle plus
antialias fringe instead of shading a full frame for every underline.

MCP caption styles accept `underline: {active_color: "#rrggbb"}` at the existing
track/cue/word scopes; it is mutually exclusive with `word_pop` and
`clear_highlight`. There are still 521 tools. Ten plain/decorated caption mode
workflows now pass actual author/undo/redo/save, preview and 18-frame ProRes
export/redecode/manifest checks; underline samples cover first word, second word
and the exact end boundary. GPU qualification covers 108 base paint/wrap cases
at three quality/cache sizes against an independent rectangle coverage equation,
plus shaped word/space/wrapping references and invalid-payload checks.

Native-window QA found two caption inspector issues during the acceptance pass:
controls overflowed the narrow sidebar, and a stale draft survived Undo even
when the document and rendered underline were restored. The editor now wraps
controls, keys drafts by document/track/target and resets them when their base
style changes. Passive fields do not clamp loaded values. Headless bounds,
passive-value and draft-isolation tests pass. In the revised native window, an
explicit disable/Apply removes the underline in one edit; Undo restores both the
picture and selected Underline controls with Apply disabled. Saving preserves
the restored document exactly while retaining normal redo history.

Final affected libraries pass 2,917 tests / 3 ignored (core 885, GUI 548,
MCP 330, render 228, video 926). App build and all-target Clippy pass with existing
warnings. The final UI tree passes all 71 native graph, 49 timeline, 3 bridge/egui,
1 unchanged Legacy golden and 6 scaling integration checks. True intra-glyph FillSweep,
differing per-word metrics/paint, nested captions and wider roadmap acceptance
remain open; this increment does not claim those capabilities.

### Explicit caption paragraph direction — cloud continuation, qualified bounded increment

The remaining karaoke work requires an explicit paragraph-direction contract.
Caption styles now store Auto/LTR/RTL at track/cue scope. Auto is omitted from
serialization so old files retain their data and raster/cache identity. New
explicit directions insert zero-width strong marks only in the shaping buffer,
including each paragraph, without changing stored words or timing. Empty and
trailing paragraphs do not acquire marker-only text. The same shaping contract
feeds glyph rendering, background bounds and active-word underline geometry.

A real geometry test caught explicit RTL being additionally right-aligned inside
the wrap-width box, moving a centered caption outside its target. Explicit
paragraphs now use a left-aligned layout box before applying the existing
measured-block anchor; bidi visual order is independent of that anchoring.
Auto's historic layout behavior is retained. Latin-first Auto and explicit LTR
pixels compare exactly; mixed Latin/Hebrew explicit RTL changes their visual
order while retaining the authored center. Arabic/Hebrew/Latin shaping fixtures
check paragraph direction and absence of missing visible glyphs on this host.

The native caption inspector and MCP style arguments expose direction. Word
scope cannot author a paragraph direction; older Auto word-style records inherit
the enclosing paragraph. Conflicting explicit word directions fail visibly.
SRT/VTT/ASS export reports when this style cannot be represented rather than
claiming direction round-trip fidelity. This does not add automatic language
assignment, complete multilingual interchange or true FillSweep yet.

Final qualification passes 2,924 library tests / 3 ignored (core 886, GUI 549,
MCP 331, render 230, video 928), all-target Clippy and the native app build.
All 73 native graph, 49 timeline, 3 bridge, 1 unchanged Legacy golden and 6
scaling integration tests pass, alongside the focused Legacy title GPU test.
Ten explicit-RTL plain/decorated animation workflows pass real preview and
18-frame ProRes export/redecode, history/save and manifest checks. Native-window
LTR/RTL Apply and Undo preserve document identity and update visual order.
Long cue summaries and selectors are now bounded so delete and Apply remain
reachable in the narrow sidebar. The program renderer displays the tested
Hebrew correctly, but the egui cue-list font shows missing-glyph boxes; UI font
coverage/bidi qualification remains open. Automatic language direction and
true FillSweep are still unfinished.

### Native spatial caption FillSweep — cloud continuation, qualified bounded increment

Managed SDR captions now resolve a real left-to-right glyph-color split rather
than Legacy's whole-word color interpolation. Active/inactive colors and their
alpha, word index and fraction are compiled and hashed. Before start a word is
inactive; after end it stays active. A partial word's opaque geometry mask is
colorized once in display-linear Rec.709, so the two colors cannot double its
coverage or create a dark overlap seam. One mask is reused across words. Wrapped
words consume progress across shaped line advances; line bands include glyph
overhang, and processing-scale changes re-shape the geometry. Bounds, duplicate
word indexes and nonfinite/out-of-range fractions fail visibly.

Explicit RTL paragraphs and embedded RTL words use direction-agnostic WordPop.
The existing Unicode bidi dependency supplies actual levels rather than guessed
script ranges. There is one stable diagnostic per affected cue in compiled
frames, MCP frame results and the optional render manifest; the caption sidebar
shows one cue notice. Auto remains backward-compatible outside this new managed
paint. Legacy rendering is unchanged. MCP accepts fill_sweep with active/inactive
colors, mutually exclusive with other highlight/clear options; 521 tools remain.

Qualification includes 126 independent binary64 mask/color/alpha GPU cases and
60 actual-glyph, wrapped-word, group-opacity, Full/Draft/cache evaluations,
compiler boundaries, hashes, FadeWords/SlideUp/Typewriter behavior and RTL
substitution. Thirty-six plain/decorated, LTR/RTL animation workflows pass
author/undo/redo/save, real preview and 18-frame ProRes export/redecode/manifest
checks, including FillSweep combined with all three caption animations.

In the native cloud window, Full frame 3 shows the red/blue split within a word;
frame 12 retains completed words in the active color. Explicit RTL Apply makes
one history edit, displays the cue notice and uses WordPop timing; Undo restores
the original picture, Auto control and clears the notice. The saved document
exactly matches its baseline apart from normal history/format metadata.

Final libraries: 2,925 passed / 3 ignored (core 886, GUI 549, MCP 331, render 230,
video 929). All 76 native graph, 49 timeline, 3 bridge/egui, 1 unchanged Legacy
golden and 6 scaling integration tests pass. Native app build and all-target
Clippy pass with warnings. Full CPU text rendering, general per-word metrics,
multilingual UI font/bidi coverage, nested captions, broader managed stages and
external professional acceptance remain open. Source is uncommitted/unpushed;
remote branch still equals 30f221c at 13:57 UTC.

### Native channel routing and typed mask ports — cloud continuation

ChannelSplit now routes all four named outputs independently in managed clip and
project graphs. ChannelCombine consumes four scalar signals, with unwired RGB
defaulting to zero and alpha to one. RGB components are unassociated scene AP1;
Image-to-Mask uses AP1 luminance, while alpha is selected explicitly through the
a output. Scalar signals have their own IR encoding and cannot accidentally
enter grading, presentation or export as color. Only demanded Split ports are
emitted, including non-primary ports reached through an animated Switch.

Mask-to-Image now produces opaque grayscale, as required by the port contract.
This corrects the earlier managed graph shape behavior: wiring a rectangle,
ellipse or polygon directly into Output no longer presents white with varying
alpha. To use a shape as transparency, wire it to ChannelCombine's alpha input.
The direct internal shape kernels keep their tested coverage representation;
explicit typed boundary operations perform the conversions. Existing Legacy
rendering and its golden corpus are unchanged.

Signed and above-white component/luminance values survive within the finite
FP16 signal range. Storage clamps to ±65504, so extreme unassociated HDR can
clip even when the premultiplied input fits; no float32-intermediate claim is
made. Alpha is clamped to 0..1 only when combining a premultiplied image. Zero
alpha has zero unassociated RGB. Bad arity, mixed domains, raw-signal display,
out-of-range ports, duplicate inputs and ignored channel parameters are refused.

Qualification: 120 independent component/luminance CPU references with three
GPU size/cache passes, 24 RGBA round trips, independent spatial channel patterns,
alpha clamping and extreme storage checks. Six actual MCP author/undo/redo/save,
preview and three-frame ProRes workflows cover clip/project reconstruction,
AP1 red/blue swapping, shape-as-alpha and image-luminance-as-alpha. Polygon
preview/export references now validate opaque grayscale with independent
triangle weights. Native Full program/alpha inspection distinguishes shaped
transparency from fully opaque grayscale; both documents remain byte-identical.
The node-editor inset is still a placeholder and is not used as render evidence.

Final libraries: 2,928 passed / 3 ignored (core 886, GUI 549, MCP 332, render 230,
video 931). All 80 native graph, 49 timeline, 3 bridge/egui, 1 unchanged Legacy
golden and 6 scaling checks pass. Native app build and all-target Clippy pass
with warnings. The remaining roadmap and external qualification are unfinished.

### Native independent MediaIn — cloud continuation

Managed clip and project graphs can now decode independent file-backed stills
and progressive video through the asset's explicit technical interpretation.
Host clip input overrides, trim and speed are deliberately not inherited. Local
time in a clip is sequence time minus host start; project-local time uses the
sequence clock, matching the existing catalog contract. Negative video time,
overflow, offline/missing assets, absent interpretation, unsupported decode
formats and interlaced input fail visibly. Referenced asset creative stacks are
not silently substituted for this graph source.

This qualification is bounded to source dimensions matching the authored canvas.
The renderer checks actual decoded dimensions before Draft normalization, not
only saved probe metadata. Stills check their original decoded dimensions rather
than resized cache entries. A stale probe after relink therefore cannot silently
stretch a changed video into the expected canvas. Broader variable-size graph
geometry remains unfinished. Dedicated source IR hashes include interpretation,
source time and expected dimensions; Legacy rendering is unchanged.

Six retained MCP workflows cover still clip/project sources and all video
clip/project Local/Sequence combinations. They import and probe real PNG/FFV1
fixtures, author and undo/redo graph edits, save/reload, compare three preview
frames and six-frame ProRes exports with independent color/alpha references,
then reject missing interpretation and changed-size relinks and recover through
Undo. Clip-composition locked-track protection is checked; project-only graphs
retain their existing independent editability. GPU tests exercise Full/Draft
resampling, source changes after cache population, restoration, logical texture
sizes and strict managed domains. Production source errors propagate visibly;
the legacy non-Result CPU reference provider still represents unavailable frames
as blank and is not claimed as the production error contract.

Final current-tree libraries pass: 2,930 / 3 ignored (core 886, GUI 549, MCP 333,
render 230, video 932). All 81 native graph, 49 timeline, 3 bridge/egui, 1 unchanged
Legacy golden and 6 scaling checks pass. All-target Clippy and the native app
build pass with existing warnings. Full native program checks show video source
frames advancing independently from the host and opaque video alpha; the still
checker retains its two alpha levels and transparent border. Both viewed files
remain byte-identical and both apps are closed.

The first parallel aggregate run overlapped a build and timed out 15 MCP jobs
under cloud memory pressure. After stopping the overlapping build, serial
libraries passed, then the final source tree passed a second full serial run.
No timeouts were weakened. This qualifies the bounded feature, not broader
performance or the complete roadmap. Source remains local and unpushed.

### Native TimeOffset and exact offset editing — cloud continuation

Native composition lowering now instantiates upstream nodes by node identity,
clock and output port. Constant offsets re-evaluate upstream animation, Switch
selection, channels and sources at `tick - offset`; equal results deduplicate by
content hash. Checked subtraction rejects overflow. Cycles and deep graphs are
refused, with a hard 8,192-instance expansion budget; more than four distinct
offsets emits a warning rather than silently dropping a branch.

ClipIn retiming honors source trim and constant speed, leaving the host's outer
effects and grades on their original timeline clock. Independent MediaIn keeps
its own Local/Sequence rule. Retimed nested sources import scene-linear output
without duplicating display transforms. An unwired project filter re-evaluates
the pre-project program at the shifted clock, without recursively reapplying
the project graph or importing outer captions. This uses a compile-stage flag,
not a cloned project per offset. Missing TimeOffset input is transparent.
Negative video/program source times and moving ClipIn with keyframed speed maps
remain explicitly refused. Static stills, solids and static text can reuse the
same result. General temporal performance qualification remains unfinished.

A typed structural offset edit preserves node identity, params, edges and layout
through serialized undo/redo. Exact repeats do nothing; locked clip graphs and
stale GUI drafts refuse. The native inspector accepts exact signed integer ticks
with a seconds readout, Apply and Revert; pending drafts reset after authoritative
changes/Undo. MCP exposes `set_graph_time_offset` using `offset_ticks`.

Real offset exports caught a pre-existing cold-seek defect. At 30 fps, a
container's 33 ms keyframe timestamp was floored to ordinal zero; decoding
therefore labeled picture one as zero and returned the following picture. CFR
keyframe origins now use nearest-boundary ordinals. Ordinary playhead floor
semantics and VFR's actual PTS table are unchanged. The failing export evidence
is retained. The correction passes independent quantization cases at nine rates
and 24 real, out-of-order cold seeks at 30 and 29.97 fps, within all 11 existing
plus new decode/media checks. This does not qualify every unusual timestamp
origin, damaged container or variable-rate source.

Focused checks cover shifted Switch/alpha ports, constant-result dedup, clock
limits, missing inputs, cycles, expansion limits, trim/speed, nested scene
boundaries and pre-project stage hashes. Six real offset MediaIn import/probe,
author/undo/redo/save, preview and ProRes workflows pass independent frame/color
references plus bad-input/relink recovery and lock/no-op refusal. Aggregate and
native-window acceptance for this increment is being completed separately.

Final libraries: 2,938 pass / 3 ignored (core 886, GUI 551, MCP 334, render 230,
video 937). All 81 native graph, 50 timeline, 3 bridge/egui, 11 decode/media,
1 unchanged Legacy golden and 6 scaling checks pass. All-target Clippy and the
native app build pass with warnings; the generated MCP reference lists 522 tools.
The integration exhaustiveness guard was extended for the new typed command,
with a new exact-integer serialization/undo/locked-graph test. A redundant
unsigned-denominator comparison flagged by Clippy was corrected without changing
its behavior. No lint or test gate was weakened.

Native Full frame 3 showed the offset picture. Typing a zero draft created no
history edit; Apply created exactly one edit and changed the program picture.
Undo restored the original negative offset, field draft and picture. Native Save
matches the baseline document apart from normal history/format metadata; app
closed. The node inset remains a placeholder and was not used as render proof.
Multi-offset sustained playback and broader temporal performance remain open;
this increment does not assert complete roadmap acceptance. Source is unpushed.

### Independent native decoder clocks — cloud continuation

Four widely separated offsets exposed an actual source-contention failure:
repeated requests for one asset reset a shared decode ring, so paused preview
could fail to finish within its existing 30-second limit. Native evaluation now
plans the demanded asset/time pairs before decoding. Equal clocks share a lane;
separate clocks use stable sorted per-asset lanes and independent decoder rings.
Planning stays inside the existing eight-live-source budget and reports capacity
limits instead of starting an unbounded number of workers. Asset invalidation
clears all affected lanes. Legacy keeps its original zero lane.

Only output ancestors and requested scope-tap ancestors are demanded. An unused
source no longer consumes a lane or starts a decoder; a requested inspector tap
still participates. Color-domain validation is unchanged. Planning, dedup,
capacity refusal and per-asset invalidation have focused tests, plus a real GPU
check of pruning versus scope-tap preservation.

Four actual clip/project Local/Sequence authoring, preview and six-frame ProRes
flows now verify a mean of four source clocks separated by 30 frames. Tests
retain independent decoded-source color references, exact history/save behavior,
input/geometry refusal and relink/Undo recovery. The larger async test setup was
split and heap-backed after a stack overflow, without enlarging stack limits or
weakening render deadlines.

A real three-second 32×16 playback run sampled 89 distinct pictures with zero new
drops or evaluation misses. Largest pixel error was 0.0007812; observed gaps
peaked at 60.92 ms. Decoded rings used 147,456 bytes and the GPU cache peaked at
11,665,408 bytes, within their budgets; document and history stayed unchanged.
The test's initial mismatch came from FFmpeg transforming untagged raw input
while adding output color tags. Explicit matching input/output tags now prove
the encoded YUV codes are byte-identical to the independent fixture before any
render comparison. This remains a bounded synthetic check, not high-resolution
throughput, long-soak or external-hardware qualification. Full gates are pending.

Final libraries pass 2,941 / 3 ignored (core 886, GUI 551, MCP 335, render 230,
video 939). All 82 native graph, 50 timeline, 3 bridge/egui, 11 decode/media,
1 temporal playback, 1 unchanged Legacy golden and 6 scaling checks pass.
All-target Clippy and native build pass with warnings. Three additional serial
playback runs each observed 90 distinct pictures in three seconds, with zero
new drops/misses and maximum observed gaps below 51.75 ms; pixel error stayed
below 0.000782. These do not replace representative high-resolution/long-soak
qualification. No timing or color tolerance was relaxed.

The native Full monitor reports four of four sources ready; Play advances the
picture, Pause holds it, and seeking back restores the original picture. The
document remains byte-identical and the app is closed. The broad roadmap is
still unfinished, and all source changes remain local and unpushed.

### Native speed-ramp and fractional-clock qualification — 2026-10-07

Moving ClipIn time offsets now support validated held, linear and bounded Bézier
speed maps, including reverse segments when the resulting source time is valid.
Constant and held segments use checked integer arithmetic; eased integration
retains the core binary64 rounding contract and refuses unsafe precision ranges.
Validation precedes ordinary source-time evaluation: malformed extreme key times
previously caused an integer-overflow panic before the offset guard. Invalid
rates, duplicate/out-of-duration keys, invalid handles, excessive key counts and
out-of-range source times now fail visibly. Demanded native video clocks also
receive an eight-source compile-time capacity check before export.

Fractional cold seeks exposed a real readiness stall: the at-or-after decoder
started at the next frame while the ring awaited the frame covering the request.
Workers now explicitly seek the covering frame; the public at-or-after API is
unchanged. VFR readiness uses actual adjacent PTS intervals rather than a nominal
CFR duration. The previously failing four half-frame-offset scene now plays 90
pictures over three seconds with zero drops/misses, maximum error 0.0007811 and
maximum observed gap 61.06 ms. This remains a tiny synthetic qualification.

Four real MCP cases cover held, linear, nonlinear cubic Bézier and reversing
speed ramps with trim, clip start and half-frame offset. Independent analytic
integrals select source pictures; actual previews, nine-frame ProRes exports,
redecode, manifests, undo/redo, save and locks pass. The nonlinear case's preview
frames 3/5/8 correspond to source frames 8/10/18. The native Full monitor displays
three distinct pictures at those times with one source ready; the document is
byte-identical after inspection and the app is closed.

Final gates: 2,945 library tests / 3 ignored (886 core, 551 GUI, 336 MCP, 230 render,
942 video); 82 native graph, 50 timeline, 3 bridge, 11 decode/media, 2 temporal
playback, 1 unchanged Legacy golden and 6 scaling checks pass. All-target Clippy
and native build pass with warnings. MCP remains 522 tools; next IR tag is 74.
No tolerances or deadlines were relaxed. High-resolution/long-soak, unusual media,
external hardware and the broader roadmap remain unfinished. Source is local
and unpushed; no PC work or coding delegation occurred.

### Offline matte worker foundation — 2026-10-07

The subject-mask prerequisite now has a pinned offline model loader and a bounded
serial background worker. The native graph node remains gated pending preview,
export and color-domain integration. See `native-matte-worker.md` for the exact
contract, primary model sources and remaining integration acceptance.

A preprocessing defect was corrected: RGB normalization used the maximum RGBA
byte, so opacity changed RGB model input. Input dimensions/length are now checked
before model access, and model output shape/finite values are checked before
normalization. The offline loader verifies the pinned SHA-256, caps file size and
loads those verified bytes. Both inference thread counts are one.

The worker has two-request admission, one serial inference thread, eight-entry /
16 MiB mask cache limits, four-megapixel input bounds, explicit pending/busy/error
states, deferred off-caller preparation and nonblocking polling. Tests cover
queue saturation, dedup, LRU/byte eviction, invalid geometry, preparation errors
and worker disconnection. Nine unit tests pass; two explicitly provisioned real
model tests pass (direct repeat and actual worker/cache). Subject center255 and
corner0 were observed; direct repeat masks are exactly equal. These synthetic
checks do not establish footage quality or real-time throughput.

Current library qualification totals 2,954 passes / 5 ignored: core 886, GUI551,
matte9, MCP336, render 230, video942. The aggregate command lost its execution
handle before a terminal receipt for video; the same built 942-test video suite
was rerun completely and passed. This is combined evidence, not a claimed
successful exit from the interrupted aggregate invocation. Native integration
suites retain their previous v66 results; the video implementation is unchanged
by this foundation. All-target Clippy and native build passed with warnings.
No native MaskFromMatte completion, commit, push or PC work is claimed.

### Bounded native subject masks — 2026-10-07

NativeManaged MaskFromMatte is now qualified for matching-canvas inputs up to
four megapixels, with at most eight distinct masks / 16 MiB of simultaneous
mask results. It uses the pinned offline U²-Net-p model and NativeMatteExtract
IR tag 74; next tag is 75. GPU readback and inference stay on one serial worker
per evaluator. No render-time download is attempted.

The input bridge checks geometry/finite values, unpremultiplies AP1, converts to
bounded 8-bit sRGB and gives zero-alpha pixels deterministic black RGB. Output
is raw subject weight, separate from source coverage, matching the existing
remove_background contract. Mask→Image is opaque grayscale. An initial
unpublished alpha-folding candidate was removed to avoid double coverage when
applying masks to images that already carry alpha. HDR/out-of-gamut input clips
for inference; this is not an HDR model-quality qualification.

Pending previews are visibly provisional. Exact inspection and export wait for
finished weights or fail; missing-model tests publish neither output nor manifest.
Completed results change cache identity so pending opaque masks cannot become
final cached pictures. Render receipts identify the model hash when actually used.

A real relink regression exposed stale masks when an asset ID stayed the same.
Invalidation now discards old in-flight results without adding another worker,
clears cached results and retries failed model loads in the next generation.
Media identity also partitions GPU cache entries without a vector document.
Actual clip/project MCP tests verify changed-picture relink and Undo recovery,
opaque/partial-alpha raw weights, history/save/locks, preview and three-frame
ProRes/redecode with model receipts. Compiler capacity and typed-port guards,
quality/cache GPU checks and worker-generation tests pass.

Playback originally forced 457 evaluations in three seconds. Pending inference
now preserves normal Play cadence and polls paused completion at 16 ms. The final
96×96 run observed 90 ticks, zero drops, 89 provisional frames, 89 evaluations,
maximum gap 45.81 ms and 35.8 MB peak GPU cache. Pause/latest-scrub resolved exactly.
This explicitly does not claim real-time segmentation. Native Full inspection
shows a readable provisional label on light footage, Pause resolving at the same
frame, and seek-back restoring the original mask. Static mask-image alpha is
opaque. Documents remain byte-identical and the app is closed.

Final library gate: 2,965 passed / 8 ignored (core 886, GUI 553, matte 11, MCP 336,
render 230, video 949). All 82 native graph, 50 timeline, 3 bridge, 11 decode/media,
2 temporal playback, 1 unchanged Legacy golden and 6 scaling checks pass. The
explicit pinned-model, missing-model and subject-playback gates pass separately.
All-target Clippy and native build pass with warnings. No tolerances or render
deadlines were relaxed; ProRes opaque alpha correctly allows its 12-bit 4095 to
16-bit 65520 expansion. Broader footage, high-resolution/long-soak, arbitrary
geometry, shared scopes, HDR and external hardware remain open. Legacy's old
placeholder is not claimed as qualified inference. Source remains local/unpushed.

### Native source-end recovery and VFR picture preservation — 2026-10-07

Independent native video requests now report a failure tied to the requested
source tick when decoding proves it cannot supply that picture. Preview displays
an error frame instead of silently holding the previous picture. Exact MCP
inspection and export reject it; a failed export publishes neither output nor
manifest. Seeking back to valid media recovers without editing, relinking or
poisoning the asset's input interpretation. Source errors are visible alongside
other frame-computation errors, rather than being mislabeled as subject-mask
failures.

The decoder remembers a proven stream end independently of container/audio
duration, and latches a definitive failed request so it cannot respawn FFmpeg
on every worker poll. Advancing farther beyond a known end also avoids repeated
seeks. Normal seek recovery and bounded worker shutdown are preserved. The
presenter's existing final-frame convention remains explicit: without a next
PTS, the last picture covers one nominal frame interval. This does not claim
exact packet-duration coverage for every VFR container or qualify broad damaged
media recovery.

The new VFR last-frame regression exposed an older picture/PTS mismatch:
FFmpeg's default rawvideo sync returned 194 pictures for the committed
216-picture VFR fixture. The raw decoder now explicitly preserves frames and
the source time base (`-fps_mode passthrough -enc_time_base -1`). The corrected
pipe returns all 216 pictures without duplicate-DTS warnings. Eight cold-seek
payload comparisons against an independent sequential decode now verify actual
pictures, including both sides of the rate change and the final frame. Earlier
VFR timestamp-only assertions were insufficient and are superseded by this
payload evidence. See the [FFmpeg sync documentation](https://ffmpeg.org/ffmpeg.html)
and the retained `vfr-decode-proof.json` for the measured fixture and commands.

Actual checks include a two-picture video muxed with longer audio; valid last
fractional ticks; cold and advancing out-of-range requests without decoder
respawn; valid-seek recovery; VFR final-interval/end/recovery; exact MCP error
responses without image payloads; failed-export cleanup and unchanged document
history. Final workspace libraries pass 2,965 / 9 ignored; the explicit
source-end MCP gate and pinned-model subject playback pass separately. All 82 native graph regressions pass on the final tree. Existing
CFR/VFR decode, timeline, bridge, Legacy golden, scaling and temporal-playback
regressions pass. The rebuilt native Full preview showed frames 0/1, a visible error at frame 2,
the detailed source-limit diagnostic in Color, and recovery to frame 0 with the
error cleared. The document remained byte-identical. All-target Clippy and the
native application build pass with existing warnings. Broader roadmap and variable-resolution graph geometry remain open.


### Explicit mixed-resolution MediaIn — 2026-10-07

File-backed native still/video sources can now retain their coded-pixel extent
until an explicitly authored canvas-sized Resize. Contain, Cover and Stretch
preserve premultiplied scene-linear AP1 and alpha. TimeOffset and selected Switch
aliases may route the source before Resize; other processing and direct output
remain refused until that explicit conversion to the sequence canvas. This does
not silently fit media or qualify arbitrary variable-extent filter chains.

The private native source-extent flag participates in cache identity. Full/Draft/
Full cache reuse and original-resolution source scope taps are checked. Sources
are bounded to 8192 pixels per dimension; mixed-resolution source instances share
a 16,777,216 padded-source-texel limit. The runtime counts actual IR instances,
including malformed duplicate hashes, before GPU evaluation. These are resource
bounds, not a real-time or broad-hardware throughput claim. General geometry,
pixel-aspect-aware conform, broader formats/proxies and high-resolution soaks
remain open.

The original compiler regression failed on an explicit 32×8-to-16×16 Resize.
It now passes all three fits, direct-output refusal, pre-resize filter refusal,
wrong resize-target rejection, and TimeOffset/Switch routing. GPU/CPU spatial
patterns exercise both source types at Full/Draft/Full, fractional alpha,
poisoned bucket padding, natural-resolution scope readback and cache reuse.
Boundary tests cover padded allocation accounting, repeated hashes, zero and
hostile dimensions without allocating oversized textures.

Eighteen actual MCP workflows cover still/video, clip/project graphs, three fit
modes, authoring history, save/reload, preview PNG, six-frame ProRes delivery and
redecode, input-interpretation refusal, stale-video-probe refusal, re-probed
canvas-sized replacement acceptance and Undo recovery. The reference resamples
premultiplied AP1 before the output transform. Their evidence metadata separates
refusal from accepted, explicitly resized replacements. Native Full UI confirms
Contain letterboxing/alpha, Cover's center crop and Stretch's aspect change; all
three inspected documents remain byte-identical.

Workspace libraries: 2,967 passed / 9 ignored (core 886, GUI 553, matte 11, MCP 337,
render 230, video 950). All 84 native graph tests, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, 1 Legacy golden and 6 scaling tests pass.
The report-only metadata correction was followed by a fresh retained eighteen-
case MCP run. Explicit pinned-model MCP, missing-model refusal and three-second
subject playback also pass; playback reports 90 distinct ticks, 0 new drops,
89 provisional ticks/89 evaluations and a 51.055 ms maximum gap. All-target Clippy
and the rebuilt native application pass with existing warnings. The broader
roadmap remains incomplete.


### Local and linked shared-look output inspection — 2026-10-07

Exact image/matte output inspection now accepts an explicit stage address for
the selected clip: its ordinary grade, its independent local look, or its
currently linked shared look ID. Missing/null MCP stage defaults to the ordinary
clip grade. Reused graph node IDs cannot select another stage. Shared addresses
must match the current link; detached, wrong-ID, missing, bypassed, unreachable
and inactive outputs fail rather than falling back to the program.

The look input includes the preceding asset/group-pre/clip pipeline and excludes
group-post, track/master and output conversion. Raw matte weights remain
unassociated; image inspection uses the qualified SDR display transform.
Existing ordinary clip taps/metadata remain compatible. Read-only GUI selectors
stay available on locked tracks and identify the actual linked look rather than
an arbitrary item selected in the shared library. Target/cache identities include
the stage; Exit drains an outstanding worker before another can start. Stage
labels have a contrasting background and sit away from the readiness badge.

Strict JSON tests exposed Serde's unit-variant acceptance of ignored fields;
explicit empty-struct decoding now enforces the declared stage address schema.
Compiler tests verify colliding IDs and ordering. Actual MCP tests verify local
and shared image values, inverted shared raw mattes, locked-document/history
purity, Make Independent/Undo recovery, wrong links and bypass. Initial fixture
errors (omitted time and reading an image-bearing result as text) were corrected;
the retained final focused run passes.

Native Full UI confirms opposite clip/shared mattes at the same node ID, local
matte isolation, shared image output and Exit-to-program. Five source/QA documents
remain byte-identical after inspection. The app is closed. Workspace library
qualification passes 2,971 / 9 ignored (887 core, 554 GUI, 11 matte, 338 MCP,
230 render, 951 video); final label-only changes also pass all 288 app tests.
All 84 native graph, 50 timeline, 3 bridge, 11 decode, 2 source-end, 2 temporal,
1 Legacy and 6 scaling integration tests pass. Clippy and the rebuilt native
app pass with existing warnings; generated MCP documentation still has 522 tools.
The final aggregate after the label adjustment also passes all 2,971 library
tests / 9 ignored at 01:09 UTC on 2026-10-08. Remote branch remains identical to
base 30f221c, rechecked at 01:05 UTC.

This closes only selected-clip look output inspection. Group/compound scope
inspection, look-stage qualifier-input picking, broader finishing, HDR and the
remaining roadmap are still open. No source commit or push is made.


### Selected look correction inputs — 2026-10-07

An additional regression reproduced an ordinary-clip sampler failure when its
qualifier operator was copied into a separate look: stage-less metadata treated
the copied ID as an ambiguous corrector. Ordinary input aliases now collect only
the ordinary grade, and separate immutable input records carry clip, stage,
operator and optional node. Automatic input lookup means the unique image
corrector; key-only utilities remain independently addressable by node and do not
silently replace or hide it.

`sample_grade_input` and `inspect_qualifier` accept strict local/linked-shared
stage addresses under Native Managed color, preserving the default clip and
Legacy behavior. Readbacks reject changed history or a replaced document even
when its new history reuses the same revision. Qualifier inspection includes
source-alpha coverage, unlike raw graph matte outputs. Read-only native menus
also expose correction-input SDR images and qualifier-key coverage, with typed,
stage-aware targets and the single pending readback worker.

The focused copied-ID/order/typed-target regressions and four actual MCP cases
(local/shared × stack/graph) pass. They exercise independently computed ACEScct
stage gains, exact versus automatic inputs, copied IDs, source alpha0.5,
inverted windows, key-source utilities, wrong links, Make Independent/Undo,
bypass, locked-document/history purity and in-flight document replacement.
Early fixture errors (bridge initialized before direct document assignment and
mistaking a qualifier-key utility for a mixed output) were corrected; their logs
are retained. The initial current-tree aggregate passed 2,974 /9 ignored and all
rendering integrations, Clippy and native build passed. Review then extended the
held-transport interruption case to the older output tools: image inspection
incorrectly returned a result after document identity changed at the same
revision. Image and raw-matte output handlers now capture/check document identity
before and after presentation; the expanded focused regression passes. The final
aggregate after this extra guard passes all 2,974 libraries /9 ignored at 01:53
UTC Oct8. The final 84 native graph, 50 timeline, 3 bridge, 11 decode, 2 source-end,
2 temporal playback, 1 Legacy golden and 6 scaling integration tests pass. Final
Clippy and the native build pass with existing warnings; generated docs retain
522 tools. Native Full UI confirms distinct clip/shared correction inputs,
alpha-aware shared key coverage versus unassociated raw matte, independent local
stack input/key inspection and Exit-to-program. Five source/QA documents remain
byte-identical; the application is closed. The unnamed generated-source shot
card is selectable but lacks a descriptive fallback label; this small UI polish
item is recorded for the following authoring pass.

This is read-only input/key inspection, not shared/local eyedropper authoring.
That authoring, group/compound scopes and the rest of the roadmap remain open.

### Selected look authoring — 2026-10-07

Independent local looks now have an explicit Color corrector editor, with the
same checked stage setter used by curve/qualifier sampled edits. The setter
rechecks a shared clip's exact current link and honors every linked locked
track. Shared creation/update and independent edits now reuse ordinary-grade
parameter/graph validation; previously the shared helper checked only graph
structure. MCP shared_look adds update_independent for an already detached look.

The focused tests pass local/shared stage selection, copied operator identity,
lock and invalid-parameter rejection, independent coalescing/Undo/Redo and MCP
negative cases. Eight actual GPU eyedropper cases cover local/shared × ordered
stack/graph × curve/qualifier, with independently calculated ACEScct/AP1 sample
coordinates and unchanged ordinary grade. The final aggregate passes all 2,978 libraries /9 existing ignored. Final rendering integrations, Clippy and formatting pass. The native build and regenerated 522-tool documentation pass. Full native interface checks now pass for local and shared look editing, curve/qualifier sampling, key coverage and Undo.

Readback cancellation drains one pending eyedropper worker
on scope/input changes, Escape and new picking intent. Samples capture document
identity and selection in addition to revision/tick/native frame identity.
Unnamed shot cards receive a fallback label; input previews identify the actual
operator ordinal/kind as well as stage and optional node.

Native Full verification completed at 03:09 UTC Oct8. The independent look's
exposure changed without changing the ordinary grade; its curve and qualifier
samples changed only that look. A shared exposure drag produced one undo step,
and both shared samples changed only the linked look library entry. Save through
the native File menu and Undo restored both documents exactly after excluding
the intended serialized history/format envelope. Escape canceled an armed local
sampler without another authored edit. Both alpha-aware key views show gray
coverage outside the inverted window and black in the center, with exact stage
and operator labels. The unnamed local shot card now has a visible short-ID
fallback. Verification used native history controls; synthetic text entry hit
an unavailable AT-SPI provider and recovered with native keyboard events. No
keyboard-shortcut reliability claim is inferred from that transport behavior.
Both applications are closed; source remains local/uncommitted/unpushed.

Remaining work is still listed in the authoritative table: group/compound
inspection, broader pipeline/geometry and tracking, assistance/finishing, HDR
and external hardware acceptance, and full end-to-end release qualification.

### Selected-member group and compound inspection — 2026-10-07 (qualified below)

Strict pre/post group addresses now resolve only against the selected member's
validated ancestry. Compiler metadata carries member plus stage while retaining
the true GroupPre/GroupPost diagnostic owner. Group samples do not enter legacy
ordinary-clip aliases; repeated graph/operator IDs across six stages stay exact.
Ambiguous repeated compiled outputs fail closed instead of selecting the first.
Read-only group menus and native group eyedroppers use the checked stage path.
The scope picker now lists the immediate group before ancestors and shows IDs;
previous root-first iteration mislabeled a root as the immediate group.

Focused compiler ordering, ancestry/strict-JSON tests, eight actual native group
eyedropper cases, and actual MCP solid/compound cases pass. Each MCP case uses
two differently colored active members plus a non-active member for valid group
membership edits. Six addressed stages cover ancestor/leaf pre, outer clip,
local look and leaf/ancestor post. Independent gains, exact/automatic inputs,
scene statistics, raw versus source-alpha mattes, wrong groups, read-only locks,
a different locked member, detach and Undo pass. The first GUI test fixture had
a forbidden singleton Normal group; its fixture was corrected, rather than
weakening the model invariant. Final aggregate/native-window results follow. The prior saved/delivered checkpoint was v73.

### Selected-member group and compound qualification — 2026-10-08

The current source passes 2,982 library tests (9 ignored), 84 native-graph,
50 timeline, 3 bridge, 11 decode, 2 source-end, 2 temporal-playback, 1 Legacy
golden and 6 scaling integration tests. All-target Clippy, native build,
formatting and 522-tool generated documentation pass. Eight actual native GPU
group eyedropper cases and solid/compound MCP workflows are included.

Native Full read-only inspection distinguishes differently colored members
within the same group and preserves locked fixture bytes. Image, raw matte and
source-alpha key previews and Exit pass. Native group-pre qualifier and curve
picking create only the addressed group edits; File Save, two history steps,
Undo and Save-and-quit restore the document except its intended history envelope.
Initial GUI interactions were unreliable; authoring was repeated with temporary
warning-only event diagnostics, then those diagnostics were removed byte-for-byte
and the clean binary rebuilt. This is not evidence of a keyboard/transport fix.

Group grades are inspected per selected member. Compound source clips are
qualified in their owning sequence; inner grades are inspected from their own
sequence. This does not establish arbitrary cross-sequence context traversal or
look/group keyframe authoring. Source remains local, uncommitted and unpushed.
The wider roadmap and external acceptance requirements remain open.

### Look and group animation authoring — 2026-10-08 (qualification in progress)

An explicit grade_stage_op animation target carries selected clip, strict grade
stage and corrector ID. Existing grade_op payloads and history commands remain
unchanged. Local/shared look and group pre/post scalar and primitive-window
lanes now use the common checked set/remove/interpolation/copy/paste operations.
The native floating and docked keyframe editors expose exact stage rows, base
values and animated values; shared/group locks affect the shared scope, while
independent local looks remain local. Each linked/member clip evaluates shared
keys in its own clip-relative clock, matching the existing render contract.
This is not sequence-global animation.

Focused core and native editor tests pass. Eight actual MCP stack/graph cases
exercise local/shared/group-pre/group-post keys, atomic invalid batches, copy,
paste, remove, history, 48 rendered frame measurements, 24 moving-window raw
matte frames and eight six-frame ProRes exports with high-bit numerical decode
checks. The first export fixture used a directory outside its test app's write
roots and was correctly rejected; retained export evidence now uses the existing
permitted qualification directory without changing the path policy. Keyframe
clipboard offset arithmetic now rejects overflow instead of overflowing.
Full aggregate and native-window interaction qualification are pending.

### Look/group animation final qualification — 2026-10-08

Local and linked shared looks plus group pre/post scalar and primitive-window
animation now have exact stage targets in native keyframe editors and all six
MCP keyframe tools. Eight stack/graph workflows pass analytical time/value
checks, source-alpha preservation, moving raw window mattes, save/history/lock
checks, and six-frame ProRes decode checks. Shared lanes use each member's local
clock; local independent lanes do not change another member. Clipboard offsets
reject arithmetic overflow.

Native edits covered four stages, local add/drag/easing, six history steps,
File Save and exact Undo restoration excluding the intended history envelope.
Initial local/group native edits used Draft; shared edits/save used Full.
Native testing found the floating editor hidden in Color workspace because it
was called only from the timeline dock. It now renders in the common Video path
and is covered by an egui rendered-text regression and actual Full native Color
workspace verification. Shared/group locked scopes remain readable with
mutating controls disabled; the locked fixture stayed byte-identical.

Current library qualification is 2,986 passing /9 ignored: core 890, GUI 561,
MCP 341, render 230, video 953, matte 11. The full workspace run passed 2,985 before
the final GUI-only follow-up; all 561 GUI tests were rerun on the final source.
84 native graph, 50 timeline, 3 bridge, 11 decode, 2 source-end, 2 temporal playback,
1 Legacy golden and 6 scaling tests pass. Final all-target Clippy, native build,
formatting and 522-tool generated documentation pass. The interrupted first
aggregate log is retained but is not counted as success. Remote was identical
to 30f221c at 05:00 UTC. No commit/push; the wider roadmap remains incomplete.

### Native directional motion blur qualification in progress — 2026-10-08

The catalog blur.motion now has a distinct scene-linear AP1 implementation.
It is a spatial line convolution, not motion vectors or temporal synthesis.
Finite angle 0–360 degrees and distance 0–500 authored pixels resolve to a
centered displacement vector. Premultiplied RGBA uses bilinear samples clamped
to logical canvas edges, with no RGB clipping or transfer conversion. Up to 129
uniform taps cover the full footprint; zero distance is identity. Draft scales
the two displacement axes independently, padding stays zero, and cache hashes
include the vector. Legacy remains on its existing implementation.

270 independent binary64 CPU/GPU reference cases pass, together with Draft/cache,
invalid domain/arity/vector and four-owner compiler checks. Four actual MCP
workflows pass 12 Full previews and four three-frame ProRes exports, high-bit
spatial color/alpha checks, angle/distance animation, Save, Undo/redo and invalid
parameter refusal. A genuine existing lock bypass was found in scoped static
effect mutations: add/remove/reorder/set now reject locked clip/track/shared
asset users, zones inherit the guard and reads remain allowed. Core and MCP
lock/history tests pass. Native interaction and final aggregate are pending;
this increment is not yet a release checkpoint or full-roadmap completion.

Native interaction follow-up: the first UI pass exposed legacy-only registry
enumeration hiding catalog parameters and animation lanes. Both editors now use
the existing manifest projection; reset defaults already came from manifests.
Two GUI regressions verify rendered angle/distance controls, unchanged passive
state and named animation lanes. Full native edits verified angle 37→90,
distance 40→0 identity, a distance key at 1 second, File Save and exact five-step
Undo restoration. An incidental reframe drag was separately undone and verified.
Only requested effect data changed; screenshots and JSON receipts are retained
under native-motion-blur. The final serial aggregate is running.

### Directional blur final checkpoint — 2026-10-08

All 2,991 library tests pass /9 ignored (core 891, GUI 563, MCP 342, render 230,
video 954, matte 11). After removing an unused import only, all 563 GUI tests were
rerun on final source. 87 native-graph tests, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, unchanged Legacy golden and both three-test
scaling suites pass. All-target Clippy passes with the existing scoped
result_large_err waiver and existing warnings; native build, touched-file
formatting, diff whitespace checks and generated 522-tool docs pass.
Four native screenshots, exact document/save/Undo checks and all four ProRes
receipts are retained. This qualifies the bounded spatial directional effect
and catalog GUI/lock fixes, not temporal motion-vector blur or universal
real-time throughput. Remote remains identical to 30f221c at 06:18 UTC. No
commit/push; the broader roadmap remains incomplete.

### Native mosaic qualification in progress — 2026-10-08

Catalog stylize.mosaic now uses origin-anchored rectangular average cells on
premultiplied scene-linear AP1 RGBA. Authored block size must be finite in 1–256
pixels and is rounded to the nearest integer. Draft scales the two integer
cell dimensions independently, rounding with a one-pixel minimum. Partial edge
cells exclude outside/padded pixels, block 1 is identity, signed/extended RGB is
not clipped, and physical padding stays zero. A single AP1 input and cell
bounds are validated; dimensions participate in content hashes.

The CPU reduces each cell once in binary64. The GPU reduces to a small cell
texture and expands it to the logical canvas, rather than repeatedly summing
all cell pixels for every output pixel. This is a bounded implementation design,
not a universal real-time or hardware-throughput claim. Legacy remains unchanged.

60 independent binary64 summed-area-reference CPU/GPU cases pass across four
canvases (through 257×259), three alpha values and five square/rectangular cell
sizes (through 256×256), plus Draft/cache, bad cell/domain/arity and four-owner
compiler/rounding/hash/keyframe checks. Four actual MCP workflows pass 12 Full
previews and four three-frame ProRes exports with high-bit spatial color/alpha
checks, animated size, Save, Undo/redo, invalid parameter refusal and locks.
Native Full editing changed block 16→32, authored a block key at 1 second, saved
only the requested data and restored the exact document via three Undo steps.
Screenshots and JSON receipts are retained. Final aggregate gates are running;
Library 77 remains the last saved checkpoint until they complete.

### Native mosaic final checkpoint — 2026-10-08

Current aggregate: 2,993 library tests pass /9 ignored (core 891, GUI 563,
MCP 343, render 230, video 955, matte 11). All 90 native graph tests, 50 timeline,
3 bridge, 11 decode, 2 source-end, 2 temporal playback, unchanged Legacy golden
and 6 scaling tests pass. All-target Clippy with the existing scoped waiver,
native build, touched-file formatting, diff whitespace and 522-tool docs pass.
Native Full static and animated mosaic edits, intended-only saved data and exact
three-step Undo restoration are verified. Numerical/export receipts and three
native screenshots are retained. No commit/push; wider roadmap remains open.

The native blur/mosaic typing pass also exposed an existing acceptance gap:
entering multi-digit numeric values can create separate history entries for
accepted prefixes. The multi-step Undo counts above describe actual behavior;
they do not establish one-gesture/one-Undo for typed numbers. The next fix is
bounded numeric text commit with Escape/invalid-input cancellation and target
isolation, preserving continuous mouse scrubbing and safe Save ordering.

### Numeric text-commit acceptance fix in progress — 2026-10-08

The shared numeric-expression helper now keeps typed numbers/expressions as
uncommitted text until Enter or focus loss. Escape cancels, invalid/out-of-range
text is refused, and continuous mouse dragging/reset behavior remains live.
Clip inspector and Color controls are scoped by document and selection;
existing correctors additionally use owner/stage/op IDs, while uncreated draft
correctors use a stable kind identity rather than a new random ID every frame.

Four egui event regressions and all 567 GUI tests pass. Native Full checks verify
one-step typed transform, effect and CDL edits, Escape, invalid text, focus-loss
commit to the old selected clip only, File Save of a pending effect value,
intended-only serialized changes and exact Undo restoration. Seven screenshots
and JSON receipts are retained. Final aggregate gates are running. This fixes
the helper-backed fields; a wider audit of raw DragValue/Slider controls remains
separate, so no universal app-wide typing claim is made. No commit/push.

### Helper-backed numeric commit final checkpoint — 2026-10-08

All 2,997 library tests pass /9 ignored (core 891, GUI 567, MCP 343, render 230,
video 955, matte 11). A test-only unused-return cleanup was followed by all
567 GUI tests, build, formatting and all-target Clippy passing again. The full
90 native graph, 50 timeline, 3 bridge, 11 decode, 2 source-end, 2 temporal,
unchanged Legacy golden and 6 scaling regressions pass; 522-tool docs and diff
whitespace checks pass. Native Full transform/effect/CDL typing, one-step Undo,
Escape, invalid input, target-safe focus loss and pending-value File Save are
verified. Only intended parameters changed; final fixture is restored exactly
apart from its history envelope. Wider raw numeric controls and the remaining
professional roadmap are still open. No commit/push.

### Raw slider and numeric controls acceptance — 2026-10-08

The Color page and Clip Inspector now route 37 raw Slider sites through a
proposed-value adapter, and 18 raw DragValue sites defer text updates until
commit. Quick-grade and graph-node widget identities include their authored
owner/stage/node. The two native-input form drafts retain their existing explicit
Apply staging. The adapter preserves ordinary egui slider rendering, pointer
scrubbing and keyboard handle nudges. Typed drafts commit on Enter/focus loss,
Escape cancels, invalid suffixes cannot commit a previously valid prefix, and
existing slider range clamping is preserved. External authored changes or a
disabled scope invalidate pending drafts. No egui fork is introduced.

Five event regressions and all 572 GUI tests pass, followed by a successful
native build. Native Full-quality checks verify exposure 1.2 commits once,
Escape/invalid text adds no edit, pointer drag has one Undo, and pending exposure
1.5 commits to the old clip when selecting the next clip. The new clip remains
unchanged. Clip Inspector rotation 0.25 radians commits once; the draft does not
rotate the preview. Saved JSON contains only intended values, and the final
authored document exactly equals its baseline after Undo. Six native captures
and JSON receipts are retained. Full aggregate qualification is running;
Library 79 remains the last saved checkpoint until it completes. Numeric controls
outside these two panels and the wider professional roadmap remain open.

### Raw Color/Clip Inspector numeric final checkpoint — 2026-10-08

All 3,002 library tests pass /9 ignored (core 891, GUI 572, MCP 343, render 230,
video 955, matte 11). All 90 native graph, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, unchanged Legacy golden and 6 scaling tests
pass. All-target Clippy with the existing scoped waiver, native build,
touched-file formatting, diff whitespace and generated 522-tool docs pass.
Native Full exposure/rotation typing and pointer dragging each undo in one
step; Escape and invalid suffixes add no edit. Next-clip focus loss commits the
old clip only. Serialized values and exact final authored-document restoration
are verified. Six native captures and JSON receipts are retained locally; five
selected captures are included in the chat-sized checkpoint. Controls
outside Color/Clip Inspector and the broader roadmap remain open. No commit/push.

### Native managed box blur acceptance — 2026-10-08

The catalog's existing `blur.box` radius schema now compiles to a typed native
AP1 operator at asset/clip/track/master scopes. Finite radius 0..500 is rounded
to the nearest authored pixel; Draft scales/rounds axes independently. Equal
weights convolve premultiplied signed/HDR RGBA with logical edge clamping and
transparent allocation padding. Radius zero is identity. Legacy's existing
bridge, which forces a minimum radius of one, is unchanged.

CPU uses binary64 sliding-window accumulation. GPU uses two separable bounded
passes, at most 1001 samples per axis, with a working-format intermediate. This
is not a universal real-time claim. Domain/arity/radius validation and content
hashes are explicit. 108 independent binary64 analytic-coverage CPU/GPU cases
pass across six canvases including thin/odd shapes, three alpha values and six
radius pairs through 500. Draft/cache/padding/refusal tests and four-scope
compiler/rounding/keyframe/hash tests pass. Four actual MCP flows pass 12 Full
previews, four three-frame high-bit ProRes round trips against independent direct
binary64 sums, Save, Undo/redo, keyframes, invalid-value refusal and locks.

Native Full controls changed radius 8→24→0 with one history edit each; zero
shows the unblurred picture. The named Radius lane authored a key at 1 second,
49.38271641731262 pixels. File Save changed only intended effect parameters;
three Undo steps restored the exact authored document. Four captures and JSON
receipts are retained. Native build passed; full aggregate gates are running.
Library 80 remains the last saved checkpoint. General effects and broader
professional acceptance are still incomplete. No commit/push.

### Native box blur final checkpoint — 2026-10-08

All 3,004 library tests pass /9 ignored (core 891, GUI 572, MCP 344, render 230,
video 956, matte 11), plus all 93 native graph, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, unchanged Legacy golden and 6 scaling tests.
All-target Clippy with the existing scoped waiver, native build, touched-file
formatting, diff whitespace and 522-tool docs pass. Box blur numerical/domain,
Draft/cache, actual preview/ProRes/history and native static/keyframe controls
are qualified within the preceding boundaries. Four native captures, JSON and
export receipts are retained. Remaining catalog effects and the broader
professional roadmap stay open. No commit/push.

### Native fixed-canvas geometric filters — 2026-10-08

Catalog Pinch, Spherize and Ripple now lower to a typed native AP1 inverse warp
at asset/clip/track/master scopes. Pinch uses a smooth-bell radial displacement;
Spherize uses the documented asin/sin radial mapping; Ripple uses a sinusoidal
row/column displacement. Amount zero is exact identity. Native coordinates use
symmetric pixel centers and an inscribed authored-space circle. Per-axis Draft
scale preserves that authored geometry, including anisotropic evaluation.
Ripple reduces phase to one cycle before sine to avoid large-argument f32 loss.
Bilinear interpolation operates on premultiplied signed/HDR RGBA, clamps logical
source edges and leaves allocation padding transparent. No color transfer or
range clipping is introduced. Existing Legacy behavior is unchanged.

Finite catalog bounds, typed domain/arity guards and content hashes cover all
parameters. 300 independent binary64 radial/wave CPU/GPU cases pass across four
canvases (including 1×1 and 257×3), three alpha values, both signs/endpoints and
three Ripple wavelengths. Eighteen Draft/cache evaluations, padding and invalid
parameter/domain/arity checks also pass. Compiler tests cover all three filters
at four scopes, keyframes, disabled invalid effects and hash separation.
Twelve actual MCP workflows pass 36 Full previews and twelve three-frame ProRes
round trips against independent binary64 spatial references, Save, Undo/redo,
keyframes, invalid values and lock refusal. Native UI and full aggregate
qualification remain pending. These are fixed-canvas artistic filters; general
variable geometry/perspective and wider professional acceptance remain open.

Native Full follow-through: Pinch 0.85, Spherize −0.65, Ripple amplitude 12 and
wavelength 32 each produced the expected visible deformation with one history
entry. The named Wavelength lane authored a key at 1 second with value
83.01234248280525. An incidental timeline marker was separately undone before
continuing. File Save changed only the three clips' intended effect parameters;
five final Undo steps restored the exact authored document. Five captures and
JSON receipts are retained, and the app is closed. Full aggregate gates are now
running; Library 81 remains the last saved checkpoint.

### Native geometric-filter final checkpoint — 2026-10-08

All 3,006 library tests pass /9 ignored (core 891, GUI 572, MCP 345, render 230,
video 957, matte 11), plus all 96 native graph, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, unchanged Legacy golden and 6 scaling tests.
All-target Clippy with the existing scoped waiver, native build, touched-file
formatting, diff whitespace and generated 522-tool docs pass. Pinch/Spherize/
Ripple numerical, Draft/cache/domain, twelve actual preview/ProRes/history
workflows and native UI/Save/Undo are qualified within the preceding limits.
Five native captures and receipts are retained locally. Other catalog effects,
general geometry and broader professional acceptance remain open. No commit/push.

### Native spatial detail filters — 2026-10-08

Catalog Smart Sharpen, High Pass and Clarity now lower to typed native AP1
operators. Raster Unsharp uses the already qualified Native Unsharp path with
its existing catalog schema. The new detail operators blur premultiplied RGBA,
normalize the blurred color by its coverage, combine in straight AP1, then
restore source alpha. Signed/HDR RGB is not clipped. High Pass uses the explicit
128/255 neutral bias; Smart Sharpen normalizes the 0..255 threshold to channel
units and ramps its edge gate over 32/255; Clarity uses an AP1-luminance midtone
bell and an authored min-dimension/8 Gaussian sigma clamped to 3..60 pixels.
Draft follows the qualified Gaussian minimum-axis scaling policy. Amount zero
and subpixel Gaussian identities preserve source for Smart Sharpen/Clarity;
High Pass at radius zero produces its neutral gray at original coverage.
Legacy rendering remains unchanged.

All three native CPU/GPU tests pass: 168 independent binary64 detail/coverage
cases plus twelve Draft/cache evaluations, transparent padding and invalid
parameter/domain/arity checks. Compiler tests cover all four catalog entries
at four owner scopes, keys, hash identity, disabled invalid effects and bounds.
Sixteen actual MCP workflows pass 48 Full previews and sixteen three-frame
high-bit ProRes round trips against independent Gaussian/detail references,
plus Save, Undo/redo, animated parameters, invalid input and locked-track refusal.
Native UI/build and full aggregate acceptance remain pending; Library 82 is
last saved. General detail/restoration, all catalog effects and the broader
professional roadmap remain incomplete; no real-time/parity claim is made.

Native Full verification also passes: Smart Sharpen amount 2/threshold 25.5,
threshold key at 1 second (40.92592433094978), High Pass radius 8 after observing
its zero-radius neutral output, Clarity 0.8 and Raster Unsharp amount 2. Six
edits produced six history entries. File Save changed only intended effect
parameters; six Undo steps restored the exact authored document. Six captures
and JSON receipts are retained. App closed; full aggregate gates are running.

### Native spatial-detail final checkpoint — 2026-10-08

All 3,008 library tests pass /9 ignored (core 891, GUI 572, MCP 346, render 230,
video 958, matte 11), plus all 99 native graph, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, unchanged Legacy golden and 6 scaling tests.
All-target Clippy with the existing scoped waiver, native build, touched-file
formatting, diff whitespace and generated 522-tool docs pass. Smart Sharpen,
High Pass, Clarity and Raster Unsharp numerical, Draft/cache/domain, sixteen
actual preview/export/history workflows and native UI/Save/Undo are qualified
within the preceding boundaries. Six native captures and receipts are retained
locally. Remaining catalog effects and the broader roadmap stay open. No commit/push.

### Native spatial stylization — 2026-10-08

Emboss, Find Edges and Chromatic Aberration now have typed native AP1 paths.
Emboss applies the catalog's sum-zero directional luminance stencil with a
128/255 bias; Find Edges uses AP1-luminance Sobel magnitude. Both preserve source
coverage and keep signed/HDR output without the Legacy u8 clipping. Their
parameterless schemas are retained; no artificial animation knobs are added.
Chromatic Aberration radially shifts red/blue sampling in opposite directions,
retains green/source alpha, and normalizes bilinear samples by their coverage.
Amount zero is identity. All three use authored-pixel coordinates, per-axis
Draft scaling, logical edge clamps and transparent allocation padding. Existing
Legacy behavior remains unchanged; this is not a calibrated physical lens model.

198 independent binary64 CPU/GPU spatial cases and fifteen Draft/cache cases
pass, including odd/thin canvases, zero/partial/full alpha, signed/HDR channels,
chromatic signs/endpoints, padding and invalid domain/arity/parameter guards.
Compiler tests cover all three entries at four owner scopes, chromatic keys and
hash separation. Twelve actual MCP workflows pass 36 Full previews and twelve
three-frame high-bit ProRes round trips against independent spatial references,
Save/Undo/redo, locked effect removal and chromatic bounds/animation. Native UI
and full aggregate qualification are pending; Library 83 remains last saved.

Native Full verification: Emboss and Find Edges enable/render correctly with one
history edit each. Chromatic Aberration amount 12 and its key at 1 second
(19.753089904785156) each add one edit. File Save changes only the two enabled
flags and chromatic parameters; four Undo steps restore the exact authored
document. A discovered long effect-header overflow was reproduced by an egui
regression (254px content in a 124px viewport), then fixed with a wrapped header.
Four narrow drawer widths and a real wrapped Remove/Undo pointer test pass.
The rebuilt native app keeps the full chromatic label, actions and controls
visible while editing. Five post-fix captures plus the pre-fix failure are kept.
All 574 GUI tests and the native build pass. App closed; full aggregate is
running, so Library 83 remains the latest saved checkpoint.

### Native spatial-stylization final checkpoint — 2026-10-08

All 3,012 library tests pass /9 ignored (core 891, GUI 574, MCP 347, render 230,
video 959, matte 11), plus all 102 native graph, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, unchanged Legacy golden and 6 scaling tests.
All-target Clippy with the existing scoped waiver, native build, touched-file
formatting, diff whitespace and generated 522-tool docs pass. Emboss, Find Edges
and Chromatic Aberration numerical, Draft/cache/domain, twelve real preview/
export/history workflows and native UI/Save/Undo are qualified within the
preceding limits. The narrow inspector header regression is fixed and tested.
General effects, geometry, real-footage/high-resolution performance and the
broader professional roadmap remain open. No commit or push.

### Native fixed-canvas projective perspective — 2026-10-08

Catalog Perspective now lowers to a true inverse homography over normalized
pixel centers. A binary64 pivoted solve maps destination TL/TR/BR/BL corners
back to the source unit square. Finite corners in -1..2 must form a convex,
nondegenerate quadrilateral; singular or numerically unsafe inverses fail
visibly rather than silently falling back to identity. Deliberate reflections
are allowed. Source UV outside [0,1) is transparent; interior sampling is
bilinear premultiplied AP1 with logical-edge clamps. Signed/HDR RGB and alpha
are retained. Identity is exact, normalized coordinates preserve Draft geometry,
and allocation padding stays transparent. The Legacy raster/GPU paths are
unchanged. This is fixed-canvas projective geometry, not variable output bounds.

144 independent binary64 CPU/GPU cases use an analytic forward homography and
adjugate inverse instead of duplicating the production solver, covering eight
quadrilaterals, six canvases and three coverages. Forty Draft/cache evaluations
plus invalid matrix/corner/domain/arity and padding checks pass. Compiler tests
cover four owner scopes, all eight animated corner parameters and full matrix
hashing. Four actual MCP workflows pass twelve Full previews and four three-frame
high-bit ProRes round trips with independent per-pixel spatial/alpha checks,
Save/Undo/redo, all eight clip corner lanes, invalid values and locked removal.
Native app verification and the full aggregate remain pending; Library 84 is
last saved. General geometry and the broader professional roadmap remain open.

Native Full follow-through passes: top corners (0.2,0.12) and (0.8,0.06)
produce a true projective result with four history edits. A separate invalid
tl_x=2 edit blanks the preview and displays the exact convex/safe-inverse
diagnostic in Color; one Undo recovers. Tr X key at 1 second is
0.703703761100769. Save changes only intended effect parameters; five final Undo
steps restore the exact authored document. Four captures and JSON receipts
are retained. App closed; full aggregate running, Library 84 remains last saved.

### Native projective-perspective final checkpoint — 2026-10-08

All 3,014 library tests pass /9 ignored (core 891, GUI 574, MCP 348, render 230,
video 960, matte 11), plus all 105 native graph, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, unchanged Legacy golden and 6 scaling tests.
All-target Clippy with the existing scoped waiver, native build, touched-file
formatting, diff whitespace and generated 522-tool docs pass. Fixed-canvas
Perspective independent numerical references, Draft/cache/domain, four real
preview/export/history workflows and native corner editing/diagnostic/keyframes/
Save/Undo are qualified. Four native captures and receipts are retained.
General variable geometry and the wider professional roadmap remain open.
Source is local, uncommitted and unpushed.

### Native fixed-canvas Drop Shadow — 2026-10-08

Catalog Drop Shadow now has a typed native AP1 operator. It shifts source alpha
with continuous bilinear authored-pixel offsets and clamped logical source
edges, applies an equal-weight separable box blur with the rounded catalog
radius, then composites the original over the tinted shadow. Encoded sRGB color
controls are decoded/adapted once to AP1. Signed/HDR source channels are never
clipped or round-tripped through an 8-bit raster. Opacity zero is exact identity.
Draft scales offsets and blur radii per axis. The existing box kernel caps
extreme-upscale evaluation radii at 500 pixels; the authored catalog maximum is
64. Allocation padding is transparent and the canvas does not expand. Existing
Legacy CPU and GPU shadow behavior is unchanged, including their prior differing
blur approximations; this qualification applies to the new native path.

162 independent binary64 direct-shift/convolution/composite CPU/GPU cases pass
across six canvases, three coverages and nine offset/radius/opacity combinations,
including fractional offsets, both signs and parameter endpoints. Five Draft/
cache evaluations, kernel-bound, padding and invalid parameter/domain/arity
checks pass. Compiler tests cover four scopes, all seven animated parameters,
color conversion and content hashing. Four actual MCP workflows pass twelve
Full previews and four three-frame high-bit ProRes round trips against independent
per-pixel spatial/alpha references, all seven clip animation lanes, Save/Undo/
redo, invalid values and locked-track removal refusal. Native UI and current-tree
full aggregate remain pending. Library 85 is the last saved checkpoint; other
catalog effects and the wider professional roadmap remain open.

Native Full follow-through passes: opacity0.8, offsets(16,8) and radius6
produce the expected colored soft shadow with four discrete history edits.
Effect Opacity key at1second is0.654321014881134 and adds one edit. File Save
changes only intended effect parameters; five Undo actions restore the exact
authored zero-opacity baseline. Three captures and JSON receipts are retained.
App closed; full aggregate is running, Library85 remains last saved.

### Native Drop Shadow final checkpoint — 2026-10-08

All 3,016 library tests pass /9 ignored (core 891, GUI 574, MCP 349, render 230,
video 961, matte 11), plus all 108 native graph, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, unchanged Legacy golden and 6 scaling tests.
All-target Clippy with the existing scoped waiver, native build, touched-file
formatting, diff whitespace and generated 522-tool docs pass. Native Drop Shadow
independent numerical references, Draft/cache/domain, four actual preview/export/
history workflows and native static/opacity-animation/Save/Undo are qualified.
Three native captures and receipts are retained. Other catalog effects and the
broader professional roadmap remain open. Source stays local and uncommitted.

### Native bounded transfer-space Posterize and Threshold — 2026-10-08

These two catalog effects now use an explicit native transfer-space path:
unpremultiply AP1, adapt to linear sRGB/D65, bound the definitionally bounded
creative input, encode sRGB, apply Posterize or Threshold, decode/adapt back
to AP1 and restore source coverage. There is no intermediate u8 raster and no
ACES output transform inside the effect. Posterize rounds its 2..255 level count
and uses explicit floor(x*steps+0.5); Threshold uses encoded Rec601 luminance
and >= threshold. Exact neutral/white endpoints avoid matrix/transfer roundoff
changing the binary white endpoint. Transparent input stays transparent.
Their deliberate boundedness does not authorize clipping in Levels, curves or
other transfer-space kernels; those remain gated until separately qualified.
Legacy paths are unchanged.

432 independent binary64 CPU/GPU cases pass using independently derived pinned
sRGB/AP1 Bradford matrices. GPU comparisons use the actual binary16 input tap,
not unrepresentable ideal values. Ten Draft/cache evaluations and two adjacent
binary16 gray values straddling encoded0.5 verify the binary threshold decision;
parameter/domain/arity and padding checks pass. Compiler tests cover both effects
at four owner scopes, keys, rounding, bounds and hashes. Eight real MCP workflows
pass 24 Full previews and eight three-frame high-bit ProRes round trips with
per-pixel independent color/alpha checks, Save/Undo/redo, animation and locks.
An initial test fixture attempted to treat setting a default as a mutation;
it was corrected to use distinct authored values, without changing production
history behavior. Native app and aggregate checks remain pending. Library86 is
last saved; the wider professional roadmap remains open.

Native Full follow-through passes: Posterize levels 8 and Threshold level 0.35
each produce one history edit and the expected output. Their named lanes author
keys at 1 second with values 20.74074088037014 and 0.654321014881134. Four total
edits are saved with only the intended effect parameters changed; four Undo
actions restore the exact authored baseline. Five captures and JSON receipts
are retained. App closed; full aggregate running, Library 86 remains last saved.

### Native Posterize/Threshold final checkpoint — 2026-10-08

All 3,018 library tests pass /9 ignored (core 891, GUI 574, MCP 350, render 230,
video 962, matte 11), plus all 111 native graph, 50 timeline, 3 bridge, 11 decode,
2 source-end, 2 temporal playback, unchanged Legacy golden and 6 scaling tests.
All-target Clippy with the existing scoped waiver, native build, touched-file
formatting, diff whitespace and generated 522-tool docs pass. Native bounded
transfer-space Posterize and Threshold numerical/reference, represented-input
threshold neighbors, Draft/cache/domain, eight real preview/export/history
workflows and native static/keyframe/Save/Undo are qualified. Five native captures
and receipts are retained. Other transfer-space kernels, broader catalog effects
and the professional roadmap remain open. No commit or push.

### Native Desaturate and shared pointwise kernels — 2026-10-08

The parameterless catalog Desaturate uses HSL lightness `(max+min)/2` in signed
encoded sRGB, with AP1 unpremultiply/adaptation and original coverage restored.
Unlike Posterize/Threshold, its transfer input is not clipped; negative/HDR
floating values survive. Exact neutral inputs remain exact and transparent
pixels remain canonical zero. The type discriminator and strict parameter-zero,
arity/domain guards distinguish it from the bounded artistic operators.

Pure Desaturate, Posterize and Threshold CPU math is now shared with the raster
implementation, preserving raster arithmetic and its existing u8 boundary.
The native CPU path calls those helpers without a u8 bridge. Signed GPU transfer
extensions and the shared Rec601 decision are independently qualified. This does
not substitute the different native-primary algorithm for catalog Vibrance.

The shared-kernel regression and 192 independent extended-range/alpha/canvas
CPU/GPU cases, five Draft/cache checks and malformed-domain/arity/parameter tests
pass. Prior Posterize/Threshold references and threshold neighbors still pass.
Four owner-scope compiler checks and four real MCP workflows pass 12 Full previews,
four three-frame ProRes files with per-pixel independent color/alpha checks,
enable/disable/Undo/redo/Save and lock refusal. Native UI and the current-tree
aggregate remain pending; Library87 remains the saved baseline. No commit/push.

Native Full UI passes: enabling Desaturate creates one history edit and the
expected grayscale output. Save changes only that enabled flag; one Undo restores
the exact authored baseline and color. Persisted history is checked separately
from authored content. Three captures and JSON receipts are retained. App closed;
current-tree aggregate follows. Library87 remains last saved.

### Native Desaturate final checkpoint — 2026-10-08

All 3,021 library tests pass /9 ignored (core892, GUI574, MCP351, render230,
video963, matte11), plus all114 native graph,50timeline,3bridge,11decode,
2source-end,2temporal playback,unchanged Legacy golden and6scaling tests.
All-target Clippy with existing scoped waiver,native build,touched formatting,
diff whitespace and generated522-tool docs pass. Shared pointwise raster math,
extended signed Desaturate, four-scope preview/export/history and native
Full enable/Save/Undo are qualified. The earlier bounded artistic effects
remain covered by the full regression suite after shared-helper extraction.
The broader effect catalogue and professional roadmap remain open. No commit/push.

### Native Channel Mixer and Black & White — 2026-10-08

A typed native transfer-space matrix path now serves the actual Channel Mixer
and Black & White manifests. It unpremultiplies AP1, adapts to linear sRGB/D65,
signed-encodes, evaluates a shared float matrix, signed-decodes/adapts to AP1 and
restores coverage. It never uses the u8 raster bridge or clips native input/output.
The exact identity matrix preserves source values; transparent/padded pixels stay
zero. Channel Mixer retains its nine -2..2 authored coefficients; Black & White
retains normalized 0..1 weights and the existing all-zero equal-third fallback.
No future constant controls were added outside the manifest.

The raster caller and native CPU now share the pure channel dot-product and weight
normalization helpers. The raster output clamp and byte quantization remain;
manifest comments document the deliberate extended-range native behavior.
324 independent binary64 CPU/GPU signed/HDR/alpha/canvas cases pass, with actual
binary16 input taps, five Draft/cache passes and domain/arity/coefficient guards.
Owner/history/export and native UI/full aggregate qualification remain pending.
Library88 is the saved baseline; no commit or push; broader roadmap remains open.

Four owner-scope compilation, every matrix/weight scalar key lane, hash changes
and invalid-range/disabled-bypass checks pass. Eight actual MCP workflows pass
24 Full previews, eight three-frame ProRes exports with independent per-pixel
color/alpha references, typed edits, clip animation, Save/Undo/redo and lock
refusal. Native editor and full aggregate remain pending.

Native Full UI passes: Channel Mixer Rg0.5 and Black & White Wr1.0 each
produce one static history edit. Named lanes author 1-second keys with values
0.6172840595245361 and0.6604938507080078. Four edits save with only intended
effect parameters/keys changed; four Undo actions restore the exact authored
baseline. History is persisted and checked separately. Five captures and JSON
receipts retained. App closed; full current-tree qualification follows.

### Native transfer matrix final checkpoint — 2026-10-08

All 3,024 library tests pass /9 ignored (core893, GUI574, MCP352, render230,
video964, matte11), plus all117 native graph,50timeline,3bridge,11decode,
2source-end,2temporal playback,unchanged Legacy golden and6scaling tests.
All-target Clippy with existing scoped waiver,native build,touched formatting,
diff whitespace and generated522-tool docs pass. Shared float channel math,
extended Channel Mixer and normalized Black & White, eight real owner workflows
and native static/keyframe/Save/Undo are qualified. The remaining catalogue and
professional roadmap are still open. No commit/push.

### Native Photo Filter — 2026-10-08

The actual five-float Photo Filter manifest now has a typed native path, sharing
its pointwise tint and optional encoded Rec601 luminosity-preservation math with
the raster editor. The raster-only post-rescale clamp remains exactly there;
native signed/HDR transfer values are not clipped. AP1 unpremultiply/adaptation,
signed sRGB encode/decode and source coverage surround the creative function.
Density zero or white tint is exact identity, transparent input/padding is zero.
The preserve control retains the Legacy f32 >0.5 switch and tinted-luma >1e-4
rescale guard; it is not silently reinterpreted as a continuous blend amount.

432 independent binary64 CPU/GPU signed/HDR/alpha/canvas cases, five Draft/cache
checks and domain/arity/parameter guards pass. Two adjacent actual binary16
neutral inputs straddling the encoded-luminosity guard verify its distinct
branches at an absolute1e-7 output tolerance. Native owner/history/export and
editor/full aggregate checks remain pending. Library89 is the saved baseline;
no commit/push and the broader roadmap remains incomplete.

Shared raster ordering/clamp regression passes. Four owner scopes, all five
scalar key paths, exact preserve-switch neighbors, hashes and invalid-range/
disabled-bypass checks pass. Four actual MCP owner workflows pass twelve Full
previews and four three-frame ProRes exports with independent color/alpha
references, density animation, preserve switch, Save/Undo/redo and locked
removal refusal. Native UI and the complete current-tree aggregate remain pending.

Native Full UI passes: density0.75 with preservation on/off shows the expected
brightness difference; each static edit adds one history entry. The named density
lane authors a2-second key at0.654321014881134. Three intended edits save without
unrelated authored changes; three Undo actions restore the exact baseline, with
persisted history checked separately. Four captures and JSON receipts retained.
App closed; current-tree aggregate follows. Library89 remains last saved.

### Native Photo Filter final checkpoint — 2026-10-08

All 3,027 library tests pass /9 ignored (core894, GUI574, MCP353, render230,
video965, matte11), plus all121 native graph,50timeline,3bridge,11decode,
2source-end,2temporal playback,unchanged Legacy golden and6scaling tests.
All-target Clippy with existing scoped waiver,native build,touched formatting,
diff whitespace and generated522-tool docs pass. Shared tint/luma math,
extended Photo Filter, preserve-switch and actual-half denominator neighbors,
four owner preview/export/history paths and native density/preserve/key/Save/Undo
are qualified. Remaining catalogue and professional roadmap stay open. No commit/push.

### Native Color Balance — 2026-10-08

The ten actual Color Balance manifest controls now use typed native signed
transfer-space evaluation. Original encoded Rec601 source luma drives the shared
bounded shadow/midtone/highlight masks; half-strength weighted channel shifts
optionally restore encoded luma under the existing strictly-positive1e-4 guard.
Only the Legacy raster caller retains RGB clamps before/after rescale. Native
AP1 adaptation, signed transfer and source coverage preserve extended values;
zero shifts are exact identity, transparent input/padding are canonical zero.
The preserve parameter retains its f32>0.5 switch. No u8 bridge or primary-grade
operator with different operand math is substituted.

432 independent binary64 CPU/GPU signed/HDR/alpha/canvas cases, five Draft/cache
and strict domain/arity/all-shift bounds tests pass. Shared raster, compiler/
MCP, native editor and aggregate checks remain pending. Library90 is last saved;
no commit or push. Remaining catalogue and professional roadmap stay open.

Shared raster ordering/clamp, four compiler owner scopes, all ten scalar lanes,
preserve-switch neighbors, hashes and invalid-range/disabled-bypass checks pass.
Four actual MCP workflows pass twelve Full previews and four three-frame ProRes
files with independent color/alpha references, animated midtone shift, preserve
switch, Save/Undo/redo and lock refusal. Two adjacent actual-half neutral inputs
straddling the luminosity guard pass at3e-5 absolute tolerance and verify the
distinct bypass/rescale branches. Native UI/full aggregate remain pending.

Native Full UI passes: shadow blue0.2, midtone red0.4, highlight green-0.3 and
preserve0 each create one history edit. Midtone R authors a2-second key at
0.30864202976226807. Five intended edits save without unrelated authored changes;
five Undo actions restore the exact baseline and persisted history is verified
separately. Four captures/JSON receipts retained. App closed; aggregate follows.
Remote branch rechecked2026-10-08 13:32UTC remains identical to30f221c; local
continuation is uncommitted/unpushed. Library90 remains last saved.

### Native Color Balance final checkpoint — 2026-10-08

All 3,030 library tests pass /9 ignored (core895, GUI574, MCP354, render230,
video966, matte11), plus all125 native graph,50timeline,3bridge,11decode,
2source-end,2temporal playback,unchanged Legacy golden and6scaling tests.
All-target Clippy with existing scoped waiver,native build,touched formatting,
diff whitespace and generated522-tool docs pass. Shared tonal shift/luma math,
extended Color Balance, actual-half luminosity-guard neighbors, all ten keys,
four owner preview/export/history paths and native three-range shifts/preserve/
key/Save/fiveUndo are qualified. Remaining catalogue/professional roadmap stay
open. No commit/push; remote branch remained30f221c at13:32UTC.

### Native toolbar and cloud recovery checkpoint — 2026-10-08

The cloud executor reset during the initial toolbar integration run. A fresh
clone at30f221c and all123 changed source-file hashes were restored from the
verified Color Balance checkpoint. The toolbar fix was reconstructed and
qualified again; interrupted-run results are historical, not acceptance for
this recovered source tree.

Current fresh-tree aggregate:3,033 library tests pass /9 ignored (core895,
GUI577,MCP354,render230,video966,matte11), plus125 native graph,50timeline,
3bridge,11decode,2source-end,2temporal playback,Legacy goldens and6scaling tests.
All-target Clippy with the existing scoped waiver,native build,touched formatting,
whitespace and generated522-tool docs pass. Three egui regressions verify
bounded long-title/status layout, reachable menu/Escape dismissal and one tooltip.
Native checks at1178px and1024px verify title/status and Zoom/Quit visibility;
Save preserves authored content exactly and leaves history empty. Neither Quit
menu action was selected. Tiny parent menu/search layouts remain outside this
bounded change. No commit/push. The complete professional roadmap remains open.

### Native catalog Curves increment — 2026-10-08 (qualification in progress)

The actual color.curves manifest now lowers to a distinct managed AP1 operation,
separate from the existing ACEScct grading curves. Five unit x/y knots and the
legacy contrast convenience override retain their catalog ordering and duplicate
policy. Raster and native share floating Fritsch–Carlson tangent preparation and
Hermite sampling. Legacy keeps its constant endpoints and256-entry byte LUT;
native uses signed encoded-sRGB via adapted AP1, with endpoint-tangent extension
and no0..1 clamp. Extreme legal slopes saturate only at finite half-storage
limits +/-65504. Identity, transparent zero and source coverage are preserved.

Focused evidence: shared Legacy LUT byte parity;432 independent f64 CPU/GPU
combinations, five Draft/cache sizes, actual-half knot neighbors and finite
storage extremes; typed domain/arity/all-scalar guards; all eleven key lanes and
four effect owners; four real MCP authoring/history/lock/save workflows with
12Full previews and four three-frame ProRes pixel-verified exports. The initial
compile borrow mismatch was corrected; the subsequent focused checks pass.
Native editor interactions and full current-tree aggregate remain pending at
this entry. No full-roadmap or publication claim.

### Native catalog Curves final checkpoint — 2026-10-08

Current aggregate passes3,036 library tests /9ignored (core896,GUI577,MCP355,
render230,video967,matte11),129 native graph,50timeline,3bridge,11decode,
2source-end,2temporal playback,Legacy golden and6scaling tests. All-target
Clippy with existing scoped waiver,native build,touched formatting,whitespace
and522-tool generated docs pass. Native p1y=.1,p3y=.9,contrast=.4 and contrast
key at2seconds=.27160489559173584 persist as exactly four intended edits;
four Undo actions restore the entire authored document exactly. Shared Legacy
LUT parity,432 independent CPU/GPU cases,five Draft/cache sizes,four actual-half
and storage-limit checks,all11 key lanes/four owners and four MCP ProRes exports
are qualified. The previous in-progress entry is superseded for this increment.
Other effects and the professional roadmap remain open. No commit or push.

### Native catalog Levels increment — 2026-10-08 (qualification in progress)

The five catalog Levels controls now lower to a managed AP1 operation with
signed transfer-space evaluation. Raster and native share the normalization and
gamma kernel. Legacy keeps its input/output RGB clamps and byte quantization.
Native applies gamma to the bounded normalized interval and restores the
out-of-range residual as linear tails: pow(clamp(v,0,1),1/gamma)+(v-clamp(v,0,1)).
This explicit extension preserves signed/headroom values and remains finite at
the legal .01/9.99 gamma extremes; only final half-storage limits +/-65504 apply.
Default identity, source alpha, transparent zero and padding are preserved.

Focused checks pass shared Legacy float/byte parity,432 independent f64 CPU/GPU
combinations,five Draft/cache sizes,six actual-half black/white and small-alpha
storage cases,domain/arity/all-five-parameter guards and animation lanes,four
owners and four real MCP author/save/history/lock/three-frame ProRes workflows
with12Full previews. Native interactions and full aggregate remain pending.
The wider professional roadmap remains open; no commit or push.

### Native catalog Levels final checkpoint — 2026-10-08

Current aggregate passes3,039 library tests /9ignored (core897,GUI577,MCP356,
render230,video968,matte11),133 native graph,50timeline,3bridge,11decode,
2source-end,2temporal playback,Legacy golden and6scaling tests. All-target
Clippy with existing scoped waiver,native build,touched formatting,whitespace
and522-tool generated docs pass. Native in_black=.1,gamma=1.8,out_white=.9 and
gamma key at2seconds=2.04296294093132 persist as exactly four intended edits;
four Undo actions restore the entire authored document exactly. Shared Legacy
float/byte parity,432 independent CPU/GPU cases,five Draft/cache sizes,six
actual-half boundary/small-alpha storage checks,all five lanes/four owners and
four real MCP ProRes workflows are qualified. Native bounded-gamma plus linear
residual tails preserve signed/headroom values and finite half storage. The
prior in-progress entry is superseded for this increment. Other effects and the
professional roadmap remain open. No commit or push.

### Native Alpha View increment — 2026-10-08 (qualification in progress)

The utility's three existing modes now run directly on managed AP1 float
images: alpha-as-gray with opaque coverage, unchanged associated RGB/alpha,
and straight RGB displayed with opaque coverage. Modes 0 and 2 deliberately
make empty logical source pixels opaque black; physical padding stays clear.
Legacy and native share the original per-pixel float helper and strict alpha
> 1e-6 divide guard. RGB is not clipped to the unit interval; straight RGB uses
only the finite half-storage boundary. The catalogue now correctly declares
that this effect can modify alpha.

Focused checks pass shared Legacy semantics and metadata, 216 independent
CPU/GPU combinations, 15 mode/size/cache evaluations, actual half-alpha guard
neighbors and storage extremes, domain/arity/mode guards, f32 mode rounding,
animated mode and all four owners. Four real MCP author/history/lock/save
workflows produce 12 Full previews and four pixel-verified ProRes movies.
Native editor interactions and the complete current-tree regression remain
pending. The broader roadmap remains open; no publication is implied.

### Native Alpha View final checkpoint — 2026-10-08

Current aggregate passes 3,043 library tests / 9 ignored (core 898, GUI 577,
MCP 357, render 230, video 970, matte 11), 137 native graph, 50 timeline,
3 bridge, 11 decode, 2 source-end, 2 temporal playback, Legacy golden and
6 scaling tests. All-target Clippy with the existing scoped waiver, native
build, touched formatting, whitespace and 522-tool generated docs pass.
All three catalog Alpha View modes now share the Legacy float helper and
run in native AP1: opaque coverage mask, exact associated-color passthrough,
and opaque straight-color view. Logical transparency and physical padding
remain distinct; extreme straight values saturate only at finite half storage.
216 independent CPU/GPU combinations, 15 Draft/cache evaluations, actual
half alpha-cutoff neighbors and storage extremes, domain/arity/mode guards,
f32 mode rounding, all four owners and animated modes pass. Four real MCP
workflows each cover author/history/lock/Save, with 12 Full previews and four
pixel-verified ProRes movies. Native mode edits 0→2→1 and a mode key at
2 seconds = 0.16049382090568542 persist as exactly three intended edits;
three Undo actions restore the entire authored document exactly.
The preceding in-progress entry is superseded for this increment. Other
effects and the full professional roadmap remain open. No commit or push.

### Native coverage Outline increment — 2026-10-08 (qualification in progress)

The catalog Outline now has a dedicated AP1 operator, strict parameter/domain
validation and all five animated controls. The GPU seeds coverage boundaries,
propagates byte-packed exact coordinates with jump flooding, and composites
source over a solid swatch band with a one-authored-pixel antialiasing rim.
Two scratch textures give radius-independent work; authored-pixel distances
respect nonuniform Draft. Empty fields have an explicit invalid seed, avoiding
a spurious outline when thickness exceeds an empty canvas. The native CPU
mirrors the same approximate distance algorithm; exact Euclidean accuracy for
all possible shapes is not claimed. Legacy rendering is unchanged.

Initial focused execution caught a WGSL dynamic-indexing restriction on an
immutable local array; the seed offset array is now function-local mutable.
Focused rerun, native interaction and complete aggregate qualification are
pending. The prior Alpha View checkpoint remains the last qualified package.

### Native coverage Outline final checkpoint — 2026-10-08

Current aggregate passes 3,049 library tests / 9 ignored (core 899, GUI 577,
MCP 358, render 230, video 974, matte 11), 139 native graph, 50 timeline,
3 bridge, 11 decode, 2 source-end, 2 temporal playback, Legacy golden and
6 scaling tests. All-target Clippy with the existing scoped waiver, native
build, touched formatting, whitespace and 522-tool generated docs pass.
Native Outline uses coverage-boundary seeds, byte-packed exact coordinates,
radius-independent jump flooding and source-over composition in AP1. Its
one-authored-pixel antialiasing rim and thickness respect nonuniform Draft.
174 independent CPU/GPU fixture cases cover eight representative shapes,
empty/tiny fields, half-coverage boundaries, finite-half extremes and seed
coordinates on both 2,057-pixel axes; five Draft/cache evaluations also pass.
The representative distance fixtures match independent brute-force Euclidean
references. JFA remains approximate for arbitrary shapes; no universal exact
Euclidean-transform or high-resolution throughput claim is made.
All five parameter lanes/four owners, hashes/domain guards, four real MCP
ProRes workflows and 12 Full previews pass. Native thickness=6, red=.2,
opacity=.7 and thickness key at 2 seconds=10.271604537963867 persist as exactly
four intended edits; four Undo actions restore the entire authored baseline.
The initial WGSL local-array indexing failure was corrected before these
checks. The preceding in-progress entry is superseded for this increment.
Legacy rendering is unchanged. Other effects and the full professional roadmap
remain open. No commit or push.

### Native catalog Hue/Saturation and Vibrance increment — 2026-10-08 (in progress)

The two LinearStraight catalog effects now have a dedicated native AP1
operator with linear Rec.709 component HSL and a continuous extended envelope.
The feature contract documents signed/HDR behavior, nominal lightness endpoints,
full desaturation and a smoothly feathered native neutral guard. Existing
Legacy rendering remains unchanged. Focused checks initially passed1,152
independent CPU/GPU combinations,10 Draft/cache evaluations and three extreme
storage cases, plus bounded Legacy byte agreement and endpoint/neutral tests.
A direct physical-midpoint branch now avoids cancellation at zero saturation;
its rerun and the newly added all-owner/key/MCP workflows are pending. Native
UI and full aggregate acceptance are also pending. Outline remains the last
qualified checkpoint. The broader roadmap remains incomplete.

### Native catalog Hue/Saturation and Vibrance final checkpoint — 2026-10-08

Current aggregate passes 3,053 library tests / 9 ignored (core 901, GUI 577,
MCP 359, render 230, video 975, matte 11), 142 native graph, 50 timeline,
3 bridge, 11 decode, 2 source-end, 2 temporal playback, Legacy golden and
6 scaling tests. All-target Clippy with the existing scoped waiver, native
build, touched formatting, whitespace and 522-tool generated docs pass.
The explicit LinearStraight contract in07-color-grading.md describes the
Photonic signed/HDR envelope, nominal lightness endpoints, direct physical
midpoint for full desaturation, smooth near-neutral Vibrance and finite half
storage. These catalog effects do not substitute the AP1 grading operator or
insert an sRGB transfer curve. Legacy raster behavior is unchanged.
1,152 independent CPU/GPU combinations,10 Draft/cache evaluations,three
storage-extreme cases,bounded Legacy byte agreement,neutral/endpoint/continuity
checks and exact neutral GPU passthrough pass. Both effects support all four
owners and their actual authored parameter lanes. Eight real MCP movies and
24 Full previews cover authoring,animation,Save/history and locks.
Native Hue/Saturation edits45/.4/.15 plus huekey2seconds=33.33333492279053
persist as exactly4edits and fourUndo restore the authored baseline. Native
Vibrance amount.65 plus key2seconds=-.1975308656692505 persist as exactly2edits
and twoUndo restore its baseline. Source and screenshot hashes were verified.
The earlier incomplete aggregate log was retained; a fresh complete run passed.
The preceding in-progress entry is superseded for these two effects. The full
professional roadmap remains open. No commit or push.

### Native catalog Invert (Raster) increment — 2026-10-08 (in progress)

The no-parameter TransferStraight catalog entry now lowers to a distinct
encoded-channel1-v native operator. AP1 is converted through linear Rec709 and
signed sRGB encode/decode, then returned to AP1 with original alpha. This is
separate from the existing bounded AP1 invert. RGB remains extended; final
storage saturates only at finite half limits. The Legacy byte fast path stays
unchanged, with an exhaustive correspondence test for the floating helper.
Focused numeric/owner/MCP checks, native UI and the complete aggregate are
pending. The fully qualified Linear HSL checkpoint remains the saved baseline.
There is no fabricated animated scalar control for this no-parameter effect.
The broader professional roadmap remains open.

### Native catalog Invert (Raster) final checkpoint — 2026-10-08

Current aggregate passes3,056 library tests/9ignored (core902,GUI577,MCP360,
render230,video976,matte11),145native graph,50timeline,3bridge,11decode,
2source-end,2temporal playback,Legacy golden and6scaling tests. All-target
Clippy with existing scoped waiver,native build,touched formatting,whitespace
and522-tool generated docs pass. This catalog entry uses signed encoded1-v
through Rec709/AP1 and is distinct from the bounded AP1 invert. It preserves
source alpha and extended values until finite-half storage saturation.
144independent CPU/GPU combinations,five Draft/cache evaluations,five double
inversions,two storage extremes and exhaustive256-channel/3-alpha correspondence
with the unchanged Legacy byte kernel pass. All four catalog owners,disable
recovery,hashes/domain/arity/unused-parameter guards,four real MCP ProRes exports
and12Full previews are qualified. There is no animated scalar parameter.
Native disable/Save and re-enable/Save create exactly two intended edits;
twoUndo actions restore the entire authored baseline and history state.
The first focused compile caught a duplicated test helper; it was removed
before the successful focused and full reruns. The preceding in-progress
entry is superseded. The full professional roadmap remains incomplete.
No commit or push.

### Executable catalog coverage audit — 2026-10-08

A separate read-only executable linked against the current production libraries
compiled all45 actual catalog manifests at default values across asset,clip,
track and master scopes:180enabled checks and180disabled-recovery checks.
39entries lower without errors and have valid native color domains in all four
scopes. Six fail visibly: blur.lens,blur.surface,color.deflicker,filter.median,
repair.dust_and_scratches and util.unpremultiply. Disabling each entry restores
a valid graph in all180cases. The audit makes no claim about every parameter
combination,GPU performance or overall roadmap completion. Source/library and
synthetic-fixture hashes accompany the machine-readable evidence. These are
catalog gaps; the broader work in the remaining-work table is still open.

### Native catalog Surface Blur candidate — 2026-10-08

Surface Blur now has a native scene-linear AP1 implementation of the catalog's
bilateral filter. The range metric is linear Rec.709 premultiplied color;
weighted color is accumulated in AP1 and renormalized to the original center
alpha. Authored integer taps, clamp-edge bilinear sampling, and per-axis pixel
scale preserve the requested footprint in nonuniform Draft views. Radius and
threshold zero are exact identities. Threshold has the existing 1/255 floor;
nonidentity transparent pixels are zero, and final RGB is bounded only by
finite half storage. Legacy raster behavior is unchanged.

A demanded-node work preflight sums a conservative 256 million texture-fetch
budget per frame before allocation/dispatch. It counts up to four bilinear
fetches per tap, includes multiple effects, exempts exact identities, and
reports a visible error rather than silently changing radius or quality.
Default radius 2 at 4K and radius 5 at 1080p fit this arithmetic budget; this
is not a measured interactive-performance claim. Larger radius/resolution
combinations can be refused. High-resolution optimization remains open.

Initial compilation, independent spatial/capacity tests, both animated catalog
lanes, four compiler owners, and four-scope MCP export tests have passed.
Capacity refusal through MCP, expanded integration coverage, native editor
Save/Undo, and the full regression suite are still being qualified. This is
not a completed checkpoint or a claim that the full roadmap is finished.

### Native catalog Surface Blur final checkpoint — 2026-10-08

The fresh aggregate passes 3,063 library tests with 9 ignored (core 902,
GUI 578, MCP 362, video 980, render 230, matte 11), 149 native graph tests,
50 timeline, 3 bridge, 11 decode, 2 source-end, 2 temporal-playback,
1 Legacy golden and 6 scaling tests. All-target Clippy with the existing
scoped waiver, native build, touched formatting, whitespace and generated
522-tool documentation pass.

The native bilateral implementation passes 336 independent CPU/GPU patch
combinations, 12 genuinely multicolor spatial cases plus a renormalization
storage extreme, 5 Draft/cache evaluations and 3 radius-limit/constant/storage
cases. Both actual catalog lanes, all four owners, f32 radius rounding,
hashes, domain/arity/range guards, four MCP ProRes movies and 12 Full previews
pass. A capacity test covers visible refusal, identity recovery, undo back
to refusal, and failed-export nonpublication of both movie and manifest.

Testing caught a real failure-propagation defect: a terminal computation
refusal produced no frame and previously became a misleading 30-second
cold-decode timeout. Evaluation-associated inspection IDs now deliver the
specific error to frame inspection and export. Terminal refusals no longer
request repeated presents; the monitor shows the actionable cause. This
change was included in the fresh aggregate, including existing exports.

Native editor radius 4 and threshold 0.6 edits, a radius key at 2 seconds
(value 6.320987701416016, rounded by the catalog at evaluation), Save with
three intended edits, and exact three-Undo restoration pass. A separate
radius-64 edit is visibly refused at 320x240 and Undo recovers the preview
and authored baseline. Seven screenshots and saved document receipts record
these checks. The earlier candidate entry is superseded.

The conservative 256-million texture-fetch budget is an explicit safety
limit, not a measured interactivity claim. Large radius/resolution or
multi-effect combinations can still be refused; high-resolution optimization
and the broader professional roadmap remain unfinished. No commit or push.

### Native catalog Lens Blur candidate — 2026-10-08

Lens Blur now has a candidate discrete circular-aperture average of
premultiplied AP1 RGBA, including averaged alpha as in the actual Legacy lens
kernel. The catalog's generic alpha-preserving metadata does not describe this
kernel's coverage behavior. Radius is continuous f32 in the catalog range
0..500 (actual manifest default 0); integer taps are included by disk distance,
not an integer-rounded or Gaussian approximation. Radius below 1 is exactly
identity. Per-axis authored tap scale supports nonuniform Draft; final RGB is
bounded only by finite-half storage. Compensated GPU accumulation protects
large apertures from avoidable sum error.

Lens and Surface Blur share the same demanded-node 256-million estimated
texture-fetch budget, so stacking them cannot obtain separate budgets. The
conservative square bounding box may refuse a disc whose exact tap count is
smaller. This is an explicit safety limit, not a claim of interactive speed at
all radii and resolutions. Initial compilation and multicolor/fractional-
aperture GPU checks, including a radius-500 storage case, pass. Owner/MCP,
remaining numerical coverage, native UI and full acceptance are pending.

### Native catalog Lens Blur final checkpoint — 2026-10-08

Fresh acceptance passes 3,069 library tests with 9 ignored (core 903,
GUI 578, MCP 364, video 983, render 230, matte 11), 153 native graph tests,
50 timeline, 3 bridge, 11 decode, 2 source-end, 2 temporal-playback,
1 Legacy golden and 6 scaling tests. All-target Clippy with the existing
scoped waiver, native build, touched formatting, whitespace and generated
522-tool documentation pass.

The discrete circular-aperture implementation passes 336 independent CPU/GPU
patch combinations, 13 multicolor/fractional-aperture cases, a radius-500
storage extreme, 5 Draft/cache evaluations and 2 radius-500 constant/storage
cases. Radius remains continuous f32 with a default of zero, and its disk
membership is not rounded to an integer radius or approximated by a Gaussian.
The actual existing Legacy kernel averages coverage. Lens-specific manifest
metadata now correctly declares alpha modification, backed by an unchanged
Legacy-kernel coverage test; no Legacy pixel implementation was changed.

All four catalog owners, the actual radius lane, hashes/domain/arity/range
guards, combined Surface/Lens workload accounting, four MCP ProRes movies
and 12 Full previews pass. Capacity checks cover refusal, identity recovery,
undo back to refusal and failed-export nonpublication of movie and manifest.
Native radius 3.25, a key at 2 seconds valued 12.345679104328156, Save with
two intended edits and exact two-Undo restoration pass. A separate radius-64
edit is visibly refused at 320x240 and Undo restores the sharp baseline.
Seven screenshots and saved document receipts capture these native checks.
The preceding candidate entry is superseded. The first aggregate passed all
library and integration tests but Clippy rejected an intentionally near-sqrt(2)
test literal. Equivalent explicit f32 bit patterns resolved it; the complete
aggregate was rerun on the final source. Runtime filter/UI code was unchanged
by this test-only correction, and the original UI source hashes are retained.

Surface and Lens Blur share the conservative 256-million estimated texture-
fetch budget; larger radius/resolution combinations may be refused. This is
not a claim of interactive high-resolution throughput or complete professional
roadmap acceptance. Source remains local, uncommitted and unpushed.

### Native catalog Unpremultiply candidate — 2026-10-08

The existing float debug utility now has a native AP1 candidate. Its established
numerical operation divides stored RGB by clamped alpha while retaining that
alpha above 1e-6, and writes transparent zero at or below the cutoff. This is
not a switch of the pipeline to straight-alpha storage: downstream operations
still interpret associated AP1. Consequently the diagnostic can brighten the
visible straight color by another 1/alpha. It is distinct from regular grading
(which unpremultiplies, grades and repremultiplies) and from opaque Alpha View
mode 2. Native output bounds RGB only at finite-half storage limits.

The Legacy utility and native CPU path share the same point helper without
changing Legacy float results. The manifest now declares coverage modification
for the tiny-alpha cutoff. There are no catalog parameters or invented animation
lanes. Initial workspace checking passed; focused numeric/owner/MCP tests,
native Save/Undo and the full aggregate remain in qualification. This is not
a completed checkpoint or overall roadmap acceptance.

### Native catalog Unpremultiply final checkpoint — 2026-10-08

The fresh aggregate passes 3,073 library tests with 9 ignored (core 904,
GUI 578, MCP 365, video 985, render 230, matte 11), 157 native graph tests,
50 timeline, 3 bridge, 11 decode, 2 source-end, 2 temporal-playback,
1 Legacy golden and 6 scaling tests. All-target Clippy with the existing
scoped waiver, native build, touched formatting, whitespace and generated
522-tool documentation pass.

The existing diagnostic's native implementation passes 72 independent CPU/GPU
combinations, 5 Draft/cache evaluations, 3 FP16 cutoff/storage cases and 8
shared Legacy float cases. It preserves the established RGB/alpha numerical
operation with retained coverage above 1e-6 and transparent zero below it.
The pipeline remains associated AP1; this is not a straight-storage conversion
or a normal grading roundtrip. Signed/HDR values are retained until native
finite-half RGB saturation. Only this utility's metadata was corrected to
account for cutoff coverage modification; Legacy float results are unchanged.

All four catalog owners, disabled recovery, distinct hash/domain/arity checks,
four MCP ProRes movies and 12 Full previews, Save/history/lock checks pass.
The catalog has no parameters and no scalar animation was invented. Native
disable/Save, re-enable/Save and exact two-Undo restoration of the authored
baseline and history pass, with four screenshots and saved-document receipts.
The preceding candidate entry is superseded. The broader professional roadmap
remains incomplete. No commit or push.

### Native Median and Dust & Scratches candidate — 2026-10-08

Both filters have a native straight-linear-Rec.709 rank implementation with AP1
input/output conversion. Median independently ranks RGB and coverage; Dust &
Scratches replaces each RGB channel only when its difference exceeds the
truncated catalog threshold divided by 255, retaining center coverage. Both
use authored integer neighborhoods, clamp-edge bilinear Draft sampling, and
an extent bound derived in authored coordinates.

Small kernels lazily specialize exact componentwise bitonic sorting with
capacities 16/32/64/128/256. Larger kernels use exact sortable-f32 radix
selection with constant-size state, avoiding a huge per-fragment window array
or quantized histogram approximation. Selected-source anchoring and neutral-
color difference transforms avoid unnecessary basis cancellation. Initial
extreme-HDR tests caught CPU/GPU cancellation; double-precision CPU operands
and source-anchored GPU reconstruction resolved the failing case without
relaxing its tolerance. Neutral strict-threshold cases were added.

Rank filters share the existing 256-million estimated texture-fetch bound,
including the selected-source read, and have a separate 2-billion comparison/
rank-step bound. Both guards use demanded nodes before dispatch and report
explicit errors with recovery/export-nonpublication checks. These are safety
limits, not measured high-resolution interactivity claims.

Native Median radius 0 is an exact neutral operation, following the core median
contract and the native Box Blur convention. The older Legacy catalog bridge
forces a minimum radius of 1; that Legacy behavior is deliberately unchanged.
This native/Legacy difference is explicit rather than a silent parity claim.
Median's alpha metadata now reflects the actual coverage-modifying kernel;
Dust retains its coverage-preserving declaration.

Initial compilation, multicolor sorting/radix/alpha/HDR checks, owner/lane/hash
and capacity checks, and eight MCP movies with 24 Full previews have passed.
Expanded image coverage, native editor verification and full regression remain
in progress. This is not a completed checkpoint or full-roadmap acceptance.

### Native Median and Dust & Scratches final checkpoint — 2026-10-08

The fresh aggregate passes 3,080 library tests with 9 ignored (core 905,
GUI 578, MCP 367, video 989, render 230, matte 11), 161 native graph tests,
50 timeline, 3 bridge, 11 decode, 2 source-end, 2 temporal-playback,
1 Legacy golden and 6 scaling tests. All-target Clippy with the existing
scoped waiver, native build, formatting, whitespace and generated 522-tool
documentation pass. The earlier aggregate was interrupted without a terminal
result; it is not counted as passed. This fresh run completed all gates.

Median and Dust & Scratches use exact componentwise linear-709 rank selection
with independent coverage ranking for Median and center coverage for Dust.
Small windows use bitonic sorting; larger windows use bounded-storage radix
selection. CPU f64 and selected-source-anchored GPU reconstruction resolve the
observed extreme-HDR cancellation failures without relaxing test tolerances.
Neutral-preserving color-difference transforms keep threshold boundaries stable.
Native Median radius zero is identity; the unchanged Legacy catalog bridge
historically forces a minimum radius of one. This difference is explicit.
Dust threshold is truncated to its authored byte range, then compared in linear
units; threshold 255 is not a universal HDR identity. Native output saturates
at finite-half storage limits, and transparent input cutoff is 1e-6.

Qualification includes 576 independent CPU/GPU combinations, 10 Draft/cache
checks, 4 constant/storage cases, 22 mixed-spatial/HDR/strict-threshold cases,
all four catalog owners, compiler/hash/domain/arity checks, eight MCP ProRes
movies and 24 Full previews, Save/history/lock and refusal/recovery checks.
Native Median radius 2 plus a two-second radius key 3.086419776082039 saved
as two edits and returned exactly to baseline with two Undo actions. Dust
radius 2/threshold 32 plus a two-second radius key 3.160493850708008 saved
as three edits and returned exactly with three Undo actions. Median radius500
and Dust radius16 at 320x240 produced visible workload refusal and recovered
through Undo, without publishing an invalid export. Screenshots and document
receipts record both flows.

Rank comparison work is capped at 2 billion steps per frame; the shared
Surface/Lens/rank texture-fetch budget is 256 million per frame. Demand pruning,
identity paths, combined stacks and recovery are tested. These are explicit
resource limits, not evidence of interactive high-resolution throughput.
The fresh default compiler audit now supports 44 of 45 catalogue entries
across four owners (180 enabled checks and 180 disabled recovery checks);
color.deflicker remains unsupported. This is not every-parameter or full-roadmap
acceptance. Broader tracking, restoration, shared workflows, HDR, performance
and release acceptance remain open. No commit or push.

### Deflicker analysis prerequisites checkpoint — 2026-10-08 EDT

The Legacy whole-range analysis now advances at most one GPU sample per engine
turn, keeping command processing available between samples. It stores RGB
statistics rather than retaining all sampled images, preserves exact sample
slots across cold-source retries, uses a 20-second job deadline, and only commits
a complete finite curve. Incomplete rolling-band bursts cannot be packed into
a false consecutive sequence. No unmeasured frame is presented while selected
analysis remains pending. Legacy deadline failure retains its established
uncorrected fallback; native Deflicker remains explicitly unsupported.

Analysis identity now includes source/media content and interpretation, speed,
sequence rate/color, complete parameter lanes and the media invalidation epoch.
Cosmetic rename/placement do not invalidate it. Changed inputs remove stale
gains, edits restart a job, and removal cancels it. Native sequences skip the
inappropriate Legacy post-display measurement pending a qualified native meter.
The bounded i128 sample grid includes partial final frames and rejects invalid
ranges, over-budget counts, missing or nonfinite samples rather than shifting
remaining samples to earlier times.

27 focused Deflicker tests pass, including an actual GPU incremental job,
no-unmeasured-frame publication, exact-step bounds, edit restart, deadline,
cancellation and native-path avoidance. The complete fresh regression passes
3,084 libraries with 9 ignored (core905,GUI578,MCP367,video993,render230,matte11),
161 native graph,50timeline,3bridge,11decode,2source-end,2temporal,1golden and
6scaling tests; all-target Clippy, native build,522-tool docs, formatting and
whitespace pass. This checkpoint is analysis infrastructure, not native
Deflicker acceptance. The default compiler audit remains44/45, and native
color-domain correction, owner/occurrence identity, animation semantics,
actual flickering-video/export validation and the broader roadmap remain open.
No commit or push.

### Native clip Deflicker checkpoint — 2026-10-08 EDT

A single clip-scoped Deflicker now measures isolated pre-display AP1 source
frames, converts to the explicit linear709 meter, and applies a resolved native
correction. Exposure operates on bounded encoded709 with signed/HDR linear
residuals retained; rolling bands divide linear RGB first. Anchored AP1 delta
reconstruction avoids neutral-HDR cancellation. Alpha is preserved; finite-half
storage bounds apply. The native median/mean baseline uses the observed range
at short-clip boundaries, fixing the endpoint ramp found by the first real-video
test without changing the Legacy solver. Analysis remains incremental and
bounded; cold sources retry their exact slots, stale input identity restarts,
and incomplete/failed native analysis cannot publish an uncorrected delivery.

Amount and chroma amount may animate. Analysis window and max_change are
currently static, and one instance is allowed per clip. Asset, track, master,
multiple-instance and broader owner analysis are still explicitly unavailable.
The whole professional roadmap is not complete. Export's structure preflight
uses a private non-rendering validator; actual export uses measured gains in
its private engine and fails if measurement is unavailable.

480 independent signed/HDR/alpha/band CPU/GPU combinations, five Draft/cache
checks, parameter/domain/arity guards and a bounded pre-display meter test pass.
Actual decoded flicker footage, out-of-order seeks, compiler/provider guards,
animated strength, disabled recovery, Save/history and locking pass. The MCP
case writes two real ProRes movies and three Full previews; decoded output
standard deviation falls from0.0534387636377508 to0.009283969511454588 for the
specified12-frame synthetic fixture. This is fixture evidence, not a general
real-footage quality or high-resolution performance guarantee.

The native editor saved amount0.6,window2,max_change0.4,chroma0.5 plus a strength
key0.6975308656692505 at0.2seconds as five edits. Five Undo actions restore the
authored baseline (the serializer omits the optional empty tracks array).
Testing an unsupported animated analysis window exposed a missing monitor
message; compiler color failures now surface in status, and pending analysis
is labeled as background computation rather than incorrectly as subject masks.
The rebuilt editor shows the explicit limitation and clears it on Undo.

Fresh regression:3,090 library tests/9ignored(core905,GUI579,MCP368,video997,
render230,matte11),164nativegraph,50timeline,3bridge,11decode,2source-end,
2temporal,1golden,6scaling; all-target Clippy, native build,522-tool docs,
formatting and whitespace pass. The no-analysis default compiler inventory
still reports44/45 across all four owners: Deflicker requires actual analysis
and only its clip scope is qualified here. Do not call this unrestricted45/45
catalog support. No commit or push; remaining owner/analysis animation support
and broader roadmap work are open.

### Native Deflicker analysis-control animation checkpoint — 2026-10-08 EDT

Window and max_change now animate in the native clip implementation. The
analysis controls resolve at the same bounded exposure-grid ticks as the source
measurements, and solved gains interpolate between those ticks. Amount/chroma
remain per-output-frame controls. This explicitly retains sampled analysis for
long clips; it is not a claim of full-rate analysis beyond the240sample budget.
Each engine turn computes at most one animated-setting solve, adjacent identical
settings reuse the prior solve, and the20second analysis deadline remains.
Invalid settings or partial curves never publish a completed native result.

33 focused video Deflicker tests plus GUI/MCP checks pass. The added decoded
60frame video test animates window0.5→1.5 and max_change0.2→0.4 and passes the
existing flicker-reduction threshold. MCP now writes three actual ProRes movies:
baseline, strength-animation correction, and analysis-controls animation with
window0.2→0.4/max_change0.25→0.4. The latter's decoded standard deviation is
0.013856642549237115 against baseline0.0534387636377508 for the12frame synthetic
fixture. Existing preview, Save/history/lock and duplicate-instance export
refusal/nonpublication checks pass. These measurements are fixture-specific.

The native editor authored four keys at0.1/0.3seconds: Window
0.5679012373089791→1.8555555678904057 and Max Change
0.20555555373430254→0.40000000298023225. Full rendering, Save and four Undo actions
restore the authored baseline/history (normal optional empty-track omission).
Five screenshots and saved documents retain the evidence.

Fresh aggregate passes3,092 library tests/9ignored(core905,GUI579,MCP368,
video999,render230,matte11),164nativegraph and all50timeline/3bridge/11decode/
2source-end/2temporal/1golden/6scaling checks, all-target Clippy, native build,
522-tool docs, formatting and whitespace. The previous static-window limitation
is superseded. Shared asset/track/master scopes and multiple instances are
still open; the default no-analysis inventory remains44/45 across all owners.
The broader professional roadmap remains incomplete. No commit or push.

### Native Deflicker shared-owner checkpoint — 2026-10-08 EDT

Asset, clip, track and master scopes now use owner-aware measurements. Keys
include sequence, owner, clip evaluation context and effect index. Shared asset
uses therefore retain independent trimmed/retimed contexts. Native track effects
keep the existing per-clip, sequence-clock behavior before track folding; master
measures the composited input. Each target measures only its pre-effect AP1 graph.
Reachable upstream Deflicker dependencies complete first; downstream placeholders
are pruned and never become deliverable frames. Nested graph imports preserve
these dependencies. Missing sources, provisional computations and upstream errors
cannot publish measurements. Transparent gaps use nearest observed exposure;
missing decoded frames retry rather than inventing samples.

Work is bounded to 240 samples per plan, 32 plans and 1,024 estimated sampling/
solve turns, one sample or solve per engine turn, with a 20-second preparation
deadline. One enabled instance per owner remains the qualified contract. These
bounds deliberately refuse larger analysis requests; this is not an unlimited
long-form or real-time performance claim.

Seven ProRes cases cover baseline, each of four owners, all four stacked, and
reused trimmed asset contexts with opposite flicker phases. Eighteen Full
previews, Save/history roundtrips and decoded per-context reduction pass. Stacked
stages measure already-corrected input. The trim test does not establish shot
matching or continuity across different contexts. An upstream Surface Blur
capacity error reports promptly, recovers after editing, returns after Undo,
and publishes neither a movie nor manifest on failed export. Nested dependency
and owner/clock/zone/budget tests pass.

Native editor review changed asset amount to 0.6, track to 0.7, master to 0.8,
saved all three independently, then three Undo actions restored the exact
baseline timeline and settled Full preview. The review exposed a misleading
track hint; its wording was corrected to 'after the clip effect stack'. This
text-only follow-up passed focused scope tests, native build and Clippy.

The fresh full aggregate before that text-only correction passes 3,101 library
tests/9 ignored (core905, GUI579, MCP370, video1006, render230, matte11),
164 native graph checks and all other integration gates, Clippy, native build,
522-tool docs, format and whitespace. Shared-owner and analysis-animation
limitations in earlier checkpoints are superseded; multiple instances, broader
restoration, long-form performance and professional workflow acceptance remain
open. Default compilation without measured analysis still reports 44/45 catalog
entries; it must not be inflated with fake unity measurements. No commit or push.

### Native Deflicker stacked-instance checkpoint — 2026-10-08 EDT

The one-enabled-instance restriction is removed within the existing bounded
analysis budget. Eight input-extraction dependencies, two instances on each of
asset/clip/track/master, prove ordered effect-index isolation. The second
instance measures the first instance's corrected AP1 output. A real two-instance
clip ProRes export passes decoded flicker reduction, manifest verification and
history restoration. Existing source/failure/lock contracts remain enforced.
The first focused run exposed an obsolete duplicate-refusal assertion; that
expected contract was updated, and the fresh focused suite passes40 video tests
plus all three MCP tests. No numerical tolerance was weakened.

Native Full preview displays two independent effect rows. Editing only the
second amount to0.4 leaves the first at0.85; Save retains both and Undo restores
the baseline, allowing only normal omission of three empty effect lists. The
fresh aggregate passes3,101 library tests/9ignored,164nativegraph and all prior
integration, Clippy, build,522-tool documentation and formatting gates. The clip
MCP fixture now has four movies including the stacked result. The32-plan,
1,024-turn estimate,240-sample/plan and20-second limits remain. This supersedes
the instance-count limitation, not long-form/performance/full-roadmap limits.
No commit or push.

### Bounded720p multi-clock and seek observation — 2026-10-09 EDT

This increment changes the qualification harness, not production rendering.
An explicitly invoked ignored HD test streams a lossless400-frame YUV oracle,
checks every logical pixel in the paused frame, then samples one playback pixel
per observed frame (or explicitly disables playback readback for profiling).
The ordinary two small temporal regressions still pass; all-target Clippy,
formatting and whitespace pass. Production code is byte-identical to107, whose
3,101-library/164-nativegraph aggregate remains the production-code evidence.
No fresh whole-library aggregate is claimed for this test-only increment.

On this cloud llvmpipe computer, optimized dev-opt(opt2,debug0) Full720p with
one clock published179frames in6seconds with0drops/0evaluation misses. Two
clocks published155 with23drops; four clocks published95 with82drops. The
four-clock no-readback comparison published93 with84drops, so test readback is
not the main remaining bottleneck. Debug Full and Draft were slower. These are
short synthetic observations, not30fps production or user-PC qualification.

A60-second four-clock cold/hot seek exercise verified138exact requested frames
at0,100,25,175 with maximum sampled error0.000559484. Playback sampled error was
0.000813593; the unchanged tolerance is0.012. Managed decoded-ring occupancy
peaked254,361,600bytes and managed GPU caches1,069,547,520bytes, within their
budgets. These are managed-cache counters, not total-process/RSS limits.
Document and history remain unchanged. Longer representative real footage,
CPU/GPU/memory profiling and multi-clock throughput improvements remain open.

The first HD fixture was too short for the extended observation; it now has400
frames. A Draft harness assertion included pooled texture padding, corrected
to logical dimensions. Full-frame CPU readback depressed observation counts;
its failed receipt is retained alongside pixel-probe and no-readback evidence.
These harness corrections did not widen numerical tolerances. The raw generated
YUV files and build products are not packaged; source can regenerate fixtures.
No commit or push. The professional roadmap is still incomplete.

### Native converted-frame cache checkpoint — 2026-10-09 EDT

Native video now reuses converted GPU frames by asset, actual decoded PTS and
all input color-intent fields. Exact requested-time aliases are bounded to256;
scrub approximations never create exact aliases. The cache holds at most32
entries/256MiB and participates in the existing1.5GiB managed GPU budget by
reserving capacity from graph caching. Source identity, probe format and input
validation happen before a cache hit. Targeted/global invalidation clears both
entries and aliases. Offline/replaced/invalid sources cannot be hidden by a hot
cached texture. Tests cover reuse, every key field, intent separation, source
replacement/probe/offline recovery, invalid intent, eviction and oversized items.

The optimized Full720p four-clock60-second seek fixture verifies5,718 exact
seeks versus138 before, with the same maximum pixel error0.000559484. This is a
synthetic repeated-seek benefit, not a general41x speed claim. Streaming remains
95 published frames/6seconds with82 dropped frames and0 evaluation misses;
30fps four-clock throughput is still unqualified. Peak managed decoded bytes
254,361,600 and GPU cache bytes1,053,818,880 remain within budgets. They are not
whole-process RSS measurements. Software-GPU results do not qualify user hardware.

Fresh regression gates pass3,102 libraries/9 ignored (core905,GUI579,MCP370,
video1007,render230,matte11),164 native graph checks and the other integration
suites, Clippy, native build,522-tool docs, formatting and whitespace. The HD
benchmark remains explicitly invoked and ignored by the routine suite.
The native editor displayed settled Full previews at1:11 and2:11, returned to
1:11 with4/4 sources ready and0 history edits, and the fixture remained byte
identical. A playback run beyond source coverage visibly refused the invalid
frame and recovered on a valid seek; this does not qualify transport end stopping.
The default no-analysis catalog audit remains44/45, with180 enabled and180
bypass checks; measured Deflicker tests are separate. The earlier interrupted
aggregate is preserved as interrupted; a completely fresh aggregate passed.
No commit or push. Broader throughput, representative footage, HDR/professional
finishing and the end-to-end roadmap remain incomplete.

### Program transport endpoint correction — 2026-10-09 EDT

The normal engine-backed1x GUI path previously followed the clock beyond the
sequence end. It now pauses and seeks the last frame strictly inside the
positive half-open content range, synchronizing GUI and engine intent. Forward
shuttle and engine-less fallback use the same final-frame rule. Loops remain
active; manual paused seeks are not clamped. The standalone engine clock API is
unchanged. This is a GUI transport correction, not a claim of all reverse/ramped
playback or external audio-device qualification.

Boundary tests cover30 and30000/1001fps, exact/end-plus offsets, subframe tails,
empty/negative content and loop bypass. A real headless engine/GUI transport test
proves final-frame settling, unchanged document/history, manual out-of-content
seek, and multiple loop wraps. All581 GUI library tests,3 engine-bridge tests,
all-target Clippy, native build, formatting and whitespace pass. Production
outside monitor.rs is byte-identical to109; its full3102-library/164-graph
aggregate remains evidence for those unchanged components. No new whole-library
aggregate is claimed for this GUI-only increment.

Native UI: a one-second Full720p four-clock fixture automatically stopped at
00:00:00:29 with4/4 sources ready and0 history edits. Looping continued across
multiple seconds; disabling the loop stopped at29 again. The fixture stayed
byte-identical. Software-GPU loop buffering remains a performance limitation.
No commit/push. The broader professional grading roadmap remains incomplete.

### Bounded live native measurements and pause alignment — 2026-10-09 EDT

The Native output measurements window now offers opt-in live sampling during
playback. It reads paired scene-linear AP1 and SDR-display textures from one
immutable rendered frame, at most4 launches/second with one worker. Live frames
are limited to2,073,600pixels; larger frames require Draft or the existing
paused capture (16,777,216pixel bound). No silent downsampling occurs. Results
show captured timestamp, dimensions, quality and requested proxy mode; they can
lag playback and are not advertised as the current displayed frame. Paused
capture remains exact and now requires both visible-frame and snapped GUI
playhead identity. Relative AP1 luminance stops remain relative to0.18, not
nits, IRE, calibrated monitoring or HDR delivery qualification.

Live results tolerate time/content-hash progression only inside identical
document, sequence, revision, snapshot generation, size, quality and proxy
context. Close/reopen and mode changes reject old worker epochs. Invalid frame
contexts clear stale values. Cadence, one-worker limits, all context fields,
mode changes, pixel boundaries and real-engine paired values are tested. Native
scene taps are retained only while requested by paused capture or opted-in live
playback. Existing MCP one-shot paired measurement is unchanged.

Native review revealed an existing asynchronous Pause drift: the GUI showed
5:18 while the paused rendered frame measured5.867seconds. Program-specific
pause reconciliation now follows Pause with an admission-retried settling Seek
to the GUI time, without changing source audition. An intentional79ms GUI-idle
regression fails with the old call and passes with the correction; its negative
control is retained. Final native UI playback/measurement review captured
2.133seconds matching00:00:02:04 at30fps, with0 history edits and byte-identical
fixture. Draft/Full live timestamps and values changed during looping. Full
software-GPU readbacks visibly lagged playback; no4Hz throughput or latency
qualification is claimed.

All585 GUI library tests,3 engine-bridge checks, all-target Clippy, native build,
format and whitespace pass on the final tree. Only five GUI source files changed
from110; video/render/core/MCP production remains byte-identical to109's fresh
3102-library/164-graph aggregate. No fresh full-workspace aggregate is claimed
for this GUI-only increment. Complete live-review screenshots, including the
pre-fix drift, are retained in Working Recovery version34; this checkpoint keeps
the final aligned capture. No commit/push. Broader scopes/HDR, representative
performance and complete professional grading workflow remain unfinished.

### Bounded mixed-source intermediate Resize — 2026-10-09 EDT

Differently sized MediaIn sources can now feed an explicit Resize whose authored
intermediate dimensions differ from the sequence canvas. The existing native
contract is unchanged: apply Contain/Cover/Stretch at the requested intermediate
size, then Stretch that result to the sequence canvas before later processing.
This expands bounded intermediate resampling, not arbitrary variable-extent
chains. Integer dimensions1..8192, at most16,777,216 target pixels, bounded padded
source allocations, explicit Resize routing and invalid/offline source refusals
remain enforced. Draft scales both intermediate and canvas sizes, minimum1pixel.

Compiler tests cover all three fit modes, several aspect ratios, one-pixel targets,
cache identity, TimeOffset/Switch routing and invalid sizes. Ninety GPU evaluations
cover video/still sources, five intermediate sizes, all three fits and Full/Draft/
Full reuse with poisoned texture padding and signed/highlight premultiplied AP1.
Source taps retain their original dimensions. CPU/GPU comparisons retain the
existing0.002 tolerance; a separate f64 two-pass bilinear oracle verifies54 MCP
PNG previews and18 six-frame ProRes movies, including partial alpha, history,
save/reload, locked edits, relink/probe refusal and undo recovery.

The first aggregate exposed three tests still expecting the old word "canvas"
in an error message. Actual unsafe unresized input remained correctly refused.
The shared assertion now expects explicit Resize; focused four-clock recovery
and a fresh full aggregate subsequently pass. The original failure log is
retained. Native editor inspection, exact source hashes and final test counts
are recorded in the adjacent qualification JSON. No commit or push. Wider graph
geometry, representative performance, HDR/professional finishing and complete
end-to-end roadmap acceptance remain open.

### Native scene-linear global shot matching — 2026-10-09 EDT

Native Managed gallery captures now retain optional versioned scene-linear AP1
balance measurements paired with their display PNG. GUI and MCP can propose an
editable Printer Lights correction from these measurements, limited to two stops
per channel. Old display-only stills remain readable/comparable and require
recapture for this assistance. No display-to-scene inversion or Legacy fallback
is used. The color specification records the bounded positive-channel histogram,
coverage, trimming and exact integer log-mean persistence contract.

Numerical tests cover signed/HDR/alpha eligibility, independent exact-sort error
bounds, caps, invalid coverage and measurement identity. Native MCP checks cover
capture, comparison, malformed/missing measurements, changed images, locks, stale
revisions, one-step application, zero residual, undo and document persistence.
A strict GUI recovery test caught one-ULP floating JSON drift; persisted log means
now use exact signed Q20 units. Native editor review confirmed a -12-point RGB
proposal for a one-stop-brighter target, a matching image after one edit, zero
residual, undo, fresh reference capture and saved native measurements.

Fresh aggregate: 3118 library tests passed (9 explicitly ignored),
239 integration checks passed (1 explicitly ignored).
Clippy, native build, generated tool documentation, formatting and whitespace
checks passed. Focused native suite separately passed 331 tests, excluding six
ignored tests. Evidence includes the initial persistence failure and correction.

This is bounded global statistical assistance. It is not chart/semantic matching,
scene grouping, representative-footage qualification or completion of the full
tracked-isolation/version/export/reimport acceptance workflow. Different framing,
subjects and later nonlinear grades can mislead a proposal. Broader roadmap work
and external hardware qualification remain open. Local, uncommitted and unpushed.

### Combined native grading workflow regression — 2026-10-09 EDT

A new 16-frame, 96x72 synthetic-texture workflow connects native reference capture,
a half-stop exposure difference, editable global shot matching, undo/redo,
tracked elliptical isolation, alternate grade versions, save/reload, 16-frame
ProRes export and sample/alpha verification, manifest integrity, reimport/probe,
portable archive collection and a cold render from collected dependencies.
Reference scene measurements survive the archive unchanged. This extends the
previous four-frame tracking-only delivery regression; that regression and the
original tracking proposal/history test also pass unchanged.

Only test code changed from qualified checkpoint113. All three affected MCP
tracking tests, workspace Clippy, formatting and whitespace checks passed; the
113 full aggregate remains the production-code baseline (3,118 library tests
and239 integration checks passed,10 explicitly ignored). No new full-suite
count is inferred from the additional test. Native GUI inspection played to the
last frame, switched Tracked/Untracked at the same time and undid the version
change. Close autosaved its history envelope; the document payload remained
identical to the initial fixture.

This is deterministic integration evidence, not representative-footage tracking
acceptance, chart/semantic matching, manual GUI execution of every delivery step,
or completion of the professional roadmap. Real-footage occlusion/rotation/scale,
broader workflow and hardware qualification remain open. No commit or push.

### Antialiased tracking analysis and external-footage observations — 2026-10-09 EDT

Tracking input reduction now integrates exact pixel-area overlap instead of
nearest-center subsampling. Premultiplied RGB/coverage are accumulated before
unpremultiplication, so hidden transparent RGB is excluded and signed/highlight
values remain available. The input16,777,216-pixel, analysis256-pixel-side,
121-frame and120-second limits and NCC confidence/margin thresholds are unchanged.
Tests independently cover alternating-pixel alias rejection, signed/HDR signals,
alpha weighting/hidden RGB and fractional overlap. Twenty focused tracking tests
pass across numerical, native/Legacy engine, MCP and GUI lifecycle paths.

On the first31frames of the pinned public OpenCV vtest sample, an unoccluded
building-window control remains stable for31frames with both samplers. CPU
moving-subject traces stop at6/3/2frames with nearest sampling and13/4/2 with area
filtering. These are trace lengths, NOT accuracy scores. A previously selected
sign control has passing occluders; the new filter correctly exposes a confidence
stop there, so the unoccluded building window is the actual stability control.
The original failed sign assertion and subsequent comparison are retained.

Actual Full/original video decoding and Native AP1 pre-grade tracking were also
observed:31samples for the fixed window,13 for the left walker before Ambiguous,
and22 for the occluded sign before LowConfidence. Analysis leaves the document
and history unchanged. Diagnostic input interpretation explicitly uses BT.709
limited-range/left chroma; the source's actual colorimetry is not established.
The initial missing-chroma refusal is preserved, not bypassed. No footage or
derived images are bundled; source URL/hash and reproducible opt-in tests are.

Fresh aggregate: 3122 library tests passed,
10 explicitly ignored; 239 integration checks
passed, 2 explicitly ignored. Two external-footage observations were
run explicitly and separately. Clippy, native build, tool documentation,
formatting and whitespace passed. No GUI interaction changed in this increment;
native UI version/preview acceptance remains the checkpoint114 baseline.

The fixed-reference translation tracker remains limited for appearance changes,
occlusion, scale and rotation. Longer traces do not establish general tracking
readiness; stronger tracking and broader workflow/hardware acceptance remain
open. No Git publication or work on the user's PC.

### Optional bidirectional short-term window tracking — 2026-10-09 EDT

`track_grade_window.method` now accepts `fixed_reference` (unchanged default) or
`adaptive_bidirectional`. The optional mode updates a previous-frame template,
uses the same NCC confidence, ambiguity and search-edge guards in both directions,
and rejects forward/backward roundtrip error above one analysis pixel. It retains
one bounded prior analysis plane and commits it only after both directions and a
final cancellation check succeed. Existing 121-sample/120-second/input/analysis
bounds remain. Integer analysis anchors are separated from the original user
center offset: an initial prototype accumulated subpixel drift on static footage;
that failure is retained and corrected by a sixty-frame static regression.
This is a translation-only NCC tracker using the forward/backward consistency
principle, not an implementation of MedianFlow or a semantic object tracker.

Proposals identify the method and adaptive samples report roundtrip error.
Old proposals without these fields decode as fixed-reference; invalid/missing
adaptive error provenance refuses apply. Existing partial-result, locked-target,
stale-revision/source and key-replacement checks remain. GUI selection is explicit,
warns about residual drift, clears prior proposals on method changes and resets to
fixed-reference whenever the dialog opens. Analyze remains read-only; apply is one
undoable edit. Scale, rotation, perspective and occlusion recovery remain open.

Twenty-five focused tracking checks pass, including Legacy/Native forward/reverse
engine cases, gradual appearance changes, static stability, cancellation/rejected
state, old serialization and invalid-provenance refusals, and the complete adaptive
sixteen-frame balance/match/version/save/export/reimport/archive workflow.
The native GUI produced sixteen samples, maximum roundtrip error 0.005 analysis
pixels, one apply edit and exactly restored current grade on undo. The synthetic
GUI setup had a stale selected-version snapshot after manually removing current
keys; normal editing synchronized that snapshot. This fixture difference is
recorded, not misreported as byte-identical full-document undo. The consistent
MCP version/save/archive workflow passes. Reopening defaults to fixed-reference.

Eight actual Native Full/original video observations compare both methods on the
same pinned three-second OpenCV sample. Fixed window: 31 samples for both, within
one analysis pixel. Left walker: 13 fixed versus 4 adaptive; middle walker: 5 fixed
versus 15 adaptive, then the roundtrip guard stops it. Occluded sign: 22 fixed
versus 31 adaptive. These are trace lengths, NOT accuracy scores. Adaptive can
perform worse, so it is opt-in and does not establish subject identity, occlusion
recovery or general tracking acceptance. All eight observations preserve document
and history. Source colorimetry remains unknown; explicit BT.709 limited/left is
diagnostic. No external footage or derived images are bundled.

Fresh full qualification: 3127 library tests passed,
10 ignored; 239 integration checks passed,
2 ignored. External CPU/native observations ran separately.
Clippy, native build, generated tool documentation, formatting and whitespace pass.
The full roadmap remains incomplete; no Git publication or PC work.

### Saved alternate-grade dependency protection — 2026-10-09 EDT

A regression reproduced a real cleanup defect: `unused_assets` considered a LUT
unused when its only reference was in an inactive saved clip-grade version.
The scan now counts every saved version's grade, including disabled operations,
bypassed grades and an empty current grade. The existing Media Pool count and
Remove unused action use this same scanner. No user media has been cleaned.
Explicit single-asset removal behavior is unchanged.

Locked-track LUT interpretation/hash changes now also inspect saved versions,
including those reached through a nested sequence protected by a locked parent.
Both mutations fail without state changes until the relevant track is unlocked
or the dependency is removed. Saved alternatives are project dependencies even
when they are not the current rendered grade.

Focused regressions verify actual orphan removal, retained inactive dependencies,
one-batch exact undo, idempotent repeated cleanup, serialization, direct/nested
lock protection and release after the final version reference disappears. An
archive integration test cleans a synthetic project, archives it, removes the
original LUT, moves/reopens the archive and activates the formerly parked version;
its LUT bytes and full-content hash remain exact. Existing archive copying of all
pool assets is unchanged. These checks do not claim every unknown future schema
or undo-history-only dependency is portable.

Fresh full qualification: 3129 library tests passed,
10 ignored; 239 integration checks passed,
2 ignored. Clippy, native build, generated tool documentation,
formatting and whitespace passed. No GUI controls changed; native interaction
acceptance remains the checkpoint116 baseline. No Git publication or PC work.
The full professional-grading roadmap remains incomplete.

### Explicit shared-look named versions — 2026-10-09 EDT

Project shared looks now support up to 64 named versions with unique trimmed
1–128-character names. The selected linked clip must address its exact shared
look stage. GUI Advanced controls explicitly label SHARED LOOK VERSIONS and warn
that activation changes every linked shot. MCP `grade_version` accepts the same
explicit shared-look stage; omitted/null stage retains ordinary clip behavior.
Local and group stages are rejected rather than silently changing clip versions.

Add, activate, rename and remove are one undoable whole-look command. Any linked
locked track across sequences refuses edits. Shared grade and scalar keyframe
edits synchronize the selected snapshot. Deleting an active version keeps the
current rendered grade. Make Independent copies only that current look. Saved
inactive versions retain LUT dependencies and protect locked metadata changes.
Empty additive fields are omitted, retaining old-document compatibility.

Seven focused tests cover ownership, no-op/refusal/read-only behavior, bounds,
locks, serialization and exact history. Actual Native AP1 renders verify repeated
version switching on two sequences and an independently detached shot. A moved
archive cold-opens and activates a saved version, then exports 30 ProRes frames;
all decoded RGB/alpha samples and the manifest are checked. The native GUI also
activated Bright, saved it, added/renamed/deleted a copy, preserved the rendered
grade after deletion and undid all four actions. The final saved document payload
matches the original exactly, excluding only format/history save envelopes.

Fresh full qualification: 3136 library tests passed,
10 ignored; 239 integration checks passed,
2 ignored. Clippy, native build, generated MCP documentation,
formatting and whitespace passed. Existing compiler warnings are retained. These
synthetic SDR checks do not qualify HDR, representative footage, local/group
versions or shared reference ownership. The full roadmap remains incomplete.

### Explicit immutable reference-gallery reuse — 2026-10-09 EDT

References can now be copied to another sequence-owned gallery through a GUI
explicit destination selector or MCP `copy_reference_still`. A copy receives a
fresh gallery ID while preserving the exact image asset, full image hash, Native
balance summary, source clip/time/revision and original capture-gallery provenance.
The origin remains stable on repeated copies and can outlive its source gallery.
No live link is implied: removing a gallery entry does not remove the other's
entry or image. No image files are copied or deleted by the action itself.

Copying requires an identical color configuration and canvas dimensions. It maps
the reference's local format index to the destination active format only after
validation. Same-sequence, missing, incompatible, invalid-name and destination
capacity (256) cases refuse without mutation. Existing comparison independently
verifies image bytes; a changed or offline file is not made trustworthy by copying.
The shared core action uses one undoable gallery edit and does not clone the whole
project for validation.

Three focused tests qualify provenance, identity, exact undo/redo, independent
removal and asset reachability. An actual Native workflow captures a nonactive
sequence, copies to a matching destination at a different format index, compares
and applies a one-stop AP1 shot match, rejects a changed PNG, removes the original
gallery entry, archives/moves the project and deletes the original PNG. Cold
archive comparison still succeeds with exact bytes and Native summary. The first
fixture attempt bypassed the required history reset after direct document setup
and timed out; the corrected fixture follows the normal revision contract.

The native UI selected a destination, copied one still, saved through File Save,
verified the destination ID/hash/summary/provenance on disk, and undid the single
history entry. Saved document payload returns to the original after only the
recorded serde-default empty-gallery normalization and save-envelope exclusion.
Control+S did not save in this observed automation session; File Save and Save all
and quit did. This observation is not yet attributed to product or input tooling.

Fresh full qualification: 3139 library tests passed,
10 ignored; 239 integration checks passed,
2 ignored. Clippy, native build, 523-tool generated MCP
 documentation, formatting and whitespace passed. Existing warnings remain.
Live shared galleries, broader reference ownership arrangements and the full
professional-grading roadmap remain incomplete.

### Native Save shortcut and menu parity — 2026-10-09 EDT

The earlier Control+S no-op is now traced to a missing file.save command/shortcut
route, rather than left as an input-tooling uncertainty. A remappable primary-
modifier S command now invokes the existing atomic save_tab path, including saved
history, dirty-state acknowledgement, failed-write handling and the existing
untitled Save As flow. File > Save uses that same command. The existing text-input
shortcut gate is preserved; this is not a claim of shortcut activation in every
focused text widget.

Three focused tests exercise actual egui Ctrl/Cmd key events, key remapping,
plain-S refusal, exact persistence without document/history edits, and failed
writes preserving dirty state and destination contents. Native desktop validation
made a reference-gallery edit, pressed Control+S, checked the saved file, repeated
the shortcut with identical serialized content/history and closed cleanly without
an unsaved prompt. The source gallery and all copied image metadata remained exact;
the one gallery edit remained the only history entry.

Fresh full qualification passes 3142 library tests (10 ignored), 239 integration
checks (2 ignored), Clippy, native build, generated 523-tool MCP documentation,
formatting and whitespace. Earlier warnings and explicit platform/footage/HDR
limits remain. No Git commit/push or work on the user's PC occurred. The complete
professional-grading roadmap remains in progress.

### Editable closed Bézier grading mattes — 2026-10-09 EDT

Typed grade graphs now provide `bezier_matte {input,window,label}` alongside
primitive and qualifier keys. A closed path has 3..32 knots with absolute
normalized anchor/incoming/outgoing coordinates in -4..4. It uses even-odd fill,
symmetric pixel-metric feather (0..0.25 of the shorter frame dimension), inversion
and a minimum half-pixel antialias transition. It produces an opaque unassociated
key independent of the input image's RGB/alpha. Existing Union/Intersect/Subtract/
Multiply and Matte Apply nodes combine these keys and isolate corrections without
changing the established AP1 stage order. External `RotoMatte` remains unresolved;
this path does not silently reinterpret that older placeholder.

The GUI has an inset curve editor, draggable anchors/tangents, numeric control
coordinates, shape-preserving midpoint subdivision, knot removal, feather and
invert. MCP authoring uses the existing checked graph/whole-grade actions, retaining
locks, one-edit undo, named-version serialization and saved history. Static paths
are qualified here; animated control points and Bézier tracking are still open.

Adaptive de Casteljau flattening uses a 0.25 processing-pixel control-to-chord
bound, maximum subdivision depth 12 and 512 segments. Nonfinite or out-of-range
geometry is refused. Frames are capped at 16 megapixels and 512-million pixel-edge
work; exceeding the limit reports a computation error rather than truncating the
curve. This is a bounded implementation, not unrestricted high-resolution roto.
GPU keys agree with f64 CPU raster arithmetic across rectangle, curved oval,
self-crossing and off-frame paths, Legacy/AP1 inputs, source alpha 0/0.5/1,
multiple resolutions and all four Boolean modes. Rectangle coverage also matches
an independent analytic distance oracle. The CPU and GPU share flattened geometry.

Native MCP checks cover authoring/undo/redo, invalid and locked edits, raw-matte
inspection, save/history reload and three-frame ProRes delivery. Every decoded
RGB16 channel is checked against explicit AP1 interpolation and the output
transform (tolerance 500/65535); alpha is at least 65000/65535. The initial test's
fixed brightness-difference assumption, then its ACEScct interpolation assumption,
were corrected to the actual documented scene-linear AP1 graph edge contract;
those failed attempts remain in the evidence. The delivery manifest verifies.

Hands-on native testing exposed an existing graph-editor history classification
issue: a single tangent drag produced four discrete Undo entries. The fix captures
non-graph discrete changes before graph controls, then treats only reported graph
topology changes as discrete. A raw-pointer full-panel regression failed before
and passes after the fix. Native retesting confirms one drag/one Undo, separate
split and invert steps, saved coordinates and exact original document restoration
after three undos (excluding only format/history save envelopes).

Qualification is intentionally staged: the full pre-UI workspace passed 3147
library tests (10 ignored) and 241 integration checks (2 ignored), plus Clippy,
native build, formatting and generated 523-tool MCP docs. The later repair changes
only two GUI source files; all non-GUI candidate hashes are identical. After that
repair, all 592 GUI library tests, focused cross-layer Bézier checks, workspace
Clippy, native build, formatting and whitespace pass. This is not misreported as
a second fresh full-workspace run. Eight new focused tests cover this increment.
Original screenshots, native files, failed-before logs and export evidence are
retained. The complete professional-grading roadmap remains in progress.

### Bounded artistic Native blend modes — 2026-10-09 EDT

Native-managed track and clip/project Merge nodes now accept Color Dodge, Color
Burn, Divide, Vivid Light and Hard Mix. These are explicitly bounded artistic
overlap operations, not extended-range arithmetic. Straight AP1 source/backdrop
channels are clipped to [0,1] only for B(Cb,Cs); source-over retains the original
signed/highlight RGB in the non-overlap contributions, preserves alpha and applies
track/graph opacity normally. Full overlap can intentionally discard negative and
highlight values. The native track dialog states this limitation. This retains
spec03 section4.5's linear-light operand contract and does not claim Photoshop or
CSS encoded-color parity. Legacy raster/merge equations are unchanged.

Color Dodge/Burn follow the branch priorities in the
[W3C Compositing draft](https://www.w3.org/TR/2024/CRD-compositing-1-20240321/#blendingcolordodge).
Divide retains the repository's zero-source result of one. Vivid Light combines
Burn below the half-channel pivot and Dodge above it. Hard Mix uses the equivalent
algebraic threshold B+S >= 1, except black backdrop/white source remains zero,
avoiding division-roundoff flips at the exact mathematical threshold. This is a
Native-only arithmetic stabilization; the old Legacy function was not changed.

The dedicated NativeBoundedMerge IR/hash tag98 and GPU mode IDs32..36 keep the
bounded contract separate from Legacy, unclamped arithmetic and AP1 whole-color
selection. Domain, mode, arity and finite-opacity checks reject malformed input.
Track and graph lowering share the same path; animated opacity changes cache
identity. GUI draft roundtrip/undo and actual MCP track authoring/history,
PNG/scopes and five ProRes exports pass.

An independent f64 oracle exercises 525 CPU/GPU cases across five modes, seven
boundary/signed/highlight/storage vectors, five alpha pairs and three opacities.
Half-float storage can move a discontinuous Hard Mix threshold: the initial
unquantized GPU comparison failed and is retained. The final GPU oracle reads
actual source textures, independently verifies bounded input quantization, then
computes expected blending from those stored inputs. CPU checks use the original
f32 source contract. This qualifies the arithmetic in each storage domain, not
unquantized CPU/GPU pixel identity at discontinuities. Ordinary output tolerance
is .01; extreme half-storage channels use an absolute tolerance32. Zero opacity,
transparent inputs, black/white special cases and signed non-overlap are covered.

The current tree passes 3152 library tests (10 ignored) and 242 integration checks
(2 ignored), all-target Clippy, native build, formatting and whitespace checks.
There are five new focused tests plus expanded animated graph coverage. Generated
MCP documentation still describes 523 tools. Native desktop interaction details
and original screenshots are recorded in the attached checkpoint. Nonseparable
Hue/Saturation/Color/Luminosity and the wider roadmap remain open. No Git commit,
push or merge was made.

### Native AP1 nonseparable blending — 2026-10-09 EDT

Hue, Saturation, Color and Luminosity now use the bounded Native artistic overlap
contract. NativeBoundedMerge uses AP1 Y coefficients rather than Legacy's artistic
0.30/0.59/0.11 luma, with RGB max-minus-min chroma, SetSat/SetLum and bounded
luminance-preserving clipping. The pure existing SetSat kernel is shared without
changing its equations. The four choices have separate GPU IDs 37..40; the stable
serialized blend names and mode-sensitive cache identity are retained. Legacy
blending is unchanged. Existing Native track and clip/project Merge authoring,
including animated opacity, lowers through the checked shared builder.

As with the five preceding bounded modes, clipping applies only to the overlap
function. Original signed/extended color participates in source-over outside the
overlap; alpha and opacity retain their established meaning. These are creative
linear AP1 coordinates, not display-encoded or perceptual HSL matching and not
HDR-preserving overlap operations. The native dialog explains both bounded
clipping and AP1 luminance. The
[W3C nonseparable structure](https://www.w3.org/TR/2024/CRD-compositing-1-20240321/#blendingnonseparable)
is adapted to the project's normative linear-light space and AP1 Y.

An independent f64 reference constructs the requested chroma/luminance and clips
a ray from the neutral target luminance against the RGB cube. It does not reuse
the implementation's extrema-branch clipping. 720 CPU/GPU cases cover the four
modes, twelve neutral/tied/primary/signed/highlight/extreme vectors, five alpha
pairs and three opacities. Actual GPU source quantization is independently checked
before comparing its arithmetic. CPU tolerance is 2e-6 for ordinary output and
0.004 at extreme f32 magnitudes; GPU tolerance is 0.01, or 32 at extreme half-storage
magnitudes. The initial extreme CPU check incorrectly used sub-ULP absolute
tolerance; its failure is retained, and the corrected tolerance follows f32
spacing. AP1 luminance-target tests and an explicit Legacy green-channel reference
prove the intended difference in weights, while unchanged Legacy goldens pass.

Five new focused library tests and one GPU integration test cover validation,
track lowering/cache separation, AP1-Y identity, GUI roundtrip/undo and four actual
MCP ProRes deliveries. Existing animated graph coverage is extended. The initial full aggregate passed every non-video library, but a session test
still expected the newly supported Color blend to be rejected. Its negative
fixture now uses malformed opacity 2.0, preserving the fail-closed check. No
production implementation changed after that aggregate; source hashes and the
one-test diff are retained. The complete affected video library was rerun, then
all 243 integration checks, workspace Clippy, native build, formatting and
whitespace. Combined qualification is 3157 library passes (10 ignored) plus 243
integration passes (2 ignored); it is not represented as a fresh single full run.
Generated MCP docs retain 523 tools. Native desktop evidence covers the visible
contract, checked edit/save/history and restart/undo behavior described in the
qualification receipt. All 26 serialized blend names now have Native contracts,
with the explicit boundaries above; this is not complete pipeline/HDR acceptance.
The broader professional-grading roadmap remains incomplete. No Git publication.

### Native Soft Light finite-intermediate repair — 2026-10-09 EDT

A targeted precision check found a real boundary failure in existing Native Soft
Light: opposite signed finite RGBA16F colors with alpha near 2e-6 overflowed the
straight-color f32 cubic. CPU produced NaN at zero opacity, and GPU produced
infinite channels. Ordinary generic Merge finite-overflow concerns were not
reproduced in the initial same-sign test; the retained failure specifically
establishes the Soft Light intermediate problem.

Native CPU Soft Light now evaluates the normative extended equation in f64 and
saturates only the final result to finite half-storage range. The GPU uses an
algebraically factored alpha-weighted overlap (internal mode 41), avoiding the
unbounded straight-color quartic. Valid finite half inputs and the established
1e-6 unpremultiply cutoff bound these f32 intermediates. Zero opacity returns the
backdrop exactly. Source alpha below the cutoff, backdrop alpha below it, signed
non-overlap and extended highlights retain the existing contract. Native-only
dispatch leaves the Legacy equations and serialized mode names unchanged.

Two new integration tests cover 198 generic extreme cases and 200 Soft Light
f64-reference cases, including opposite signs, source/backdrop alpha at zero,
cutoff and just above cutoff, unit alpha, and opacity 0, 1e-30, 0.125, 0.5 and 1.
The latter validates actual GPU source quantization separately before computing
its oracle. RGB CPU tolerance is max(2e-6, |reference|*2e-7); GPU is
max(0.01, |reference|*0.001), with alpha tolerance 0.0005. This makes no claim of
bit-identical CPU/f16 storage. Original failing logs are preserved. An initial
oracle test referenced an unavailable half crate; the harness instead checks
source quantization bounds without adding a dependency.

The entire workspace library suite passes 3157 tests (10 ignored), and all 245
integration checks pass (2 ignored), including unchanged Legacy goldens and the
existing actual Soft Light MCP ProRes export. Workspace all-target Clippy, native
build, regenerated 523-tool MCP documentation, formatting and whitespace checks
pass. No UI controls changed, and this increment makes no new manual UI claim.
This closes the reproduced numerical failure, not the broader professional
roadmap, representative-footage or external-hardware acceptance. No Git push.

### Native Normal small-alpha parity repair — 2026-10-09 EDT

An all-26-mode matrix at exact half-float alpha neighbors around the artistic
1e-6 unpremultiply cutoff reproduced twelve Normal CPU/GPU differences. The CPU
reference discarded a valid nonzero premultiplied source below that cutoff;
the existing GPU correctly used direct source-over. Signed or extended straight
color can retain meaningful premultiplied RGB even at very small coverage.

Native Normal now uses direct premultiplied source-over on CPU, without dividing
by alpha, matching the existing GPU fast path and the normative reduction in
03 §3.3. Final RGB obeys the existing finite RGBA16F storage boundary. The GPU,
Legacy CPU equations, serialized mode names and alpha cutoff for artistic blend
functions are unchanged. This does not silently clamp scene color to 0..1.

One new GPU integration test covers 1248 cases: all 26 modes, eight alpha pairs
including zero and exact half-float neighbors below/above 1e-6, three opacity
values and signed/positive source variants. Every source RGBA channel is read
back and asserted exactly equal before output comparison, so conversion rounding
cannot disguise a disagreement. GPU RGB tolerance is max(0.003, |CPU|*0.001),
and alpha tolerance max(6e-8, |CPU|*0.001). A new 60-case CPU library test checks
Normal against an independent f64 source-over reference across zero/tiny/unit
coverage, four opacity values and three magnitudes up to 65504; it explicitly
asserts the unchanged Legacy cutoff result. Zero-alpha fixtures carry zero RGB.
The reproduced failures and corrected checks are retained. An initial focused
unit invocation selected zero tests because of an exact short-name filter; the
corrected invocation and full suite execute the actual test.

Fresh qualification passes all 3158 library tests (10 ignored) and 246 integration
tests (2 ignored), including unchanged Legacy goldens and actual export checks.
Workspace all-target Clippy, native build, 523-tool MCP documentation regeneration,
formatting and whitespace checks pass. No UI controls changed and no new manual
UI claim is made. The broader roadmap, representative-footage and external
hardware acceptance remain unfinished. No Git publication or PC work.

### Fixed-topology animated Bézier windows — 2026-10-09 EDT

Closed Bézier graph windows now support explicit fixed-topology shape keyframes.
A separate AnimatedBezierMatte node serializes 1–128 strictly ordered nonnegative
integer-tick keys. Older readers reject this new node tag rather than silently
ignoring its animation. Existing static BezierMatte serialization is unchanged.
All keys retain the same knot count. Anchors, incoming/outgoing handles and feather
use hold or linear interpolation; invert remains held. Sampling clamps before
and after the key range and computes time differences in i128 before conversion.
Invalid or degenerate sampled geometry produces a located native diagnostic and
transparent failed frame; actual export refuses it without a partial final movie.

The Color controls explicitly enable animation, add/select a sampled key at the
stage playhead, edit the selected key, remove keys, choose Hold/Linear and freeze
the current sample to a static shape. Knot insertion/removal is disabled while
animated. Clip, asset, local/shared look and group scopes use the selected member's
clip-relative time; track/master use sequence time. Playback outside the selected
member disables key creation/freezing. Existing checked whole-grade history,
locks and MCP graph authoring remain in use; no new shader or MCP tool is required.

Nine new library tests cover bounded serde, topology/times, interpolation at large
ticks, atomic insertion, read-only viewing, live stage clocks, single-gesture
undo, eight compiler scopes and actual three-frame PNG/ProRes delivery. The MCP
fixture checks every RGB16 pixel within500 of its independently evaluated window
reference and alpha at least65000, save/load, locked edits, undo/redo, read-only
inspection and degenerate-export rejection. All18 focused Bézier tests pass.
Test-only compilation/assertion issues and the precisely restored scripted fixture
edit are recorded in retained logs; no implementation was silently dropped.

Fresh full qualification passes3167 library tests (10 ignored) and 246 integration
tests (2 ignored), all-target Clippy, native build, 523-tool documentation and
formatting. The final whitespace gate initially caught trailing blank lines in
three previously appended documentation files; trimming only their EOF whitespace
and rerunning the gate passed, without any implementation change after tests.

Native desktop review created two keys at0 and682080000 ticks, dragged the second
anchor, saved, froze the current shape, and performed two clean app restarts.
Visible History controls undo the drag in one transaction, redo it exactly, undo
freezing, and restore the original document exactly after the three authoring
transactions (excluding only persisted format/history metadata). Ctrl+S worked.
An attempted Ctrl+Z did not mutate that native state; keyboard undo is not claimed
verified by this review. Original screenshots and checked saved documents are
retained. The app is closed. Automatic Bézier tracking, changing topology and
compound/stabilization coordinate contracts remain open, as does the full roadmap.

### Qualification preferences isolation — 2026-10-09 EDT

The prior animated-window review's Ctrl+Z observation was explained by the
configured shortcut: the existing keymap roundtrip unit test had persisted
edit.undo=Ctrl+U into the isolated test home. It was not an animated-window undo
defect. The actual native Ctrl+U binding has now been exercised and restores the
original document exactly; history and Ctrl+S remain verified. One immediately
following Save preceded the UI's completed undo frame, so the final save was
performed after observing the new state and its file was checked.

Unit-test builds now resolve preference persistence to a per-thread UUID temporary
path, without mutating HOME/XDG_CONFIG_HOME. Production preferences paths and
behavior are unchanged; no existing preferences were reset or rewritten by this
fix. The import-keymap test now asserts the saved/reloaded values as well as the
in-memory result. Two additional tests cover parallel thread separation and
leaving the installed preferences file untouched. A hash comparison around the
complete suite confirms the configured application preferences stayed unchanged.

Fresh qualification passes3169 library tests (10 ignored) and 246 integration
tests (2 ignored), all-target Clippy, native build,523-tool documentation,
formatting and whitespace. This closes a test-isolation/qualification problem,
not another rendering capability. The full roadmap remains incomplete.

### Bounded 1080p multi-clock observation — 2026-10-09 EDT

The explicit ignored temporal harness now also accepts1080p; default720p and
ordinary32x16 tests remain unchanged. Raw source planes stream to FFmpeg stdin,
avoiding a2.49GB raw temporary. Every lossless decoded source sample is checked
before the independent native AP1/display reference is used. Production renderer,
cache, source-conversion and decoder code did not change in this increment.

On this unoptimized Cargo test build with software Vulkan llvmpipe, Full/original
1080p produced119 frames in6seconds with one source clock (59 dropped), and34
frames with four separated source clocks (137 dropped). Four-clock Draft960x540
produced39 frames (134 dropped). Removing the playback readback probe produced33
frames (138 dropped), so that probe alone does not explain the slow observation.
These measurements do not establish release-build or physical-GPU performance.

Each pixel-checked run also passed a60second repeated-seek loop across four target
times:5558 one-clock seeks,5299 four-clock Full and5307 four-clock Draft. Those are
repeated hot/cold target cycles with cache reuse, not thousands of distinct cold
frames. Peak decoded rings were62,208,000/248,832,000bytes for1/4 clocks; peak graph
GPU cache was about1.07GB, within reported budgets. Maximum checked pixel errors
were below0.00082 (gate0.012); all paused output pixels were verified, with one
pixel sampled during playback/seeks. Uniform synthetic frames isolate temporal
correctness and do not qualify representative footage. Documents/history stayed
unchanged. The no-readback control still checked its complete paused frame, but
makes no playback-pixel claim and had three transient evaluation misses.

Two ordinary temporal tests and all four explicit observations pass their existing
correctness/progress gates. Workspace all-target Clippy, formatting and whitespace
pass. The last complete production-tree qualification remains checkpoint127:
3169 library tests plus246 integrations,12 ignored. That whole suite was not rerun
for this harness-only extension. Real-time high-resolution acceptance remains open;
next diagnostics must distinguish development-build overhead, software-GPU work,
decode and source conversion before choosing a production optimization.

### 1080p diagnostic controls — 2026-10-09 EDT

A separate control compiled only photonic-core/render/video at opt-level2, with
incremental compilation disabled. Other dependencies remained development builds;
this is not the shipped release profile. Four-clock1080p Full produced36 frames
in6seconds with 138 drops, a small change from34/137. Removing the playback probe
had likewise not improved the baseline. Neither control establishes a specific
production bug or rules out all build/hardware overhead.

Temporary per-node CPU wall-time instrumentation then observed source-node median
23.1ms, Merge11.2ms and SDR-output31.8ms. These are enqueue/readiness/backpressure
wall times, not hardware GPU timestamps; startup waits inflate aggregate sums.
They indicate source handling and downstream passes both contribute on llvmpipe,
without proving an exact hardware bottleneck. That diagnostic published33 frames
with 137 drops and preserved checked pixels/document/history. No performance gate
was relaxed. The temporary evaluator instrumentation was removed and its complete
source hash verified equal to qualified checkpoint128 before packaging. The
harness and reports remain available; no production optimization was guessed from
these observations. Real-time performance requires further optimized-build and
physical-GPU qualification; correctness and bounded-cache results remain valid.

### Native title SlideUp entrance — 2026-10-09 EDT

Native titles now accept the existing None and SlideUp animation values. SlideUp
moves the entire painted title, including outline and background, from five percent
of authored frame height below its settled position while fading from transparent
to full opacity over the first 200 milliseconds of clip-relative timeline time.
The effect precedes clip composition and grading. At and after 200 milliseconds
the compiled graph is identical to the static title graph. No new persisted enum,
IR node, shader or MCP tool is introduced; Legacy rendering is unchanged.

The native Titles panel offers an explicit None / Slide up choice. This creates
one lock-aware, undoable source edit; merely viewing the panel leaves the document
unchanged. Existing numeric gesture coalescing remains intact. Typewriter,
FadeWords and karaoke are still refused for native titles without qualified
word-timing/paint semantics; this does not finish general title animation.

Four new library tests cover clip-clock graph structure and static endpoint,
read-only UI, discrete reversible/locked edits, and actual MCP preview/export.
Seven 128x80 Full PNG frames verify whole-paint opacity and displacement against
the settled frame; midpoint is two pixels down at half opacity. Three half-size
Preview API outputs verify the corresponding one-pixel displacement. Retained
files labeled draft use quality=preview with scale=0.5, not an independent raw
native Draft full-frame assertion. Seven decoded ProRes frames verify alpha at
16-bit tolerance 700; an independent ProRes RGB oracle is not claimed here.
Export manifest, document/history stability, save/load and undo/redo are checked.

Native desktop review visibly exercised start, frame 3 and frame 6, saved the
single edit, cold-reloaded its history, then used the configured Ctrl+U shortcut
and saved an exact restoration of the original document (excluding only format
and history envelopes). Original screenshots and saved documents are retained.
Fresh full qualification passes 3173 library tests and 246 integration tests,
with 12 explicit ignores, all-target Clippy, native build, generated 523-tool
documentation, formatting and whitespace checks. The full roadmap remains open.

### Exact JSON precision and real-footage delivery workflow — 2026-10-09 EDT

A new opt-in actual-footage combined workflow exposed a reproducible integrity
failure: the default JSON parser changed a tracked subpixel f64 key by one ULP
on reopening, so a legitimately published render manifest failed its exact
snapshot identity check. The value 0.09638437298605207 changed from bits
4591609646683394096 to 4591609646683394097. This was a persistence/parser defect,
not a reason to relax manifest verification or round authored tracking data.

The workspace now enables serde_json's existing float_roundtrip feature, without
changing dependency versions or Cargo.lock. Three ordinary regressions cover the
observed values and numeric extremes, 10,000 deterministic bit patterns each for
finite f64/f32, and complete timeline/embedded-document manifest roundtrip plus
tamper rejection. Both timeline and embedded-document integrity checks remain
strict. Native application open/save/cold-restart/save preserves the complete
fixture document exactly, excluding only its format/history envelopes; the
formerly failing key retains its exact bits and no history edit is introduced.

The opt-in end-to-end test uses the first 16 frames of the pinned public OpenCV
vtest sample, 768x576 at 10fps. It captures a reference, proposes and applies a
known exposure correction, verifies undo/redo, tracks a static architectural
window, switches untracked/tracked versions, saves/reloads, renders all frames,
exports and verifies a ProRes manifest, decodes delivery, reimports/probes and
cold-renders a portable archive. All 21,233,664 RGB comparisons between sRGB PNG
preview and decoded BT709 ProRes, with explicit transfer conversion, give maximum
error 0.0114191 and RMSE 0.00206092 against preselected 0.04/0.01 limits; alpha
is also checked. This is delivery consistency, not an independent color oracle.
Camera colorimetry is unknown and the diagnostic interpretation is explicitly
BT709 scene, limited range and left chroma. A static target and 1.6 seconds do
not qualify moving-subject accuracy, occlusion or broad representative acceptance.
External media and derivative images/video are not redistributed in this archive;
source URL/hash and numeric reports are retained for an explicit local rerun.

Fresh full qualification passes 3176 library tests and 246 integration tests,
with 13 explicit ignores, plus the separately invoked external-footage test.
All-target Clippy, native build, generated 523-tool documentation, formatting and
whitespace checks pass. An earlier interrupted aggregate produced no complete
result and is preserved as interrupted; the later full rerun supplies these
counts. This closes a concrete persistence defect and expands workflow evidence;
it does not complete the full roadmap or hardware/HDR acceptance.

### Catalog outgoing fades and narrow Inspector controls — 2026-10-09 EDT

All six known catalog transition kinds now follow the existing sequence contract
for outgoing edges: an eased opacity fade to transparent at a sequence end or
gap. Duration and easing apply; the stored kind and other parameters are preserved
but do not create a two-source wipe, push or color dip into empty space. Incoming
adjacent-cut transitions retain their existing kind-specific behavior. Unknown
kinds, invalid durations and nonadjacent incoming edges remain visibly refused.
No renderer, shader, serialization format or additional MCP tool is introduced.

The Inspector explicitly presents Out as Fade out and hides inert kind/direction
controls. A native review caught a clipped Remove button and explanation in the
narrow drawer; wrapped rows/text and stacked easing controls fix that layout.
Tests cover 140/180/220/320-pixel drawers without horizontal overflow or document
mutation. Native Remove makes one edit, Undo restores the exact original document,
and cold reopening/saving retains its original Wipe kind and all authored data.
The title is visibly faded at frame 22/30 after reopening. Only format/history
wrappers are excluded from exact document comparison.

Compiler coverage compares 432 programs across six kinds, four curves, nine
clocks and sequence-end/gap contexts. Actual PNG and ProRes delivery is exercised
for Solid, Title, Vector and Nested sources across all six kinds, with 24 retained
case folders. Three PNG center-alpha samples and five decoded ProRes center-alpha
samples per case meet their existing tolerances; manifest identity, save/undo/redo
and read-only render behavior are checked. These center samples are not an
independent all-pixel RGB oracle.

Verification is staged: the complete pre-layout run passes 3179 library and 246
integration tests, with 13 explicit ignores. After the GUI-only layout correction,
all 603 GUI tests pass, and final-tree all-target Clippy, native build, formatting
and whitespace checks pass. The unchanged core/render/MCP code retains the full
run's evidence; these counts are not described as a fresh full final-tree run.
Generated documentation still contains 523 tools. The full roadmap remains open.

### Caption clock overflow and complete current-tree regression — 2026-10-09 EDT

A boundary regression reproduced a panic in the existing FadeWords opacity
helper on an accepted native cue: cue 0..i64::MAX, word 0..1 and tick
 i64::MAX-1. Although the cue duration passes the existing checked-range guard,
subtracting the word's 150ms lead-in from that late tick overflowed i64.
The helper now computes elapsed ticks in i128 and clamps before converting to
its existing f32 opacity. Ordinary 150ms lead-in behavior is unchanged; no
animation timing, persisted format, shader or UI contract is added.

A native-batch regression failed with the original helper and passes with the
fix. A second test covers signed tick extremes and lead-in endpoints/midpoints.
All 34 focused caption tests pass. Fresh complete current-tree qualification
passes 3182 library and 246 integration tests, with 13 explicit ignores;
all-target Clippy, native build, 523-tool generated documentation, formatting
and whitespace checks pass. This complete run also includes the preceding
outgoing-fade narrow-Inspector correction. No native visual change is claimed
for this arithmetic-only fix. The full professional roadmap remains incomplete.

### Continuous scene-relative AP1 spatial scopes — 2026-10-09 EDT

Native scene-linear ACEScg taps now expose luminance waveform, RGB parade,
RGB waveform and Y/R/G/B histogram views through the existing bounded,
single-pending asynchronous scope readback. Positive straight AP1 values are
plotted as log2(value/0.18) over [-12,+12) in 256 bins; these are relative scene
stops, not nits, IRE, HDR mastering or calibrated display measurements.
All logical pixels contribute; source columns aggregate to at most 256 columns,
with at most 16,777,216 pixels and approximately 1.05 MiB accumulator storage.
Transparent/invalid pixels and per-channel nonpositive/below/above-range values
are counted separately instead of being clamped onto the graph edge.

Existing get_scopes and asynchronous measure_scopes expose the same tagged
scene domain and excluded counts. AP1 vectorscope and video-legal results are
explicitly unavailable/null; Program retains its qualified sRGB display path.
Existing Legacy/sRGB scope production math is unchanged. No new persisted
format, dependencies or renderer operation is introduced.

Four focused CPU/GPU tests cover independent numeric bin expectations, alpha,
extended values, logical padding, spatial/channel ordering and count conservation.
The initial GPU fixture used an older upload helper that flushes half subnormals;
rewriting that fixture with exact half bits resolved the test discrepancy without
changing the production algorithm. An initial MCP command matched zero tests
and is not evidence; the corrected exact native-managed ProRes workflow test
passes and verifies scene measurement/job parity and unchanged document/history.

Native cloud UI inspection covers floating waveform/parade/histogram, unavailable
AP1 chroma, Program sRGB switching, persisted docking on cold reopen, docked
RGB waveform and playback to the final valid frame. Measurements identify their
frame tick, revision and domain. Native visual checks used Draft preview; MCP
checks used Full/original. No full-resolution claim is made for Draft plots.
Close removes the panel and saving after viewing/playback preserves the authored
document exactly, with no history edit. Narrow monitor transport time readout
can still truncate when both side drawers are open; scope timestamps and bottom
status remain visible. Scope Close correctly wraps. Further workspace layout
and per-measurement quality/proxy labeling remain useful follow-up work.

Fresh complete current-tree qualification passes 3186 library and 246 integration
tests, with 13 explicit ignores. All-target Clippy, native build, generated
523-tool documentation, formatting and whitespace checks pass. Original UI
captures, native fixtures and receipts are retained. No repository commit/push
or release acceptance is claimed. HDR absolute scales/output, high-resolution
scope throughput and the broader professional roadmap remain incomplete.

### Measured scope sampling provenance — 2026-10-09 EDT

Scope measurements now label the measured frame's Draft/Full quality, logical
tap dimensions and proxy policy. The proxy label describes policy (auto,
prefer proxy or original), never an unsupported claim about actual source use.
Results are invalidated when quality, policy or logical dimensions change,
while normal playback retains the existing explicitly timestamped prior result
until the next asynchronous measurement arrives. Labels derive from the stored
measurement stamp, not newer monitor settings. Rendering and scope numerical
math are unchanged.

The existing context-invalidation regression now covers these three settings
and every quality/policy label combination. All 603 GUI tests, all-target
Clippy and native build pass after the change. This is staged validation over
the preceding complete 3186-library/246-integration checkpoint; it is not a new
full-workspace test run. Native UI inspection verifies Close/reopen, Draft/auto,
Full/original and Draft/prefer-proxy switching on the same 128x80 title fixture.
Save preserves the authored document exactly. Original screenshots and receipts
are retained. Broader HDR, proxy footage qualification and the full roadmap
remain incomplete.

### High-resolution scene-scope timing diagnostic — 2026-10-09 EDT

An explicitly ignored, opt-in synthetic GPU test now exercises scene-scope
histogram and waveform count conservation at 512x288, 1920x1080 and 3840x2160,
with three timed measurements at each size. Output storage remains 1,052,728
bytes. Timings include pipeline creation, dispatch and readback, excluding
input upload; they are development-build llvmpipe observations rather than
hardware GPU throughput, interactive acceptance or statistical percentiles.

Initial total readback observations (milliseconds) were:
- 512x288: 13.44, 10.30, 7.12
- 1920x1080: 31.59, 22.63, 24.51
- 3840x2160: 109.20, 69.86, 68.20

A workgroup-local histogram experiment preserved the focused numerical tests
but was slower: 1080p 49.52/36.38/35.76ms and 4K157.79/119.87/118.05ms.
It was rejected, and scene_scopes.rs was restored byte-for-byte to checkpoint135.
A repeated baseline passed count conservation and recorded 1080p40.20/29.86/33.24ms
and 4K116.16/82.80/97.02ms, demonstrating timing variability as well as the
remaining software-GPU cost. No performance improvement is claimed.

Correction: this intermediate snapshot also retained an in-progress scope-pipeline reuse draft; see the subsequent complete-regression entry. The diagnostic test is not the only source change in that snapshot.
All 240 ordinary render tests pass, with that one test explicitly ignored;
its explicit diagnostic invocation passes separately. Four focused scene-scope
tests pass after restoration, and all-target Clippy passed for the retained test
with the baseline shader. This is staged validation over checkpoints134/135,
not a fresh aggregate run; a future aggregate now includes one additional
intentional ignore (14 total if the other suites remain unchanged).
Original before/experiment/restored timing receipts and rejected patch are
retained. Broader footage, scope scheduling, hardware performance and the full
professional roadmap remain unfinished.

### Scope pipeline reuse and complete regression — 2026-10-09 EDT

Correction to the preceding timing checkpoint: its source snapshot also retained
an in-progress ScopeKernel extraction and GUI reuse draft. The statement that
only the diagnostic test had changed was incomplete; that intermediate snapshot
was not a newly fully qualified release. This checkpoint supersedes it with
complete current-tree validation and native UI review.

Scope views now retain one immutable compute pipeline and binding layout for
their current shader/entry/dimension/configuration signature. Per-request
counters, uniforms and staging buffers remain fresh, preventing counts from
accumulating across frames or signals. A different GPU device identifier resets
the view's pipeline and pending/result state. This is a per-view cache within
the current GUI renderer lifetime, not a cross-device or persisted global cache.
Existing source-signal interpretation, sample math and measured provenance are
unchanged. The rejected workgroup-histogram shader remains reverted.

A focused GPU regression compares cached and uncached output exactly while
changing logical extent and Legacy/sRGB signal, then repeats the same check for
scene scopes. The high-resolution diagnostic now measures both routes in the
same invocation. Observed submit/setup times fell from roughly 4–6ms to
0.4–2.1ms with reuse. Total readback examples were 1080p uncached
27.06/19.73/19.89ms versus cached15.74/15.63/16.97ms, and 4K uncached
101.98/61.76/61.03ms versus cached58.63/57.32/74.03ms. These are three-sample
synthetic development-build llvmpipe observations, not percentiles, interactive
frame-rate acceptance or a hardware-GPU guarantee. Pipeline setup is excluded
from the cached measurements; input upload is excluded in both routes.

Fresh full current-tree validation passes 3187 library and 246 integration tests,
with 14 explicit ignores (including the opt-in diagnostic, which separately
passes). All-target Clippy, native build, generated523-tool docs, formatting and
whitespace checks pass. Native review of the final build verifies cold-open
Program SDR, selected Clip AP1, repeated playback to the final valid frame and
Full/original switching. Saving after viewing preserves the authored document
exactly. Original screenshots and verification logs are retained.

The full professional roadmap remains incomplete. High-resolution interactive
scope performance, broader tracking/animation, HDR finishing and end-to-end
acceptance still require work. Cloud disk headroom is now1.5GB; permission has
been requested to remove only11GB of obsolete, regenerable incremental compiler
cache before further heavy builds. No source/assets/checkpoints/evidence were
deleted, and no repository publication occurred.
