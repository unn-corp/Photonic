//! P8 release-gating SS-3 sync-drift export test (11-testing-phasing.md §5,
//! normative). `#[ignore]`d by default (nightly); skip-without-ffmpeg/GPU per
//! the crate's established convention (`session_playback.rs`,
//! `export_synthetic.rs`).
//!
//! SS-3: "Exported ... A/V sync error stays under one video frame across a
//! 10-minute sequence." This builds the synthetic 10-minute timeline 11 §5
//! describes — `beep_flash.mp4`/`.wav` repeated 10x on a timeline (not a
//! single 10-minute source file, keeping the fixture corpus small) — compiles
//! and evaluates it through the real frame-graph pipeline (`graph::compile` +
//! `graph::eval::Evaluator`, 02 §2), exports it through the real
//! `export::render_loop` (02 §7), decodes the export back, detects each of
//! the ~600 flash-frame / beep-onset instants by simple thresholding (luma
//! spike / RMS spike), and asserts the worst-case offset across the whole
//! span stays under one frame duration (33.3ms at 30fps).
//!
//! Decode efficiency: `beep_flash.mp4`'s content is bit-identical every loop
//! (10 back-to-back timeline clips over the same asset, each `source_in`
//! reset to 0), so its one 60s/1800-frame loop is decoded sequentially exactly
//! once (`DecodeSource::seek` + `pump`, the real decode path, 02 §3) and
//! served to the real compiled graph's `DecodeVideo` node once per requested
//! export frame via a small [`GpuFrameSource`] — real decode, color-convert,
//! and frame-graph-eval code for all 18,000 frames, without paying for 18,000
//! independent process spawns. Audio is likewise mixed once through the real
//! `Mixer`/`FfmpegPcmSource` pair (09 §4/§7) and repeated into the full 600s
//! buffer.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use photonic_core::timeline::{
    AssetId, AssetKind, Clip, ClipAudio, ClipSource, FrameRate, MasterBus, MediaAsset, Sequence,
    SequenceId, Tick, TimelineProject, Track, TrackAudio, TrackId, TrackKind, VectorRef,
    VectorStateKey, TICKS_PER_SECOND,
};
use photonic_render::color::Colorimetry;

use photonic_video::audio::{ClipVoice, Mixer, PcmSource, TrackVoice, BLOCK_FRAMES, CHANNELS};
use photonic_video::decode::scheduler::{PtsKind, SourceParams};
use photonic_video::decode::{DecodeSource, DecodedFrame, PixFmt, SharedRing};
use photonic_video::export::encoder::AudioStreamSpec;
use photonic_video::export::presets::built_in_presets;
use photonic_video::export::render_loop::{export_frames, ExportEvent, Frame, ResolvedExport};
use photonic_video::graph::compile::{compile, Quality};
use photonic_video::graph::eval::{read_texture_rgba16f, Evaluator, GpuFrame, GpuFrameSource};
use photonic_video::media::ffmpeg_locate::{locate_for_test, FfmpegTools};
use photonic_video::media::keyframe_index::KeyframeIndex;
use photonic_video::media::probe::probe_details;
use photonic_video::playback::FfmpegPcmSource;
use photonic_video::{colorimetry_for_probe, GpuContext};

const LOOPS: u64 = 10;
const FIXTURE_SECS: u64 = 60;
const RATE: FrameRate = FrameRate::FPS_30;
const FRAMES_PER_LOOP: u64 = FIXTURE_SECS * (RATE.num as u64) / (RATE.den as u64);
const TOTAL_FRAMES: u64 = FRAMES_PER_LOOP * LOOPS;
const W: u32 = 320;
const H: u32 = 180;
const SAMPLE_RATE: u32 = 48_000;
/// Search radius around each expected flash/beep instant — generous next to
/// the < 1-frame tolerance under test, but well inside the 1s gap to the
/// neighboring pair.
const LUMA_WINDOW_FRAMES: i64 = 8;
const AUDIO_WINDOW_SAMPLES: i64 = SAMPLE_RATE as i64 / 4;
/// Threshold separating on-beep energy (rms > 0.05) from the off-beep noise
/// floor (rms < 1e-3) — same value `session_playback.rs`'s `find_onset_tick`
/// uses, and `pcm.rs`'s own unit test verifies that separation.
const BEEP_THRESHOLD: f32 = 0.02;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn tmp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "photonic-ss3-sync-drift-{}-{name}",
        std::process::id()
    ))
}

