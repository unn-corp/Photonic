//! P3 facade/playback integration tests (02 §1/§4; fixtures are ground truth).
//!
//! Skip conventions: GPU tests skip-with-message when no adapter is available
//! (pool.rs convention); decode tests additionally skip when ffmpeg/ffprobe
//! are absent (decode_media.rs convention). Neither ever fails the suite on a
//! headless machine.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use photonic_core::timeline::{
    AssetKind, Clip, ClipAudio, ClipSource, FrameRate, MasterBus, MediaAsset, Sequence, Tick,
    TimelineCmd, TimelineProject, Track, TrackAudio, TrackId, TrackKind, TICKS_PER_SECOND,
};
use photonic_core::{Color, Command, CommandHistory, Document};

use photonic_video::audio::{ClipVoice, Mixer, PcmSource, TrackVoice, BLOCK_FRAMES, CHANNELS};
use photonic_video::decode::scheduler::{PtsKind, SourceParams};
use photonic_video::decode::{DecodeSource, PixFmt, SharedRing};
use photonic_video::graph::eval::read_texture_rgba16f;
use photonic_video::media::ffmpeg_locate::locate_for_test;
use photonic_video::media::keyframe_index::KeyframeIndex;
use photonic_video::media::probe::probe_details;
use photonic_video::playback::FfmpegPcmSource;
use photonic_video::{
    colorimetry_for_probe, EngineCmd, EngineFrame, EngineSession, GpuContext, VideoEngine,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

macro_rules! gpu_or_skip {
    () => {
        match GpuContext::request_blocking() {
            Some(gpu) => gpu,
            None => {
                eprintln!(
                    "no GPU adapter — skipping session test at {}:{}",
                    file!(),
                    line!()
                );
                return;
            }
        }
    };
}

macro_rules! tools_or_skip {
    () => {
        match locate_for_test() {
            Some(t) => t,
            None => {
                eprintln!(
                    "ffmpeg/ffprobe not found — skipping session test at {}:{}",
                    file!(),
                    line!()
                );
                return;
            }
        }
    };
}

/// Poll `latest_frame` until `pred` holds (engine work — probe, keyframe
/// index, cold seek — happens on its own thread).
fn wait_frame(
    session: &EngineSession,
    timeout: Duration,
    pred: impl Fn(&EngineFrame) -> bool,
) -> Option<Arc<EngineFrame>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(frame) = session.latest_frame() {
            if pred(&frame) {
                return Some(frame);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

fn doc_with_project(project: TimelineProject, w: f64, h: f64) -> Arc<Mutex<Document>> {
    let mut doc = Document::new("session-test", w, h);
    doc.timeline = Some(project);
    Arc::new(Mutex::new(doc))
}

// ── 1. Headless session: 2 tracks over counter.mp4 + color_bars.mp4 ──────────

#[test]
fn headless_session_seek_then_step_matches_decode_truth() {
    let gpu = gpu_or_skip!();
    let tools = tools_or_skip!();

    let rate = FrameRate::FPS_30;
    let tpf = rate.ticks_per_frame().0;

    // Tiny 2-track project: counter.mp4 on V1 [0,10s), color_bars.mp4 on V2
    // [4s,8s) — at the probed ticks (frames 75/76 ≈ 2.5s) only the counter is
    // visible, so the sampled pixels are decode truth for counter.mp4.
    let mut project = TimelineProject::new();
    let counter_id = project.media.insert(MediaAsset::from_file(
        AssetKind::Video,
        fixture("counter.mp4"),
    ));
    let bars_id = project.media.insert(MediaAsset::from_file(
        AssetKind::Video,
        fixture("color_bars.mp4"),
    ));

    let mut seq = Sequence::new("seq", rate, 320, 180);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::Asset { asset: counter_id },
        Tick(0),
        Tick::from_seconds(10),
    ));
    let mut v2 = Track::new(TrackKind::Video, "V2");
    v2.clips.push(Clip::new(
        ClipSource::Asset { asset: bars_id },
        Tick::from_seconds(4),
        Tick::from_seconds(4),
    ));
    seq.video_tracks.push(v1);
    seq.video_tracks.push(v2);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);

    let doc = doc_with_project(project, 320.0, 180.0);
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));

    let engine = VideoEngine::new(gpu.clone());
    let session = engine.open_session(doc, history);

    // Seek to a MID-frame tick inside frame 75: EngineFrame.time must be the
    // exact frame-start tick (cover-interval rule), not the raw seek target.
    session.send(EngineCmd::Seek(Tick(75 * tpf + tpf / 3)));
    let f75 = wait_frame(&session, Duration::from_secs(30), |f| {
        f.time == Tick(75 * tpf)
    })
    .expect("frame 75 presented with exact frame-start time");
    assert_eq!(f75.sequence, seq_id);
    assert_eq!(f75.time, Tick(75 * tpf), "EngineFrame.time is tick-exact");
    let engine_px_75 = read_texture_rgba16f(&gpu, &f75.texture, 320, 180);

    // Decode truth: the same frame through DecodeSource + the same
    // YUV→working conversion, entirely outside the engine.
    let truth_px_75 = decode_truth_pixels(&gpu, &tools, Tick(75 * tpf));
    assert_frames_match(&engine_px_75, &truth_px_75, 320, "frame 75");

    // Step exactly one frame (CAP-004): presented tick = frame 76's start.
    session.send(EngineCmd::Step(1));
    let f76 = wait_frame(&session, Duration::from_secs(15), |f| {
        f.time == Tick(76 * tpf)
    })
    .expect("step presents exactly frame 76");
    assert_eq!(
        f76.time,
        Tick(76 * tpf),
        "Step lands on the exact next tick"
    );
    let engine_px_76 = read_texture_rgba16f(&gpu, &f76.texture, 320, 180);
    let truth_px_76 = decode_truth_pixels(&gpu, &tools, Tick(76 * tpf));
    assert_frames_match(&engine_px_76, &truth_px_76, 320, "frame 76");

    // The burn-in digits advanced 75 → 76: the top-left region must differ.
    let burn_in_diff: f32 = (0..30)
        .flat_map(|y| (0..80).map(move |x| y * 320 + x))
        .map(|i| {
            (0..3)
                .map(|c| (engine_px_75[i][c] - engine_px_76[i][c]).abs())
                .sum::<f32>()
        })
        .sum();
    assert!(
        burn_in_diff > 1.0,
        "burn-in region differs between frames 75 and 76 (diff {burn_in_diff})"
    );

    // Status reflects the paused, stepped state.
    let status = session.status();
    assert!(!status.playing);
    assert_eq!(status.playhead, Tick(76 * tpf));
    assert_eq!(status.active_sequence, Some(seq_id));

    session.shutdown();
}

