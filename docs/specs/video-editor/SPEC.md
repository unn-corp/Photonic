# SPEC — Photonic Video Editor Module

**Status:** Draft
**Version:** 0.1
**Date:** 2026-07-07

---

## Why

Photonic edits static vector + raster documents; it has no concept of time, so users who need motion — animated brand graphics, edited footage, captioned social clips — must leave for Premiere, CapCut, or Resolve and lose the vector assets they built. The goal is a video editor inside Photonic: a multi-track timeline with playback, effects, captions, grading, node compositing, and export, where Photonic documents are first-class animatable assets.

---

## Capabilities

CAP-001 — Editor can import video, audio, and image files into a project media pool and see duration, resolution, frame rate, and audio channels for each.
  ↳ Test: import files of several common formats; verify metadata display matches known source properties.

CAP-002 — Editor can arrange clips on a multi-track timeline: move, trim in/out, split at playhead, delete, with snapping to clip edges, playhead, and markers.
  ↳ Test: perform each operation via pointer input; verify resulting clip boundaries to frame accuracy.

CAP-003 — Editor can perform ripple, roll, slip, and slide edits.
  ↳ Test: apply each edit type; verify neighbouring clips shift or hold exactly as the edit type defines.

CAP-004 — Editor can play the timeline with synchronized audio and video, pause, scrub, and step by single frames in both directions.
  ↳ Test: play a sequence containing A/V clips; measure audio/video offset; step frame-by-frame and verify displayed frame index.

CAP-005 — Editor can nest a sequence inside another sequence and edit the nest's contents without flattening.
  ↳ Test: nest, modify inner sequence, verify outer sequence reflects the change on next play.

CAP-006 — Editor can place a Photonic vector document (or artboard) on the timeline as a clip that renders at full quality at any preview size.
  ↳ Test: place a vector asset, zoom preview; verify edges stay sharp at all zoom levels.

CAP-007 — Editor can animate clip and vector-object properties (position, scale, rotation, opacity, effect parameters) with keyframes and adjustable easing curves.
  ↳ Test: keyframe a property with two distinct eases; verify interpolated values at intermediate frames match curve definitions.

CAP-008 — Editor can apply, reorder, and remove visual effects and transitions on clips, with parameters editable and animatable.
  ↳ Test: apply an effect and a cross-transition; verify render changes, parameter edits take effect, removal restores original.

CAP-009 — Editor can request automatic captions for any clip or sequence; captions arrive with word-level timing and appear as an editable caption track.
  ↳ Test: caption a clip with known speech; verify text accuracy sample and that each word carries start/end times.

CAP-010 — Editor can edit caption text, timing, grouping, and styling (font, size, colors, background, per-word highlight animation).
  ↳ Test: modify each attribute; verify preview and export reflect edits.

CAP-011 — Editor can generate spoken voiceover from typed text via a configured speech service and place it as an audio clip.
  ↳ Test: submit text; verify an audio clip appears whose duration matches returned audio.

CAP-012 — Editor can switch a sequence between aspect ratios (16:9, 9:16, 1:1, 4:5, custom) with per-clip reframing controls, and preview a mobile-framed view.
  ↳ Test: switch ratios; verify canvas and export dimensions change and reframe offsets persist per ratio.

CAP-013 — Editor can export a sequence to common delivery formats with codec, resolution, frame-rate, quality/bitrate, and container options, including presets for social platforms.
  ↳ Test: export with distinct settings; verify output container/codec/dimensions/duration probe correctly.

CAP-014 — Editor can transcode imported media to editing-friendly proxies and toggle proxy/original playback.
  ↳ Test: generate proxies for high-resolution footage; verify scrubbing uses proxy files and export uses originals.

CAP-015 — Editor can color-grade any clip: exposure/contrast/temperature controls, lift-gamma-gain wheels, tone curves, HSL adjustments, and 3D LUT application, with live waveform, vectorscope, and histogram displays.
  ↳ Test: apply each control class; verify scope displays shift accordingly and grade persists in export.

CAP-016 — Editor can open any clip as a node composition (sources → operators → output) and the composition's result plays back in the timeline; a project-level node graph post-processes final sequence output.
  ↳ Test: build a two-input merge composition on a clip; verify timeline playback and export show the composed result; add a project-graph operator and verify it affects final output only.

CAP-017 — Editor can mix audio: per-clip gain/fade, per-track volume/pan/mute/solo, keyframed automation, EQ and compressor per track, master bus with level meters, and waveform display on clips.
  ↳ Test: exercise each control while metering output; verify audible/measured result and persistence.

CAP-018 — Editor can undo and redo every timeline, grading, caption, audio, and node-graph edit.
  ↳ Test: perform a mixed edit session, undo to start, redo to end; verify document state identical at both endpoints.

CAP-019 — An automation agent can perform every capability above through the machine interface without the GUI.
  ↳ Test: script the three acceptance stories end-to-end through the machine interface; verify outputs equal GUI-produced outputs.

CAP-020 — Editor can save and reopen a project containing all above state in Photonic's native file format, with older Photonic files still loading unchanged.
  ↳ Test: round-trip a project with all feature classes; diff state before/after; open a pre-video-era file and verify identical behaviour.

CAP-021 — Editor can render SVG assets to video frames with animation applied (the motion-graphics path), including transparent-background export.
  ↳ Test: animate a vector title, export with alpha; verify frames show correct animation and transparency.