macro_rules! gpu_or_skip {
    () => {
        match GpuContext::request_blocking() {
            Some(gpu) => gpu,
            None => {
                eprintln!(
                    "no GPU adapter — skipping SS-3 sync-drift test at {}:{}",
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
                    "ffmpeg/ffprobe not found — skipping SS-3 sync-drift test at {}:{}",
                    file!(),
                    line!()
                );
                return;
            }
        }
    };
}

/// Sequentially decode `beep_flash.mp4`'s one 60s/1800-frame loop, in order,
/// via a single ffmpeg sidecar (`seek` once + `pump` forward) — the real
/// decode path (02 §3), driven directly rather than through 18,000 independent
/// engine seeks, since the 10 repeated timeline clips are bit-identical.
fn decode_one_loop(tools: &FfmpegTools) -> (Vec<Arc<DecodedFrame>>, Colorimetry) {
    let path = fixture("beep_flash.mp4");
    let details = probe_details(tools, &path).expect("probe beep_flash.mp4");
    let colorimetry = colorimetry_for_probe(&details);
    let keyframes = KeyframeIndex::build(tools, &path).expect("keyframe index");
    let params = SourceParams {
        input: path,
        width: W,
        height: H,
        pix_fmt: PixFmt::Yuv420p,
        pts_kind: PtsKind::Cfr(RATE),
        keyframes,
    };
    let mut src = DecodeSource::new(tools.clone(), params, SharedRing::preview());
    let tpf = RATE.ticks_per_frame().0;

    let first = src.seek(Tick::ZERO).expect("seek to frame 0");
    assert_eq!(first.pts, Tick::ZERO, "first frame is pts-exact");
    let mut frames = Vec::with_capacity(FRAMES_PER_LOOP as usize);
    frames.push(first);
    for n in 1..FRAMES_PER_LOOP as i64 {
        let target = Tick(n * tpf);
        src.pump(1).expect("sequential pump");
        // Slide the ring's eviction window with the consumer (mirrors the
        // engine's own `prefetch::pump_ahead`) so the just-decoded frame
        // isn't pruned out from under a purely-forward, one-at-a-time read.
        src.ring().set_playhead(target);
        let f = src
            .ring()
            .get(target)
            .unwrap_or_else(|| panic!("frame {n} missing from the ring after a sequential pump"));
        frames.push(f);
    }
    assert_eq!(
        frames.len() as u64,
        FRAMES_PER_LOOP,
        "beep_flash.mp4 decoded its documented {FRAMES_PER_LOOP} frames"
    );
    (frames, colorimetry)
}

/// Serves the pre-decoded one-loop `beep_flash.mp4` frames to the real
/// `graph::eval::Evaluator` for whichever asset the compiled 10-loop timeline
/// asks for, converting through the real YUV→working GPU path
/// (`photonic_render::video::convert_yuv_planes_to_working`) per request — one
/// real decode-and-convert per exported frame, backed by a cache-free lookup
/// into the once-decoded loop instead of a fresh ffmpeg seek.
struct LoopFrameSource<'a> {
    asset: AssetId,
    frames: &'a [Arc<DecodedFrame>],
    colorimetry: Colorimetry,
    tpf: i64,
}