fn decode_truth_pixels(
    gpu: &GpuContext,
    tools: &photonic_video::media::ffmpeg_locate::FfmpegTools,
    target: Tick,
) -> Vec<[f32; 4]> {
    let tex = decode_truth_texture(gpu, tools, target);
    read_texture_rgba16f(gpu, &tex, 320, 180)
}

/// Decode `counter.mp4` at `target` outside the engine and upload it with the
/// identical probe-derived colorimetry — the ground-truth pixel path.
fn decode_truth_texture(
    gpu: &GpuContext,
    tools: &photonic_video::media::ffmpeg_locate::FfmpegTools,
    target: Tick,
) -> wgpu::Texture {
    let path = fixture("counter.mp4");
    let details = probe_details(tools, &path).expect("probe counter");
    let colorimetry = colorimetry_for_probe(&details);
    let keyframes = KeyframeIndex::build(tools, &path).expect("keyframe index");
    let params = SourceParams {
        input: path,
        width: 320,
        height: 180,
        pix_fmt: PixFmt::Yuv420p,
        pts_kind: PtsKind::Cfr(FrameRate::FPS_30),
        keyframes,
    };
    let mut src = DecodeSource::new(tools.clone(), params, SharedRing::preview());
    let frame = src.seek(target).expect("truth seek");
    assert_eq!(frame.pts, target, "truth decode is pts-exact");
    photonic_render::video::convert_yuv_planes_to_working(
        gpu.device(),
        gpu.queue(),
        &frame.planes.as_yuv_planes(),
        colorimetry,
    )
}

/// Full-frame compare with an f16-round-trip tolerance: the engine path adds
/// two extra `Rgba16Float` render hops (Transform2D + Output), which must be
/// pixel-exact identity maps over the decode-truth upload.
fn assert_frames_match(engine: &[[f32; 4]], truth: &[[f32; 4]], width: usize, label: &str) {
    assert_eq!(engine.len(), truth.len(), "{label}: same pixel count");
    for (i, (e_px, t_px)) in engine.iter().zip(truth.iter()).enumerate() {
        for c in 0..4 {
            let (e, t) = (e_px[c], t_px[c]);
            assert!(
                (e - t).abs() < 0.02,
                "{label}: pixel ({},{}) channel {c}: engine {e} vs truth {t}",
                i % width,
                i / width
            );
        }
    }
}

