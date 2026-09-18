# 18 — DJI-Editor Parity Gap-List (drone / action-cam distinctive features)

> **Status: Historical; superseded for live status.** Use [ROADMAP.md](ROADMAP.md), [21](21-dji-core-workflows.md), and [22](22-dji-advanced-workflows.md). This file preserves research and original ranking rationale; its HAVE claims predate the status audit.

**Depends on:** 07-color-grading.md (grade + `.cube` LUT infra), 03-render-color-pipeline.md (working space, scopes), 05-import-export.md (presets, ingest), 04-ui-mode-timeline.md (timeline surface + markers), 01-data-model.md (`ClipTransform`, `ClipEffect`, `Marker`, `ProbedColor`), 09-audio-mixer.md (DSP), 17-nle-parity-round2.md (the pro-editing spine this rides on).
**Owns:** the prioritized backlog of **DJI-distinctive** features — the drone/action-cam things DJI's own apps (LightCut / Fly / Mimo) do that a general NLE does not. Does **not** re-own NLE editing parity (that is 14/16/17) or generic color/caption/audio parity — only the drone-specific layer on top.
**Source:** DJI product research (LightCut / DJI Fly / Mimo / Studio, D-Log/D-Log M/HLG LUTs, flight-telemetry SRT, QuickShots/MasterShots, Hyperlapse, Panorama, RockSteady/HorizonSteady) cross-audited (2026-07-10) against Photonic's shipped surfaces: `crates/photonic-render/src/{lut,grade,scopes}.rs`, `crates/photonic-video/src/{media/probe,decode/sidecar,captions/interchange/srt,export/presets,graph}.rs`, `crates/photonic-gui/src/panels/video/{color_page,export_dialog}.rs`, and `app/reframe.rs`.

