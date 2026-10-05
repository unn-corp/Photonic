# Professional color grading implementation

Status: Milestone 1A implemented; Milestone 1B, the Color workspace, and parts
of Milestones 3 and 5 are in progress. The full roadmap remains in progress. This is a staged implementation of the approved roadmap,
not a claim of Resolve parity, complete managed color, HDR mastering, or facility readiness.
Native-managed video-track SDR preview is now available under explicit limits;
qualified full-resolution ProRes MOV delivery is available. Other managed
timeline stages and export formats remain gated.

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

Remaining: dockable scope arrangements and complete saved workspace layouts. Establish ownership/identity before
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
soft-edge tests cover the same threshold behavior. Spatial matte denoise, blur,
and grow/shrink remain pending.
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
with Boolean masks and matte refinement. Define mask
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

Latest full library verification (2026-10-04): core **873**, render **195**,
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