CAP-022 — Editor's project state survives an unexpected termination: on relaunch after a crash or kill, the editor offers recovery of the timeline project with at most two minutes of work lost (the autosave interval; see 37 §2.5).
  ↳ Test: build a timeline edit session, kill the process without saving, relaunch; verify the recovery prompt restores the project including timeline state.

---

## Constraints

- No copyleft (GPL/AGPL) code may be linked into Photonic binaries; external codec tooling runs only as separate subprocesses.
- Existing vector-editing behaviour and performance must not regress: all current tests pass, and interactive canvas responsiveness is unchanged when no video features are active.
- Every document mutation, without exception, is undoable through the existing command history.
- Native file format stays backward compatible: files written by current Photonic load unchanged; new files without video features load in a build one version back (existing COMPAT_WINDOW policy).
- Caption/voiceover cloud services are pluggable and optional: all non-AI capabilities work fully offline.
- All existing CI gates (build, test, fmt, clippy, cargo-deny, MCP doc-drift) pass on every phase's merge.
- Media files are referenced, never embedded, in the project file.

---

## Non-goals

- Collaborative / multi-user editing.
- Third-party plugin API (OpenFX or similar) — internal effects only in v1.
- Native HDR display presentation, Dolby Vision, bundled HEVC encoding, and HDR authoring beyond HLG/PQ. Ten-bit HLG/PQ decode, explicit Rec.2020 HDR working state, HDR scopes, deterministic SDR tone mapping, and preflighted AV1 Main10 or ProRes-compatible 10-bit delivery are in scope.
- Optical-flow stabilization, motion tracking, object tracking, rolling-shutter correction, and ML horizon detection. Gyro-metadata stabilization with explicit synchronization and calibrated lens profiles is in scope for approved camera dialects.
- Live capture, broadcast switching, remote sources, collaborative switching, and more than nine simultaneously keyboard-addressable multicam angles. Local-file multicam grouping, timecode/audio/marker/manual sync, multiview preview, and frame-accurate angle cuts are in scope.
- Live capture / streaming input.
- VR authoring, headset preview, spatial audio, stereoscopic delivery, and 360-degree video timeline editing. Still-image panorama-set stitching, equirectangular/spherical projection, virtual-camera reframe, and little-planet output are in scope.
- Mobile or web builds of the editor.
- Audio recording (import + TTS only in v1).
- Online stock catalogs, stock footage, account-backed licensing services, automatic content recommendation, and media without release-grade rights evidence. A small offline `Photonic Starter Audio` pack of rights-cleared music beds and ambient SFX is in scope and works without a network.

---

## Success Signal

SS-1: A 1080p30 timeline with 3 concurrent video layers, a grade, and captions plays at full frame rate without dropped frames on the reference development machine; 4K sources achieve the same via proxies.
SS-2: All three acceptance stories (social clip, short film, motion graphics — defined in 00-overview.md) are completable end-to-end by a first-time user without touching a config file — the social-clip story in under 30 minutes — and by an automation agent through the machine interface.
SS-3: Exported frames match preview rendering within a defined pixel tolerance on a golden-frame corpus, and exported A/V sync error stays under one video frame across a 10-minute sequence.

---

## Decisions

D-01: Full hybrid v1 — NLE + motion graphics + node compositing + grading, phased build (locked 2026-07-07)
D-02: Mode switch preserves existing layout; timeline panel docks at bottom; canvas doubles as program monitor (locked 2026-07-07)
D-03: FFmpeg as sidecar subprocess for decode/encode — no linking (locked 2026-07-07)
D-04: Captions/TTS via pluggable provider interface; user's hosted transcription + TTS services are the default backend (locked 2026-07-07)
D-05: Full audio mixer in v1 — automation, EQ/compression, ducking (locked 2026-07-07)
D-06: Node flows at both levels — per-clip compositions and a project-level output graph (locked 2026-07-07)
D-07: Acceptance = all three stories (locked 2026-07-07)
D-08: Architecture Approach A — timeline-first with node-ready frame-graph IR (locked 2026-07-07)
D-09: Video working color state is explicit per sequence. `LinearRec709Sdr` remains the default for new sequences and the compatibility default for every existing project. An explicitly selected `LinearRec2020Hdr` mode is permitted for approved HLG/PQ workflows; in that mode `1.0` represents 203 cd/m² HDR Reference White and the initial mastering-peak default is 1,000 cd/m². Every existing SDR golden remains pixel-stable. (Product/color review, locked 2026-07-12)
D-10: Renderer prerequisite work (dirty tracking, persistent GPU buffers, wire COMPOSITE_SHADER) precedes playback phases (locked 2026-07-07)
D-11: v1 ships a small starter set of vector-based title/lower-third templates and may ship a small offline `Photonic Starter Audio` pack under the asset-rights gate in `23-legal-open-source-implementation-routes.md`. Online stock catalogs, stock footage, account-backed licensing services, automatic content recommendation, and media without release-grade rights evidence remain out of scope. (Product/legal review, locked 2026-07-12)
D-12: Crash recovery extends Photonic's existing recovery machinery (recovery_path + relaunch prompt) to timeline projects — CAP-022; verified as a P3 exit criterion (PM review, locked 2026-07-07)

---

## Open Questions

<!-- None blocking. Provider API contract for hosted transcription/TTS is captured as an integration detail in 06-captions-ai.md, to be pinned before Phase 5 implementation. -->