impl GpuFrameSource for LoopFrameSource<'_> {
    fn video_texture(
        &mut self,
        gpu: &GpuContext,
        asset: AssetId,
        src_time: Tick,
        _proxy: bool,
    ) -> Option<GpuFrame> {
        if asset != self.asset {
            return None;
        }
        let local = src_time.0.div_euclid(self.tpf) % self.frames.len() as i64;
        let frame = &self.frames[local as usize];
        let tex = photonic_render::video::convert_yuv_planes_to_working(
            gpu.device(),
            gpu.queue(),
            &frame.planes.as_yuv_planes(),
            self.colorimetry,
        );
        let (width, height) = (tex.width(), tex.height());
        Some(GpuFrame::new(Arc::new(tex), width, height))
    }

    fn still_texture(
        &mut self,
        _gpu: &GpuContext,
        _asset: AssetId,
        _w: u32,
        _h: u32,
    ) -> Option<GpuFrame> {
        None
    }

    fn vector_texture(
        &mut self,
        _gpu: &GpuContext,
        _vref: VectorRef,
        _key: VectorStateKey,
        _w: u32,
        _h: u32,
    ) -> Option<GpuFrame> {
        None
    }
}

/// The synthetic 10-minute timeline (11 §5): 10 back-to-back 60s clips over
/// the single `beep_flash.mp4` asset, `source_in` implicitly `Tick::ZERO` for
/// each (a fresh `Clip` per repeat, not a single looped clip) so every repeat
/// plays the fixture's own ground-truth flash/beep pattern from its start.
fn build_ten_minute_project() -> (TimelineProject, SequenceId, AssetId) {
    let mut project = TimelineProject::new();
    let asset_id = project.media.insert(MediaAsset::from_file(
        AssetKind::Video,
        fixture("beep_flash.mp4"),
    ));
    let mut seq = Sequence::new("ss3-ten-minute", RATE, W, H);
    let seq_id = seq.id;
    let mut track = Track::new(TrackKind::Video, "V1");
    let clip_len = Tick::from_seconds(FIXTURE_SECS as i64);
    for i in 0..LOOPS {
        let start = Tick(clip_len.0 * i as i64);
        track.clips.push(Clip::new(
            ClipSource::Asset { asset: asset_id },
            start,
            clip_len,
        ));
    }
    seq.video_tracks.push(track);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);
    (project, seq_id, asset_id)
}

/// Render exactly `frames` stereo samples of the engine's own mixed PCM for
/// one `beep_flash.wav` clip starting at tick 0 — the same [`Mixer`]/
/// [`FfmpegPcmSource`] combination `session.rs`'s `feeder_main` and the real
/// export audio path both drive (09 §4/§7), called directly for a headless,
/// deterministic render.
fn render_wav_mix_exact(tools: &FfmpegTools, sample_rate: u32, frames: usize) -> Vec<f32> {
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
    let clip_len = Tick::from_seconds(FIXTURE_SECS as i64);

    let mut mixed: Vec<f32> = Vec::with_capacity((frames + BLOCK_FRAMES) * CHANNELS);
    let mut out = vec![0f32; BLOCK_FRAMES * CHANNELS];
    let mut t = Tick::ZERO;
    while mixed.len() / CHANNELS < frames {
        let mut tracks = [TrackVoice {
            id: track_id,
            audio: &track_audio,
            clips: vec![ClipVoice {
                audio: &clip_audio,
                elapsed: t,
                remaining: clip_len.saturating_sub(t),
                source: &mut src as &mut dyn PcmSource,
            }],
        }];
        mixer.render_block(t, &mut tracks, &master, &mut out);
        mixed.extend_from_slice(&out);
        t = t + block_ticks;
    }
    mixed.truncate(frames * CHANNELS);
    mixed
}

/// Decode `path`'s video back to one mean-luma sample per frame via a single
/// sequential `gray` rawvideo pipe (11 §5's "flash = frame luma spike"
/// thresholding) — one ffmpeg decode pass over the whole export, not 600
/// independent seeks.
fn decode_export_luma(tools: &FfmpegTools, path: &Path) -> Vec<f32> {
    let mut child = Command::new(&tools.ffmpeg)
        .args(["-v", "error", "-nostdin"])
        .arg("-i")
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "gray"])
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("ffmpeg gray decode spawns");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let frame_bytes = (W * H) as usize;
    let mut buf = vec![0u8; frame_bytes];
    let mut means = Vec::new();
    loop {
        match stdout.read_exact(&mut buf) {
            Ok(()) => {
                let sum: u64 = buf.iter().map(|&b| b as u64).sum();
                means.push(sum as f32 / frame_bytes as f32);
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("reading decoded luma frames: {e}"),
        }
    }
    let status = child.wait().expect("ffmpeg gray decode exits");
    assert!(status.success(), "ffmpeg gray decode of {path:?} failed");
    means
}

