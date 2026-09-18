# 06 — Captions & AI: Provider Abstraction, Editing, Rendering, Interchange

**Normative for:** 10-mcp-tools.md, 11-testing-phasing.md. **Depends on:** 01-data-model.md §7 (CaptionTrack/Cue/Word/Style), §10 (undo), 02-engine.md §1 (threading), §2 (frame-graph IR), §7 (export). **Decisions:** D-04 (pluggable providers, hosted default, offline-capable non-AI path). **Capabilities:** CAP-009, CAP-010, CAP-011.

---

## 1. Scope

Scope per 00 §5 (document map). Covers: the AI provider trait abstraction (transcription + TTS), the auto-caption workflow, caption editing interactions, karaoke/animation rendering semantics, TTS voiceover generation, and subtitle interchange (SRT/VTT/ASS). Caption **data model** is 01 §7 and is not restated except where this doc adds semantics (line-wrap, style cascade resolution, animation timing). Timeline panel chrome (track lane rendering, panel layout) belongs to 04; this doc owns interaction semantics and the commands they issue.

Provider services are optional and pluggable per D-04: with no provider configured, every non-AI capability (manual caption authoring, editing, styling, interchange, karaoke rendering, export) works fully offline. Only CAP-009 (auto-transcribe) and CAP-011 (TTS) require a reachable provider.

---

## 2. Provider abstraction

### 2.1 Traits

`photonic-video` is thread-based, not `async`/`await` (02 §1: engine thread, worker pool, crossbeam channels). Provider calls follow the same model: a blocking trait method runs on a provider worker (part of the existing worker pool), progress streams out over a channel, cancellation is a shared flag. This keeps the AI path consistent with the rest of the engine rather than introducing a second concurrency model.

```rust
// photonic-video/src/ai/provider.rs

pub struct CancelToken(Arc<AtomicBool>);
impl CancelToken {
    pub fn is_cancelled(&self) -> bool;
    pub fn cancel(&self);
}

pub type ProgressSink = crossbeam_channel::Sender<ProviderProgress>;

pub enum ProviderProgress {
    Started,
    Uploading { sent: u64, total: Option<u64> },
    Processing { percent: Option<f32> },       // provider-reported; None if provider gives no signal
    Partial(TranscriptionResult),               // streamed partial words (transcription only, if supported)
    Done,
}

pub enum ProviderError {
    Unavailable,           // no network / service unreachable
    Unauthorized,          // auth token missing/expired/rejected
    RateLimited,
    InvalidRequest(String),
    Timeout,
    Cancelled,
    Other(String),
}

pub trait TranscriptionProvider: Send + Sync {
    fn id(&self) -> &str;                       // registry key, e.g. "hosted", "whisper-local"
    fn transcribe(
        &self,
        req: TranscriptionRequest,
        progress: ProgressSink,
        cancel: CancelToken,
    ) -> Result<TranscriptionResult, ProviderError>;
}

pub struct TranscriptionRequest {
    pub audio_path: PathBuf,                     // 48k mono WAV, sidecar cache dir (§3)
    pub language_hint: Option<String>,
    pub model: Option<String>,                   // provider-specific model id, optional
}

pub struct TranscriptionResult {
    pub words: Vec<TranscribedWord>,
    pub language: Option<String>,
    pub degraded: bool,                          // true if adapter approximated word timing from cue-level source (§2.2)
}

pub struct TranscribedWord {
    pub text: String,
    pub start: Tick, pub end: Tick,               // sequence-relative after §4 offset mapping
    pub confidence: Option<f32>,
}

pub trait TtsProvider: Send + Sync {
    fn id(&self) -> &str;
    fn voices(&self) -> Result<Vec<VoiceDescriptor>, ProviderError>;
    fn synthesize(
        &self,
        req: TtsRequest,
        progress: ProgressSink,
        cancel: CancelToken,
    ) -> Result<TtsResult, ProviderError>;
}

pub struct VoiceDescriptor { pub id: String, pub name: String, pub params: Vec<ParamSpec> }
pub struct ParamSpec { pub key: String, pub label: String, pub kind: ParamKind, pub range: Option<(f32, f32)>, pub default: f32 }
pub enum ParamKind { Float, Enum(Vec<String>) }   // UI builds controls from this — provider-agnostic panel (§6)

pub struct TtsRequest { pub text: String, pub voice: String, pub params: HashMap<String, f32> }

pub struct TtsResult {
    pub audio: Vec<u8>,                           // PCM/WAV bytes
    pub sample_rate: u32, pub channels: u16,
    pub word_timings: Option<Vec<TranscribedWord>>, // Some if provider aligns words to its own generated audio
}
```

