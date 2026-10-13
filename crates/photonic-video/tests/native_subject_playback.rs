//! Bounded responsiveness qualification, not real-time segmentation quality.
use photonic_core::timeline::color::*;
use photonic_core::timeline::*;
use photonic_core::{Color, CommandHistory, Document};
use photonic_video::{EngineCmd, GpuContext, PreviewQuality, ProxyMode, VideoEngine};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires explicitly provisioned pinned local subject model"]
fn subject_inference_keeps_playback_and_scrub_responsive() {
    photonic_matte::CachedMatteModel::load_cached().expect("pinned model");
    let tools = photonic_video::media::ffmpeg_locate::locate_for_test().unwrap();
    let retained = std::env::var_os("PHOTONIC_SUBJECT_PLAYBACK_DIR").map(std::path::PathBuf::from);
    let dir = retained.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("photonic-subject-play-{}", uuid::Uuid::new_v4()))
    });
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(dir).unwrap();
    let path = dir.join("moving-source.mkv");
    let result = std::process::Command::new(&tools.ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=96x96:rate=30:duration=5",
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
    let id = asset.id;
    let mut project = TimelineProject::new();
    project.media.insert(asset);
    let rate = FrameRate::FPS_30;
    let ticks = rate.ticks_per_frame().0;
    let mut seq = Sequence::new("Subject playback", rate, 96, 96);
    let sequence = seq.id;
    seq.color = SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
    let mut track = Track::new(TrackKind::Video, "Subject source");
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
    let mut graph = NodeGraph::new_project_graph("Subject cutout");
    let source = GraphNode::new(GraphOp::MediaIn {
        asset: id,
        time_source: TimeSource::Sequence,
    });
    let source_id = source.id;
    let matte = GraphNode::new(GraphOp::MaskFromMatte);
    let matte_id = matte.id;
    graph.nodes.insert(source_id, source);
    graph.nodes.insert(matte_id, matte);
    graph.edges = vec![
        GraphEdge {
            from: (source_id, OutPort(0)),
            to: (matte_id, InPort(0)),
        },
        GraphEdge {
            from: (matte_id, OutPort(0)),
            to: (graph.output, InPort(0)),
        },
    ];
    project.project_graph = Some(graph.id);
    project.graphs.insert(graph.id, graph);
    let mut document = Document::new("Subject playback", 96., 96.);
    document.timeline = Some(project);
    let before = serde_json::to_value(&document).unwrap();
    std::fs::write(
        dir.join("subject-playback.photon"),
        document.to_json().unwrap(),
    )
    .unwrap();
    let doc = Arc::new(Mutex::new(document));
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let engine = VideoEngine::new(GpuContext::request_blocking().expect("GPU required"));
    let session = engine.open_session(doc.clone(), history.clone());
    session.send(EngineCmd::SetPreviewQuality(PreviewQuality::Full));
    session.send(EngineCmd::SetProxyMode(ProxyMode::ForceOriginal));
    session.send(EngineCmd::Seek(Tick::ZERO));
    let await_exact = |target| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(frame) = session.latest_frame() {
                assert!(frame.color_errors.is_empty(), "{:?}", frame.color_errors);
                if frame.time == target
                    && frame.pending_computations == 0
                    && frame.subject_matte_used
                {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "exact subject frame never completed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    await_exact(Tick::ZERO);
    let baseline = session.status();
    session.send(EngineCmd::Play);
    let start = Instant::now();
    let mut times = BTreeSet::new();
    let mut pending_frames = 0;
    let mut progress = start;
    let mut gap = Duration::ZERO;
    let mut peak_cache = 0;
    while start.elapsed() < Duration::from_secs(3) {
        if let Some(frame) = session.latest_frame() {
            assert!(frame.color_errors.is_empty(), "{:?}", frame.color_errors);
            if times.insert(frame.time.0) {
                let now = Instant::now();
                gap = gap.max(now.duration_since(progress));
                progress = now;
                if frame.pending_computations > 0 {
                    pending_frames += 1;
                } else {
                    assert!(frame.subject_matte_used);
                }
            }
        }
        let status = session.status();
        peak_cache = peak_cache.max(status.memory.gpu_cache_bytes);
        assert!(status.memory.gpu_cache_bytes <= status.memory.gpu_cache_budget_bytes);
        assert!(
            progress.elapsed() < Duration::from_millis(750),
            "subject inference stalled transport"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let played = session.status();
    let pause = Instant::now();
    session.send(EngineCmd::Pause);
    while session.status().playing {
        assert!(
            pause.elapsed() < Duration::from_millis(500),
            "Pause blocked behind inference"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    for frame in [20, 40, 10] {
        session.send(EngineCmd::Seek(Tick(frame * ticks)));
    }
    await_exact(Tick(10 * ticks));
    let finished = session.status();
    session.shutdown();
    let evaluations = played.evaluations.saturating_sub(baseline.evaluations);
    let report = serde_json::json!({"distinct_ticks":times.len(),"provisional_ticks":pending_frames,"three_second_evaluations":evaluations,"maximum_gap_ms":gap.as_secs_f64()*1000.0,"peak_gpu_cache_bytes":peak_cache,"pause_and_latest_scrub_exact":true,"dropped_delta":played.dropped.saturating_sub(baseline.dropped),"final_buffering":finished.buffering});
    std::fs::write(
        dir.join("checks.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("{report}");
    assert!(times.len() >= 60, "too few transport frames");
    assert!(pending_frames > 0, "fixture did not exercise pending masks");
    assert!(
        evaluations <= 180,
        "pending masks forced redundant evaluations: {evaluations}"
    );
    assert_eq!(serde_json::to_value(&*doc.lock().unwrap()).unwrap(), before);
    assert_eq!(history.lock().unwrap().revision(), 0);
    if retained.is_none() {
        std::fs::remove_dir_all(dir).unwrap();
    }
}
