//! Actual video EOF is independent of container/audio duration. Native exact
//! requests fail visibly and recover on a valid seek without document edits.
use photonic_core::timeline::color::*;
use photonic_core::timeline::*;
use photonic_core::{Color, CommandHistory, Document};
use photonic_video::{EngineCmd, GpuContext, PreviewQuality, ProxyMode, VideoEngine};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[test]
fn native_source_end_reports_error_and_valid_seek_recovers() {
    let tools = photonic_video::media::ffmpeg_locate::locate_for_test().unwrap();
    let retained = std::env::var_os("PHOTONIC_SOURCE_END_DIR").map(std::path::PathBuf::from);
    let dir = retained.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("photonic-source-end-{}", uuid::Uuid::new_v4()))
    });
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(dir).unwrap();
    let path = dir.join("short-video.mkv");
    let result = std::process::Command::new(&tools.ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=96x96:rate=30",
            "-frames:v",
            "2",
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv444p",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let muxed = dir.join("long-audio.mkv");
    let result = std::process::Command::new(&tools.ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(&path)
        .args([
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=mono",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "copy",
            "-c:a",
            "pcm_s16le",
            "-t",
            "0.5",
        ])
        .arg(&muxed)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let path = muxed;
    // Start cold beyond EOF, then advance farther. The first definitive EOF
    // must remain known even though seek discarded every pre-target picture.
    {
        use photonic_video::decode::worker::{worker_stats, DecodeWorker};
        use photonic_video::decode::{
            scheduler::{PtsKind, SourceParams},
            DecodeSource, PixFmt, SharedRing,
        };
        use photonic_video::media::keyframe_index::KeyframeIndex;
        let rate = FrameRate::FPS_30;
        let tpf = rate.ticks_per_frame().0;
        let ring = SharedRing::preview();
        let decode = Arc::new(Mutex::new(DecodeSource::new(
            tools.clone(),
            SourceParams {
                input: path.clone(),
                width: 96,
                height: 96,
                pix_fmt: PixFmt::Yuv420p,
                pts_kind: PtsKind::Cfr(rate),
                keyframes: KeyframeIndex::build(&tools, &path).unwrap(),
            },
            ring.clone(),
        )));
        let worker = DecodeWorker::spawn(decode, rate);
        for ordinal in 60..65 {
            let target = Tick(ordinal * tpf);
            let before = worker_stats().0;
            worker.steer(target);
            let started = Instant::now();
            while worker.failure_at(target, false).is_none() {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "cold EOF not reported"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            if ordinal > 60 {
                assert_eq!(worker_stats().0, before, "known EOF spawned decoder again");
            }
        }
        worker.steer(Tick::ZERO);
        let started = Instant::now();
        while ring.get(Tick::ZERO).is_none() {
            assert!(worker.failure_at(Tick::ZERO, false).is_none());
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        worker.steer(Tick(2 * tpf - 1));
        let started = Instant::now();
        while ring.get(Tick(tpf)).is_none() {
            assert!(worker.failure_at(Tick(2 * tpf - 1), false).is_none());
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            worker.failure_at(Tick(2 * tpf - 1), false).is_none(),
            "last fractional frame rejected"
        );
        drop(worker);
    }
    let mut asset = MediaAsset::from_file(AssetKind::Video, &path);
    asset.probe = Some(
        photonic_video::media::probe::probe_details(&tools, &path)
            .unwrap()
            .probe,
    );
    asset.native_input_color = Some(NativeInputColorInterpretation {
        version: 1,
        standard: NativeInputStandard::Bt709Scene,
        range: InputSignalRange::Limited,
        matrix: InputMatrix::Bt709,
        chroma_location: None,
        hlg_peak_nits: None,
        reference_white_nits: None,
    });
    assert!(
        asset.probe.as_ref().unwrap().duration.0 > FrameRate::FPS_30.ticks_per_frame().0 * 2,
        "fixture must have audio longer than its two video frames"
    );
    let id = asset.id;
    let mut project = TimelineProject::new();
    project.media.insert(asset);
    let rate = FrameRate::FPS_30;
    let ticks = rate.ticks_per_frame().0;
    let mut seq = Sequence::new("Source end recovery", rate, 96, 96);
    let sequence = seq.id;
    seq.color = SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
    let mut track = Track::new(TrackKind::Video, "Source");
    track.clips.push(Clip::new(
        ClipSource::SolidColor {
            color: Color::BLACK,
        },
        Tick::ZERO,
        Tick(ticks * 150),
    ));
    seq.video_tracks.push(track);
    project.insert_sequence(seq);
    project.active_sequence = Some(sequence);
    let mut graph = NodeGraph::new_project_graph("Independent video");
    let source = GraphNode::new(GraphOp::MediaIn {
        asset: id,
        time_source: TimeSource::Sequence,
    });
    let source_id = source.id;
    graph.nodes.insert(source_id, source);
    graph.edges = vec![GraphEdge {
        from: (source_id, OutPort(0)),
        to: (graph.output, InPort(0)),
    }];
    project.project_graph = Some(graph.id);
    project.graphs.insert(graph.id, graph);
    let mut document = Document::new("Source end recovery", 96., 96.);
    document.timeline = Some(project);
    let before = serde_json::to_value(&document).unwrap();
    std::fs::write(dir.join("source-end.photon"), document.to_json().unwrap()).unwrap();
    let doc = Arc::new(Mutex::new(document));
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let engine = VideoEngine::new(GpuContext::request_blocking().expect("GPU required"));
    let session = engine.open_session(doc.clone(), history.clone());
    session.send(EngineCmd::SetPreviewQuality(PreviewQuality::Full));
    session.send(EngineCmd::SetProxyMode(ProxyMode::ForceOriginal));
    let mut checks = Vec::new();
    for (ordinal, should_fail) in [
        (0, false),
        (1, false),
        (2, true),
        (0, false),
        (60, true),
        (1, false),
    ] {
        let target = Tick(ordinal * ticks);
        let start = Instant::now();
        session.send(EngineCmd::Seek(target));
        loop {
            if let Some(frame) = session.latest_frame() {
                if frame.time == target {
                    assert_eq!(
                        !frame.color_errors.is_empty(),
                        should_fail,
                        "at {ordinal}: {:?}",
                        frame.color_errors
                    );
                    assert_eq!(frame.pending_computations, 0);
                    assert!(!frame.subject_matte_used);
                    checks.push(serde_json::json!({"frame":ordinal,"error":frame.color_errors,"elapsed_ms":start.elapsed().as_millis()}));
                    break;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "source request stalled at {ordinal}: {:?}",
                session.status().computation_errors
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        if should_fail {
            let seeks = photonic_video::decode::worker::worker_stats().0;
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(
                photonic_video::decode::worker::worker_stats().0,
                seeks,
                "failed request kept respawning decoder"
            );
        }
    }
    session.shutdown();
    assert_eq!(serde_json::to_value(&*doc.lock().unwrap()).unwrap(), before);
    assert_eq!(history.lock().unwrap().revision(), 0);
    std::fs::write(
        dir.join("checks.json"),
        serde_json::to_vec_pretty(&checks).unwrap(),
    )
    .unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&checks).unwrap());
    if retained.is_none() {
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn vfr_end_keeps_last_nominal_interval_and_recovers() {
    use photonic_video::decode::worker::DecodeWorker;
    use photonic_video::decode::{
        scheduler::{PtsKind, SourceParams},
        DecodeSource, PixFmt, SharedRing,
    };
    use photonic_video::media::keyframe_index::{KeyframeIndex, PtsIndex};
    let tools = photonic_video::media::ffmpeg_locate::locate_for_test().expect("FFmpeg required");
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vfr_sample.mp4");
    let info = photonic_video::media::probe::probe_details(&tools, &path)
        .unwrap()
        .probe
        .video
        .unwrap();
    let table = Arc::new(PtsIndex::build(&tools, &path).unwrap());
    let last = *table.pts.last().unwrap();
    let tpf = info.frame_rate.ticks_per_frame().0;
    let ring = SharedRing::preview();
    let decode = Arc::new(Mutex::new(DecodeSource::new(
        tools.clone(),
        SourceParams {
            input: path.clone(),
            width: info.width,
            height: info.height,
            pix_fmt: PixFmt::Yuv420p,
            pts_kind: PtsKind::Vfr(table),
            keyframes: KeyframeIndex::build(&tools, &path).unwrap(),
        },
        ring.clone(),
    )));
    let worker = DecodeWorker::spawn(decode, info.frame_rate);
    let inside = Tick(last.0 + tpf - 1);
    worker.steer(inside);
    let start = Instant::now();
    while ring.get(last).is_none() {
        assert!(
            worker.failure_at(inside, false).is_none(),
            "VFR error {:?}, requested {:?}, expected last {:?}, newest {:?}, rate {:?}",
            worker.failure_at(inside, false),
            inside,
            last,
            ring.newest(),
            info.frame_rate
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "VFR last frame unavailable"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(30));
    assert!(
        worker.failure_at(inside, false).is_none(),
        "VFR error {:?}, requested {:?}, expected last {:?}, newest {:?}, rate {:?}",
        worker.failure_at(inside, false),
        inside,
        last,
        ring.newest(),
        info.frame_rate
    );
    let end = Tick(last.0 + tpf);
    worker.steer(end);
    let start = Instant::now();
    while worker.failure_at(end, false).is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "VFR EOF not reported"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.steer(Tick::ZERO);
    let start = Instant::now();
    while ring.get(Tick::ZERO).is_none() {
        assert!(worker.failure_at(Tick::ZERO, false).is_none());
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "VFR seek did not recover"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
