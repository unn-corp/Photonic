# MCP color-grading authoring notes

The generated [MCP API reference](../mcp-api.md) comes from the tool schema.
This manually maintained companion records grading contracts and workflow
limits. The [grading specification](../specs/video-editor/07-color-grading.md)
and [current roadmap](professional-color-grading.md) remain authoritative.

## Direct local clip-grade animation

Keyframe tools now accept `target: "grade_op"`, `clip_id`, and `op_id`.
Use the stable corrector ID returned inside that clip's Grade, including an
ordinary clip grading graph's correctors. `path` names a registered scalar,
such as `params.source_kelvin`, `params.stops[1]`, or `params.slope[0]`.
`set_keyframe` accepts `value: {"t":"float","v":3200}` and an interpolation
object such as `{"kind":"linear"}`. Times are clip-relative via the normal
`at_ticks`/`at_tc`/`at_seconds` fields. `batch_set_keyframes` groups entries into
one undo operation; a bad target/property/value rejects the entire batch.

`get_keyframes` and `copy_keyframes` are read-only and remain available on locked
tracks. `set_keyframe`, `remove_keyframe`, and `paste_keyframes` reject locked
owners; GUI keyframe edits use the same checked path. Grade values must be
finite Float values within their registered scalar range. Unknown loaded
tracks remain stored and readable; these tools do not create new unknown grade
properties. Coupled/animated rendering constraints, such as ordered tonal
boundaries, are still validated on resolved grades and can block export.

The Clip Inspector's embedded and floating Keyframes editor lists each local
grade corrector separately. The existing Float curve/Bezier/undo machinery
edits these rows. Color Controls points to this editor. Timeline automation
markers include grade lanes. Track/asset/master/group/shared-look grades,
composition-node embedded grades, structural curve vectors, power-window
animation and tracking are outside this new clip-local target surface.

## Direct qualifier cleanup

HSL qualifier ops optionally accept `qualifier_spatial: {denoise, grow, blur}`.
These static Native Managed controls refine the combined key/window before
source coverage: median denoise, grow/shrink within ±0.02, then Gaussian sigma
within 0..0.02 (fractions of the shorter logical frame dimension). Omitted or
neutral settings preserve the original path. Non-neutral Legacy evaluation
reports an unavailable-correction diagnostic. Whole-grade edits remain undoable.

### Kelvin grade operator

Full grade and grade-graph corrector objects may use kind `white_balance_cct`
with params.base `{ "kind": "white_balance_cct", "source_kelvin": 3200.0,
"target_kelvin": 6500.0 }`. Both finite temperatures must be within
1667..=25000 K. Equal values bypass exactly. This is explicit approximate
Planckian source-white to target-white Bradford adaptation, not a RAW camera
setting. Existing `white_balance` normalized temp/tint semantics are unchanged.
Animation uses the registered `source_kelvin` and `target_kelvin` properties.

### Tonal exposure grade

Full grade/grade-graph objects may use `tonal_exposure` with params.base
`{"kind":"tonal_exposure","stops":[1,0,-1],"boundaries":[-2,2],"softness":1}`.
Stops select shadows/midtones/highlights; boundaries and softness are stops
relative to linear Y=0.18. Bounds: stops -8..8, ordered boundaries -12..12,
softness 0.01..6. All six scalar paths are animatable. This applies linked RGB
exposure using the active working domain's linear-luminance weights. It is not
a display tone mapper. Equal zero stops bypass exactly.

## Track compositing authoring

`set_track_prop` additionally accepts `blend` (the existing snake_case
BlendMode name, for example `soft_light`) and `opacity` (finite 0..1).
Only supplied fields change, in one undoable edit. A no-op creates no history
entry. Audio tracks reject these image-compositing fields. A locked video/text
track rejects a changed blend or opacity, including a request that tries to
unlock and change compositing in the same call; unlock separately first.
Any rejected composite edit also leaves supplied name/settings unchanged.
Unsupported Native Managed blend modes can be represented in the project,
but their renderer refusal remains in force; setting a field is not render
qualification. Native GUI choices are limited to qualified modes.

The timeline track-name context menu opens Compositing with an explicit
Apply/Cancel draft. Opening, typing, dragging or dismissing does not modify
the document until Apply. Apply combines blend and opacity into one history
entry and rejects a stale draft if another edit changed those track values.
Document/sequence switches invalidate the draft. Historical serialized
SetTrackProp commands missing composite fields leave blend and opacity alone;
new commands capture both for exact undo/redo.

## Primitive window animation