Jobs share the engine's `JobId` space (02 §1) so GUI progress UI and `CancelExport`-style cancellation follow one pattern. `CaptionCmd`/`TtsCmd` (§3.4, §6) are only constructed on job completion — a cancelled or hard-failed job never touches the document.

### 2.2 Provider contract requirements

Any adapter implementing these traits — hosted or otherwise — must satisfy:

| Requirement | Level | Detail |
|---|---|---|
| Word-level timestamps | Required (transcription) | ≤ 50 ms granularity at the source; converted to `Tick` at ingestion. |
| Cue-only source (SRT/VTT-shaped provider output) | Accepted, degraded | Adapter splits cue text into words and distributes timing proportionally by character count across the cue span. Result marked `degraded: true`; no schema change in 01 — the approximation lives entirely in the adapter, not in `CaptionWord`. |
| Confidence score | Optional | Omitted if provider doesn't return it; UI simply shows no confidence indicator. |
| Language detection/hint | Optional | Passed through if provider supports it. |
| Chunking for long audio | Required (adapter concern) | Provider or adapter must split audio exceeding the provider's per-request limit and re-stitch timestamps by segment offset. Never Photonic's problem to solve twice — one chunking helper in the adapter layer, reused by any provider needing it. |
| Streaming partial results | Optional | If supported, adapter emits `ProviderProgress::Partial` for long jobs; GUI may preview-populate the caption track before job completion. |
| Auth mechanism | Required | Bearer token / API key; injected by the app-level provider registry (§2.3), never a trait parameter. |
| TTS voice discovery | Required | `voices()` must enumerate usable voices; UI never hardcodes a voice list. |
| TTS word-level alignment | Optional | If absent, a generated clip gets no auto-captions; user can still auto-caption the resulting audio clip via CAP-009 same as any other clip. |
| Timeout | Required (adapter concern) | Default budget: `2 × source audio duration + 30s`. Configurable per provider. |

### 2.3 Config model (pointer only)