/// Decode `path`'s audio back to mono f32 PCM at `sample_rate` (11 §5's
/// "beep = audio RMS spike" thresholding; mono is fine since the beep is
/// duplicated across both channels, same as `FfmpegPcmSource`'s `-ac 2`).
fn decode_export_audio_mono(tools: &FfmpegTools, path: &Path, sample_rate: u32) -> Vec<f32> {
    let out = Command::new(&tools.ffmpeg)
        .args(["-v", "error", "-nostdin"])
        .arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "f32le",
        ])
        .arg("pipe:1")
        .stdin(Stdio::null())
        .output()
        .expect("ffmpeg audio decode spawns");
    assert!(
        out.status.success(),
        "ffmpeg audio decode of {path:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// The frame index within `[center-window, center+window]` with the highest
/// mean luma — the measured flash-frame position for the flash expected at
/// `center`.
fn find_flash_frame(luma: &[f32], center: i64, window: i64) -> i64 {
    let lo = (center - window).max(0);
    let hi = (center + window).min(luma.len() as i64 - 1);
    (lo..=hi)
        .max_by(|&a, &b| luma[a as usize].partial_cmp(&luma[b as usize]).unwrap())
        .unwrap_or_else(|| panic!("no luma samples near frame {center}"))
}

/// The first sample within `[center-window, center+window]` whose magnitude
/// clears `threshold` — the measured beep-onset position for the beep
/// expected at `center`.
fn find_beep_onset(mono: &[f32], center: i64, window: i64, threshold: f32) -> i64 {
    let lo = (center - window).max(0);
    let hi = (center + window).min(mono.len() as i64 - 1);
    for i in lo..=hi {
        if mono[i as usize].abs() > threshold {
            return i;
        }
    }
    panic!("no beep onset above {threshold} within samples [{lo},{hi}] (expected near {center})");
}

#[test]
#[ignore = "10-minute synthetic export; run with --ignored where ffmpeg + a GPU adapter are present (nightly, 11 §5/§1.6, SS-3)"]
fn ss3_sync_drift_ten_minute_export_under_one_frame() {
    let gpu = gpu_or_skip!();
    let tools = tools_or_skip!();

    let tpf = RATE.ticks_per_frame().0;

    // 1. The synthetic 10-minute timeline (11 §5) — 10 back-to-back
    //    beep_flash.mp4 clips — compiled and evaluated through the real
    //    frame-graph pipeline per exported frame.
    let (project, seq_id, asset_id) = build_ten_minute_project();
    let (loop_frames, colorimetry) = decode_one_loop(&tools);
    let mut source = LoopFrameSource {
        asset: asset_id,
        frames: &loop_frames,
        colorimetry,
        tpf,
    };
    let mut evaluator = Evaluator::new(gpu.clone());

    // 2. The engine's own mixed PCM for one 60s loop, repeated into the full
    //    600s buffer (bit-identical every loop — same wav, reset to its own
    //    start each repeat, exactly mirroring the video side).
    let one_loop_mix = render_wav_mix_exact(
        &tools,
        SAMPLE_RATE,
        FIXTURE_SECS as usize * SAMPLE_RATE as usize,
    );
    let mut audio_samples = Vec::with_capacity(one_loop_mix.len() * LOOPS as usize);
    for _ in 0..LOOPS {
        audio_samples.extend_from_slice(&one_loop_mix);
    }

    // 3. Export via the real render loop (02 §7): compile + evaluate + read
    //    back every one of the 18,000 frames, in order.
    let preset = built_in_presets()
        .into_iter()
        .find(|p| p.name == "Social 16:9")
        .expect("built-in preset");
    let out = tmp_path("export.mp4");
    let resolved = ResolvedExport {
        width: W,
        height: H,
        frame_rate: RATE,
        audio: Some(AudioStreamSpec {
            sample_rate: SAMPLE_RATE,
            channels: 2,
        }),
        out_path: out.clone(),
        colorimetry: Colorimetry::BT709_LIMITED,
        prefer_hardware: false,
        encoder_speed: None,
        raw_encoder_args: vec![],
        burn_in_timecode: false,
        two_pass: false,
    };
    let cancel = AtomicBool::new(false);
    let mut done = false;
    export_frames(
        &tools,
        &preset,
        &resolved,
        TOTAL_FRAMES,
        |i| {
            let t = Tick(i as i64 * tpf);
            let compiled = compile(&project, seq_id, 0, t, Quality::FULL, None);
            let tex = evaluator
                .evaluate(&compiled.graph, (W, H), &mut source)
                .unwrap_or_else(|| {
                    panic!("evaluator produced no output at frame {i} (tick {t:?})")
                });
            let px = read_texture_rgba16f(&gpu, &tex, W, H);
            let mut rgba = Vec::with_capacity((W * H * 4) as usize);
            for p in px {
                rgba.extend_from_slice(&p);
            }
            if i % 1800 == 0 {
                eprintln!("ss3_sync_drift: exporting frame {i}/{TOTAL_FRAMES}");
            }
            Frame {
                width: W,
                height: H,
                rgba_premult: rgba,
            }
        },
        Some(audio_samples),
        &cancel,
        |e| {
            if matches!(e, ExportEvent::Done) {
                done = true;
            }
        },
    )
    .expect("10-minute synthetic export succeeds");
    assert!(done, "export did not emit ExportEvent::Done");

    // Free the (large) pre-export buffers before the decode-back phase.
    drop(loop_frames);

    // 4. Decode the export back and detect flash/beep instants (11 §5).
    let video_luma = decode_export_luma(&tools, &out);
    assert_eq!(
        video_luma.len() as u64,
        TOTAL_FRAMES,
        "exported video frame count matches the sequence's declared {TOTAL_FRAMES} frames"
    );
    let audio_mono = decode_export_audio_mono(&tools, &out, SAMPLE_RATE);

    // 5. Compute the offset at each of the ~600 flash/beep pairs; assert the
    //    worst case across the whole 10-minute span is under one frame.
    let mut max_offset_ticks: i64 = 0;
    let mut worst_pair: (u64, u64) = (0, 0);
    let mut pairs = 0usize;
    for loop_i in 0..LOOPS {
        for sec in 0..FIXTURE_SECS {
            let expected_sec = loop_i * FIXTURE_SECS + sec;
            let center_frame = expected_sec as i64 * (RATE.num as i64 / RATE.den as i64);
            let flash_frame = find_flash_frame(&video_luma, center_frame, LUMA_WINDOW_FRAMES);

            let center_sample = expected_sec as i64 * SAMPLE_RATE as i64;
            let beep_sample = find_beep_onset(
                &audio_mono,
                center_sample,
                AUDIO_WINDOW_SAMPLES,
                BEEP_THRESHOLD,
            );

            let video_tick = flash_frame * tpf;
            let beep_tick = beep_sample * (TICKS_PER_SECOND / SAMPLE_RATE as i64);
            let offset = (video_tick - beep_tick).abs();
            if offset > max_offset_ticks {
                max_offset_ticks = offset;
                worst_pair = (loop_i, sec);
            }
            pairs += 1;
        }
    }
    assert_eq!(
        pairs,
        (LOOPS * FIXTURE_SECS) as usize,
        "~600 flash/beep pairs measured across the 10-minute span"
    );

    let max_offset_ms = max_offset_ticks as f64 * 1000.0 / TICKS_PER_SECOND as f64;
    let one_frame_ms = 1000.0 * RATE.den as f64 / RATE.num as f64;
    assert!(
        max_offset_ms < one_frame_ms,
        "SS-3: max A/V offset {max_offset_ms:.3}ms (loop {}, second {}) exceeds one frame \
         ({one_frame_ms:.3}ms) across the 10-minute export",
        worst_pair.0,
        worst_pair.1,
    );

    let _ = std::fs::remove_file(&out);
}