> **Audited foundation list.** Photonic has substantial NLE, LUT/grade, caption, mixer, export, and MCP infrastructure. Adjustment layers and titles/text are partial foundations with residual GUI/responsive work (G-7/G-12); MCP parity is continuous, not globally complete (G-21/D-9). Everything below is DJI-specific residue on top.
> - **Generic 3D-LUT support is foundation only for D-1.** [21 §4](21-dji-core-workflows.md#4-d-1--dji-log-and-hlg-normalization) supersedes this file's GradeOp-only path with source-boundary classification/conversion, proxy-domain, range/matrix, cache, and no-double-conversion contracts. Preferred delivery is a validated Photonic-authored analytical or clean-room transform; sampled Photonic `.cube` is optional. Vendor LUTs are optional user-installed or redistribution-licensed fallbacks, never calibration oracles.
> - **Aspect switch + Social vertical export presets are DONE**, so "vertical export for TikTok/Reels" is **excluded** — it is already HAVE. The DJI-distinctive residue on framing is *little-planet pano reframe* (**D-8**), not generic 9:16.
> - **Rotation on `ClipTransform` + the reframe transform overlay are DONE** (`clip.rs:327`, `reframe.rs`), so horizon-level is a rotation-correction *effect + auto-crop* (**D-5**), not new transform math.
> - **Markers + the timeline command spine are DONE**, so music editing is a *beat-detection DSP that drops markers + snaps cuts* (**D-4**), not new marker infra.

---

## How to read this

Each gap carries: **DJI** (which DJI app ships it) · **Impact** (for a drone / action-cam editor) · **On-device** (the realistic egui/wgpu/Rust build — what rides existing infra) · **Cloud-AI OUT** (the sub-part that needs an ML model / cloud, explicitly deferred) · **Territory** (the single build-wave lane that owns it) · **Files** · **Effort** (S ≈ hours, M ≈ 1–2 days, L ≈ multi-day / needs a mini-spec) · **Class** (Quick win / Medium / Larger).

**Territories** (same lanes as 17; one agent per lane in a build wave):

| Territory | Root | Owns |
|-----------|------|------|
| `core-timeline` | `photonic-core/src/timeline/` | clip/track/marker model + pure ops |
| `timeline-panel` | `photonic-gui/src/app/timeline/` + `app/command_center.rs` + `commands.rs` | the timeline surface, its commands, keymap |
| `monitor` | `photonic-gui/src/app/monitor.rs` + `app/reframe.rs` | program monitor, transport, on-canvas overlays |
| `panels-video` | `photonic-gui/src/panels/video/` | color page, inspector, effect-controls, export dialog |
| `photonic-video-engine` | `crates/photonic-video/src/` | ingest/probe/decode + graph eval/composite + DSP |
| `photonic-render` | `crates/photonic-render/src/` | GPU/CPU raster kernels, LUT sampler, scopes |
| `photonic-mcp` | `crates/photonic-mcp/src/` | headless tool parity for new verbs |

**Strategic note.** DJI has effectively *ceded the desktop* — LightCut/Fly/Mimo are mobile-only, DJI Studio is legacy 360-reframe. So a desktop editor that natively does the drone-specific things below fills a genuinely unserved gap; several (notably **flight-telemetry overlays**, **D-1**) are things DJI itself never ships on any platform, only third-party tools do.

---

## Historical ranked top-10 (original builder pick-order)

Not live queue. See [ROADMAP.md §4](ROADMAP.md#4-corrected-priority-bands). D-5 is completed for v1 and must not be selected as open work.

| # | Gap | Territory | Class | Effort |
|---|-----|-----------|-------|--------|
| 1 | **D-1** — D-Log / D-Log M / HLG → Rec.709 one-click convert (ship DJI `.cube` LUTs + metadata auto-detect) | `panels-video` (+`photonic-video-engine`) | Quick win | S–M |
| 2 | **D-7** — DJI flight-telemetry SRT parse → data model + burned-in data HUD overlay | `photonic-video-engine` (+`panels-video`) | Medium | M |
| 3 | **D-5** — Horizon auto-level (manual rotation correction + auto-crop) — ✅ DONE v1 | `photonic-video-engine` (+`monitor`) | Medium | M |
| 4 | **D-4** — Beat-detection DSP → beat markers + snap-cut-to-beat | `photonic-video-engine` (+`timeline-panel`) | Medium | M |
| 5 | **D-6** — Hyperlapse / timelapse assembly from image sequence (+ deflicker) | `photonic-video-engine` | Medium | M |
| 6 | **D-10** — Full flight-telemetry overlay: gauges + GPS mini-map + graphs (builds on D-7) | `photonic-video-engine` (+`panels-video`/`monitor`) | Larger | L |
| 7 | **D-11** — Template-based auto-edit-to-music (conform clips to a beat-timed template; builds on D-4) | `timeline-panel` (+`photonic-video-engine`) | Larger | L |
| 8 | **D-8** — DJI panorama ingest + little-planet / equirectangular reframe viewer | `photonic-video-engine` (+`monitor`) | Medium | M |
| 9 | **D-12** — Gyro-metadata stabilization / auto-straighten (Gyroflow-style, for Avata/FPV + Action) | `photonic-video-engine` | Larger | L |
| 10 | **D-13** — HDR / HLG 10-bit timeline + PQ/HLG scopes + HLG→SDR tone-map | `photonic-video-engine` (+`photonic-render`) | Larger | L |

**Below the line (supporting / lower value):** D-2 (DJI creative/vivid LUT look-picker, QW), D-3 (bundled beat-synced music + theme ambient-SFX starter library, QW–M), D-9 (MCP parity for the new verbs, M), D-14 (own panorama *stitcher*, L), D-15 (auto-highlight-reel via on-device shot detection, L).

Territory spread for a 6-lane wave: `panels-video` ×1 (D-1), `photonic-video-engine` ×4 (D-4/D-5/D-6/D-7), plus the Larger tail. Lanes are file-disjoint except D-4↔D-11 and D-7↔D-10 (phase-1 → phase-2 of the same feature — sequence them, don't parallelize).

---

## 1. Quick wins to do now

### D-1 — D-Log / D-Log M / HLG → Rec.709 one-click convert · `panels-video` (+`photonic-video-engine`)

> **Superseded implementation path:** do not implement the GradeOp-only/bundle-vendor-LUT plan below. [21 §4](21-dji-core-workflows.md#4-d-1--dji-log-and-hlg-normalization) owns source-boundary conversion and the Photonic-authored analytical/clean-room route. Vendor LUTs remain optional licensed/user-installed fallbacks.

- **DJI:** LightCut ships a device-specific **LUT library** and a **"Color Recovery"** one-tap ("recover colour for footage shot in D-Log or D-Cinelike"); DJI publishes official per-camera `.cube` conversion LUTs (D-Log→709, D-Log M→709, plus vivid variants) at dji.com/lut.
- **Impact:** *every* DJI clip shot for grading comes off the camera flat (D-Log / D-Log M) or HDR (HLG) and is unwatchable until normalized. This is the first thing a drone editor does to every clip, on every project. A **correctness trap** DJI itself warns about: a D-Log LUT applied to D-Log M footage (or vice-versa) gives wrong color — the convert must match the exact profile.
- **On-device:** 100% local, and near-free on shipped infra. (a) **Bundle DJI's per-camera `.cube` files** as built-in read-only LUT assets (the parser + trilinear/tetrahedral sampler already exist, `lut.rs`; a LUT is just an `AssetKind::Lut3d`, `media.rs:90`). (b) A **"Convert Log → Rec.709"** button on the color page that appends a `GradeOpKind::Lut3d` op bound to the profile-matched bundled LUT (the op-builder `default_op` in `color_page.rs:177` already knows how to make a Lut3d op — this just pre-selects the right asset). (c) **Auto-detect the profile** from `ProbedColor.transfer` / `color_primaries` (`probe.rs:124`, already parsed) with a make/model + filename-tag fallback (DJI does not always tag D-Log in a standard transfer field), to pre-pick the correct LUT and surface a "looks like D-Log M — convert?" nudge on import.
- **Cloud-AI OUT:** none — no ML, no cloud. (The only "AI" LightCut uses here is scene-classification for *creative* look suggestion, which is D-2's optional stretch, not the convert.)
- **Files:** bundle dir + registration in `crates/photonic-video/src/media/` (built-in LUT assets); `panels/video/color_page.rs` (Convert-Log one-click + device picker beside the existing LUT browser at `:580`); `media/probe.rs` (profile-guess helper off `ProbedColor`). **Effort:** S–M. **Class:** Quick win. Highest value-per-effort in the list.

### D-2 — DJI creative / vivid LUT look-picker · `panels-video`
- **DJI:** LightCut pairs the 709-convert with **creative LUTs** so one tap does *convert + look*; the library is organized **by device** ("choose the DJI device you used — Air series, Avata 2…").
- **Impact:** after D-1 normalizes, a one-tap "cinematic / vivid" look is the second move; a device-scoped look gallery is exactly the LightCut mental model.
- **On-device:** trivial once D-1 exists — bundle DJI's creative/vivid `.cube` files and add a thumbnailed "look" gallery that stacks a second `Lut3d` grade op after the convert. Pure asset + UI; no new engine.
- **Cloud-AI OUT:** LightCut's *auto-pick-the-look-from-scene-content* (aerial/outdoor/skiing…) needs a scene-classification model — **out of v1**; ship the manual gallery.
- **Files:** `panels/video/color_page.rs` (look gallery). **Effort:** S. **Class:** Quick win (companion to D-1).

### D-3 — Bundled beat-synced music + theme ambient-SFX starter library · `panels-video` (+`photonic-video-engine`)
- **DJI:** LightCut/Mimo templates ship licensed music, and LightCut **auto-recommends ambient SFX by theme** for drone footage (Forest / Sea / Field / Urban Street).
- **Impact:** template/music editing (D-4/D-11) is inert without a music+SFX library on hand; DJI's whole social loop assumes bundled audio.
- **On-device:** bundle a small royalty-free music + theme-SFX set as importable audio assets, insertable as audio clips through the existing audio-track path. Asset-heavy, engine-light.
- **Cloud-AI OUT:** theme *auto-recommendation* (match SFX to detected scene) needs a classifier — out of v1; expose the themed folders for manual pick.
- **Files:** bundled audio assets + a small browser in `panels/video/`. **Effort:** S–M. **Class:** Quick win.

---

## 2. Medium parity features

### D-4 — Beat-detection DSP → beat markers + snap-cut-to-beat · `photonic-video-engine` (+`timeline-panel`)
- **DJI:** every LightCut/Mimo template is **beat-synced** — cuts land on the music's beats. DJI bakes the beat map into each template; the value is cuts-on-the-beat.
- **Impact:** music-driven cutting is the single most-used social-edit move and Photonic has no beat concept — you eyeball cuts against a waveform. On-device beat markers turn the shipped waveform + marker + snap infra into a rhythm editor.
- **On-device:** fully local DSP — onset/tempo detection (spectral-flux onset + autocorrelation tempo, or a compact beat-tracker) over an audio clip's PCM, emitting beat times as `Marker`s (the model + `add_marker` op exist, `ops.rs:1327`). Then a **snap-to-beat** mode so razor/trim/insert land on the nearest beat marker, reusing the existing snap machinery. The DSP crate already houses envelope/loudness analysis (`audio/dsp/`), the natural home for an onset detector.
- **Cloud-AI OUT:** none for beat detection (classic DSP). Auto-*choosing which template* fits the footage is D-11's ML stretch, not this.
- **Files:** `crates/photonic-video/src/audio/dsp/` (onset/tempo → beat times); `app/command_center.rs` + `commands.rs` (`video.detect_beats` → drop markers; snap-to-beat toggle); snap hook in `app/timeline/`. **Effort:** M. **Class:** Medium. Foundation for D-11.

### D-5 — Horizon auto-level (rotation-correction) effect + auto-crop · `photonic-video-engine` (+`monitor`)
**Status (2026-07-10): ✅ DONE for v1 manual leveling + auto-crop.** The active-format inspector edits finite roll correction in degrees, stores radians, and applies the exact minimum shared scale multiplier for centered footage. GPU `Transform2D` now matches the CPU reference for nearest/bilinear sampling, native-size sources, pool padding, and singular transforms. Automatic horizon estimation remains deferred.

- **DJI:** horizon-leveling is a *capture* feature on DJI (gimbal roll trim; Action-cam **HorizonBalancing** ±45° / **HorizonSteady** full-360°). **Post-hoc horizon straightening is a gap in DJI's own software** — no DJI app does it after the fact.
- **Impact:** action-cam and FPV footage arrives tilted; a "level the horizon" control is a constant fix and nobody in the DJI ecosystem ships it in post. Photonic can, cheaply.
- **On-device:** shipped locally: a dedicated **Level Horizon** degree control plus exact **auto-crop** scaling for centered footage, riding `ClipTransform` and per-format reframe overrides. The evaluator now performs real inverse-affine GPU sampling and normalizes native-size decoded frames consistently with the CPU reference. An optional gradient/Hough roll estimate remains a stretch.
- **Cloud-AI OUT:** robust horizon *detection* for arbitrary content (sea/sky/urban) benefits from an ML horizon model. It is **out of v1**; the shipped v1 is manual correction + deterministic auto-crop. Gyro-driven straightening is D-12's job.
- **Files:** `crates/photonic-video/src/graph/{eval,eval_cpu,compile,ops,cache}.rs`, `crates/photonic-video/src/{pool,session}.rs`, `app/reframe.rs`, and `panels/video/clip_inspector.rs`. **Effort:** M. **Class:** Medium. **Status:** DONE v1.

### D-6 — Hyperlapse / timelapse assembly from image sequence (+ deflicker) · `photonic-video-engine`
- **DJI:** DJI Fly **Hyperlapse** (Free / Circle / Course-Lock / Waypoint) shoots frames and **assembles the timelapse on-device automatically**; it can also keep the RAW frames for later manual assembly. Params: interval, duration, speed.
- **Impact:** drone shooters keep the source frames precisely to assemble/deflicker/re-stabilize on desktop — but Photonic can only ingest finished video, not a **folder of frames → one clip**. A natural desktop feature DJI already proves in-app.
- **On-device:** fully local. An **image-sequence clip source** (ingest `DJI_0001.JPG…`-style numbered stills as one time-based clip at a chosen fps) + an optional **deflicker** pass (normalize per-frame luma to a rolling mean) + optional post-stabilization of the sequence. Export already has an *image-sequence container* on the **output** side (`presets.rs`); this is the missing **input** dual — a probe/decode path that treats a numbered still folder as a source.
- **Cloud-AI OUT:** none — arithmetic + optional stabilization; no cloud.
- **Files:** `crates/photonic-video/src/media/` (image-sequence probe/ingest), `decode/` (serve frames as a source), `graph/` (deflicker op). **Effort:** M. **Class:** Medium.

### D-7 — DJI flight-telemetry SRT parse → data model + burned-in data HUD overlay · `photonic-video-engine` (+`panels-video`)
- **DJI:** enabling "Video Captions" makes the drone write flight data as an **SRT track** (sidecar `.srt` or embedded): **GPS lat/long, altitude, height, H/V speed, distance, ISO, shutter, aperture, f-number, focal length, date/time, frame count**. DJI records it but **burns in no overlay** — users must leave for third-party tools (Telemetry Overlay, etc.).
- **Impact:** **the single biggest gap in DJI's own software and the strongest differentiator** — a native "burn my flight data onto the video" that DJI never ships on any platform. Phase 1 (this gap) is the parse + a text HUD; phase 2 (D-10) is gauges/map.
- **On-device:** 100% local. Photonic already parses SRT for captions (`captions/interchange/srt.rs`) and already runs a **sidecar** discovery path (`decode/sidecar.rs`); a **DJI-telemetry SRT** is a different *payload* on the same skeleton — a per-second record parser (regex the `[latitude: …] [altitude: …] [speed: …]` DJI fields) producing a time-indexed `TelemetryTrack`. Then a minimal **data-HUD overlay** (a new `EffectKind::TelemetryHud` or overlay clip) renders formatted text ("ALT 120m · SPD 15 m/s · 37.42°N") via the existing caption/text glyph path, sampled at the playhead tick.
- **Cloud-AI OUT:** none — pure parse + text render. Map tiles (D-10) are the only external asset, deferred to phase 2.
- **Files:** `crates/photonic-video/src/decode/sidecar.rs` (telemetry-SRT detect), a new `telemetry.rs` (parse → `TelemetryTrack`), `graph/` + `panels/video/` (HUD overlay + field toggles). **Effort:** M. **Class:** Medium. Phase 1 of the flagship.

### D-8 — DJI panorama ingest + little-planet / equirectangular reframe viewer · `photonic-video-engine` (+`monitor`)
- **DJI:** DJI Fly captures **Sphere (little-planet 360) / 180° / Wide / Vertical** panos and **auto-stitches a JPEG on-device**; the Asteroid QuickShot opens on a little-planet sphere.
- **Impact:** DJI users have piles of stitched equirectangular/sphere JPEGs with no desktop editor that reframes them into "tiny-planet" or panning shots. This is exactly DJI Studio's *only* surviving job (360 reframe) — Photonic can absorb it for stills.
- **On-device:** local — ingest the DJI-stitched equirectangular JPEG as an image asset, then a **reframe projection** (equirectangular → rectilinear virtual-camera, or → stereographic "little planet") as a shader over the sampled image, with pan/tilt/FOV keyframable through the *existing* `ClipTransform` + keyframe infra. Reuses the auto-reframe overlay for the virtual-camera handles.
- **Cloud-AI OUT:** none — projection math is closed-form. (Building the pano *from raw frames ourselves* is D-14, separate.)
- **Files:** `crates/photonic-video/src/graph/` (projection op), `photonic-render/` (projection sampler), `app/reframe.rs`/`monitor.rs` (virtual-camera handles). **Effort:** M. **Class:** Medium.

### D-9 — MCP parity for the new DJI verbs · `photonic-mcp`
- **DJI:** internal parity (CAP-019 discipline) — every editing verb also drives headless.
- **Impact:** MCP has no tools for convert-log (D-1), detect-beats (D-4), level-horizon (D-5), assemble-timelapse (D-6), or telemetry-overlay (D-7); headless acceptance can't drive the drone verbs.
- **On-device:** mirror each new `ops_bridge`/engine verb as an MCP tool as it lands.
- **Files:** `crates/photonic-mcp/src/`. **Effort:** M (grows with the above). **Class:** Medium — track alongside whichever gaps ship.

---

## 3. Larger / deferred

### D-10 — Full flight-telemetry overlay: gauges + GPS mini-map + graphs · `photonic-video-engine` (+`panels-video`/`monitor`)
- **DJI:** DJI ships **nothing** here; third-party tools render **speedometer/altimeter gauges, a GPS mini-map with the flight path, altitude-vs-time and speed-tracker graphs, course/heading, and camera-settings readouts**.
- **Impact:** the marquee differentiator's full form — the reason drone editors currently pay for Telemetry Overlay. Phase 2 on top of D-7's parsed `TelemetryTrack`.
- **On-device:** local rendering of animated gauge/graph widgets + a path polyline projected onto **map tiles**. All compositing is on-device; the **map tiles** are the sole external asset — ship an **offline tile cache** (pre-fetch tiles for the flight bbox once, then render offline) or accept user-supplied tiles, so there is **no live cloud dependency at render time**. Widgets are drawn through the render/tessellator path; styling/position keyframable.
- **Cloud-AI OUT:** none (no ML). Map-tile *fetch* is a one-time network step, cacheable offline — not a cloud-render dependency; flag it as the only external touch.
- **Files:** `crates/photonic-video/src/graph/` + `photonic-render/` (gauge/graph/map widgets), `panels/video/` (overlay designer: which gauges, where, style). **Effort:** L. **Class:** Larger — mini-spec. Consumes D-7.

### D-11 — Template-based auto-edit-to-music (beat-conformed edit templates) · `timeline-panel` (+`photonic-video-engine`)
- **DJI:** LightCut's marquee **"One-Tap Edit"** — drop clips into a template and it **conforms cuts to the template's beat map**, adding transitions/filters/music ("hundreds of templates, more weekly," 4K out for instant sharing). Mimo Story Mode is the same idea.
- **Impact:** the flagship social-edit experience; with D-4's beat markers in place, a template engine that auto-assembles a beat-timed cut is the payoff.
- **On-device:** local. A **template** = an ordered list of beat-relative slots (durations, transition, optional look) + a music track; the engine picks segments from the source clips, trims each to its slot, and lays them so cuts fall on D-4's beats — all through the shipped timeline ops (insert/overwrite/trim) and grade/transition stacks. The *cut-assembly, trim, music overlay, and export are all on-device* (exactly how DJI splits it).
- **Cloud-AI OUT:** the **AI-dependent** pieces DJI runs cloud/ML-side — **scene classification** (aerial/skiing/food…), **highlight-moment detection**, and **auto-picking the template from footage content** — are **out of v1**. Ship user-picked templates with beat-conform + optional shot-detection (D-15) feeding candidate segments.
- **Files:** a template schema + engine in `crates/photonic-video/src/` or `core-timeline`; `app/timeline/` + `command_center.rs` (apply-template → batched edit); reuses D-4 beats. **Effort:** L. **Class:** Larger — mini-spec. Consumes D-4.

### D-12 — Gyro-metadata stabilization / auto-straighten (Gyroflow-style) · `photonic-video-engine`
- **DJI:** in-camera EIS (RockSteady / HorizonSteady) is capture-side only; **FPV/Avata footage is routinely stabilized in post via Gyroflow using the embedded gyro data** — a model DJI has no desktop answer for.
- **Impact:** FPV and Action-cam footage is the shakiest DJI genre and the community already reaches for gyro-based post-stabilization; a native path is a strong pull for that segment (and the *accurate* horizon-lock D-5 can't do without gyro).
- **On-device:** local but non-trivial — parse the embedded gyro/quaternion metadata, integrate orientation, and warp each frame to counter rotation (+ synchronized horizon-lock), with a smoothness/crop control. No cloud.
- **Cloud-AI OUT:** stabilization for clips **without** gyro data needs optical-flow/ML motion estimation — **out of v1**; require gyro metadata for the v1 path.
- **Files:** `crates/photonic-video/src/media/` (gyro-metadata parse), `graph/` + `photonic-render/` (per-frame warp). **Effort:** L. **Class:** Larger — mini-spec.

### D-13 — HDR / HLG 10-bit timeline + PQ/HLG scopes + HLG→SDR tone-map · `photonic-video-engine` (+`photonic-render`)
- **DJI:** newer cams shoot **10-bit HLG** (and D-Log M) as the HDR path; DJI's own apps target SDR social output and punt serious HDR to Resolve/Premiere.
- **Impact:** HLG shooters currently have nowhere in-ecosystem to grade/deliver HDR properly; a real 10-bit HLG timeline + HDR scopes + a clean HLG→SDR down-map is a pro pull. But it is heavy color engineering.
- **On-device:** local but deep — a 10-bit HLG/PQ-aware working path, PQ/HLG **scopes in nits** (extends the shipped waveform/vectorscope/histogram in `scopes.rs`), and an HLG→Rec.709 SDR tone-map operator. Touches the render color pipeline's working-space assumptions.
- **Cloud-AI OUT:** none — no ML; purely a color-pipeline lift.
- **Files:** `crates/photonic-render/src/{scopes,grade,color}.rs` + `crates/photonic-video/src/graph/` (10-bit/HDR working path, tone-map op). **Effort:** L. **Class:** Larger — mini-spec.

### D-14 — Own panorama stitcher (feature-match + blend + projection) · `photonic-video-engine`
- **DJI:** DJI Fly auto-stitches Sphere/180°/Wide panos on-device from an overlapping frame grid (25–35 / 21 / 9 frames) at JPEG quality; users can keep the RAW frames to stitch elsewhere.
- **Impact:** lets Photonic ingest the **kept RAW pano frames** and stitch them itself (higher quality than the in-app JPEG), then reframe via D-8 — closing the loop for pano shooters. But real CV.
- **On-device:** local CV — feature detect/match across the overlapping grid, estimate homographies, warp+blend into an equirectangular canvas. Substantial but no cloud.
- **Cloud-AI OUT:** none required (classic CV); an ML feature matcher is an optional upgrade, not needed.
- **Files:** a stitcher module in `crates/photonic-video/src/`. **Effort:** L. **Class:** Larger (nice-to-have; D-8 ingest covers the common case first).

### D-15 — Auto-highlight reel via on-device shot detection · `photonic-video-engine` (+`timeline-panel`)
- **DJI:** **MasterShots** flies a canned cinematic set and **auto-edits a templated, music-backed highlight short**; LightCut auto-selects **highlight moments**.
- **Impact:** "make me a highlight reel from this footage" is the lazy-share dream; the *shot-detection* half is on-device and also feeds D-11's segment candidates.
- **On-device:** local **shot / scene-change detection** (luma-histogram / edge-energy delta between frames) to segment a long clip into shot candidates, plus simple motion/steadiness heuristics to rank them — feeding a template (D-11).
- **Cloud-AI OUT:** *quality* selection (is this a good shot? is the subject well-framed?) benefits from an ML model — **out of v1**; ship deterministic shot-cut detection + heuristic ranking, leave semantic highlight-scoring for later.
- **Files:** `crates/photonic-video/src/graph/` (scene-change detector), `app/timeline/` (drop candidate markers / assemble). **Effort:** L. **Class:** Larger (nice-to-have).

---

## Cloud-AI boundary (what we deliberately do NOT build on-device)

Per the research, DJI runs a small set of pieces server/ML-side; Photonic ships the on-device 90% and flags these **OUT of v1**:
- **Scene classification** (aerial / skiing / food / party …) that auto-picks a *creative look* (D-2) or *template* (D-11).
- **Semantic highlight-moment detection** / shot-quality scoring (D-15) — we ship deterministic shot-cut detection instead.
- **ML/gradient-Hough horizon detection** for gyro-less footage (D-5) remains deferred; v1 ships manual correction + deterministic auto-crop. Gyro-based correction is D-12.
- **Panorama auto-stitch quality via learned matchers** (D-14) — classic CV suffices.

Everything else in this list — telemetry parse+overlay, log→709 convert, beat detection, horizon roll+auto-crop, timelapse assembly, pano reframe, gyro stabilization, HDR pipeline — is **fully on-device** in an egui/wgpu/Rust app. The only non-compute external touch is **map tiles** for D-10, which we cache offline so render never needs the network.

---

## Bottom line

Subtracting Photonic's HAVE list leaves a tight, high-leverage drone layer. Manual horizon leveling + auto-crop (**D-5 v1**) is now shipped. The next high-value move is **D-1** through the validated Photonic-authored analytical/clean-room route in [21 §4](21-dji-core-workflows.md#4-d-1--dji-log-and-hlg-normalization) and [ROADMAP S6](ROADMAP.md#8-architecture-decisions-and-defaults); vendor LUT redistribution is optional, not the primary blocker. The **strongest differentiator** remains the flight-telemetry overlay (**D-7 → D-10**), and the **social-edit heart** remains beat detection → templates (**D-4 → D-11**). Timelapse assembly and pano reframe (**D-6 / D-8**) remain open capture-parity work. Cloud/ML remains explicitly outside the v1 boundary.
