# Native matte worker foundation

Status: bounded NativeManaged composition integration is implemented. Actual
GPU, MCP preview/ProRes, missing-model refusal and relink/Undo checks pass. Final
aggregate acceptance and the contrast-improved native UI recheck pass.
This does not establish general footage quality or real-time segmentation.

## Model identity

The existing U²-Net-p dependency is retained. The review copy comes from:
https://github.com/danielgatis/rembg/releases/download/v0.0.0/u2netp.onnx

- File length: 4,574,861 bytes
- SHA-256: `309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8`
- Upstream published MD5: `8e83ca70e441ab06c318d82300c84806`, verified
- Published downloader: https://github.com/danielgatis/rembg/blob/main/rembg/sessions/u2netp.py
- RGB preprocessing reference: https://github.com/danielgatis/rembg/blob/main/rembg/sessions/base.py
- Original model project: https://github.com/xuebinqin/U-2-Net

`CachedMatteModel` only loads a provisioned local file. It never downloads,
accepts at most 8 MiB, verifies the pinned SHA-256 and loads the verified bytes
rather than reopening the path. Existing explicit background-removal behavior
is separate. No model file is included in the source patch/checkpoint.

## Numerical corrections and bounds

Public raster fields can bypass checked constructors. Input dimensions, byte
length and pixel limits are checked before loading a model or allocating model
input. The ImageNet normalization divisor uses RGB channels only; alpha had
incorrectly influenced the old divisor. Output must be exactly [1,1,320,320],
with finite samples. Normalization is binary64 to prevent finite extreme
predictions overflowing the range. A constant prediction produces zero weight.
These guards never substitute a fabricated opaque result for a failed model.

The pinned model ran on an actual synthetic subject: foreground center255,
background corner0, two direct evaluations exactly equal. A real asynchronous
worker evaluation also returned the expected subject and reused the identical
cached mask allocation. These checks do not qualify diverse footage, fine hair,
transparency, motion coherence or HDR scene interpretation.

## Scheduling and cache contract

- One inference worker per evaluator; model construction and inference off caller thread
- ORT intra/inter-op threads limited to one
- At most two outstanding requests, one queued behind the active request
- Nonblocking Pending/Ready/Failed/Busy responses
- At most eight cached results and 16 MiB cached mask payload
- At most four megapixels per prepared input
- Caller supplies an immutable content key covering source, dimensions, quality
  and interpretation; reusing a key for different pixels violates the API
- Optional preparation closure executes on the worker, allowing future GPU
  readback to stay off the engine thread; closure must bound its own allocation
- Source invalidation clears cached results and retries cached model-load failures
  on the same worker; stale in-flight results are discarded by generation
- Worker disconnect resolves explicitly; it cannot silently remain pending
- No blocking destructor join; an in-flight bounded inference can finish after
  the caller drops the worker, then channels disconnect

Deterministic tests hold inference behind a condition variable while checking
polling, deduplication and queue refusal. Other checks cover LRU/byte eviction,
invalid/oversized input, wrong output geometry, preparation failure and worker
termination. External references to returned Arc masks are caller-owned memory,
not included in the internal cache budget.

## NativeManaged integration contract

`NativeMatteExtract` (IR tag 74) consumes one scene-linear AP1 image and returns
raw subject weights as a scalar Mask signal. The input bridge unpremultiplies,
converts AP1/ACES-white to linear sRGB/D65, clips to 0..1 and encodes to 8-bit sRGB for
the pinned model. Exactly zero alpha has deterministic black RGB. The output
weights do not include source coverage: folding alpha into a mask would double
coverage when applying it to an already-alpha-bearing image. Mask→Image remains
opaque AP1 grayscale. An unwired node yields opaque weight 1 without inference.
This is an explicitly bounded inference encoding, not an HDR output transform.

GPU readback and inference run on the serial worker. Held source texture Arcs
prevent pool recycling during readback. Pending preview frames carry an explicit
provisional flag and visible status. Paused frames request another present until
ready; playback keeps its normal frame cadence. Exact MCP inspection and export
wait for completed weights or report failure. A completed/error generation changes
GPU cache identity so a provisional opaque result cannot masquerade as final.

The cache accommodates at most eight distinct masks and 16 MiB of mask payload at
one exact frame. A compile-time demanded-input check rejects frames that cannot
fit simultaneously. Each input is bounded to four megapixels; geometry must
match the canvas. Source relink/probe generations participate in GPU and worker
cache identity even when no vector document is attached. Invalidation retains
one worker, drops stale in-flight results and refreshes cached loader failures.
The render receipt records the pinned model hash when actually used.

## Verified evidence and remaining qualification

- Independent RGB/AP1 input checks, finite/geometry guards, actual worker-thread
  GPU readback and logical-region cropping
- Actual model pending→ready, quality/cache reuse and changed-source invalidation
  against separate direct-model reference masks
- Two clip/project MCP authoring/history/save paths with opaque/partial-alpha
  sources, raw-weight preview, three-frame ProRes/redecode, actual model receipt,
  and changed-picture relink/Undo recovery
- Missing model refuses exact preview promptly and publishes no export or manifest
- A 96×96 three-second playback run observed 90 ticks, zero drops, 89 provisional
  frames, 89 evaluations, maximum gap 45.81 ms and 35.8 MB peak GPU cache; pause/latest
  scrub settled to exact weights. This does not claim per-frame real-time inference
- Native Full output and opaque mask-image alpha were inspected. A low-contrast
  provisional label was found and given a dark, wrapping backdrop; native recheck passes

Historical failing tests are retained: scalar Mask→Image coercion was initially
missing; ProRes 12-bit opaque alpha expands to 65520 (not 65535); playback originally
performed 457 redundant evaluations; source invalidation reused stale masks.
Each has an explicit correction and targeted passing regression. Initial
unpublished alpha-folding semantics were removed to match raw-weight contracts.

Broader footage (hair, transparency, motion coherence), high-resolution/long-soak,
HDR model quality, arbitrary source geometry and external hardware remain open.
Legacy placeholder behavior is unchanged and is not claimed as qualified inference.
The model binary is not bundled in the checkpoint. Provision the exact pinned
model through the existing background-removal setup or a trusted local path;
after provisioning, re-probe the source or reopen the session to invalidate a
previous loader failure. Rendering itself never downloads a model.

Final aggregate: 2,965 library passes / 8 ignored, all targeted integration suites,
Clippy and native build pass with warnings. Source is local and unpushed.