The existing local `grade_op` keyframe target additionally accepts
`mask.center[0]`, `mask.center[1]`, `mask.size[0]`, `mask.size[1]`,
`mask.rotation` and `mask.softness`. A matching PowerWindow must already exist;
no window is created implicitly. Geometry uses normalized sequence coordinates,
size is half-extent, and rotation is radians. Sizes must be positive. Values
must be finite and representable as f32; feather keys and evaluated feather
values lie in 0..1. Gradient windows expose center, Y falloff and rotation,
not the inapplicable width/feather fields. Shape and invert remain static.

These paths live in the operator's existing params.tracks array, distinct from
its params.* primary fields. Unknown/dormant tracks are retained; clearing a
mask or changing to a shape without that field makes its lanes dormant. Use
current-runtime support when rendering projects with these new paths; old
builds may preserve unknown paths without evaluating them.

Keyframe time for the local clip grade is timeline-relative to clip start,
independent of source retiming. Window geometry is canvas-relative after the
clip transform; it does not automatically follow a moving subject. This is
manual scalar keyframing, not optical tracking, a Bézier mask path, or a
stabilization-coordinate workflow. The generic Float/Bezier editor and
set/batch/get/copy/paste/remove tools use the same checked grade target.
Invalid resolved animation diagnoses the corrector and prevents qualified
export instead of silently clamping. Inactive/disabled operators remain inert.

While window keys are active, Color Controls labels its disabled fields as
base values. The viewer outline samples the current window and is read-only;
edit its keyframes in Clip Inspector / Keyframes. Remove the active window keys
to return to static base/handle editing. This prevents a static drag from
silently fighting an animated property.

## Multiple primitive windows through the grade graph

After converting a grade to a graph, `effect_stack` with
`op="add_grade_graph_utility"` accepts
`grade_graph_node={"kind":"window_matte","input":0,"op":"<corrector UUID>","label":"Window"}`.
Use an actual image-node ID for input. The operator's animated mask is the key;
RGB parameters and source coverage are ignored. No mask means one; disabled means
zero; unresolved roto/invalid geometry fails qualification. Key-only sources are
parked until a reachable KeyMixer/MatteApply branch uses them. Query the grade for
assigned node IDs rather than assuming IDs in a live document.

Create one masked neutral corrector per independent window, use each WindowMatte
as a KeyMixer input, and route a distinct unmasked correction into MatteApply.
Union, intersect, subtract and multiply apply max, min, max(a-b,0) and a*b to the
soft weights. Mask scalar keyframes use the existing checked grade_op targets.
Use an explicit MatteRefine after a window key for spatial cleanup. Each graph
utility insertion is a validated atomic undo step, and typed image/matte ports
reject mismatches. `inspect_grade_matte` supports reachable Native clip and selected local/shared-look
window/mixed outputs as opaque raw grayscale without editing history. Group and
compound scope inspection and optical tracking remain separate work.

## Paired Native program/output measurement

`measure_native_output` takes a sequence_id, the usual at_ticks / at_tc /
at_seconds time selection and optional format_index. It reads the final Native
program immediately before the SDR output transform and the actual sRGB output
texture from the same Full/original EngineFrame. It returns a display PNG plus
separate unclipped, straight RGB min/max/mean and below-zero/above-one counts for
both stages, alpha extrema, covered/transparent counts and exact frame provenance
(document, sequence, tick, revision, snapshot generation and output content hash).