Provider selection, endpoint URLs, auth token references (env var or OS keychain), and default model/voice choices live in an app-level provider registry — **not** in the Document (D-04: providers are pluggable and swappable without touching project files; matches SPEC's "media referenced not embedded" philosophy for anything environment-specific). Registry schema is out of scope for this doc.

### 2.4 Default adapter and optional local fallback

- **Default:** `HostedTranscriptionProvider` and `HostedTtsProvider` (`photonic-video/src/ai/hosted.rs`) target the user's own hosted transcription and TTS services (D-04). They implement the traits above like any other adapter — no special-cased calling convention elsewhere in the engine.
- **Integration note (decision gate before Phase 5 implementation):** the hosted services' exact request/response contract (endpoint paths, payload shape, auth scheme) is not yet pinned. This is the single sanctioned open item for this doc (tracked in SPEC.md Open Questions) — implementation of `hosted.rs` blocks on it, but the trait/adapter boundary above is normative today and does not change shape once the contract is pinned; only the adapter's internal HTTP glue does.
- **Optional post-v1 local provider:** `LocalWhisperProvider`, a second `TranscriptionProvider` impl over `whisper-rs` (MIT) wrapping whisper.cpp, small/medium int8 models, token-level timestamps. Fully offline. Selectable in the registry alongside the hosted provider. Not required for v1 acceptance (AS-1 uses the hosted provider); speced here so the trait boundary is proven to support more than one implementation from day one.

---

## 3. Auto-caption workflow (CAP-009)

### 3.1 Trigger and scope selection

User selects one or more clips, or a timeline range (in/out on the sequence), and invokes Auto-Caption (GUI action or MCP tool, CAP-019 parity — 10 owns tool wiring). Two audio-source modes, user-chosen at invocation:

- **Mixdown** — all enabled audio tracks over the selected range, summed (matches what a viewer actually hears; default for sequence-range selections).
- **Per-clip** — a single clip's own audio only (default when selection is exactly one clip; useful for isolating a voiceover track from background music).

### 3.2 Audio extraction

Engine renders the selected audio (mixdown or per-clip, per 09's offline mix path reused from 02 §7's export audio pipeline) to 48 kHz mono WAV via the ffmpeg sidecar, written to `<project>.photon.cache/ai/extract/<job_id>.wav` (sidecar cache dir convention, 01 §9). This file is the `TranscriptionRequest::audio_path`.

### 3.3 Provider call

`TranscriptionRequest { audio_path, language_hint, model }` submitted to the registry's active `TranscriptionProvider` (default: hosted) on a provider worker thread. Engine surfaces `ProviderProgress` as GUI status (Started → Uploading → Processing → Done), cancellable via `CancelToken` tied to the job's `JobId`.

### 3.4 Timestamp offset mapping

Provider timestamps are relative to the extracted WAV's start. Engine offsets every `TranscribedWord.start/end` by the extraction range's sequence-relative origin (mixdown: the range's sequence start `Tick`; per-clip: `clip.start`) so results land directly in sequence-tick coordinates matching `CaptionCue`/`CaptionWord` conventions (01 §7).

### 3.5 Grouping algorithm (words → cues)

Raw word stream becomes a `Vec<CaptionCue>` using these parameters (defaults; overridable per-project in caption settings):

| Param | Default | Meaning |
|---|---|---|
| `max_cells_per_line` | 84 (Latin) | **Half-width cells, not `char`s** — see the correction below. Per-language; caps cue-building only |
| `max_lines_per_cue` | 2 | |
| `min_cue_duration` | 0.8 s | cues shorter than this get merged forward |
| `max_cue_duration` | 6.0 s | cues longer than this get split |
| `gap_merge_threshold` | 250 ms | silence gap below this keeps words in the same cue |

Pass 1 (build):
```
cue = []
for word in words:
    if cue.is_empty():
        cue.push(word); continue
    gap = word.start - cue.last().end
    projected_chars = char_len(cue) + 1 (space) + char_len(word)
    prev_ends_sentence = cue.last().text ends with one of . ! ?
    if gap > gap_merge_threshold
       or projected_chars > max_chars_per_line * max_lines_per_cue
       or prev_ends_sentence:
        flush(cue); cue = [word]
    else:
        cue.push(word)
flush(cue)  // trailing
```
Pass 2 (repair):
- Any cue with `duration > max_cue_duration` splits at the largest internal gap nearest its midpoint; if no gap exists, splits at the nearest sentence-ending word nearest the midpoint; if neither, splits at the midpoint word boundary.
- Any cue with `duration < min_cue_duration` merges into the following cue (or the preceding one if it's the last cue), provided the merge doesn't exceed `max_chars_per_line * max_lines_per_cue`; otherwise left short (better than losing text).

Line breaks are **not** stored on `CaptionCue` — only words and their timings persist (01 §7).

> **Corrections ([42](42-localization.md)).** Three, and the first is a live defect:
>
> 1. **The budget unit is wrong.** `max_chars_per_line: 42` counts Unicode scalars, so a Japanese cue wraps at 42 characters where the correct budget is **13** full-width. Replace with a **half-width cell** count over grapheme clusters, weighted by East Asian Width, skipping zero-advance combining marks — deterministic integer arithmetic, safe to persist, and exactly the model Netflix's own style guides use. Budgets and reading speeds are **per-language** ([42 §6.4](42-localization.md#64-per-language-budgets)); `CaptionTrack` gains `language: Option<String>` (additive, no format-version bump).
> 2. **Render-time wrap does not consult this budget.** The claim that wrapping targets `max_chars_per_line` "and font metrics" is incorrect — the render path wraps on `style.max_width` alone, using shaped advances and UAX #14. **The character budget is authoring-time only**, and it must stay that way: cue boundaries are persisted, so they may never depend on font metrics, which vary per machine ([42 §6.1](42-localization.md#61-the-rule-that-decides-the-architecture)).
> 3. **Tokenization and sentence detection are Latin-only.** `split_whitespace` collapses an entire Japanese cue to one token; the terminator set is ASCII `.!?`. Both are corrected in [42 §6.5](42-localization.md#65-tokenization-terminators-reveal).

### 3.6 Commit

All cues from one job commit as a single undo step:

```rust
// extends CaptionCmd, 01 §10's TimelineCmd::CaptionEdit(CaptionCmd)
CaptionCmd::BulkInsertCues { track: TrackId, cues: Vec<CaptionCue>, replace_range: Option<(Tick, Tick)> }
```

If no caption track exists on the target sequence yet, track creation is folded into the same command (mirrors 01 §10's "first video-mode action creates it undoably" pattern for `TimelineProject`). `replace_range` lets a re-run over an already-captioned range atomically swap old cues for new ones.

### 3.7 Error handling

- **Offline / unreachable:** `ProviderError::Unavailable` → GUI toast, no document mutation. If a local provider (§2.4) is configured, offer it as fallback.
- **Auth failure:** `ProviderError::Unauthorized` → toast pointing at provider settings (app config, not document).
- **Timeout:** per §2.2 budget; treated as a hard failure unless partial results arrived first (below).
- **Partial results:** if the job produced some words before failing (crash mid-stream, provider-side truncation), the words already received are offered as a commit with a "partial — N seconds transcribed" warning; user may re-run Auto-Caption over the remaining range. Never auto-committed silently.
- **Hard failure with nothing received:** no `CaptionCmd` is ever constructed — document is untouched, consistent with 01 §10's undo-atomicity guarantee.

---

## 4. Caption editing UX (CAP-010)

Caption tracks render as a lane in the bottom timeline panel alongside video/audio tracks (04 owns the chrome); this section owns the interaction semantics and resulting commands, all via `CaptionCmd` under `TimelineCmd::CaptionEdit` (01 §10).

- **Inline text edit** — double-click (or Enter on selected cue) opens an inline editor over the cue's word tokens. On commit, new text is re-tokenized into words:
  - same word count as before → original per-word `start`/`end` kept (only text changes).
  - different word count → new words' timings redistribute proportionally (char-weighted) across the unchanged cue `[start, end]` span; user retimes manually afterward if needed (word-level retime, below).
  - Command: `CaptionCmd::SetCueText { track, cue, old_words, new_words }`.
- **Split cue** — user places a split point between two words; command `CaptionCmd::SplitCue { track, cue, at_word_index }` creates two cues, boundary at the midpoint between `words[i-1].end` and `words[i].start` (or exactly at that boundary if the words are contiguous).
- **Merge cues** — select two adjacent cues, `CaptionCmd::MergeCues { track, a, b }` concatenates their words in time order. Style resolution: the earlier (left) cue's `style_override` wins if present; if the left cue has none, the right cue's applies. Deterministic, no ambiguity left to authoring order.
- **Retime by dragging cue edges** — dragging a cue's start/end handle: `CaptionCmd::RetimeCue { track, cue, old: (start,end), new: (start,end) }`, clamped so it cannot cross into a neighboring cue's span. Same snap targets as CAP-002 clip edges (clip edges, playhead, markers) apply when the caption track is aligned to a voiceover clip.
- **Retime by dragging a word edge** — dragging one word's boundary inside a cue: `CaptionCmd::RetimeWord { track, cue, word, old: (start,end), new: (start,end) }`. Adjusts only that word; if the drag would overlap the neighboring word, clamps to the midpoint between them.
- **Style editor panel** — resolves and edits the cascade defined in 01 §7 (word `style_override` → cue `style_override` → track `style`). Selecting a scope (word/cue/track) and editing a field sets an override at that level; "clear override" removes it and the panel shows the next level's resolved value. Command: `CaptionCmd::SetStyle { track, target: StyleTarget, old: Option<CaptionStyle>, new: Option<CaptionStyle> }` where `new: None` means "clear, fall through to next cascade level" and `target: StyleTarget::{Track, Cue(CueId), Word(CueId, usize)}`.
- **Style presets** — named `CaptionStyle` bundles, persisted at app level (not document state, matching 01 §11's "render/export settings presets → app-level config" rule). Applying a preset sets the `style_override` at whatever scope is currently selected, in one command. Presets export/import as plain JSON (`CaptionStyle` is already `Serialize`/`Deserialize`).

---

## 5. Karaoke and animation rendering

Rendering reads `CaptionTrack`/`CaptionCue`/`CaptionWord`/`CaptionStyle` (01 §7) at frame-graph compile time (02 §2 step 5: `CaptionOverlay` from enabled caption tracks, cues covering tick `t`). All animation state below is **resolved at compile time** into the IR node's `CaptionBatch` — the evaluator stays time-ignorant, per 02 §2's normative rule.

### 5.1 KaraokeStyle timing (per word, at eval tick `t`)

For a word `w` with `[w.start, w.end)`:

- **FillSweep** — before `w.start`: fully `inactive_color`. At or after `w.end`: fully `active_color` (already-spoken words stay active-colored — standard karaoke read). Within the window: sweep fraction `f = (t − w.start) / (w.end − w.start)`; glyph renders as a left-to-right linear split at `f`, `active_color` on the left portion, `inactive_color` on the right (RTL: **FillSweep degrades to WordPop for RTL runs** — a binary colour swap is direction-agnostic, so it is correct rather than merely less wrong — and the substitution is reported once per cue. Paragraph direction is an explicit stored `TextDirection`, not inferred from the first strong character. See [42 §7.3](42-localization.md#73-refused-cleanly-in-v1)).
- **WordPop** — binary swap, no interpolation: `t` in `[w.start, w.end)` → render with `active_color`; else `inactive_color`. `KaraokeStyle` (01 §7) carries no separate pop-scale field, so v1 "pop" is a color swap only — no size/weight change beyond what `CaptionStyle.weight` already sets.
- **Underline** — same active-window test as WordPop, but instead of recoloring the glyph, draws an underline decoration (using the style's stroke, or `active_color` if no stroke set) beneath the active word only; all glyphs otherwise render at `fill`/`inactive_color`.

### 5.2 CaptionAnim behaviors

Applied independently of (and combinable with) karaoke coloring:

- **FadeWords** — each word's opacity ramps `0 → 1` over a fixed 150 ms lead-in ending at `w.start`, holds at `1` after.
- **SlideUp** — whole cue enters via vertical translate + fade over the cue's first 200 ms (`[cue.start, cue.start + 200ms]`), anchored at `style.position`. Cue-level, not per-word.
- **Typewriter** — characters reveal left-to-right, timed proportionally within each word's `[start, end]` window: reveal count at tick `t` for word `w` = `floor(char_count(w) * clamp((t − w.start) / (w.end − w.start), 0, 1))`.

### 5.3 CaptionOverlay IR batching and GPU/CPU parity

- Compiler (02 §2 step 5) emits one `CaptionOverlay` IR node per active caption track per compiled frame, carrying a `CaptionBatch`: the cue(s) covering `t` (non-overlapping cues per 01 §4 invariant, so v1 never has two cues open on one track at once), each word's fully cascade-resolved `CaptionStyle`, and the computed animation state (sweep fraction / pop bool / reveal count / fade opacity) baked as `ResolvedParams` — same "resolved at compile time" contract as every other IR op (02 §2).
- **GPU path** — the eval pass batches all words across all active cues into glyphon glyph runs (one text pass per frame, atlas-cached per the existing GPU text pipeline). Text shaping and line-wrap (§3.5's `max_chars_per_line`/`max_lines_per_cue` against `style.max_width` and font metrics) happen here, at render time, confirming why cues store words rather than pre-wrapped lines.
- **CPU parity path** — export determinism (SS-3) requires the CPU compositor path (`eval_cpu`, 02 §2) to implement identical wrap and animation-state math in the same shared reference arithmetic used elsewhere for GPU/CPU parity (03 §6), so burned-in export frames match preview within tolerance.

---

## 6. TTS voiceover (CAP-011)

- **Script mini-panel** (04 owns chrome) — text box, voice picker populated from `TtsProvider::voices()`, and a param panel built generically from each voice's `ParamSpec` list (§2.1) — UI never hardcodes provider-specific knobs.
- **Generate** — submits `TtsRequest { text, voice, params }` to the active `TtsProvider` (default hosted) as a cancellable job with `ProviderProgress`. On success:
  1. `TtsResult.audio` writes to `<project>.photon.cache/ai/tts/<hash>.wav` (hash below).
  2. A new `MediaAsset { kind: Audio, source: File { path }, content_hash }` is added to the media pool.
  3. A `Clip` referencing that asset is placed at the playhead on the targeted audio track (or a new track if none targeted).
  4. If `TtsResult.word_timings` is `Some` and the user has the "also caption this voiceover" option checked, the same grouping algorithm (§3.5) builds cues on the sequence's caption track (created if absent).

  All of the above commits as **one** undo entry:
  ```rust
  // sibling to CaptionEdit/AudioEdit under TimelineCmd (01 §10)
  TimelineCmd::TtsEdit(TtsCmd)

  pub enum TtsCmd {
      GenerateAndPlace {
          asset: AssetId, clip: ClipId, track: TrackId,
          caption: Option<(TrackId, Vec<CaptionCue>)>,
      },
      Regenerate { clip: ClipId, old_asset: AssetId, new_asset: AssetId },
  }
  ```
- **Regenerate** — editing text/voice/params on a TTS-generated clip and choosing Regenerate re-runs `synthesize`; on success, `TtsCmd::Regenerate` swaps the clip's asset reference (old asset garbage-collected on next sweep if no longer referenced elsewhere, per 01 §9 sidecar cache rules). Fully invertible.
- **Caching** — cache key `hash(provider_id, voice_id, params, text)` (xxh3, same approach as 01 §3's `content_hash`), stored under `<project>.photon.cache/ai/tts/`. Identical inputs on Regenerate hit the cache — no network call, instant. Reference-counted by the clips using them; swept alongside other sidecar caches (01 §9).

---

## 7. Interchange (SRT/VTT/ASS)

<!-- spec-assert: dep-absent subparse -->
<!-- SD-13 (27 §3): `subparse` was proposed but never adopted; the hand-written parsers stand. Pinned so a re-introduction reds the gate against this "not a dependency" claim. -->
Rust dependency: **subparse** was proposed (MIT/Apache-2.0) as a single parser for SRT/VTT/ASS, but is **not a dependency** — `captions/interchange/{srt,vtt,ass}.rs` are hand-written, which is also the precedent [34](34-interchange.md) follows. Retained here as rationale for the shape — one permissive dependency instead of format-specific ones (satisfies the no-copyleft constraint). `libass` is explicitly **not** a dependency: rendering is our own `CaptionOverlay` IR path (§5), never libass compositing.

### 7.1 Import mapping

| Source | Maps to | Notes |
|---|---|---|
| SRT cue text + timing | `CaptionCue.start/end` + `words` | No native word-level timing — words split from text, timing distributed proportionally by character count across the cue span (same approximation as §2.2's degraded-provider path; `degraded` semantics apply conceptually though this is import, not a provider job). |
| VTT cue + inline `<hh:mm:ss.mmm>` word timestamps | `CaptionCue` + `CaptionWord` timings | If present, real per-word timing is extracted; else same proportional fallback as SRT. |
| ASS `Dialogue` line text + Start/End | `CaptionCue.start/end` + `words` | Same proportional fallback unless karaoke tags present. |
| ASS `\k`/`\kf`/`\ko` tags | `CaptionWord.start/end` | Duration-based, cumulative from the line's start time; converted to `Tick`. Gives real word-level import for karaoke-authored ASS. |
| ASS `Style` (font, size, Bold, PrimaryColour, OutlineColour, Outline, Alignment, MarginV/L/R) | `CaptionStyle` (`font_family`, `font_size`, `weight` from Bold flag, `fill`, `stroke` from Outline+OutlineColour, `position` from Alignment+Margins) | Best-effort field mapping. |
| ASS `BorderStyle=3` (opaque box) | `CaptionStyle.background` (`CaptionBackground`) | Only this box form maps; other border styles map to `stroke` only. |
| ASS `\move`, `\fad`, transform tags, drawing commands, karaoke modes beyond `\k`/`\kf`/`\ko` | Dropped | Not representable in `CaptionStyle`/`CaptionAnim` v1. Import surfaces a non-blocking "N styling directives dropped" summary per file — never silent. |

### 7.2 Export mapping

- **SRT/VTT** — plain text export: cue's words joined with spaces (line-wrap is a player-side/visual concern, not stored — §3.5). VTT additionally emits per-word inline timestamp tags when the target player class is expected to honor them (optional, off by default).
- **ASS** — full-fidelity path: `CaptionStyle` → an ASS `Style` line; `CaptionWord` timings → `\k` tags (karaoke round-trip); `KaraokeStyle` mode maps to nearest ASS equivalent: FillSweep → `\kf`, WordPop → `\k`, Underline → `\k` plus a `\u1` override scoped to the active word (ASS has no per-word-only underline primitive — this is a best-effort approximation, flagged in the export summary, not silently perfect).
- **Burned-in export** — not a separate code path. Captions are `CaptionOverlay` IR nodes (02 §2 step 5); enabling caption tracks and rendering through the normal export path (02 §7) *is* burned-in export. No special "burn-in mode" exists or is needed.

### 7.3 Concrete acceptance procedures

Duplicates SPEC.md's CAP-009/CAP-010 tests as runnable steps:

**CAP-009 procedure:**
1. Import a clip with known speech (fixture with a hand-labeled transcript) into the media pool.
2. Select the clip, invoke Auto-Caption with the mock/hosted provider (per environment).
3. Wait for job completion; verify a caption track exists with cues covering the clip's audio span.
4. For each cue, verify every `CaptionWord.start < end` and words are chronologically ordered with no gaps larger than a single silence.
5. Diff transcribed text against the fixture's known transcript; verify word accuracy above the sample's tolerance threshold (11 owns exact threshold).

**CAP-010 procedure:**
1. On a caption track from the above, inline-edit one cue's text (change word count); verify `SetCueText` command in history and re-tokenized words retain the cue's original `[start,end]` span.
2. Split a multi-word cue mid-word-list; verify two cues result, boundary at the correct word index, both retrievable via undo/redo round-trip.
3. Merge the two split cues back; verify `style_override` resolution follows the left-wins rule (§4).
4. Drag a cue edge to retime; verify neighboring cue is unaffected and the moved cue's span updates.
5. Drag a single word's edge; verify only that word's timing changes.
6. Set a word-level style override, then a cue-level override, then a track-level style; verify the style editor panel shows correct cascade resolution at each scope per 01 §7.
7. Export the sequence with captions burned in; verify rendered frames show the edited text/timing/style (§7.2 burned-in = normal export).

---

## 8. Risks and test hooks (feeds 11)

| Risk | Mitigation |
|---|---|
| Hosted API contract unpinned delays Phase 5 | Integration note gate (§2.4) before P5 implementation start; trait/adapter boundary is stable regardless of final endpoint shape. |
| Provider network flakiness breaks CI | `MockTranscriptionProvider`/`MockTtsProvider` (fixture-backed, deterministic, no network) used by all CI-run tests; real hosted-provider tests are tagged and run in a separate network-gated suite, not blocking merges. |
| GPU/CPU karaoke math drifts (breaks SS-3) | Shared reference arithmetic for sweep-fraction/reveal-count/opacity, used by both `eval_cpu` and the golden-frame generator (mirrors 02 §2 / 03 §6 pattern). |
| ASS import fidelity loss surprises users | Dropped-directives summary surfaced at import time, never silent (§7.1). |
| Grouping algorithm misbehaves on edge-case speech (overlapping speakers, rapid interjections) | v1 assumes single-speaker or pre-mixed audio; multi-speaker diarization is out of scope for this doc — treat dense-overlap input as a known degraded-quality case, not a bug, until diarization is separately scoped. |

**Test hooks for 11:**
- `MockTranscriptionProvider`/`MockTtsProvider`: fixture audio/text pairs, deterministic word timings, wired into golden-frame corpus generation (no live network dependency for CI).
- Timing-accuracy fixtures: reference audio with hand-labeled word timestamps; assert adapter output within a defined tolerance.
- Grouping-algorithm unit cases (pure function, no engine needed): dense speech (all merge), long pause (forced break), long monologue (`max_cue_duration` split), punctuation-heavy input (sentence breaks), short-cue merge-forward.
- Karaoke golden frames: render a cue mid-word at several sweep fractions for FillSweep; compare against golden PNGs (ties into 03/11 corpus).
- ASS round-trip: import a `\k`-tagged file → export ASS → diff word timings within `Tick`-rounding tolerance.
