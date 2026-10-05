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
fn native_managed_prores_export_decodes_as_ten_bit_bt709() {
    use photonic_core::timeline::color::{
        InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
        NativeManagedColorConfig, SequenceColorConfig,
    };
    use photonic_core::timeline::{
        AssetKind, Grade, GradeOp, GradeOpKind, GradeOpParams, MediaAsset,
    };
    use photonic_video::export::{job::run_export_job, presets::built_in_presets};
    use std::sync::atomic::AtomicBool;

    let gpu = gpu_or_skip!();
    let tools = tools_or_skip!();
    let directory =
        std::env::temp_dir().join(format!("photonic-native-export-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("source.mkv");
    let generated = std::process::Command::new(&tools.ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=32x32:rate=30:duration=1",
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv444p",
            "-y",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(generated.success());
    let mut project = TimelineProject::new();
    let mut asset = MediaAsset::from_file(AssetKind::Video, &source);
    asset.probe = Some(photonic_video::media::probe::probe_asset(&tools, &source).unwrap());
    asset.native_input_color = Some(NativeInputColorInterpretation {
        hlg_peak_nits: None,
        reference_white_nits: None,
        version: 1,
        standard: NativeInputStandard::Bt709Scene,
        range: InputSignalRange::Limited,
        matrix: InputMatrix::Bt709,
        chroma_location: None,
    });
    let asset_id = project.media.insert(asset);
    let mut seq = Sequence::new("native ProRes", FrameRate::FPS_30, 32, 32);
    seq.color = SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
    let seq_id = seq.id;
    let mut track = Track::new(TrackKind::Video, "V1");
    let mut clip = Clip::new(
        ClipSource::Asset { asset: asset_id },
        Tick::ZERO,
        FrameRate::FPS_30.ticks_per_frame(),
    );
    let mut grade = Grade::new();
    grade.ops.push(GradeOp::new(
        GradeOpKind::Exposure,
        GradeOpParams::Exposure { stops: 1.0 },
    ));
    clip.grade = Some(grade);
    track.clips.push(clip);
    seq.video_tracks.push(track);
    project.insert_sequence(seq);
    let mut preset = built_in_presets()
        .into_iter()
        .find(|p| p.name == "ProRes Mezzanine")
        .unwrap();
    preset.alpha = false;
    preset.audio = None;
    let output = directory.join("graded.mov");
    let job = ExportJob {
        sequence: seq_id,
        format_index: 0,
        preset,
        output: output.clone(),
        range: None,
        options: Default::default(),
    };
    run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .expect("qualified native sequence should export");
    let probe = std::process::Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,pix_fmt,color_space,color_transfer,color_primaries",
            "-of",
            "json",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(probe.status.success());
    let metadata: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    let video = &metadata["streams"][0];
    assert_eq!(video["codec_name"], "prores");
    assert_eq!(video["pix_fmt"], "yuv422p10le");
    assert_eq!(video["color_space"], "bt709");
    assert_eq!(video["color_transfer"], "bt709");
    assert_eq!(video["color_primaries"], "bt709");
    let decoded = std::process::Command::new(&tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&output)
        .args([
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuv422p10le",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    assert_eq!(decoded.stdout.len(), 32 * 32 * 4);
    let luma = u16::from_le_bytes([decoded.stdout[0], decoded.stdout[1]]);
    let cb_offset = 32 * 32 * 2;
    let cr_offset = cb_offset + 32 * 32;
    let cb = u16::from_le_bytes([decoded.stdout[cb_offset], decoded.stdout[cb_offset + 1]]);
    let cr = u16::from_le_bytes([decoded.stdout[cr_offset], decoded.stdout[cr_offset + 1]]);
    assert!(
        (64..=940).contains(&luma) && cb < 512 && cr > 512,
        "decoded codes: {luma} {cb} {cr}"
    );
    let mut alpha_job = job.clone();
    alpha_job.preset.alpha = true;
    alpha_job.output = directory.join("graded-alpha.mov");
    run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &alpha_job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .expect("qualified native alpha sequence should export");
    let alpha_probe = std::process::Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=pix_fmt",
            "-of",
            "json",
        ])
        .arg(&alpha_job.output)
        .output()
        .unwrap();
    assert!(alpha_probe.status.success());
    let alpha_metadata: serde_json::Value = serde_json::from_slice(&alpha_probe.stdout).unwrap();
    assert_eq!(alpha_metadata["streams"][0]["pix_fmt"], "yuva444p12le");
    let alpha_decoded = std::process::Command::new(&tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&alpha_job.output)
        .args([
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuva444p12le",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .unwrap();
    assert!(alpha_decoded.status.success());
    assert_eq!(alpha_decoded.stdout.len(), 32 * 32 * 8);
    let first_alpha = u16::from_le_bytes([
        alpha_decoded.stdout[32 * 32 * 6],
        alpha_decoded.stdout[32 * 32 * 6 + 1],
    ]);
    assert_eq!(first_alpha, 4095);
    let published = std::fs::read(&output).unwrap();
    let original_probe = project.media.assets[&asset_id].probe.clone();
    let original_backup = directory.join("source-original.mkv");
    let changed_source = directory.join("source-changed.mkv");
    let generated = std::process::Command::new(&tools.ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=32x32:rate=30:duration=1",
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv422p",
            "-y",
        ])
        .arg(&changed_source)
        .status()
        .unwrap();
    assert!(generated.success());
    std::fs::rename(&source, &original_backup).unwrap();
    std::fs::rename(&changed_source, &source).unwrap();
    let changed_file = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(
        changed_file.contains("project records yuv444p"),
        "{changed_file}"
    );
    assert!(
        changed_file.contains("current file reports yuv422p"),
        "{changed_file}"
    );
    assert_eq!(std::fs::read(&output).unwrap(), published);
    std::fs::remove_file(&source).unwrap();
    std::fs::write(&source, b"not a video stream").unwrap();
    let unreadable_file = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(
        unreadable_file.contains("could not be probed or opened"),
        "{unreadable_file}"
    );
    assert_eq!(std::fs::read(&output).unwrap(), published);
    std::fs::remove_file(&source).unwrap();
    std::fs::rename(&original_backup, &source).unwrap();
    let mut unsupported_probe =
        photonic_core::timeline::MediaProbe::basic(Tick(TICKS_PER_SECOND), "matroska", "ffv1");
    unsupported_probe.pixel_format = Some("gbrp".into());
    project.media.assets.get_mut(&asset_id).unwrap().probe = Some(unsupported_probe);
    let unsupported = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(unsupported.contains("gbrp"), "{unsupported}");
    assert_eq!(std::fs::read(&output).unwrap(), published);
    project.media.assets.get_mut(&asset_id).unwrap().probe = None;
    let preflight = photonic_video::export::job::resolve_export_job(&project, &job)
        .unwrap_err()
        .to_string();
    assert!(
        preflight.contains("probed source pixel format"),
        "{preflight}"
    );
    let unprobed = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(
        unprobed.contains("probed source pixel format"),
        "{unprobed}"
    );
    assert_eq!(std::fs::read(&output).unwrap(), published);
    project.media.assets.get_mut(&asset_id).unwrap().probe = original_probe;
    let original_source = project.media.assets.get(&asset_id).unwrap().source.clone();
    project.media.assets.get_mut(&asset_id).unwrap().source =
        photonic_core::timeline::AssetSource::File {
            path: directory.join("offline.mkv"),
            rel_path: None,
        };
    let offline = photonic_video::export::job::resolve_export_job(&project, &job)
        .unwrap_err()
        .to_string();
    assert!(offline.contains("offline"), "{offline}");
    assert_eq!(std::fs::read(&output).unwrap(), published);
    project.media.assets.get_mut(&asset_id).unwrap().source = original_source;
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
        .grade
        .as_mut()
        .unwrap()
        .ops[0]
        .params
        .base = GradeOpParams::Exposure { stops: 100.0 };
    let failure = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    );
    assert!(
        failure.is_err(),
        "unsupported native primary must block delivery"
    );
    assert_eq!(std::fs::read(&output).unwrap(), published);
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
        .grade
        .as_mut()
        .unwrap()
        .ops[0]
        .params
        .base = GradeOpParams::Exposure { stops: 1.0 };
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[0]
        .clips
        .push(Clip::new(
            ClipSource::SolidColor {
                color: Color::rgb(0.0, 0.0, 1.0),
            },
            FrameRate::FPS_30.ticks_per_frame(),
            FrameRate::FPS_30.ticks_per_frame(),
        ));
    let failure = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    );
    assert!(
        failure.is_err(),
        "an unsupported later frame must abort delivery"
    );
    assert_eq!(std::fs::read(&output).unwrap(), published);
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[0]
        .clips
        .pop();
    let mut later_asset =
        MediaAsset::from_file(AssetKind::Video, directory.join("later-offline.mkv"));
    later_asset.native_input_color = project.media.assets[&asset_id].native_input_color.clone();
    let later_id = project.media.insert(later_asset);
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[0]
        .clips
        .push(Clip::new(
            ClipSource::Asset { asset: later_id },
            FrameRate::FPS_30.ticks_per_frame(),
            FrameRate::FPS_30.ticks_per_frame(),
        ));
    let failure = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(failure.contains("offline"), "{failure}");
    assert_eq!(std::fs::read(&output).unwrap(), published);
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[0]
        .clips
        .pop();
    project
        .media
        .assets
        .get_mut(&asset_id)
        .unwrap()
        .native_input_color = None;
    let failure = run_export_job(
        gpu,
        Arc::new(project),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    );
    assert!(
        failure.is_err(),
        "missing native source interpretation must block delivery"
    );
    assert_eq!(std::fs::read(&output).unwrap(), published);
    std::fs::remove_dir_all(directory).unwrap();
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

#[test]
fn export_rejects_missing_grade_lut_until_corrector_is_disabled() {
    use photonic_core::timeline::{
        AssetId, AssetKind, Grade, GradeOp, GradeOpKind, GradeOpParams, LutInterp, MediaAsset,
    };
    use photonic_video::export::{job::run_export_job, render_loop::ExportEvent};
    use std::sync::atomic::AtomicBool;

    let gpu = gpu_or_skip!();
    let tools = tools_or_skip!();
    let mut project = TimelineProject::new();
    let mut sequence = Sequence::new("grade failure", FrameRate::FPS_30, 32, 32);
    let seq_id = sequence.id;
    let mut track = Track::new(TrackKind::Video, "V1");
    let mut clip = Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 0.5,
                g: 0.5,
                b: 0.5,
                a: 1.0,
            },
        },
        Tick(0),
        FrameRate::FPS_30.ticks_per_frame(),
    );
    let mut grade = Grade::new();
    let lut_asset = AssetId::new();
    grade.ops.push(GradeOp::new(
        GradeOpKind::Lut3d,
        GradeOpParams::Lut3d {
            asset: lut_asset,
            intensity: 1.0,
            interp: LutInterp::Trilinear,
        },
    ));
    clip.grade = Some(grade);
    track.clips.push(clip);
    sequence.video_tracks.push(track);
    project.insert_sequence(sequence);
    let directory = std::env::temp_dir().join(format!("photonic-grade-export-{}", AssetId::new()));
    std::fs::create_dir_all(&directory).unwrap();
    let output = directory.join("grade.mp4");
    let job = ExportJob {
        sequence: seq_id,
        format_index: 0,
        preset: h264_preset(),
        output: output.clone(),
        range: None,
        options: Default::default(),
    };
    let mut done = false;
    let failure = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |event| done |= matches!(event, ExportEvent::Done),
    );
    let error = failure
        .expect_err("missing LUT must not produce a successful final render")
        .to_string();
    assert!(
        error.contains("LUT") && error.contains("unavailable"),
        "{error}"
    );
    assert!(!done);
    assert!(
        !output.exists(),
        "failed export must not publish an altered look"
    );

    let lut_path = directory.join("incomplete-shaper.cube");
    std::fs::write(&lut_path, "LUT_1D_SIZE 3\n0 0 0\n1 1 1\nLUT_3D_SIZE 2\n").unwrap();
    let mut asset = MediaAsset::from_file(AssetKind::Lut3d, &lut_path);
    asset.id = lut_asset;
    project.media.insert(asset);
    let failure = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    );
    let error = failure
        .expect_err("incomplete LUT shaper must block final export")
        .to_string();
    assert!(
        error.contains("LUT") && error.contains("unavailable"),
        "{error}"
    );
    assert!(!output.exists());

    project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
        .grade
        .as_mut()
        .unwrap()
        .ops[0]
        .enabled = false;
    run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .expect("explicitly disabled unresolved corrector is safe to export");
    assert!(output.exists());
    std::fs::remove_file(&output).unwrap();

    std::fs::write(&lut_path, "LUT_1D_SIZE 2\n0 0 0\n1 1 1\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n").unwrap();
    project
        .media
        .assets
        .get_mut(&lut_asset)
        .unwrap()
        .lut_full_hash = Some(photonic_video::media::full_content_hash(&lut_path).unwrap());
    project.sequences.get_mut(&seq_id).unwrap().video_tracks[0].clips[0]
        .grade
        .as_mut()
        .unwrap()
        .ops[0]
        .enabled = true;
    run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .expect("complete 1D shaper and 3D LUT must render on final export");
    assert!(output.exists());
    std::fs::remove_file(&output).unwrap();

    project.media.assets.get_mut(&lut_asset).unwrap().lut_color =
        Some(photonic_core::timeline::color::LutColorInterpretation {
            purpose: photonic_core::timeline::color::LutPurpose::Technical,
            ..photonic_core::timeline::color::LutColorInterpretation::legacy_creative()
        });
    let error = run_export_job(
        gpu.clone(),
        Arc::new(project.clone()),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .expect_err("technical LUT cannot run in the creative Legacy SDR grade")
    .to_string();
    assert!(
        error.contains("LUT") && error.contains("unavailable"),
        "{error}"
    );
    assert!(!output.exists());
    project.media.assets.get_mut(&lut_asset).unwrap().lut_color =
        Some(photonic_core::timeline::color::LutColorInterpretation::legacy_creative());

    let mut changed = std::fs::read_to_string(&lut_path).unwrap();
    changed = changed.replacen("1 1 1\n", "0 1 1\n", 1);
    std::fs::write(&lut_path, changed).unwrap();
    let error = run_export_job(
        gpu,
        Arc::new(project),
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .expect_err("changed pinned LUT must block final export")
    .to_string();
    assert!(
        error.contains("LUT") && error.contains("unavailable"),
        "{error}"
    );
    assert!(!output.exists());
    std::fs::remove_dir_all(directory).unwrap();
}
