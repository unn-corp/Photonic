//! `EngineCmd::Export` end-to-end (02 §7, K-0.1): open a real `EngineSession`
//! over a solid-colour sequence, fire `EngineCmd::Export`, poll
//! `status().export` to `done`, and ffprobe the encoded output for
//! container/codec/dimensions/duration.
//!
//! This exercises the SAME relocated `export::job::run_export_job` path the MCP
//! `export_sequence` tool uses — but from the engine side, proving the GUI's
//! `EngineCmd::Export` (previously an unwired stub) now actually renders.
//!
//! Skip conventions (never fail the suite headless): skip-with-message when no
//! GPU adapter is present (pool.rs convention) and when ffmpeg/ffprobe are
//! absent (decode_media.rs convention).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use photonic_core::timeline::{
    Clip, ClipSource, FrameRate, Sequence, Tick, TimelineProject, Track, TrackKind,
    TICKS_PER_SECOND,
};
use photonic_core::{Color, CommandHistory, Document};

use photonic_video::export::presets::{
    Container, ExportPreset, FrameRatePolicy, QualityMode, ResolutionSpec, VideoCodec,
    VideoEncodeSpec,
};
use photonic_video::media::ffmpeg_locate::locate_for_test;
use photonic_video::{EngineCmd, ExportJob, GpuContext, VideoEngine};

macro_rules! gpu_or_skip {
    () => {
        match GpuContext::request_blocking() {
            Some(gpu) => gpu,
            None => {
                eprintln!(
                    "no GPU adapter — skipping export EngineCmd test at {}:{}",
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
                    "ffmpeg/ffprobe not found — skipping export EngineCmd test at {}:{}",
                    file!(),
                    line!()
                );
                return;
            }
        }
    };
}

/// A minimal video-only H.264/MP4 preset at source format + sequence rate.
fn h264_preset() -> ExportPreset {
    ExportPreset {
        name: "engine-cmd-export-test".into(),
        container: Container::Mp4,
        video: Some(VideoEncodeSpec {
            codec: VideoCodec::H264,
            quality: QualityMode::Crf(28.0),
        }),
        audio: None,
        resolution: ResolutionSpec::SourceFormat,
        frame_rate: FrameRatePolicy::MatchSequence,
        alpha: false,
        faststart: false,
        loudness_target: None,
        stems: false,
    }
}

#[test]
fn engine_cmd_export_renders_solid_colour_sequence() {
    let gpu = gpu_or_skip!();
    let tools = tools_or_skip!();

    // 1-second solid-red 128x128 sequence at 30 fps (30 output frames).
    let rate = FrameRate::FPS_30;
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("export-seq", rate, 128, 128);
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
        Tick(TICKS_PER_SECOND),
    ));
    seq.video_tracks.push(v1);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);

    let mut doc = Document::new("export-cmd-test", 128.0, 128.0);
    doc.timeline = Some(project);
    let doc = Arc::new(Mutex::new(doc));
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let engine = VideoEngine::new(gpu.clone());
    let session = engine.open_session(Arc::clone(&doc), Arc::clone(&history));

    // Settle: wait for the engine to snapshot the project and present a frame
    // (this also confirms the GPU path works before we ask it to export).
    session.send(EngineCmd::Seek(Tick(0)));
    let settle = Instant::now() + Duration::from_secs(20);
    while session.latest_frame().is_none() {
        assert!(
            Instant::now() < settle,
            "engine never produced a first frame"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let out = std::env::temp_dir().join(format!(
        "photonic-engine-cmd-export-{}.mp4",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&out);

    session.send(EngineCmd::Export(Box::new(ExportJob {
        sequence: seq_id,
        format_index: 0,
        preset: h264_preset(),
        output: out.clone(),
        range: None,
        options: Default::default(),
    })));

    // Poll the wait-free status snapshot to a terminal (done) export state.
    let deadline = Instant::now() + Duration::from_secs(120);
    let snapshot = loop {
        if let Some(export) = session.status().export.clone() {
            if export.done {
                break export;
            }
        }
        assert!(
            Instant::now() < deadline,
            "export did not reach done in time (last: {:?})",
            session.status().export
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        snapshot.error.is_none(),
        "export failed: {:?}",
        snapshot.error
    );
    assert_eq!(snapshot.total, 30, "1s @30fps = 30 frames");
    assert_eq!(snapshot.frame, 30, "all frames encoded");
    assert!(out.exists(), "output file missing: {}", out.display());

    session.shutdown();

    // ffprobe: MP4 container, H.264 video, 128x128, ~1 s duration.
    let probe = std::process::Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(&out)
        .output()
        .expect("run ffprobe");
    assert!(probe.status.success(), "ffprobe failed");
    let meta: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();

    let format_name = meta["format"]["format_name"].as_str().unwrap_or("");
    assert!(
        format_name.contains("mp4") || format_name.contains("mov"),
        "expected an mp4/mov container, got {format_name:?}"
    );
    let stream = meta["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == "video")
        .expect("video stream present");
    assert_eq!(stream["codec_name"], "h264", "expected H.264 video");
    assert_eq!(stream["width"], 128);
    assert_eq!(stream["height"], 128);
    let duration: f64 = meta["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (duration - 1.0).abs() < 0.2,
        "expected ~1s of video, got {duration}s"
    );

    let _ = std::fs::remove_file(&out);
}