// ── 2. Soft-clock playback: monotonic presentation, exact frame ticks ────────

#[test]
fn playback_presents_monotonic_exact_frame_ticks() {
    let gpu = gpu_or_skip!();

    // Solid-color project: no ffmpeg dependency; the audio engine opens
    // lazily and falls back to the soft clock when no device exists.
    let rate = FrameRate::FPS_30;
    let tpf = rate.ticks_per_frame().0;
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("seq", rate, 16, 16);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 0.2,
                g: 0.4,
                b: 0.8,
                a: 1.0,
            },
        },
        Tick(0),
        Tick::from_seconds(10),
    ));
    seq.video_tracks.push(v1);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);

    let doc = doc_with_project(project, 16.0, 16.0);
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let engine = VideoEngine::new(gpu);
    let session = engine.open_session(doc, history);

    session.send(EngineCmd::Seek(Tick(0)));
    wait_frame(&session, Duration::from_secs(10), |f| f.time == Tick(0)).expect("initial frame");

    session.send(EngineCmd::Play);
    let mut times: Vec<Tick> = Vec::new();
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        if let Some(f) = session.latest_frame() {
            if times.last() != Some(&f.time) {
                times.push(f.time);
            }
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    session.send(EngineCmd::Pause);

    assert!(
        times.len() >= 3,
        "several distinct frames presented over 400ms of playback (got {times:?})"
    );
    assert!(
        times.windows(2).all(|w| w[0] < w[1]),
        "presentation times are strictly monotonic: {times:?}"
    );
    assert!(
        times.iter().all(|t| t.0 % tpf == 0),
        "every presented time is an exact frame-start tick: {times:?}"
    );

    // Pause is asynchronous — poll status until the engine processed it.
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.status().playing {
        assert!(Instant::now() < deadline, "engine paused after Pause");
        std::thread::sleep(Duration::from_millis(5));
    }
    session.shutdown();
}

// ── 3. Snapshot-on-revision: a TimelineCmd edit reaches the next compile ─────