Covered pixels have equal weight regardless of alpha. Fully transparent pixels
are excluded from color measurements, with null extrema/means for an entirely
transparent frame. The AP1 Y histogram has 128 bins over [-12,12) stops relative
to 0.18, plus distinct nonpositive, below-range and above-range counts; all covered
pixels are accounted for. The Y coefficients are independently derived from the
[ACEScg AP1 primaries and white point](https://docs.acescentral.com/encodings/acescg/).
These are working-value measurements, not reconstructed scene radiometry, absolute
nits, video IRE, HDR delivery, waveform or vectorscope qualification. Display-referred
source material does not acquire a recovered physical scene exposure merely by
conversion into AP1. PNG quantization/clamping does not affect the reported float
readback statistics.

Legacy, unsupported color stages, tap fallback, cached playback, altered coverage,
nonfinite readbacks, changed document identity/revision and invalid frame/format
requests fail explicitly. Paired readback is bounded to 16,777,216 pixels; larger
formats are rejected rather than silently downsampled. This is a synchronous,
transport-serialized read-only MCP measurement surface. New interactive scene/HDR
scope plots and broader monitoring/performance qualification remain separate work.


## Bounded window translation proposals

`track_grade_window` is asynchronous read-only snapshot analysis. Required
clip_id/op_id/start_ticks/end_ticks address an ordinary clip's enabled ordered
ellipse/rectangle. Optional step_frames 1–60, format_index, search_radius 2–24,
minimum_score 0.5–1 and minimum_margin 0–0.5 expose bounded matching controls.
Poll get_job_status or cooperatively cancel_job. The job result holds center
samples, confidence/margins, frame/analysis dimensions, requested interval and
completion/stop reason. One tracking job per MCP session; at most 121 samples,
120 seconds, 16M input pixels and 256-pixel analysis side.

`apply_window_track(job_id)` is a separate mutation, exactly one undo step.
Existing center animation requires replace_center_keys=true. A partial result
requires allow_partial=true. Stale document/revision/source metadata, expired or
wrong-kind jobs, locked targets and invalid samples refuse before changing
history. Outside keys survive but adjacent interpolation may change. Other mask
properties are preserved. GUI calls the same analyzer and apply-command path.
Native/Legacy forward/reverse engine tests and an actual MCP async
analysis/admission/apply/repeat-refusal/undo regression pass. Broader tracking
models and the complete tracked-isolation delivery workflow remain open.

### Native top-level caption fills and animation

`set_caption_style` shares a partial style schema with title insertion. Native
caption tracks accept `clear_stroke:true`, finite bounded font/fill/layout,
`animation` (`none`, `slide_up`, `fade_words`, `typewriter`), and `word_pop`
with `active_color`/`inactive_color` hex strings. `clear_highlight:true` removes
an inherited highlight and cannot be combined with `word_pop`. Overrides and
undo/redo use the existing caption style command. Native title clips continue
to reject animations/highlights. Bounded outlines/rounded boxes are now qualified as described below; active
nested captions and unsupported per-word metric/paint overrides remain gated. Grading scopes measure the pre-caption image; export and viewer captions
are applied in display-linear Rec.709 after their matching SDR transform.

### Static native graph text

Create `{"op":"text","text":{...}}` using `add_graph_node`, then wire its
zero-input image output into a clip or project graph. `set_graph_text` takes
`graph_id`, `node_id` and a complete `text` payload: `text`, `font_family`,
`font_size`, normalized RGBA `fill`, and normalized `position`. It creates one
undoable payload edit, preserves wiring/params/layout, rejects invalid or locked
changes, and returns `changed:false` for an exact repeat. Native rendering uses
the qualified scene-linear AP1 title path; Legacy graph text remains unqualified.
The GUI's Apply/Revert draft controls use the same core edit constructor.

### Bounded title/caption outlines and backgrounds

The shared style object now accepts `stroke: {color: "#rrggbbaa", width: number}`
and `background: {color: "#rrggbbaa", padding: number, corner_radius: number}`.
Outline width is 0..8 pixels; box padding/radius are each 0..128 pixels.
`clear_stroke` and `clear_background` explicitly remove inherited paint and
cannot accompany a replacement value for the same field. Unknown style keys,
invalid colors and unsupported numeric bounds are rejected before history edits.

Native title clips render these as scene graphics; top-level captions render
in display-linear light after SDR output. Decorated SlideUp fades the entire
cue once; FadeWords keeps its word-geometry fade separate from fill alpha.
Uniform cue-level paint is required across word overrides. Underline, true
intra-glyph FillSweep, title-style animation and active nested captions remain
gated. The title inspector's Apply/Revert paint controls use the normal locked,
undoable clip-source replacement path.

### Native polygon shape authoring

`set_graph_mask_shape` edits an existing `MaskShape` payload as one reversible
history command, preserving node identity, edges, layout and geometry lanes.
The shape is `{"shape":"ellipse"}`, `{"shape":"rect"}` or
`{"shape":"polygon","vertices":[[0,0],[1,0],[0.5,1]]}`. Polygon payloads
require 3–128 finite normalized local-box vertices. Exact repeats are no-ops;
wrong node kinds, invalid payloads and locked clip graphs fail without mutation.
Unknown edit arguments and shape fields refuse. Native polygon rendering uses
even-odd fill and inward local-space feather; center/half-extent size, rotation
and feather lanes remain animatable. Individual vertices are static in v1.
Legacy polygon rendering is still unqualified.

### Caption active-word underline

Caption style edits now accept `underline: {"active_color":"#ff0000"}`.
`underline`, `word_pop` and `clear_highlight` are mutually exclusive; invalid
colors or conflicting fields reject without changing document/history. Glyph
fill stays unchanged. The active word's stroke supplies underline color/width
when present; otherwise the active color and a bounded font-relative width are
used. Native preview and ProRes delivery are qualified, including plain and
decorated caption workflows. Legacy glyph-only underline rendering remains
unqualified; true FillSweep remains gated in Native.

### Caption paragraph direction

Caption styles accept direction `auto`, `ltr` or `rtl` at track/cue scope.
Word-level direction edits refuse without mutation. Default Auto preserves older
files; explicit directions are stored without inserting control characters into
caption words. SRT/VTT/ASS export notes warn when explicit direction metadata is
not represented in those outputs. Native preview and ProRes qualification is
recorded in the current implementation checkpoint; true FillSweep remains gated.

### Qualified spatial caption sweep

set_caption_style accepts fill_sweep: {active_color, inactive_color}, mutually
exclusive with word_pop, underline and clear_highlight. Managed SDR preview and
ProRes use an actual spatial glyph split with wrapped-word progress, alpha and
caption animations. RTL uses WordPop; render_frame_at.caption_notes reports one
notice per active cue and optional render manifests record affected cues within
the exported interval. UI Apply/Undo/save and 36 LTR/RTL plain/decorated export
workflows pass. Legacy's previous approximation is unchanged. This does not
qualify nested captions, general per-word layout/paint or full multilingual UI.

### Native channel and mask ports

Existing add_graph_node/add_graph_edge tools now qualify managed ChannelSplit
ports 0=R, 1=G, 2=B, 3=alpha and ChannelCombine's matching inputs. Components
are unassociated scene AP1 signals. Image inputs connected to mask ports use
AP1 luminance; mask inputs connected to image ports become opaque grayscale.
Use ChannelCombine alpha for shaped transparency. Six clip/project authoring,
undo/redo/save and real preview/ProRes workflows pass; tool count remains 521.
Signals retain signed/HDR values within finite FP16 range; this does not add
float32 intermediates, AI MaskFromMatte or a live node-editor inset viewer.


## Exact clip/look output inspection

`inspect_grade_image` and `inspect_grade_matte` accept an optional `stage`:

- Missing, null or `{"kind":"clip"}` selects the ordinary clip grade
- `{"kind":"local_look"}` selects that clip's independent look
- `{"kind":"shared_look","id":"<look UUID>"}` selects its currently linked shared look

A shared ID must match the clip's actual current link and exist in the project;
this is not an arbitrary library-look preview with a fabricated input. Unknown
stage kinds/fields, missing IDs, the wrong link, bypassed grades and unreachable
outputs are refused. `graph_node_id` is local to that exact stage, so reused node numbers
cannot silently inspect an earlier clip grade. Responses include the resolved
non-null stage alongside revision, generation, tick and signal provenance.

Look inputs include preceding asset/group-pre/clip processing. The selected look
node excludes later look nodes, group-post, track and master processing. Image
statistics remain straight scene-linear AP1 over covered pixels; the accompanying
PNG alone receives the SDR display transform. Matte output remains opaque raw
unassociated weights without alpha division or display transformation. Inspection
is read-only even on locked tracks. Make Independent and Undo change the valid
address and invalidate stale selections; they do not change the stage's render
position. Group/compound output scopes and look-stage qualifier-input picking
are not covered by this interface.

## Selected look correction inputs

`sample_grade_input` and `inspect_qualifier` accept the same optional strict
`stage` object as output inspection. Missing/null still selects the ordinary
clip grade. Native Managed local and currently linked shared looks may be
addressed explicitly. `graph_node_id` selects an exact corrector or qualifier-key
utility input; omitted node selects an unambiguous image corrector, never an
arbitrary branch or a key-only utility. Copied operator IDs in another stage do
not make the clip input ambiguous.

Sampling returns bounded straight ACEScct/AP1 coordinates from before the
selected correction. The local/shared look input includes earlier clip grades
and excludes later look, group-post, track/master and output conversion. Key
inspection returns qualifier/window/spatial coverage, including source alpha;
this differs intentionally from unassociated raw graph-matte output inspection.
Responses include stage and frame provenance. Wrong links, detach/bypass,
inactive/unqualified inputs, zero sample coverage and document/revision changes
are rejected. Read-only operations remain usable on locked tracks. The Color
panel also offers read-only correction-input SDR images and qualifier-key
coverage views for these stages. Authored shared/local eyedropper edits are not
part of this read-only extension.

Current qualification: copied-ID regression and four real MCP
stack/graph, local/shared cases pass, including Make Independent/Undo, bypass,
source alpha and in-flight document-identity replacement. Final 2,974-library aggregate /9 ignored, rendering regressions, Clippy, native build and Full native-window acceptance pass; refer to the authoritative roadmap for scope and remaining work.

All four selected-stage inspection tools also bind the original document
identity, not only its history revision. Replacing the document during a pending
transport/readback cannot reuse the same revision to publish a result for the
previous request. The output-image regression initially failed this boundary;
the same identity guard now protects output images and raw-matte presentation.
