# 200 — X-3 EDL (CMX 3600) import/export; AAF and FCPXML via X-2 (mini-spec)

## Status: Draft mini-spec — pre-code gate

Written to satisfy the [26 §19.1](../specs/video-editor/26-kdenlive-mlt-parity.md#191-bands)
K-Band 5 exit condition: *"an accepted mini-spec exists **before** code, naming its
data-model change, migration, undo unit, MCP surface and acceptance fixtures. No
item here starts without one."* It carries **no code authorization**
([26 §2](../specs/video-editor/26-kdenlive-mlt-parity.md#2-clean-room-and-licensing-fence)
point 5); acceptance of this document is what authorizes X-3.

Owner docs: [34 §5](../specs/video-editor/34-interchange.md#5-x-3--edl) and
[26 §18 X-3](../specs/video-editor/26-kdenlive-mlt-parity.md#x-3--edl-aaf-fcpxml).

**X-3 is downstream of X-2.** [196](196-x-2-opentimelineio-interchange.md) §10
enumerates what X-3 inherits, and this document builds on it rather than
restating or forking it: `InterchangeReport` / `Unsupported` / `Approximation` /
`Location` in `interchange/mod.rs`; the `Tick` ↔ rational-time conversion and
rate-recovery ladder; the source-timecode rebase convention; the
one-batch-one-undo import commit shape; the five `DiagFamily::Interchange`
codes; the **Interchange** File-drawer column and the report sheet; and the
fixture-provenance discipline. Where X-3 deviates from a 196 commitment it says
so out loud and justifies it — there is exactly one such deviation (§8.3).

---

## 1. Problem and user outcome

**Today.** Photonic can exchange a timeline structurally with nothing. The only
interchange code in the tree is subtitle-shaped
(`crates/photonic-video/src/captions/interchange/`), and 196 is a proposal, not
code. A user handing a cut to a colourist, a conform house, an audio post house
or an online editor has exactly one option: rendered pixels.

EDL is the format those houses still ask for. It is thirty-five years old, plain
ASCII, ~60 bytes per event, and universally readable — Resolve, Avid, Baselight,
Flame, Premiere, Pro Tools and every telecine-era deck controller ingest one.
34 §1 places it correctly: **conform and colour round-trips**, "cuts and
timecode only".

**After X-3.**

1. A user can **export the active sequence's V1 as a `.edl`** and hand it to a
   colourist, who can conform it against the camera masters by source timecode.
2. A user can **import a `.edl`** — a conform list returned from an assistant, a
   telecine log, a colourist's re-cut — and get a real Photonic sequence of cuts
   and dissolves, with the media pool matched by reel/clip name and everything
   the format could not carry named in a report.
3. Where the EDL carries **ASC CDL** correction comments (`*ASC_SOP` / `*ASC_SAT`
   — the whole reason colourists still trade EDLs), those land as real
   `GradeOpParams::Cdl` ops rather than being discarded (§4.6).
4. In both directions the user is told, per item and per reason, exactly what did
   not survive — the never-drop-silently rule of 34 §2 and 196 §8.
5. A user with an **AAF or an FCPXML** is told, in the app, precisely what to do
   with it and why Photonic does not read it directly (§9). They are **not** left
   to discover it through a generic parse failure.

**The honest framing, stated first because everything else follows from it.** An
EDL is a far weaker document than a Photonic sequence. It expresses: one video
layer of cuts, simple dissolves and wipes, source and record timecode, an 8-character
reel name, constant speed, and free-text comments. It expresses no effects, no
transforms, no opacity, no blend modes, no nesting, no groups, no captions, no
multi-layer compositing, no audio levels, and **no frame rate** — the rate is
out-of-band knowledge the file assumes you already have. Export is therefore
*always* lossy and *usually* very lossy, and X-3's design centre is making that
loss legible rather than pretending it away.

**Non-goal.** EDL is not a Photonic project format and is not a substitute for
OTIO. If a user wants structural fidelity they want `.otio` (X-2); if they want
lossless they want `.photon`. EDL exists here because conform houses ask for it.

---

## 2. Current state in code

Exact, as of `feat/video-editor-module` @ `8a33f32`. Read this before disagreeing
with §5.

### 2.1 What exists and is directly usable

| Thing | Where | Note |
|---|---|---|
| `Timecode { hours, minutes, seconds, frames, drop_frame }` | `crates/photonic-core/src/timeline/time.rs:169` | K-A12 landed |
| `Timecode::parse_to_tick(tc, rate) -> Option<Tick>` — **real** SMPTE drop-frame | `time.rs:244` | `;` selects drop-frame; rejects `;` at a non-DF rate (`:261`), rejects `ff >= nominal` (`:258`), rejects the labels drop-frame skips (`:285`) |
| `Timecode::format_tick(tick, rate, start, prefer_drop)` | `time.rs:297` | adds the sequence start offset and labels — exactly the record-TC writer X-3 needs |
| `Timecode::from_frame_index` / `from_frame_index_drop` | `time.rs:192`, `:213` | |
| `FrameRate::is_drop_frame_rate()` — true only for `30000/1001` and `60000/1001` | `time.rs:157` | the authority on whether `FCM: DROP FRAME` is even legal |
| `FrameRate::nominal_fps()`, `ticks_per_frame()`, `frame_at()`, `frame_start()`, `snap()`, `is_exact()` | `time.rs:150,108,119,133,126,141` | |
| `Sequence.start_timecode: Tick`, whose own doc comment names `01:00:00:00` as the common delivery origin | `crates/photonic-core/src/timeline/sequence.rs:162-166` | the record-TC origin (§4.2) |
| `TimelineCmd::SetSequenceStartTimecode` + `ops::set_sequence_start_timecode` | `commands.rs:459`, `ops.rs:1266` | |
| `Clip { start, duration, source, source_in, speed, transition_in, transition_out, markers, link_group, … }` | `clip.rs:27` | half-open `start`+`duration` (PA-7) |
| `Transition { kind, duration, params }`, `TransitionKind::{CrossDissolve, DipToBlack, DipToColor, Wipe, Push, Unknown}` | `clip.rs:698`, `:719` | |
| `SpeedMap::{Constant(Ratio), Keyframed{keys}}`, `Ratio { num: i32, den: u32 }` | `clip.rs:375`, `:292` | `num: i32` — reverse speed is representable exactly |
| `ClipSource::{Asset, Vector, NestedSequence, SolidColor, Adjustment, Text, Unknown}` | `clip.rs:165` | `SolidColor` is the EDL `BL` reel |
| `Marker { at, duration, name, note, category, color, anchor }` | `sequence.rs:833` | K-A2; the `* LOC:` comment target (§4.7) |
| `CdlParams { slope: [f32;3], offset: [f32;3], power: [f32;3], sat: f32 }` and `GradeOpParams::Cdl { … }` | `crates/photonic-core/src/timeline/grade.rs:129`, `:172` | **already the exact ASC CDL shape** — §4.6 needs no new type |
| `MediaAsset { id, kind, source, probe, proxy, content_hash, bin, effects, grade, rating, tags }` | `media.rs:42` | `rating`/`tags` were added additively inside v5 (`media.rs:68-73`) |
| `TimelineCmd::SetAssetRating { asset, old, new }` + `ops::set_asset_rating` | `commands.rs:434`, `ops.rs:119` | the exact three-line shape §5.2's new command copies |
| `TimelineCmd::SetAssetMeta { asset, old_probe, new_probe, old_hash, new_hash }` + `ops::set_asset_meta` | `commands.rs:426`, `ops.rs:295` | a probe-side field rides this for free |
| `TimelineCmd::AddSequence { sequence: Box<Sequence> }` — a whole sequence, tracks and clips inline | `commands.rs:451` | why import is O(1) commands, not O(clips) |
| `Command::Batch` inverse = reversed batch of inverses | `crates/photonic-core/src/history/mod.rs:3172-3179` | the undo story (§7) |
| `history.execute_discrete(Command::Batch(cmds), …)` for a multi-asset import | `crates/photonic-mcp/src/handlers/video.rs:2622` | the precedent 196 §6.1 adopts |
| `DiagFamily::Interchange` | `crates/photonic-core/src/diag.rs:160` | declared, **zero codes**; enumerated by `diag_taxonomy.rs:113` and `diag_catalogue.rs:116` |
| `serde_json`, `thiserror` already in `photonic-video` | `crates/photonic-video/Cargo.toml:17-18` | a hand-written EDL reader/writer needs **no new crate** |

### 2.2 What does not exist yet — plainly

- **No `interchange/` module in `photonic-video`.** 196 creates it; X-3 adds
  `interchange/edl/`. The only `interchange` path today is `captions/interchange/`.
- **No `InterchangeReport`.** 34 §2 sketches it; 196 §4.1 defines it; nothing
  implements it. X-3 is a *consumer*, not an author, of that type.
- **No `DiagCode` in the `Interchange` family.** X-2 registers the first five.
- **No source timecode anywhere in the model or the probe.** `MediaAsset`
  (`media.rs:42`) and `MediaProbe` (`media.rs:153`) carry none —
  `grep -n timecode crates/photonic-core/src/timeline/media.rs` returns nothing.
  Nor does the probe *collect* it: `probe.rs`'s `FfStream`/`FfFormat`
  (`crates/photonic-video/src/media/probe.rs:118`, `:142`) deserialize no `tags`
  map at all, so ffprobe's `format.tags.timecode` / `stream.tags.timecode` are
  discarded before they reach any Photonic type. **This is the item's blocking
  gap** and §5 closes it.
- **`parse_cdl_xml` / `write_cdl_xml` have no product caller.**
  `grep -rn 'parse_cdl_xml\|write_cdl_xml' --include=*.rs .` returns only
  `grade.rs` (definitions + its own unit tests at `grade.rs:509-552`) and the
  re-export at `timeline/mod.rs:66`. 07 §6.2's CDL interchange is written and
  unreachable; §4.6 gives `CdlParams` its first product consumer.
- **Clip positions are not frame-snapped.** `Sequence::validate`
  (`sequence.rs:378`) enforces positive duration, sorted order and non-overlap
  only; nothing calls `FrameRate::snap`. Sub-frame positions are legal in the
  model and **illegal in an EDL** (§4.5).
- **No GUI route for caption import/export** — `import_captions` / `export_captions`
  are MCP-only (`handlers/video.rs:5362`, `:5410`). 196 §7.3 introduces the
  Interchange column that fixes this class of gap; X-3 must land inside it, not
  beside it.
- **`list_media` exposes no timecode**; there is no `get_asset` tool at all
  (the tool-name list is `handlers/video.rs:8330+`).

### 2.3 The three findings from the previous round, applied

1. **The graph content hash encodes neither the eval canvas nor media bytes.**
   X-3's new field must therefore never become a graph input. Rule, stated once
   and tested (§10 test T14): **source timecode is resolved exactly once, at
   import/edit time, into `Clip.source_in`; it never reaches the compiler or a
   cache key.** An implementer who resolves it during evaluation would put an
   unhashed input behind a per-node cache.
2. **`TimelineCmd::apply` debug-asserts `Sequence::validate()` after every
   command, and `Command::Batch` applies members one at a time**
   (`commands.rs:1749-1757`; `history/mod.rs:3172`). X-3's import therefore emits
   **no per-clip commands at all**: each imported `Sequence` is built whole,
   validated, and handed to one `AddSequence` (`commands.rs:451`). A 2 000-event
   EDL is 1 + 1 + N + 1 commands, and every command boundary is valid by
   construction. This is 196 §6.1's shape, adopted verbatim (§7).
3. **`AvLink` groups do not propagate trims** — three places in the code
   deliberately do the opposite, and 35 §3.5's claim to the contrary is wrong
   (194 §5a.3 and its Follow-up 1). X-3 creates A/V-linked pairs from `B` and
   `AA/V` events (§4.4). It must rely only on *move* propagation, must not
   assume trim propagation, and must not "fix" 35 §3.5 under cover of this item.

---

## 3. The EDL dialect — exactly what is supported

Being vague here is how an interchange parser becomes a pile of special cases.
This is the whole grammar X-3 accepts and the whole grammar it writes.

### 3.1 Statements

| Line | Read | Written | Meaning |
|---|---|---|---|
| `TITLE:  <text>` | yes | yes | sequence name |
| `FCM:  DROP FRAME` / `FCM:  NON-DROP FRAME` | yes | yes | timecode **labelling** mode; applies to every event that follows until the next `FCM` |
| event line (§3.2) | yes | yes | one edit |
| `M2   <reel>  <±fps>  <src-tc>` | yes | yes | motion memory — constant speed |
| `* <text>` comment | yes | yes | §3.3 |
| blank line | ignored | — | |
| anything else | **`Unsupported`**, line number reported, parse continues | — | never a hard failure |

An EDL declares **no frame rate**. `FCM: DROP FRAME` implies `30000/1001` or
`60000/1001` (`FrameRate::is_drop_frame_rate`, `time.rs:157`) and nothing else;
`NON-DROP` implies nothing at all. §8.1's `fps` argument resolves it.

### 3.2 The event line

```
001  REEL0001 V     C        01:00:00:00 01:00:04:00 01:00:00:00 01:00:04:00
002  REEL0002 V     D    024 02:00:00:00 02:00:06:00 01:00:04:00 01:00:10:00
```

Fields, in order: **event number · reel · channel · edit type · [transition
duration in frames] · source-in · source-out · record-in · record-out**.

**Parsing rule: whitespace-tokenised, not column-fixed.** CMX 3600 is nominally
a fixed-column format (3-digit event, 8-char reel at column 5, …) but every tool
that emits long reel names has already broken the columns, and the four trailing
timecodes anchor the parse unambiguously. The transition-duration token is
present exactly when the edit type is not `C`. Reject the line — one
`Unsupported`, parse continues — if it does not end in four parsable timecodes.

| Field | Accepted on read | Written on export |
|---|---|---|
| event number | 1–6 digits, any value, not required to be sequential | `001`… ; 4 digits past 999, one `Approximation` when that happens |
| reel | any non-blank token; `BL` = black, `AX` = auxiliary/unidentified | §4.3's derived 8-character name |
| channel | `V`, `A`, `A2`…`A8`, `AA` (A1+A2), `B` (V+A1), `AA/V` (V+A1+A2), `NONE` | `V` only (v1 exports video) |
| edit type | `C` cut · `D` dissolve · `W` + 3-digit pattern wipe · `K`/`KB`/`KO` key | `C`, `D` |
| transition duration | 1–4 digits, frames | 3 digits |
| timecodes | `HH:MM:SS:FF` or `HH:MM:SS;FF`, via `Timecode::parse_to_tick` | `Timecode::format_tick` (`time.rs:297`) |

### 3.3 Comments actually honoured

`*`-prefixed lines are free text by the format, and the useful ones are de-facto
conventions rather than part of the original spec. X-3 reads and writes exactly
these, attaches them to the **preceding** event, and reports every other comment
form as ignored (not as `Unsupported` — an unrecognised comment costs the user
nothing and reporting each one would be report spam; one coalesced `Info` names
the count):

| Comment | Direction | Maps to |
|---|---|---|
| `* FROM CLIP NAME:  <name>` | read + write | `Clip.name`; asset-match key 2 (§4.3) |
| `* TO CLIP NAME:  <name>` | read + write | the incoming clip of a dissolve |
| `* SOURCE FILE:  <path>` | read + write | asset-match key 1 — a real path beats a reel name |
| `*ASC_SOP  (s s s)(o o o)(p p p)` | read + write | `CdlParams.slope/offset/power` (§4.6) |
| `*ASC_SAT  <f>` | read + write | `CdlParams.sat` |
| `* LOC: <tc> <COLOUR> <name>` | read + write | `Sequence.markers` (§4.7) |
| `* BLEND, DISSOLVE` | read (ignored) | noise emitted by several tools |

### 3.4 Deliberately not a supported dialect

GVG 4 Plus, Sony 9100, Avid ALE, and the "CMX 3600 with two video layers"
variants. They are separate grammars whose events X-3 will reject line-by-line
with a reported `Unsupported` rather than mis-parse. Say this in the report's
first line when more than half a file's non-comment lines fail to parse: *"this
does not look like a CMX 3600 EDL"* — a specific message beats 400 generic ones.

---

## 4. The mapping, and the two timecode rebases

### 4.1 Source timecode — the crux, and the reason 34 §5 blocks EDL on K-A12

196 §3.6 names source timecode as OTIO's highest-probability defect class, where
it is *one field among many*. **In an EDL it is the entire content.** Every
event's source-in/out are absolute timecodes on the camera master; Photonic's
`Clip.source_in` is an offset from the media file's **first frame**. The bridge
between them is the media's own start timecode — and §2.2 establishes that
Photonic does not have it, does not store it, and does not even ask ffprobe for it.

```
clip.source_in = parse(event.source_in, media_rate) − asset_start_timecode
```

A reader that omits the subtraction gives every clip a `source_in` of one hour
and reads a hundred hours past the end of a ten-second file. This is not a
hypothetical: `01:00:00:00` is the default start timecode of essentially every
professional camera and deck.

**The resolution ladder**, in order, resolved once at import (finding #1):

1. `asset.source_timecode` — the user's explicit override (§5.1). Exact.
2. `asset.probe.start_timecode` — folded from ffprobe's `timecode` tag (§5.3).
   Exact.
3. **Zero, with a report entry.** One `Approximation` per asset naming the reel
   and stating "source timecode unknown; assumed 00:00:00:00", plus the new
   `InterchangeSourceTimecodeUnknown` diagnostic (§8.3).

Step 3 is where the design earns its keep, because Photonic can *detect* the
common failure rather than merely disclaim it. When a probe exists and

```
source_in + source_delta(duration) > probe.duration
```

the assumption is provably wrong — the event reads past the end of the file. The
entry is then escalated in wording and the clip is left in place with
`source_in` clamped to zero, so the user sees the right structure with an
unmistakable warning rather than a black frame with no explanation. That check is
cheap (`SpeedMap::source_delta`, `clip.rs:397`, and `MediaProbe.duration`,
`media.rs:154`) and it is the single highest-value assertion in this document.

Rate mismatch: the event's source timecode is labelled at the EDL's declared
rate; the media's own rate is `probe.video.frame_rate`. When they differ, parse
the label at the **media's** rate (that is the rate the camera used) and emit one
`Approximation` naming both. Parsing at the sequence rate would shift every
source-in by the ratio between them.

### 4.2 Record timecode — the second rebase, and it is not the same one

Record timecode is absolute too, and `Sequence` tick 0 is *not* absolute:
`Sequence.start_timecode` (`sequence.rs:166`) is the display origin, and its own
doc comment names `01:00:00:00` as the usual delivery value.

```
clip.start = parse(event.record_in, seq_rate) − record_origin
sequence.start_timecode = record_origin
```

**`record_origin` defaults to `min(record_in)` across all events in the file.**
Justification, because two other candidates are tempting and both are worse:

- *Zero* leaves a one-hour empty lead-in on every real EDL.
- *Floor to the containing hour* produces a 59-minute lead-in for the very
  common `00:59:5x` pre-roll slate — it fails on exactly the files it was meant
  to help.

`min(record_in)` puts the first event at tick 0 always, and displays the file's
original timecode on the ruler because `start_timecode` carries it. For the
overwhelmingly common `01:00:00:00` EDL, `start_timecode` becomes exactly
`01:00:00:00` — the value `sequence.rs:163-165` already documents as the
delivery convention. An explicit `record_origin` argument (§8.1) overrides it.

Export inverts this exactly: `record_tc = Timecode::format_tick(clip.start,
rate, sequence.start_timecode, prefer_drop)` (`time.rs:297`), which is that
function's designed purpose.

### 4.3 Reels and media matching

**Export.** The reel field is 8 characters of uppercase alphanumerics by
convention, and Photonic has no reel-name field. The name is *derived*, not
stored:

1. uppercase the asset's file stem, drop everything outside `[A-Z0-9]`,
   truncate to 8;
2. on collision, overwrite the trailing characters with a counter (`CAMA0001`,
   `CAMA0002`);
3. empty result → `AX`.

The full, untruncated name goes in `* FROM CLIP NAME:` and the path in
`* SOURCE FILE:` — the two comments every conform tool actually reads. One
`Approximation` carries the whole truncation/disambiguation map when any name
was changed, so the user can hand the colourist a correct reel list.

**Import.** Match against the existing pool, in order — the same ladder
`media.rs`'s module doc describes for relink and 196 §3.2 adopts:

1. `* SOURCE FILE:` path (absolute, or relative to the `.edl`'s directory) →
   existing asset by path, else by `content_hash` of that file if it is readable;
2. `* FROM CLIP NAME:` matched case-insensitively against existing assets' file
   stems;
3. reel name matched case-insensitively against existing assets' file stems, and
   against the first 8 sanitized characters of them (the inverse of the export
   derivation, so a Photonic → EDL → Photonic round trip relinks);
4. no match → **create an offline asset** named after the reel, with
   `AssetSource::File` pointing at a non-existent path. Offline is a first-class
   state (`media.rs:3-6`), not an error; one `InterchangeMediaUnresolved` entry
   per unmatched reel, coalesced.

Special reels: `BL` → `ClipSource::SolidColor` black, no asset created. `AX` →
follow the comment keys; if they fail, an offline asset named `AX`.

**Import never probes.** Probing is `SetAssetMeta`'s job (`commands.rs:426`) and
belongs to the L1/L2 ladder in [24](../specs/video-editor/24-preview-media-load.md),
not to a parser — 196 §3.6 sets that rule and X-3 keeps it. The consequence is
worth stating: a freshly imported EDL whose assets were newly created has no
probe, therefore no `probe.start_timecode`, therefore lands on ladder step 3.
That is correct and it is why §8.1's `dry_run` and the report matter.

### 4.4 Structure, channels and A/V links

| EDL | Photonic |
|---|---|
| file | one new `Sequence` (import always creates, never merges — 196 §3.2) |
| event, channel `V` | a `Clip` on `video_tracks[0]` |
| event, channel `A`/`A2`…`A8` | a `Clip` on `audio_tracks[n-1]`, tracks created as needed |
| event, channel `AA` | two clips, `audio_tracks[0]` and `[1]` |
| event, channel `B` | one video + one audio clip, **A/V-linked** |
| event, channel `AA/V` | one video + two audio clips, A/V-linked |
| event, channel `NONE` | dropped, `Unsupported` |
| gap in record TC between events | absence — Photonic has no gap object |
| record ranges that overlap on one channel (other than the §4.5 dissolve pair) | later event dropped, `Unsupported` naming both event numbers |

**A/V links are minted inline.** Both halves are built into the `Sequence` before
it is handed to `AddSequence`, so the link costs no extra command: each pair
shares a fresh `LinkGroupId` in `Clip.link_group` *and* is pointed at a matching
`GroupNode { kind: GroupKind::AvLink }` in `Sequence.groups`. That is exactly the
shape the v4→v5 migration produces (`migration.rs:212`), so an EDL-imported pair
is indistinguishable from a migrated one. Per finding #3, the pair **moves**
together and **trims** independently; do not rely on 35 §3.5's contrary claim.

Events are sorted by record-in before assembly (real EDLs are not always in
order), then the whole `Sequence` is run through `Sequence::validate`
(`sequence.rs:378`) before any command is constructed.

### 4.5 Transitions — the one place EDL fits Photonic *better* than OTIO does

196 §3.4 records a real geometric mismatch for OTIO: OTIO's `Transition` carries
`in_offset` and `out_offset` around the cut, while Photonic's `transition_in`
window is `[clip.start, clip.start + duration)` — **entirely after the cut**
(`active_transition`, `crates/photonic-video/src/graph/compile.rs:744`), with the
outgoing clip borrowed past its own end from its remaining handle. An
`in_offset > 0` therefore costs an `Approximation`.

CMX 3600's dissolve has **exactly Photonic's geometry**. A dissolve is a pair of
events sharing one event number: a zero-record-duration `C` on the outgoing reel
marking the cut, then a `D` on the incoming reel whose record-in *is* that cut
and whose transition duration is consumed from the incoming clip's head. Nothing
lives before the cut.

- **Import:** detect the pair (same event number, first has `record_in ==
  record_out`), emit one `Clip` for the incoming reel with
  `transition_in = Transition { kind: CrossDissolve, duration: N frames }`. The
  outgoing clip is the *previous* event, already placed; the pair adds no clip.
  No approximation entry — this is exact.
- **Export:** a `transition_in` of duration `D` on clip B emits the pair, with
  the outgoing reel taken from the preceding clip. Write the **authored** `D`
  (clamping against handles is a render-time decision, 196 §3.4).
- `W###` wipe → `TransitionKind::Wipe` + one `Approximation` (the 3-digit SMPTE
  pattern code has no Photonic analogue; the code goes in the clip's name-adjacent
  comment on re-export, nowhere else). `K`/`KB`/`KO` keys → `Unsupported`, the
  event is dropped.
- Exporting `DipToBlack` / `DipToColor` / `Push` → `D` + one `Approximation`
  ("will render as a cross dissolve"). `Clip.transition_out` (a fade to
  transparent, valid only into a gap or the sequence end —
  `Sequence::validate_transitions`, `sequence.rs:414`) has **no EDL form** →
  `Unsupported`.
- A `D` with no preceding event, or whose partner `C` is missing, is dropped with
  an `Unsupported` — Photonic cannot anchor it (same rule as 196 §3.4).

**Frame alignment.** Every EDL position is an integer frame, and Photonic
permits sub-frame clip positions (§2.2). Export snaps with `FrameRate::snap`
(`time.rs:126`) and emits one `Approximation` naming every clip whose start or
end moved, with the shift in ticks. It never refuses: a conform EDL rounded to
the frame is useful; no EDL is not.

### 4.6 Speed — derived from frame counts, not from `M2`

`M2` carries speed as a decimal fps string (`045.0`, `-024.0` for reverse). Two
sources of truth exist in the file and they disagree in practice, so pick the
exact one:

```
ratio = (source_out − source_in) / (record_out − record_in)      // in FRAMES
```

Both sides are integers, so the ratio is **exactly rational** and lands directly
in `SpeedMap::Constant(Ratio { num: i32, den: u32 })` (`clip.rs:375`, `:292`)
with no float anywhere — PA-8 held. `M2` supplies only the **sign** (reverse) and
acts as a cross-check: when `M2`'s implied ratio and the derived ratio differ by
more than one frame over the event, emit one `Approximation` naming both and keep
the derived value. A freeze (`source_in == source_out`, record duration > 0)
becomes `Ratio::new(0, 1)`, which `source_delta` integrates to a zero source
advance — a freeze frame, exactly.

Export writes `M2` from `Ratio::as_f64() × nominal_fps` formatted `%05.1f`, plus
the negative sign for reverse. `SpeedMap::Keyframed` has no EDL form: export
writes the clip's overall average ratio
(`speed.source_delta(duration) / duration`) and emits an `Approximation` naming
the key count — the same choice 196 §3.5 makes for OTIO, for the same reason
(dropping the warp entirely makes the source-side length visibly wrong in the
receiving tool).

### 4.7 CDL and markers — the two comment payloads worth carrying

**ASC CDL.** `*ASC_SOP (s s s)(o o o)(p p p)` and `*ASC_SAT f` are a published
ASC specification and map 1:1 onto `CdlParams` (`grade.rs:129`), which already
exists with exactly those four fields and is already a `GradeOpParams::Cdl`
payload (`grade.rs:172`). Import therefore lands a real, editable grade:
`Grade { ops: [GradeOp { kind: GradeOpKind::Cdl, params: AnimProps::new(
GradeOpParams::Cdl { … }), … }], bypass: false }` on the clip. Export writes the
comments back from the **first enabled `Cdl` op** in the clip's grade stack; any
other grade op present produces one `Unsupported` naming it. This is the only
place an EDL carries look, it is why colourists still trade EDLs, and it costs no
new type.

This is opt-in-by-default (`apply_cdl`, §8.1) so that a user importing a conform
list who does *not* want the colourist's grade can say so. See §12.2 Q3.

**Markers.** `* LOC: 01:00:05:00 RED Slate` is the widely-emitted marker comment.
Import → `Sequence.markers` with `anchor: MarkerAnchor::Timecode` and `at`
rebased by `record_origin` (§4.2); the colour word becomes `Marker.color` by the
same named-colour table 196 §3.3 defines, and **`category` stays `None`** —
196 §3.3's rule that import never invents `MarkerCategory` rows applies verbatim.
Export writes `* LOC:` for sequence markers, using the effective colour
(`marker.color` ?? category colour ?? neutral) snapped to the nearest named
colour, exactly as 196 §3.3 specifies. Clip markers have no EDL form →
`Unsupported`, coalesced to one entry with a count.

### 4.8 The lossiness register — what export drops

Every row produces an `Unsupported` entry naming what, where, and what the user
will see instead. This table **is** the acceptance criterion for test T9.

| Photonic | Where | Why there is no EDL form |
|---|---|---|
| `video_tracks[1..]` and all of `audio_tracks` | sequence | EDL v1 export is single-layer V1 (§8.1's `track` arg picks which). One entry per omitted track with its clip count |
| `Clip.effects`, `Track.effects`, `Sequence.master_effects`, `MediaAsset.effects` | all four `VfxOwner` scopes | EDL carries no effects |
| `Clip.grade` ops other than the first enabled `Cdl`; `Track.grade`, `master_grade`, `MediaAsset.grade` | all | only clip-level CDL survives (§4.6) |
| `Clip.transform` (`AnimProps<ClipTransform>`) and `Clip.reframe` | clip | **a scaled or repositioned clip exports as if untouched** — the most visually surprising loss, and it must be worded that way in the report |
| `Clip.composition: Option<GraphId>` | clip | no node graphs |
| `ClipSource::{NestedSequence, Adjustment, Text, Vector, Unknown}` | clip | §4.9 |
| `Sequence.formats`, `active_format` | sequence | EDL has no resolution field at all |
| `Sequence.caption_tracks` | sequence | the report names `export_captions` as the route |
| `Clip.audio`, `Track.audio`, `Sequence.audio_master`, `Track.blend`, `Track.opacity` | all | no mixing or compositing model |
| `Clip.group` / `Sequence.groups` / `Clip.link_group` | clip | no grouping |
| `MulticamGroup` inactive angles (`clip.rs:276`) | clip | only the active angle is expressed |
| `Clip.transition_out`; non-dissolve transition kinds | clip | §4.5 |
| `Sequence.work_range`; `MediaAsset.rating`/`tags`; the `MediaBin` hierarchy | project | no analogue |
| clip markers | clip | §4.7 |
| `Clip.enabled == false` | clip | skipped; one entry with a count |

### 4.9 Sources that are not media

A `NestedSequence`, `Adjustment`, `Text`, `Vector` or `Unknown` clip cannot name
a reel. It is nonetheless exported as an event with reel `AX`, source
`00:00:00:00` + the clip's record duration, the source name in
`* FROM CLIP NAME:`, and one `Unsupported` entry.

The alternative — omitting the event — is worse and the reason is specific to
this format: an EDL's value is a **contiguous, correct record-side timeline**. A
hole in the record timecode silently re-frames the meaning of nothing, but it
does silently remove a shot the conform operator is expecting to account for.
`AX` is the format's own published idiom for "source not identified", so using it
is honest in the receiving tool's vocabulary rather than only in ours.

---

## 5. Data-model change

X-3 is the **one** interchange item that needs a model change, and it is the
change 34 §7 already sequences ahead of EDL as "K-A12 timecode incl. drop-frame
— *needed for source-TC in all three*". K-A12 landed the `Timecode` type and the
sequence-side origin (§2.1); the **asset-side** half was never built (§2.2). X-3
either lands it or X-3 is blocked forever. §12.2 Q1 records the scheduling call.

### 5.1 One new type and two new fields

```rust
// crates/photonic-core/src/timeline/media.rs

/// The timecode of a media file's FIRST frame (K-A12 / 34 §5).
///
/// `start` is a resolved `Tick` offset, not a label: the label is rate-dependent
/// (`01:00:00:00` is a different instant drop-frame than non-drop), so the
/// conversion happens once, at the edge, against the media's own frame rate —
/// the same discipline `time.rs`'s module doc states for every other time value.
/// `drop_frame` is retained for re-LABELLING only and never affects `start`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceTimecode {
    pub start: Tick,
    #[serde(default)]
    pub drop_frame: bool,
}

// appended to MediaProbe (media.rs:153) — a PROBED fact
/// Start timecode as the container reported it (ffprobe `timecode` tag).
/// Additive; absent in files written before X-3, and absent for media that
/// carries none.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub start_timecode: Option<SourceTimecode>,

// appended to MediaAsset (media.rs:42) — a USER override
/// User-set start timecode, overriding `probe.start_timecode`. Set when the
/// media carries no embedded timecode, or carries the wrong one. Survives
/// re-probing, which is the entire reason it is not stored on the probe.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub source_timecode: Option<SourceTimecode>,
```

**Why two fields rather than one.** Each means exactly one thing, and collapsing
them creates a real bug either way. On the probe alone, a re-probe silently
clobbers the user's correction — and re-probing is routine (`SetAssetMeta`,
`commands.rs:426`). On the asset alone, either the probe cannot populate it
(losing the free, correct value for the majority of professional media) or it
does and re-probe clobbers again. Two `Option`s plus one accessor is the smallest
shape with no clobber:

```rust
impl MediaAsset {
    /// The effective start timecode: user override, else probed, else zero.
    /// The ONE reader every consumer uses (§4.1's ladder, steps 1–3).
    pub fn effective_source_timecode(&self) -> SourceTimecode { … }
}
```

An `origin: Probed | UserSet` enum on a single field was considered and rejected:
it encodes the same two facts in a way that makes "which value would I get back
if I cleared the override" unanswerable.

### 5.2 One new command, copied from an existing one

`probe.start_timecode` rides `TimelineCmd::SetAssetMeta` (`commands.rs:426`) for
free — it replaces the whole `Option<MediaProbe>` old/new. The override needs its
own arm, and it is a three-line copy of `SetAssetRating` (`commands.rs:434`):

```rust
/// X-3: user-set media start timecode (K-A12 asset half).
SetAssetTimecode {
    asset: AssetId,
    old: Option<SourceTimecode>,
    new: Option<SourceTimecode>,
},
```

with `ops::set_asset_timecode` mirroring `ops::set_asset_rating` (`ops.rs:119`),
plus the three places every variant already appears: a `mem_estimate` arm
(`commands.rs:1631`), a `description` arm (`commands.rs:1660` — "Set source
timecode"), and an `invert` arm swapping `old`/`new` (`commands.rs:2219`'s
pattern).

### 5.3 One engine change, not a model change

`crates/photonic-video/src/media/probe.rs` gains a `tags: Option<HashMap<String,
String>>` on `FfStream` (`probe.rs:118`) and `FfFormat` (`probe.rs:142`), and
`fold` (`probe.rs:147`) reads `timecode` from the video stream's tags, then the
format's, then a `codec_type == "data"` stream's — the three places ffprobe
surfaces it. The string is parsed with `Timecode::parse_to_tick` against the
probed `r_frame_rate`; an unparsable value is left `None` and reported, never
guessed. This is a probe extension, not a model change, and it is why the
majority of professional media will land on ladder step 2 without the user
touching anything.

### 5.4 Everything else X-3 needs is not in the document

The EDL reader, writer, dialect types, event model and report live in
`crates/photonic-video/src/interchange/edl/{mod,read,write,report}.rs` — a
sibling of 196's `interchange/otio/`, under the `interchange/mod.rs` that 196 §4.1
creates. `InterchangeReport` / `Unsupported` / `Approximation` / `Location` are
**reused, not redefined**; X-3 adds no reporting type. No new dependency: the
format is line-oriented ASCII and `photonic-video` already has `thiserror` and
`serde_json` (`Cargo.toml:17-18`).

---

## 6. Migration and format-version impact

**`CURRENT_FORMAT_VERSION` stays 5** (`crates/photonic-core/src/document.rs:117`).
X-3 lands additively inside v5. Point by point:

1. **Both new fields are `Option` with `#[serde(default,
   skip_serializing_if = "Option::is_none")]`** — byte-identically to how
   `probe`, `proxy`, `content_hash`, `bin`, `rating` and `tags` were each added
   to `MediaAsset` (`media.rs:49-73`). `rating`/`tags` landed for K-C2 inside v5
   with no bump; 195 §3.3 adds `derived_from` the same way for the same reason.
2. **Nothing is reinterpreted.** `migration.rs:43-56` defines a `Migration` as a
   function that reinterprets existing data on the way from N to N+1. An old file
   loads with both fields `None`, and `None` is the *correct and complete*
   meaning — "no source timecode is known", which is precisely today's state of
   the world. There is no v5→v6 step to write and nothing for `run_migrations` to
   do.
3. **A no-op bump is actively harmful.** `COMPAT_WINDOW = 1`
   (`migration.rs:16`), so every version stamped is a version of forward-read
   tolerance spent. `V1ToV2` and `V2ToV3` exist only to stamp a number for purely
   additive changes; repeating that pattern for a change that reinterprets no
   field would shrink the effective window for every user and make
   `MigrationV5ToV6` a lie about what changed. **Bump only when data must be
   reinterpreted.**
4. **The new `TimelineCmd` arm is not a document-format change.** It appears only
   in the sibling `photon_history` key, which `load_photon` restores best-effort
   — a payload that fails to deserialize yields `None` history while the document
   still opens (194 §4 point 2). An older build opening a newer file drops undo
   history rather than failing: the existing, accepted degradation.
5. **ROADMAP §10 point 5** ("additive serde/migration round-trip passes when
   model changes") *does* apply here — unlike 194/195/196, X-3 changes the model.
   It is answered by tests T11 and T12, not waived.

The versioned surface X-3 introduces beyond that is **none**. The EDL format has
no version field and no metadata namespace; 196 §3.7's `photonic` namespace has
no EDL analogue and must not be simulated with comment lines. That is a
deliberate exclusion (§12.3), not an oversight: a `* PHOTONIC:` comment carrying
a private payload would be a `.photon` with extra steps, exactly the failure
196 §3.7 argues against, and in a format with no escaping rules.

---

## 7. Undo unit and its exact inverse

Repo rule: one user verb = one undo unit (01 §10.0; 39 §1). Three verbs.

### 7.1 "Import EDL…" — one `Command::Batch`

The shape 196 §6.1 establishes and `import_media` already uses
(`handlers/video.rs:2622`):

```rust
history.execute_discrete(Command::Batch(cmds), &mut doc);
```

`cmds`, in order:

1. `Command::Timeline(ops::create_project())` — only when `doc.timeline.is_none()`.
2. `Command::Timeline(ops::create_bin(file_stem, None))` (`ops.rs:1942`) — one bin
   named after the `.edl`, so the import is undoable *and* visibly grouped.
3. `Command::Timeline(ops::add_asset(a))` × N (`ops.rs:101`) — only for reels that
   matched nothing; `a.bin` is set at construction, the same trick `import_media`
   uses to dodge the `AssignAssetBin` ordering problem.
4. `Command::Timeline(ops::add_sequence(s))` × 1 (`ops.rs:325`) — **one** command
   carrying the fully-built `Sequence`, tracks, clips, A/V link groups, markers
   and CDL grades inline (`commands.rs:451`).
5. `Command::Timeline(ops::set_active_sequence(p, Some(new)))` (`ops.rs:341`).

**Exact inverse**, mechanical rather than hand-written: `Command::Batch` inverts
as the reversed batch of inverses (`history/mod.rs:3172-3179`), so the inverse is
`SetActiveSequence{old}` → `RemoveSequence` → `RemoveAsset` × N → `RemoveBin` →
`RemoveProject`. Every member already has a tested inverse. Redo re-applies the
forward batch.

**No per-clip command exists**, which is finding #2's requirement rather than an
optimisation: a 2 000-event EDL is `1 + 1 + N + 1 + 1` commands and every command
boundary satisfies `Sequence::validate` by construction.

**Validate-then-commit** (39 §1.1): the whole file is parsed, the `Sequence` is
built in full and run through `Sequence::validate` (`sequence.rs:378`) **before**
the first command is constructed. A parse or validation failure yields
`Err(InterchangeError)` and mutates nothing. A half-imported EDL is not an
acceptable outcome and is not reachable.

`mem_estimate` (39 §1.3) must be honest: this batch carries a whole `Sequence`.
It is bounded by the history byte budget like `CaptionCmd::BulkInsertCues`.

### 7.2 "Set Source Timecode…" — one `SetAssetTimecode`

Inverse: the same command with `old` and `new` swapped (`commands.rs:2219`'s
pattern). Committed through `execute_discrete` so it never folds into an adjacent
gesture. Note this verb is *why* a failed import is cheap: the user sets the reel
timecode, undoes nothing, and re-imports.

### 7.3 "Export EDL…" — no undo entry

Export mutates no document state and records nothing (39 §1.6). "Remember the
last export path" is view state and belongs in the 39 §1.6 sidecar, not in
`Document`.

---

## 8. MCP surface and GUI parity

An MCP surface **is** warranted: CAP-019 parity is a definition-of-done item
(ROADMAP §10 point 3), PA-11 (full MCP parity) is explicitly *not* a held
property yet (ROADMAP §9), and 196 §2.2 records the caption import/export gap —
MCP-only, no GUI — as the mistake not to repeat. X-3 lands both arms.

### 8.1 Tools

**`import_edl`**

| Arg | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | — | `.edl` file to read |
| `fps` | string? | active sequence's rate, else **required** | `"25"`, `"30000/1001"`, `"29.97"` — resolved through 196 §3.1's rate ladder; an EDL declares no rate |
| `record_origin` | string? | `min(record_in)` | record-TC origin → `Sequence.start_timecode` (§4.2) |
| `bin` | string? | file stem | media-pool bin for created assets |
| `apply_cdl` | bool | `true` | land `*ASC_SOP`/`*ASC_SAT` as `GradeOpParams::Cdl` (§4.6) |
| `audio` | bool | `true` | import `A`/`AA`/`B`/`AA/V` channels; `false` = video only |
| `dry_run` | bool | `false` | parse and report; execute no command |

Returns the report as structured data, the same shape 196 §7.1 defines:
`{ "clips": …, "assets_created": …, "assets_reused": …, "markers": …,
"unsupported": [{ "what", "where", "consequence" }], "approximated": [ … ] }`.

`dry_run` is what lets the GUI's pre-import sheet and an agent show the loss
report **before** the user commits — §1's requirement, and the mechanism by which
"source timecode unknown for 6 reels" is seen rather than discovered.

**`export_edl`**

| Arg | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | — | destination `.edl` |
| `sequence_id` | uuid? | active sequence | which sequence |
| `track` | int | `0` | which `video_tracks` index becomes V1 |
| `title` | string? | sequence name | the `TITLE:` line |
| `drop_frame` | bool? | `frame_rate.is_drop_frame_rate()` | the `FCM:` mode; forced false at non-DF rates |
| `include_cdl` | bool | `true` | write `*ASC_SOP`/`*ASC_SAT` from clip grades |

Returns `{"path":…, "events_written":…, "unsupported":[…], "approximated":[…]}`.
It **succeeds with warnings**: a lossy export is not an error (196 §12.2 Q3
settles this), `is_error` stays false, and `unsupported` is non-empty.

**`set_asset_timecode`** — `{ asset_id, timecode: string?, fps?: string }`.
`timecode: null` clears the override. The string is parsed with
`Timecode::parse_to_tick` (`time.rs:244`) against `fps` ?? the asset's probed
rate ?? the active sequence's rate; `;` selects drop-frame and is rejected at a
non-DF rate by the existing parser, which is the correct behaviour to surface
rather than to work around.

**`list_media`** (existing, `handlers/video.rs:2651`) gains `"source_timecode"`
in its per-asset payload — the effective value plus which ladder step produced
it, because "why is this clip an hour off" is unanswerable otherwise.

Wiring follows the existing pattern exactly: arg structs in
`protocol/args/video.rs`, handlers in `handlers/video.rs`, dispatch arms in
`dispatch.rs`, names added to the tool-name list (`handlers/video.rs:8330+`),
then `schema_gen.rs` regenerated. **CI gates the docs**: `.github/workflows/ci.yml:163-167`
regenerates `docs/mcp-api.md` and fails on any diff, so regeneration is
mandatory, not optional.

### 8.2 GUI route

`FILE_OPTIONS` is `&["Document", "Save", "Export"]`
(`crates/photonic-gui/src/app/mod.rs:290`), rendered by the File drawer
(`menu_drawer.rs:31`). 196 §7.3 adds a fourth column, **"Interchange"**. X-3 adds
two entries to *that* column — not a new surface:

- **"Import EDL…"** and **"Export EDL…"**, each an `rfd` picker filtered to
  `edl`, through the existing `run_file_dialog` helper
  (`crates/photonic-gui/src/app/mod.rs:1848`).
- Both open the **same modal report sheet** 196 §7.3 introduces — import before
  committing (driven by the same `dry_run` pass the MCP tool exposes, so GUI and
  MCP text are byte-identical), export after writing.
- Import's sheet gains one EDL-specific control it genuinely needs: a **frame
  rate** selector, prefilled from the active sequence, because the file does not
  say. A rate the user cannot see is a rate they cannot correct.

The media pool's asset context menu gains **"Set Source Timecode…"**, a small
dialog reusing `duration_dialog.rs`'s timecode-entry pattern
(`crates/photonic-gui/src/panels/video/duration_dialog.rs:59-77`) — which already
formats with `Timecode::format_tick` and parses with `Timecode::parse_to_tick`.

### 8.3 Diagnostics — one new code, and why

X-3 reuses 196 §8's five codes:

| Code | Used by X-3 for |
|---|---|
| `InterchangeParseFailed` | an unreadable file; the §3.4 "this is not a CMX 3600" case; an AAF/FCPXML handed to `import_edl` (§9) |
| `InterchangeUnsupportedConstruct` | keys, unparsable lines, omitted tracks, dropped clip markers, `NONE` channels |
| `InterchangeRateApproximated` | the `fps` argument falling to the approximation rung; an event's source rate differing from the media's; frame-snapping on export |
| `InterchangeMediaUnresolved` | a reel that matched no asset — the clip is offline |
| `InterchangeLossyExport` | any non-empty `unsupported` list on export |

**One code is added, and this is a deliberate, narrow deviation from 196 §10's
"X-3 adds no new ones".**

| Code | Default severity | Consequence line |
|---|---|---|
| `InterchangeSourceTimecodeUnknown` | `Warning` | "The media's start timecode is not known; clips may play the wrong part of the file." |

Justification, stated because a commitment is being varied. 196's five codes were
chosen against a format whose time is entirely self-describing; in OTIO the
missing-source-TC case is a sub-case of `available_range` handling. In an EDL it
is *the* central failure mode (§4.1), it is the one thing a user must act on, and
none of the five carries a truthful consequence line for it —
`InterchangeMediaUnresolved` says "the clip is offline", which is exactly what has
*not* happened and would teach the user to look in the wrong place.
[36 §2.2](../specs/video-editor/36-error-model.md) requires each code to carry a
distinct consequence; folding this into an ill-fitting code would violate that to
honour a count. Adding it costs updating `DiagCode::family()`, `default_severity()`
and `consequence()` in lockstep (`diag.rs:268`, `:311`, `:331`; the macro at
`diag.rs:170` generates `ALL`, `as_str` and `FromStr` for free) plus the catalogue
tests; `families_partition_all_codes` (`diag_taxonomy.rs:102-120`) and
`family_partitions_the_catalogue` (`diag_catalogue.rs:105-128`) already enumerate
`Interchange`, so both keep passing. Follow-up 3 records the amendment to 196 §10.

Two surfaces, one source of truth (`InterchangeReport`): the sheet and the tool
result carry full per-item detail; the diagnostic log coalesces to one entry per
code per subject, so a 400-event file with 400 dropped transforms fires one toast.

---

## 9. AAF and FCPXML — the user-side route, made concrete

34 §4.3 and 26 §18 both say AAF and FCPXML are reachable "through OTIO adapters".
196 §10 makes the reading explicit and X-3 is where it becomes user-visible, so
state it without hedging:

> **Photonic ships no OTIO adapter runtime, no Python, and no first-party AAF or
> FCPXML reader.** "Via OTIO adapters" is a *routing* statement, not a bundling
> commitment. The OTIO adapter ecosystem is Python; intaking it would be a
> dependency decision requiring a
> [23 §3.3](../specs/video-editor/23-legal-open-source-implementation-routes.md#33-required-evidence-record)
> evidence record — transitive licences, build scripts, maintenance owner —
> produced and accepted **before** intake, not alongside it. No such record
> exists and X-3 does not seek one.

**What the user actually does.** Outside Photonic, they convert once —
`otioconvert -i cut.aaf -o cut.otio`, or their NLE's own OTIO export (Resolve,
Premiere and Kdenlive 25.04+ all write `.otio` natively, which is usually the
better route and skips the adapter entirely). Then, inside Photonic:
**File → Interchange → Import OTIO…**.

**What the app tells them**, because "the user is left to discover it" is the
failure mode §1 exists to prevent:

1. The Interchange column carries a **non-executing** entry, *"AAF / FCPXML…"*,
   that opens a short explainer sheet: the one-line conversion command, the
   "your NLE probably exports `.otio` directly" alternative, and a plain statement
   that Photonic does not bundle a converter. It launches nothing and touches no
   file — it is documentation placed where the user is already looking.
2. The Import OTIO and Import EDL file pickers **do not list** `.aaf` or
   `.fcpxml`.
3. If a user reaches one anyway — "All files", a drag-and-drop, or an agent
   passing a path — the extension is checked **before** the parser runs and the
   refusal is specific: *"Photonic does not read AAF. Convert it to
   OpenTimelineIO first (`otioconvert -i … -o ….otio`), then use Import OTIO."*
   `InterchangeParseFailed` with that detail string, in both the GUI and the MCP
   result, so an agent gets the routing hint rather than a byte-level parse error.
   That is an extension check and one message; it is the difference between a
   product decision and a bug report.

An AAF/FCPXML importer is **out of scope** (§12.3) and would be its own item with
its own mini-spec. AAF in particular is a binary Structured-Storage container, a
different order of work from every format in 34, and its value is Avid
interchange that a conform EDL plus an OTIO already largely covers.

---

## 10. Acceptance fixtures and tests

### 10.1 Fixtures — Photonic-authored, and **X-3 is not a gated item**

All EDL fixtures are **hand-written ASCII**, authored in this repo against the
published CMX 3600 format description and the published ASC CDL specification,
committed under `crates/photonic-video/tests/fixtures/edl/` with a `README.md`
recording provenance per
[23 §12](../specs/video-editor/23-legal-open-source-implementation-routes.md#12-cross-cutting-provenance-manifests),
following the existing corpus README (`crates/photonic-video/tests/fixtures/README.md`).
**No file is copied or adapted from any other project's test suite** — 26 §7 and
23 §3.4 item 4, the same rule 196 §9.1 applies.

One **new media fixture** is required and it is Photonic-generated: `tc_hour1.mp4`,
a 2 s 320×180 clip written with an embedded start timecode of `01:00:00:00`,
added to `tools/gen-test-fixtures.py` beside `color_bars.mp4`. It is the only way
to test §5.3's probe fold end-to-end, it is synthesised from a colour source like
every other fixture in that corpus, and at ~5 KiB it is negligible against the
5 MB budget (the corpus is ~2.5 MiB today, per its README).

**No third-party or rights-encumbered content is required, so X-3 is not
`legal-or-fixture-blocked`** — unlike G-20 / K-D1. No `AssetRightsManifest`
(23 §7.2) is needed. Recording that explicitly matters: it is the difference
between a schedulable item and a blocked one.

| Fixture | Exercises |
|---|---|
| `basic_cuts.edl` | `TITLE`, `FCM: NON-DROP FRAME`, 5 `C` events at 25 fps, one record gap |
| `source_tc_hour1.edl` | every source TC at `01:00:00:xx` — §4.1's trap |
| `dropframe_2997.edl` | `FCM: DROP FRAME`, 29.97, a record TC crossing a minute boundary |
| `dropframe_illegal_label.edl` | `00:01:00;00` — a label drop-frame skips (`time.rs:285` rejects it) |
| `dissolve_pair.edl` | the two-event `C`+`D` pattern, 24-frame dissolve |
| `wipe_and_key.edl` | `W001` (approximated) and `K` (unsupported) |
| `speed_m2.edl` | 2× fast, 0.5× slow, `-024.0` reverse, and a freeze (`src_in == src_out`) |
| `m2_disagrees.edl` | `M2` fps inconsistent with the derived frame counts |
| `audio_channels.edl` | `V`, `A`, `A2`, `AA`, `B`, `AA/V`, `NONE` |
| `asc_cdl.edl` | `*ASC_SOP` + `*ASC_SAT` on three events |
| `locators.edl` | `* LOC:` point markers in four named colours |
| `long_reels.edl` | reel names > 8 chars, `* SOURCE FILE:`, `* FROM CLIP NAME:` |
| `out_of_order.edl` | events not sorted by record-in |
| `overlapping.edl` | two `V` events overlapping in record time |
| `not_an_edl.edl` | prose text — §3.4's "this is not a CMX 3600" path |
| `gvg_dialect.edl` | a GVG-style line set, to prove §3.4 reports rather than mis-parses |

Total added fixture weight: text, on the order of 12 KB, plus the ~5 KiB media
clip.

### 10.2 Tests

| # | Test | Asserts |
|---|---|---|
| T1 | **Source-TC rebase** — `source_tc_hour1.edl` against an asset whose `source_timecode` is `01:00:00:00` yields `source_in == 0` for every clip, not one hour | §4.1 |
| T2 | **Source-TC unknown** — the same file with no timecode on the asset yields exactly one `Approximation` and one `InterchangeSourceTimecodeUnknown` per asset, escalated when a probe proves the read runs past `probe.duration` | §4.1, §8.3 |
| T3 | **Record-TC rebase** — `basic_cuts.edl` starting at `01:00:00:00` gives `sequence.start_timecode == 01:00:00:00` and `clips[0].start == 0`; `record_origin` overrides it | §4.2 |
| T4 | **Drop-frame** — `dropframe_2997.edl` round-trips with **zero** tick drift over one hour; the same labels parsed as non-drop differ by 108 frames, proving `;` is honoured | §3.1, `time.rs:244` |
| T5 | **Illegal DF label** — `dropframe_illegal_label.edl` snaps forward to the first legal frame, one `Approximation`, and does **not** fail the import | §3.2 |
| T6 | **Dissolve geometry** — `dissolve_pair.edl` produces one clip with `transition_in.duration == 24 frames`, the cut at the outgoing clip's `end()`, and **zero** approximation entries | §4.5 |
| T7 | **Half-open boundary** — abutting events give `end() == next.start`; a 1-frame event survives both directions | PA-7 |
| T8 | **Speed is exactly rational** — `speed_m2.edl` yields `Ratio` values with no float intermediate; reverse is `num < 0`; freeze is `Ratio::new(0,1)`; `m2_disagrees.edl` emits one `Approximation` and keeps the derived ratio | §4.6, PA-8 |
| T9 | **Export lossiness register** — a sequence carrying every §4.8 row exports successfully with exactly one `Unsupported` entry per row, each naming what/where/consequence | §4.8 |
| T10 | **Frame snapping** — a clip at a sub-frame start exports snapped, with one `Approximation` naming the shift; the export never refuses | §4.5, §2.2 |
| T11 | **Additive serde round-trip** — a v5 document carrying `MediaAsset.source_timecode` and `MediaProbe.start_timecode` survives `to_json` → `from_json` → `finalize_load` byte-identically; a v5 document *without* them loads as `None` and re-serializes without the keys | ROADMAP §10.5 |
| T12 | **Format version unchanged** — a document containing an EDL-imported sequence and a source timecode saves at `format_version == 5` and reloads unchanged | §6 |
| T13 | **Undo identity** — `assert_undo_roundtrip` for `SetAssetTimecode`; and importing `audio_channels.edl` is **one** `execute_discrete`, one undo restoring a byte-identical `to_json` | §7 |
| T14 | **Source timecode never reaches the graph** — `grep`-equivalent structural assertion that no `compile`/`ContentHash` path reads `source_timecode`; changing an asset's `source_timecode` alone does not invalidate a cached node | finding #1, §2.3 |
| T15 | **Validate-then-commit** — `overlapping.edl` fails with no document mutation and no history entry | §7.1 |
| T16 | **Mid-batch validity (debug build)** — importing a 500-event EDL with adjacent same-track clips does not trip `commands.rs:1749`'s `validate` assert | finding #2 |
| T17 | **A/V link** — a `B` event produces a linked pair that moves together and trims independently; the pair carries both `link_group` and an `AvLink` `GroupNode` | §4.4, finding #3 |
| T18 | **CDL** — `asc_cdl.edl` lands `GradeOpParams::Cdl` with the exact slope/offset/power/sat; export reproduces the comment lines; `apply_cdl: false` lands no grade | §4.6 |
| T19 | **Markers** — `locators.edl` markers land at the right sequence ticks after the §4.2 rebase, with colours mapped and **`category == None`** | §4.7, 196 §3.3 |
| T20 | **Reel derivation** — `long_reels.edl` round-trips: export truncates and disambiguates, import relinks by the inverse ladder, one `Approximation` carries the map | §4.3 |
| T21 | **Not-an-EDL** — `not_an_edl.edl` and `gvg_dialect.edl` each produce one summary entry, not one per line, and mutate nothing | §3.4 |
| T22 | **AAF refusal** — `import_edl`/`import_otio` given a `.aaf` or `.fcpxml` path returns the specific routing message before any parse is attempted | §9 |
| T23 | **Probe fold** — `tc_hour1.mp4` probes to `probe.start_timecode == Some(01:00:00:00)`; a file without the tag probes to `None` | §5.3 |
| T24 | **GUI arm** — headless EDL import/export through `photonic-gui`'s interchange path | ROADMAP §10.2 |
| T25 | **GUI/MCP parity story** — the GUI sheet text and the MCP `unsupported` array come from one `InterchangeReport` and agree | ROADMAP §10.10 |

T1 and T2 deserve the emphasis 34 §6 gives its off-by-one test and 196 §9.2 gives
its test A: the defect is invisible in casual testing (an EDL whose media happens
to start at zero passes either way), and when it is wrong it is wrong for every
clip in the project.

---

## 11. Definition of done (ROADMAP §10), made answerable

| # | Requirement | How X-3 answers it |
|---|---|---|
| 1 | Core op/engine service with unit tests | `photonic-video/src/interchange/edl/{mod,read,write,report}.rs`; `ops::set_asset_timecode`; tests §10.2 |
| 2 | GUI route, or a recorded exception | Interchange column (§8.2) + media-pool "Set Source Timecode…"; T24. **No exception sought** |
| 3 | MCP tool/schema/generated docs | `import_edl`, `export_edl`, `set_asset_timecode`, `list_media` payload; `docs/mcp-api.md` regenerated, `ci.yml:163-167` gate green |
| 4 | One verb = one undo unit; undo/redo identity | §7; T13 |
| 5 | Additive serde/migration round-trip when the model changes | **The model does change** (§5). No v6 (§6). T11, T12 |
| 6 | Pixel/audio IR/eval/golden/sync coverage | **N/A — X-3 touches no pixel or audio path.** The CDL op it writes is an existing, already-covered `GradeOpKind`. No new goldens; state this rather than invent coverage (196 §11 makes the same call) |
| 7 | Hard gates green; trend metrics not regressed | Parsing is off the hot path. One added hard gate, because it is deterministic: a 5 000-event EDL imports in < 1 s on the CI runner. T14's cache-invalidation assertion is also a hard gate |
| 8 | Offline, privacy, licensing, content, product gates | Offline: parsing is local, no network, no telemetry, no subprocess. Licensing: §13, no dependency. Content: §10.1 — **not gated** |
| 9 | Protected surfaces not regressed | PA-7 (half-open, T7), PA-8 (flicks + exact rational, T4/T8), PA-9 (typed model — `SourceTimecode` is a struct, not a string) are exactly what §4 defends. Linked A/V untouched (T17) |
| 10 | Goal-backward L1–L4, including GUI/MCP parity | L1 module exists → L2 real parser/writer → L3 wired into the Interchange column and dispatch → L4 an EDL exported from Photonic conforms in a third-party tool and re-imports; T25 pins parity |

---

## 12. Risks, open questions, and deliberate exclusions

### 12.1 Risks

1. **Source timecode (§4.1).** Highest probability, highest blast radius, lowest
   visibility — and unlike 196's version, it is not one field among many but the
   entire content of the format. T1 and T2 are mandatory and must use a fixture
   whose media does *not* start at zero.
2. **Record-origin choice (§4.2).** Getting this wrong shifts every clip by an
   hour in the *other* direction, and it looks fine on a timeline that starts at
   zero. T3.
3. **Drop-frame.** 34 §5 calls this out: "every 29.97 EDL will be silently wrong
   by ~3.6 s/hour" if `;` is not honoured. The 27 SD-11 defect **is fixed**
   (`time.rs:245-246,261,285`), so the risk is now in *X-3's* use of it — passing
   the wrong rate to `parse_to_tick`, which silently rejects rather than misparses
   (a good failure mode, but only if the rejection is reported). T4, T5.
4. **Two-line dissolve detection (§4.5).** Treating the zero-duration `C` as a
   real event produces a spurious zero-length clip that `Sequence::validate`
   rejects with `NonPositiveDuration` — which at least fails loudly. Treating the
   `D` as a plain cut silently loses every dissolve. T6.
5. **Report fatigue.** A 400-event export from a graded sequence produces 400
   `Unsupported` entries. The sheet must group by reason with counts ("400 clips:
   transform dropped") and enumerate only on expansion (196 §12.1 risk 4), or
   users stop reading it — which defeats §1.
6. **Two timecode fields (§5.1).** Every future feature will want to read "the"
   timecode. `effective_source_timecode()` is the only reader; any direct access
   to `probe.start_timecode` outside the probe fold is a review failure.
7. **CDL colour space.** ASC CDL is defined on the working-space values it is
   applied to, and Photonic grades in a colour-managed linear space (PA-2). A CDL
   authored in a log space in another tool will not look identical. The report
   must say so once per import; X-3 does **not** attempt a space conversion —
   guessing the authoring space is worse than naming the assumption.

### 12.2 Open questions needing a product call (each with a recommendation)

- **Q1 — does X-3 own the asset-side timecode field, or does K-A12?** 34 §7
  sequences K-A12 first and 26 K-A12 sits in K-Band 3, but K-A12 shipped only the
  `Timecode` type and `Sequence.start_timecode`; the asset half (§2.2) was never
  built. *Recommendation: X-3 lands it, as specified in §5, and K-A12's residual
  is amended to record it as delivered by X-3.* The alternative — reopening
  K-A12 to land three fields, then scheduling X-3 behind it — adds a scheduling
  round trip and a second review of the same design. **This is a scheduling call,
  not a technical one, and it is the only thing between this spec and
  implementability.**
- **Q2 — does import create audio tracks?** 34 §5 describes import as "an
  assembly of cuts against source timecode" and export as "a flat cut list from a
  single video track", which is silent on audio import. *Recommendation: yes,
  import audio channels (`audio: true` by default); export video-only in v1.* The
  asymmetry is deliberate — import loses nothing by being generous, whereas a
  multi-channel EDL *export* needs an A/V-sync story across channels that nothing
  has specified.
- **Q3 — should an EDL import be allowed to write grades?** §4.6 lands ASC CDL as
  a real `Grade`, which means an interchange file mutates the look, not only the
  structure. *Recommendation: yes, default `apply_cdl: true`.* Carrying CDL is
  the principal reason EDLs are still traded, the data is explicit and inspectable
  in the file, the op is a first-class editable `GradeOp` the user can delete, and
  the report names it. A user who wants structure only passes `apply_cdl: false`.
- **Q4 — should export refuse when loss exceeds a threshold?**
  *Recommendation: no*, consistent with 196 §12.2 Q3. Export always succeeds with
  a report; a user exporting for conform does not care that transforms were
  dropped, and a tool that refuses the thing asked is worse than one that
  explains what it did.

### 12.3 Deliberately out of scope

- **AAF and FCPXML readers or writers**, and any OTIO adapter runtime (§9).
- **`.ccc` / `.cdl` XML sidecar files**, even though `parse_cdl_xml` /
  `write_cdl_xml` already exist unused (`grade.rs:332`, `:380`). Reading a
  sidecar beside an EDL needs a matching convention nobody has specified; §4.6's
  inline comments give `CdlParams` its first product consumer without inventing
  one. Follow-up 6.
- **GVG, Sony and Avid ALE dialects** (§3.4), and multi-video-layer EDL variants.
- **A `photonic` metadata namespace in EDL comments** (§6). The `.otio` writer's
  namespace has no EDL analogue and simulating one in a comment field would be a
  second, unversioned serialization of the timeline model in a format with no
  escaping rules.
- **Captions in either direction** — `import_captions` / `export_captions` own
  subtitles, and the report says so.
- **Merging an imported EDL into an existing sequence.** Import always creates
  (196 §3.2, §12.2 Q1). Merge needs a conflict model that is a mini-spec of its
  own, and create-only is strictly forward-compatible with adding it later.
- **Deriving source timecode from a file name or a sidecar log.** Guessing a
  timecode is exactly the class of silent corruption §4.1 exists to prevent;
  `set_asset_timecode` is the explicit route.
- **Auto-relink on `set_asset_timecode`.** Changing an asset's start timecode
  after an import does *not* retroactively fix already-placed clips —
  `source_in` was resolved at import (finding #1). The correct workflow is set,
  then re-import, and the report says so. Making it retroactive would mean the
  timecode *is* a live graph input, which §2.3 forbids.

---

## 13. Clean-room provenance

Required by [26 §7](../specs/video-editor/26-kdenlive-mlt-parity.md#7-how-to-read-the-item-tables)
and [23 §3.4](../specs/video-editor/23-legal-open-source-implementation-routes.md#34-clean-room-protocol);
this item's provenance risk is specific (two published formats plus a possible
dependency), so the note is explicit rather than inherited.

- **Design sources.** (a) The **published CMX 3600 EDL format description** —
  the `TITLE` / `FCM` statements, the event-line field order, the edit-type
  letters, the reel conventions (`BL`, `AX`), the `M2` motion-memory line, and
  the de-facto comment conventions (§3.3) that every conform tool documents in
  its own user manual. (b) The **published ASC CDL specification** for
  `*ASC_SOP` / `*ASC_SAT` and the slope/offset/power/saturation model, which
  Photonic already implements as `CdlParams` (`grade.rs:129`) from that same
  source per 07 §6.2. (c) **SMPTE timecode**, including drop-frame, already
  implemented in `time.rs`. (d) Photonic's own code and specs, cited by
  `file:line` throughout. Formats, standards and interfaces are facts, not
  expression (34 §1).
- **Not derived from.** Kdenlive's or MLT's source trees or their EDL handling —
  `REJECT` under 26 §2 — nor Resolve's, Avid's or any other product's
  implementation, nor **OpenTimelineIO's `cmx_3600` adapter**. OTIO is Apache-2.0
  and therefore not an *excluded* source in the way MLT is, but the reader and
  writer here are designed from the format description rather than transcribed
  from any implementation, so no expression is carried across and the 23 §3.4
  attestation is available at merge without qualification. The implementer records
  that attestation for the `photonic-video-engine` and `core-timeline` subsystems,
  and an independent reviewer checks identifiers, comments, constants, control
  flow and test provenance before merge (26 §2 point 2).
- **No dependency is introduced.** The parser and writer are Photonic-authored
  over `std`; `photonic-video` already has `thiserror` and `serde_json`
  (`Cargo.toml:17-18`) and needs neither for the format itself. This is the route
  ROADMAP §2 states as preferred for X-2 and 34 §4.2 states as first choice, and
  it applies a fortiori to a line-oriented ASCII format. **No
  [23 §3.3](../specs/video-editor/23-legal-open-source-implementation-routes.md#33-required-evidence-record)
  evidence record is required for X-3 as specified here.** If a future implementer
  proposes an EDL crate instead, that record is required **before** intake.
  Explicitly: §9's AAF/FCPXML route introduces no dependency because it introduces
  no code.
- **Fixtures** are Photonic-authored (§10.1); none is copied or adapted from any
  other project's test suite, and the one new media fixture is synthesised by
  `tools/gen-test-fixtures.py` like the rest of the corpus.
- **Photonic-ahead properties preserved** (26 §5, ROADMAP §9). Ranges stay
  half-open (PA-7; T7). All time is `Tick` flicks over exact rational rates, and
  speed is derived as an exact `Ratio` from integer frame counts rather than from
  `M2`'s decimal (PA-8; T4, T8). Failures are typed (`InterchangeError`,
  `SourceTimecode`), never stringly-typed (PA-9). No graph or cache key changes
  (PA-1; T14). **No reference-tool limitation is ported backwards:** EDL's
  single-video-layer, integer-frame, no-effects model constrains the *file*, never
  the Photonic sequence — nothing in §4 narrows the model to what an EDL can say.
- **Naming discipline.** Describe the capability as "reads and writes CMX 3600
  EDL files" and "reads ASC CDL correction comments", never as certification,
  endorsement, or an official relationship with the ASC, SMPTE or any vendor.

---

## Follow-ups

Changes this document deliberately did **not** make to existing docs (this item
may not edit them; each needs its own change):

1. **[34 §5](../specs/video-editor/34-interchange.md#5-x-3--edl)** should absorb
   §4.1's source-TC resolution ladder and §4.2's record-origin rule, and should
   record that the 27 SD-11 drop-frame defect it warns about **is already fixed**
   (`time.rs:245-246,261,285`) — the paragraph currently reads as an open blocker.
   Its acceptance table (34 §6) should gain T1, T2 and T6.
2. **[34 §7](../specs/video-editor/34-interchange.md#7-sequencing)** row 1 says
   K-A12 "blocks EDL". It blocks it *because the asset-side field was never
   built*, which is not what the row implies now that K-A12 is otherwise landed.
   Amend to name the specific missing field, and resolve §12.2 Q1 there.
3. **[196 §10](196-x-2-opentimelineio-interchange.md)** says "X-3 adds no new
   [diag] codes". §8.3 adds exactly one, with justification. 196 should be
   amended to "X-3 adds at most one, for the source-timecode case" so the two
   documents do not disagree.
4. **[196 §3.6](196-x-2-opentimelineio-interchange.md)** has an internal gap that
   X-3 inherits and should be closed at the source: it requires the discarded
   `available_range.start_time` to be "written into the asset's Photonic
   metadata", but 196 §4.1 adds no model field to hold it and 196 §3.6's export
   rule then writes `start_time: 0`. Once §5.1's `MediaAsset.source_timecode`
   exists, 196 §3.6 should read from and write to that field, and its metadata
   fallback becomes redundant.
5. **[36 §3.2](../specs/video-editor/36-error-model.md)** line 82 reserves
   `Unsupported`, `Approximated`, `MalformedInput` for the `Interchange` family;
   196 §8 registers five differently-named codes and §8.3 adds a sixth. One of
   the two should be updated so the family has a single vocabulary.
6. **`parse_cdl_xml` / `write_cdl_xml` have no product caller**
   (`grade.rs:332`, `:380`; verified by grep). 07 §6.2's `.cdl`/`.ccc` sidecar
   interchange is written and unreachable. It is an independent gap, not X-3's to
   fix, but the Interchange File-drawer column is its natural home and it should
   be considered when that gap is scheduled (§12.3).
7. **[26 §18 X-3](../specs/video-editor/26-kdenlive-mlt-parity.md#x-3--edl-aaf-fcpxml)**
   should record §9's statement that AAF/FCPXML conversion is user-side and
   Photonic ships no adapter runtime, so "via OTIO adapters" is not read as a
   bundling commitment — this is 196's Follow-up 5, restated because §9 now makes
   it user-visible. Its **Effort: S–M (EDL)** line should also be revisited: the
   parser is small, but the source-timecode field, its command, the probe fold and
   the GUI dialog are not.
8. **[26 K-A12](../specs/video-editor/26-kdenlive-mlt-parity.md#k-a12--timecode-as-a-first-class-concept)**
   residual should be narrowed to the asset-side field once §12.2 Q1 is decided,
   so the item is not read as still owing work that landed.
9. **[ROADMAP.md](../specs/video-editor/ROADMAP.md)** §0 progress table gains an
   X-3 row when the item lands, per the existing convention; the §2 X-3 row should
   link this proposal once accepted.