#[test]
fn snapshot_on_revision_edit_is_visible_in_next_compile() {
    let gpu = gpu_or_skip!();

    let rate = FrameRate::FPS_30;
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("seq", rate, 16, 16);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        },
        Tick(0),
        Tick::from_seconds(2),
    ));
    let v2 = Track::new(TrackKind::Video, "V2"); // empty, edited below
    let v2_id = v2.id;
    seq.video_tracks.push(v1);
    seq.video_tracks.push(v2);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);

    let doc = doc_with_project(project, 16.0, 16.0);
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let engine = VideoEngine::new(gpu.clone());
    let session = engine.open_session(Arc::clone(&doc), Arc::clone(&history));

    session.send(EngineCmd::Seek(Tick(0)));
    let red = wait_frame(&session, Duration::from_secs(10), |f| f.time == Tick(0))
        .expect("initial red frame");
    let px = read_texture_rgba16f(&gpu, &red.texture, 16, 16)[8 * 16 + 8];
    assert!(
        px[0] > 0.9 && px[1] < 0.1,
        "V1 red visible before the edit ({px:?})"
    );

    // Edit the document through the real command path: insert a green clip on
    // V2 via a TimelineCmd. `CommandHistory::execute` bumps `revision`, which
    // the engine polls (doc_generation) to re-snapshot.
    {
        let mut doc = doc.lock().unwrap();
        let mut history = history.lock().unwrap();
        let green = Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 0.0,
                    g: 1.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
            Tick(0),
            Tick::from_seconds(2),
        );
        history.execute(
            Command::Timeline(TimelineCmd::InsertClip {
                seq: seq_id,
                track: v2_id,
                clip: Box::new(green),
            }),
            &mut doc,
        );
        assert!(history.revision() > 0, "execute bumped the revision");
    }

    // The engine re-snapshots on the revision bump and re-presents: the next
    // compile at the same tick must see the green top clip.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = px;
    loop {
        assert!(
            Instant::now() < deadline,
            "edit became visible (last pixel {last:?})"
        );
        if let Some(f) = session.latest_frame() {
            last = read_texture_rgba16f(&gpu, &f.texture, 16, 16)[8 * 16 + 8];
            if last[1] > 0.9 && last[0] < 0.1 {
                break; // green won
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let status = session.status();
    assert!(
        status.doc_revision > 0,
        "status carries the snapshot revision"
    );
    session.shutdown();
}

// ── 4. An unserviceable EngineCmd surfaces a real diagnostic on status ───────
//
// This test used to assert that `Probe` answered "not implemented", pinning
// the pre-K-0.8 stub. K-0.8 implemented `EngineCmd::Probe` (probe file →
// `set_asset_meta` + content hash) and K-0.1 implemented `Export`, so that
// assertion had been false for many commits — it just never ran, because CI
// was gated to `main` and this branch never opened a PR.
//
// The contract worth pinning now is the one that outlives the stubs: a command
// the engine genuinely cannot service must surface an informative error on
// `EngineStatus.last_error` rather than being dropped silently. `Export`'s own
// success path is covered by `tests/export_engine_cmd.rs`; this covers the
// error channel.

#[test]
fn unserviceable_engine_cmd_surfaces_an_error_on_status() {
    let gpu = gpu_or_skip!();
    // Empty project, so no timeline snapshot ever reaches the engine thread —
    // a Probe for an asset that does not exist cannot be serviced.
    let doc = doc_with_project(TimelineProject::new(), 16.0, 16.0);
    let history = Arc::new(Mutex::new(CommandHistory::new(8)));
    let engine = VideoEngine::new(gpu);
    let session = engine.open_session(doc, history);

    let unknown = photonic_core::timeline::AssetId::new();
    session.send(EngineCmd::Probe(unknown));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = session.status();
        if let Some(err) = &status.last_error {
            let text = err.to_string();
            assert!(
                !text.contains("not implemented"),
                "Probe is implemented since K-0.8; a 'not implemented' error \
                 means the command regressed to a stub: {text}"
            );
            assert!(
                !err.message.trim().is_empty(),
                "the surfaced Diagnostic must say something actionable"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "an unserviceable Probe must surface on EngineStatus.last_error, \
             not be dropped silently"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    session.shutdown();
}

// ── 5. A/V sync: beep_flash.mp4 flash frame matches the beep onset (CAP-004) ─
//
// 02 §4's master clock and 11 §3.3's A/V-sync hook: play (here, sample)
// `beep_flash.mp4`+`.wav`, whose flash-every-1s / beep-every-1s ground truth
// is documented in `tests/fixtures/README.md`, and assert the offset between
// them stays under one video frame.

/// Mean of the RGB channels (unpremultiplied is fine here — everything in
/// `beep_flash.mp4` is fully opaque, alpha == 1) across every pixel: high for
/// the all-white flash frame, near zero for every other (black) frame.
fn mean_brightness(pixels: &[[f32; 4]]) -> f32 {
    let sum: f32 = pixels.iter().map(|p| (p[0] + p[1] + p[2]) / 3.0).sum();
    sum / pixels.len() as f32
}

/// Mean alpha across every pixel. Real decoded content is always fully
/// opaque (D-09); the evaluator's failure-substitute (`Evaluator::transparent`,
/// `graph/eval.rs`) fills `[0.0; 4]` — alpha 0 — so this is the unambiguous
/// signal that a frame is the diagnostic placeholder rather than genuinely
/// black decoded content (beep_flash.mp4 is mostly black by design).
fn mean_alpha(pixels: &[[f32; 4]]) -> f32 {
    pixels.iter().map(|p| p[3]).sum::<f32>() / pixels.len() as f32
}

/// Render the engine's own mixed PCM for `beep_flash.wav` on a single
/// unfaded clip starting at tick 0, until at least `min_frames` stereo
/// frames exist. This is the exact [`Mixer`]/[`FfmpegPcmSource`] combination
/// `session.rs`'s `feeder_main` drives for real playback (09 §4/§7: "runs
/// identically on the interactive mixer-worker path and the offline export
/// path ... same function, no device clock") — called directly so the test
/// is headless-safe (no cpal device required) while still exercising the
/// engine's real audio-mix code, not a synthetic stand-in.
fn render_beep_flash_mix(
    tools: &photonic_video::media::ffmpeg_locate::FfmpegTools,
    sample_rate: u32,
    min_frames: usize,
) -> Vec<f32> {
    let mut src =
        FfmpegPcmSource::spawn(tools, &fixture("beep_flash.wav"), Tick::ZERO, sample_rate)
            .expect("spawn beep_flash.wav pcm sidecar");
    let mut mixer = Mixer::new(sample_rate);
    let clip_audio = ClipAudio::new();
    let track_audio = TrackAudio::new();
    let master = MasterBus::new();
    let track_id = TrackId::new();

    let tick_per_sample = TICKS_PER_SECOND / sample_rate as i64;
    let block_ticks = Tick(BLOCK_FRAMES as i64 * tick_per_sample);

    let mut mixed: Vec<f32> = Vec::with_capacity((min_frames + BLOCK_FRAMES) * CHANNELS);
    let mut out = vec![0f32; BLOCK_FRAMES * CHANNELS];
    let mut t = Tick::ZERO;
    while mixed.len() / CHANNELS < min_frames {
        let mut tracks = [TrackVoice {
            id: track_id,
            audio: &track_audio,
            clips: vec![ClipVoice {
                audio: &clip_audio,
                elapsed: t,
                remaining: Tick::from_seconds(60) - t,
                source: &mut src as &mut dyn PcmSource,
            }],
        }];
        mixer.render_block(t, &mut tracks, &master, &mut out);
        mixed.extend_from_slice(&out);
        t = t + block_ticks;
    }
    mixed
}

/// Earliest sample at or after `search_from_frame` (left channel — the mono
/// beep is duplicated across both, `FfmpegPcmSource`'s `-ac 2`) whose
/// magnitude clears `threshold`, as a [`Tick`] at `sample_rate`. `threshold`
/// sits well above the off-instant noise floor and well below the beep's
/// amplitude (`playback/pcm.rs`'s own test: off-window rms < 1e-3, on-window
/// rms > 0.05). `search_from_frame` skips past the beep-flash pattern's t=0
/// instant (frame 0 also satisfies `N % 30 == 0`, fixtures/README.md) so this
/// finds a genuine *mid-stream* onset, not the trivially-first one.
fn find_onset_tick(
    mixed: &[f32],
    sample_rate: u32,
    threshold: f32,
    search_from_frame: usize,
) -> Tick {
    let tick_per_sample = TICKS_PER_SECOND / sample_rate as i64;
    let frames = mixed.len() / CHANNELS;
    for f in search_from_frame..frames {
        if mixed[f * CHANNELS].abs() > threshold {
            return Tick(f as i64 * tick_per_sample);
        }
    }
    panic!("no beep onset found above threshold {threshold} within {frames} rendered frames");
}

#[test]
fn av_sync_beep_flash_offset_under_one_frame() {
    let gpu = gpu_or_skip!();
    let tools = tools_or_skip!();

    let rate = FrameRate::FPS_30;
    let tpf = rate.ticks_per_frame().0;
    let sample_rate = 48_000u32;

    // 1. Measure the beep's onset tick from the engine's OWN mixed PCM —
    //    not the documented ground truth directly, an actual measurement of
    //    the engine's real audio-mix code. Render past the documented
    //    t=1.0s instant (fixtures/README.md) and take the first
    //    above-noise-floor sample from t=0.5s onward (the pattern also beeps
    //    at t=0, which this skips past to land on a genuine mid-stream pair).
    let mixed = render_beep_flash_mix(&tools, sample_rate, sample_rate as usize * 11 / 10);
    let onset_tick = find_onset_tick(&mixed, sample_rate, 0.02, sample_rate as usize / 2);

    // 2. Drive the real engine (graph compile + eval + decode — CAP-004's
    //    video path) over beep_flash.mp4 and sample which frame it presents
    //    AT that exact measured instant.
    let mut project = TimelineProject::new();
    let video_id = project.media.insert(MediaAsset::from_file(
        AssetKind::Video,
        fixture("beep_flash.mp4"),
    ));
    let mut seq = Sequence::new("seq", rate, 320, 180);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::Asset { asset: video_id },
        Tick(0),
        Tick::from_seconds(2),
    ));
    seq.video_tracks.push(v1);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);

    let doc = doc_with_project(project, 320.0, 180.0);
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let engine = VideoEngine::new(gpu.clone());
    let session = engine.open_session(doc, history);

    session.send(EngineCmd::Seek(onset_tick));
    let presented = wait_frame(&session, Duration::from_secs(30), |f| {
        (f.time.0 - onset_tick.0).abs() < tpf
    })
    .expect("engine presents the frame covering the measured beep-onset tick");

    // CAP-004/SS-1: beep_flash.mp4 is white for exactly the one frame whose
    // instant the beep coincides with (fixtures/README.md) and black
    // everywhere else. If the engine's audio-mix timing (step 1) and its
    // video seek/decode timing (step 2) agree to within one frame, the frame
    // covering the *measured* onset tick must be that flash frame — a >=1
    // frame A/V drift in either direction presents a black frame instead, so
    // this brightness check IS the "offset < 1 frame" assertion (a raw tick
    // comparison against the seek target would be tautological: the
    // cover-interval rule already guarantees the presented frame's start is
    // within one frame of *whatever* tick was requested, seek accuracy
    // aside).
    let flash_brightness =
        mean_brightness(&read_texture_rgba16f(&gpu, &presented.texture, 320, 180));
    assert!(
        flash_brightness > 0.7,
        "frame presented at the measured beep-onset tick ({onset_tick:?}, engine \
         presented {:?}) is not the flash frame — mean brightness {flash_brightness} \
         (A/V offset >= 1 frame)",
        presented.time,
    );

    // Control: half a second away from any flash instant (flashes are 1s
    // apart, each lasting 1/30s) is unambiguously off — rules out an
    // "every frame looks bright" false positive.
    let off_tick = Tick(onset_tick.0 - TICKS_PER_SECOND / 2);
    session.send(EngineCmd::Seek(off_tick));
    let off = wait_frame(&session, Duration::from_secs(15), |f| {
        f.time != presented.time && (f.time.0 - off_tick.0).abs() < tpf
    })
    .expect("engine presents the off-instant control frame");
    let off_brightness = mean_brightness(&read_texture_rgba16f(&gpu, &off.texture, 320, 180));
    assert!(
        off_brightness < 0.3,
        "off-instant control frame (half a second from the beep) should be dark, \
         got mean brightness {off_brightness}"
    );

    session.shutdown();
}

// ── 6. Sidecar-failure containment: a killed decode never blocks the engine ──

/// Repeatedly SIGKILLs every process whose command line contains `needle`
/// (the fixture path) until `stop` is set. Simulates "kill/starve a sidecar
/// mid-decode" (02 §3, 11 §3.3) from outside the engine, which owns the
/// child process itself and exposes no handle to it.
fn kill_matching_loop(needle: &str, stop: &AtomicBool, period: Duration) {
    while !stop.load(Ordering::Relaxed) {
        let _ = std::process::Command::new("pkill")
            .args(["-9", "-f"])
            .arg(needle)
            .status();
        std::thread::sleep(period);
    }
}

/// 02 §3's failure-containment guarantee — "sidecar crash/EOF ... scheduler
/// restarts process (max 3, backoff); frame slot renders diagnostic
/// placeholder. A wedged pipe never blocks the engine thread" — under
/// realistic conditions: continuously kill the real ffmpeg decode process
/// the engine spawns for `beep_flash.mp4` while it's playing, and assert
/// (a) the engine's status-publish loop keeps making forward progress the
/// whole time (never wedges on the dead pipe/child), (b) decode misses are
/// contained by holding the last opaque frame rather than flashing a transparent
/// placeholder, and (c) the engine is still fully controllable (`Pause`
/// completes) once the killing stops.
///
/// Needs a real ffmpeg subprocess to kill, plus `pkill` (Linux/macOS) — the
/// one test class 11 §3.3 flags as needing a real subprocess — so it is
/// `#[ignore]`d by default; run explicitly with `cargo test -- --ignored`
/// (ffmpeg present locally + CI, per this story's DoD).
#[test]
#[ignore = "spawns and kills real ffmpeg subprocesses; run with --ignored where ffmpeg is present"]
fn sidecar_kill_mid_decode_never_blocks_the_engine() {
    let gpu = gpu_or_skip!();
    // Gate on ffmpeg being present (the engine locates its own tools
    // internally); this test only shells out to `pkill` directly.
    let _tools = tools_or_skip!();
    if std::process::Command::new("pkill")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("pkill not found — skipping sidecar-failure containment test");
        return;
    }

    let rate = FrameRate::FPS_30;
    let video_path = fixture("beep_flash.mp4");
    let mut project = TimelineProject::new();
    let video_id = project
        .media
        .insert(MediaAsset::from_file(AssetKind::Video, video_path.clone()));
    let mut seq = Sequence::new("seq", rate, 320, 180);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::Asset { asset: video_id },
        Tick(0),
        Tick::from_seconds(60),
    ));
    seq.video_tracks.push(v1);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);

    let doc = doc_with_project(project, 320.0, 180.0);
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let engine = VideoEngine::new(gpu.clone());
    let session = engine.open_session(doc, history);

    // Baseline: normal decode works before any interference.
    session.send(EngineCmd::Seek(Tick(0)));
    wait_frame(&session, Duration::from_secs(30), |f| f.time == Tick(0))
        .expect("baseline decode succeeds before sidecar interference");

    session.send(EngineCmd::Play);
    let (_, misses_before) = photonic_video::session::decode_stats();

    // Kill every ffmpeg decoding this fixture, repeatedly and tightly, for a
    // few seconds — long enough to land on multiple in-flight decode/restart
    // attempts.
    let stop = Arc::new(AtomicBool::new(false));
    let killer = {
        let stop = Arc::clone(&stop);
        let needle = video_path.to_string_lossy().into_owned();
        std::thread::spawn(move || kill_matching_loop(&needle, &stop, Duration::from_millis(3)))
    };

    // The engine's status-publish loop must keep making forward progress
    // throughout (never stall for longer than the bounded restart/backoff
    // budget — MAX_RESTARTS=3 with ~20/40/80ms backoff between attempts,
    // well under this ceiling) — proof the dead pipe never wedges the
    // engine thread itself, only bounds each blocked call.
    let kill_window = Duration::from_millis(3_000);
    let stall_ceiling = Duration::from_secs(5);
    let mut last_playhead = session.status().playhead;
    let mut progress_at = Instant::now();
    let probe_deadline = Instant::now() + kill_window;
    let mut saw_opaque_frame = false;
    let mut saw_transparent_frame = false;
    while Instant::now() < probe_deadline {
        let status = session.status();
        if status.playhead != last_playhead {
            last_playhead = status.playhead;
            progress_at = Instant::now();
        }
        assert!(
            progress_at.elapsed() < stall_ceiling,
            "engine status hasn't advanced in {:?} under sidecar-kill pressure — the \
             engine thread is blocked past its bounded read/restart deadline",
            progress_at.elapsed()
        );
        if let Some(frame) = session.latest_frame() {
            let px = read_texture_rgba16f(&gpu, &frame.texture, 320, 180);
            if mean_alpha(&px) < 0.5 {
                // Alpha 0 only ever comes from `Evaluator::transparent`'s
                // failure substitute — real decoded content is always
                // opaque, even the (mostly black) genuine content here.
                saw_transparent_frame = true;
            } else {
                saw_opaque_frame = true;
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    stop.store(true, Ordering::Relaxed);
    let _ = killer.join();

    let (_, misses_after) = photonic_video::session::decode_stats();
    assert!(
        misses_after > misses_before,
        "sidecar kill pressure must cause at least one bounded decode miss"
    );
    assert!(
        saw_opaque_frame,
        "the last good frame must remain presentable"
    );
    assert!(
        !saw_transparent_frame,
        "sidecar failure must hold the last good frame, never flash the transparent substitute"
    );

    // Recovery: with the killing stopped, the engine must still be fully
    // controllable — Pause completes within a generous bounded deadline.
    session.send(EngineCmd::Pause);
    let deadline = Instant::now() + Duration::from_secs(10);
    while session.status().playing {
        assert!(
            Instant::now() < deadline,
            "engine failed to process Pause after sidecar-kill pressure — engine thread wedged"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    session.shutdown();
}

// ── Cut-ahead prefetch + hold-last-frame (Phase 1) ───────────────────────────

/// Playing across a hard cut A→B must not flash a transparent placeholder:
/// cut-ahead warms B's decoder before the boundary, and hold-last-frame covers
/// any residual worker catch-up. Asserts every frame presented in the ±3-frame
/// window around the cut is opaque (real content, not `Evaluator::transparent`).
#[test]
fn cut_ahead_crosses_hard_cut_without_transparent_frame() {
    let gpu = gpu_or_skip!();
    let _tools = tools_or_skip!();

    let (w, h) = (320u32, 180u32);
    let rate = FrameRate::FPS_30;
    let tpf = rate.ticks_per_frame().0;
    let cut = Tick::from_seconds(2);

    // Single video track, two clips of different assets: counter.mp4 [0,2s),
    // color_bars.mp4 [2s,4s). Single track ⇒ the composite is exactly one
    // source, so a transparent frame can only come from a source miss.
    let mut project = TimelineProject::new();
    let a = project.media.insert(MediaAsset::from_file(
        AssetKind::Video,
        fixture("counter.mp4"),
    ));
    let b = project.media.insert(MediaAsset::from_file(
        AssetKind::Video,
        fixture("color_bars.mp4"),
    ));
    let mut seq = Sequence::new("seq", rate, w, h);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips
        .push(Clip::new(ClipSource::Asset { asset: a }, Tick(0), cut));
    v1.clips
        .push(Clip::new(ClipSource::Asset { asset: b }, cut, cut));
    seq.video_tracks.push(v1);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);

    let doc = doc_with_project(project, w as f64, h as f64);
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let session = VideoEngine::new(gpu.clone()).open_session(doc, history);

    // Warm clip A at ~1.4s so its own cold-start isn't in the measured window.
    let start = Tick(cut.0 - 20 * tpf);
    session.send(EngineCmd::Seek(start));
    wait_frame(&session, Duration::from_secs(30), |f| f.time >= start)
        .expect("clip A frame before the cut");

    session.send(EngineCmd::Play);
    let mut samples: Vec<(Tick, f32)> = Vec::new();
    let end = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < end {
        if let Some(f) = session.latest_frame() {
            if samples.last().map(|(t, _)| *t) != Some(f.time) {
                let px = read_texture_rgba16f(&gpu, &f.texture, w, h);
                samples.push((f.time, mean_alpha(&px)));
            }
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    session.send(EngineCmd::Pause);
    session.shutdown();

    let lo = Tick(cut.0 - 3 * tpf);
    let hi = Tick(cut.0 + 3 * tpf);
    let in_window: Vec<_> = samples
        .iter()
        .filter(|(t, _)| *t >= lo && *t <= hi)
        .collect();
    assert!(
        in_window.iter().any(|(t, _)| *t < cut) && in_window.iter().any(|(t, _)| *t >= cut),
        "captured frames on both sides of the cut (window={in_window:?})"
    );
    for (t, alpha) in &in_window {
        assert!(
            *alpha > 0.5,
            "frame at {t:?} across the cut is opaque, not a transparent placeholder \
             (alpha {alpha}); window={in_window:?}"
        );
    }
}

/// While playing, a transient source miss must hold the previous frame, never a
/// transparent placeholder. Plays from a cold start (after warming frame 0) and
/// asserts no presented frame is the transparent substitute.
#[test]
fn hold_last_frame_keeps_playback_opaque() {
    let gpu = gpu_or_skip!();
    let _tools = tools_or_skip!();

    let (w, h) = (320u32, 180u32);
    let rate = FrameRate::FPS_30;

    let mut project = TimelineProject::new();
    let a = project.media.insert(MediaAsset::from_file(
        AssetKind::Video,
        fixture("counter.mp4"),
    ));
    let mut seq = Sequence::new("seq", rate, w, h);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::Asset { asset: a },
        Tick(0),
        Tick::from_seconds(9),
    ));
    seq.video_tracks.push(v1);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);

    let doc = doc_with_project(project, w as f64, h as f64);
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let session = VideoEngine::new(gpu.clone()).open_session(doc, history);

    session.send(EngineCmd::Seek(Tick(0)));
    wait_frame(&session, Duration::from_secs(30), |f| f.time == Tick(0)).expect("frame 0");

    session.send(EngineCmd::Play);
    let mut count = 0usize;
    let end = Instant::now() + Duration::from_millis(1000);
    let mut last = Tick(-1);
    while Instant::now() < end {
        if let Some(f) = session.latest_frame() {
            if f.time != last {
                last = f.time;
                let px = read_texture_rgba16f(&gpu, &f.texture, w, h);
                assert!(
                    mean_alpha(&px) > 0.5,
                    "playing frame at {:?} is opaque (held or real), not transparent",
                    f.time
                );
                count += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    session.send(EngineCmd::Pause);
    session.shutdown();
    assert!(
        count >= 3,
        "several frames presented while playing (got {count})"
    );
}
