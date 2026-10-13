//! Bounded real-decoder temporal playback qualification, independent of MCP.
//! This tiny frame contract detects decoder contention and stale pictures; it
//! does not claim high-resolution throughput or external-hardware qualification.
use photonic_core::timeline::color::*;
use photonic_core::timeline::*;
use photonic_core::{Color, CommandHistory, Document};
use photonic_video::color::native::{decode_video_sample_to_ap1, NativeVideoInput};
use photonic_video::color::native_output::Aces2SdrOutput;
use photonic_video::graph::eval::read_texture_rgba16f;
use photonic_video::graph::ir::FrameColorEncoding;
use photonic_video::media::{ffmpeg_locate::locate_for_test, probe::probe_details};
use photonic_video::{EngineCmd, GpuContext, PreviewQuality, ProxyMode, VideoEngine};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn source_codes(frame: usize) -> [u16; 3] {
    [
        24 + ((frame * 7) % 200) as u16,
        100 + ((frame * 3) % 55) as u16,
        100 + ((frame * 5) % 55) as u16,
    ]
}

#[test]
fn four_independent_native_clocks_play_without_stalling_or_stale_pictures() {
    check_native_temporal_playback(false, false);
}

#[test]
fn fractional_native_clocks_use_covering_frames_on_cold_seek_and_playback() {
    check_native_temporal_playback(true, false);
}

#[test]
#[ignore = "explicit720p/1080p multi-clock throughput qualification; not ordinary CI"]
fn hd_native_multiclock_playback_observation() {
    check_native_temporal_playback(true, true);
}

fn check_native_temporal_playback(fractional: bool, hd: bool) {
    use std::io::{Read, Write};
    let (w, h) = if hd {
        match std::env::var("PHOTONIC_TEMPORAL_HD_SIZE").as_deref() {
            Ok("1080p") => (1920u32, 1080u32),
            Ok("720p") | Err(_) => (1280u32, 720u32),
            Ok(other) => panic!("unsupported bounded qualification size:{other}"),
        }
    } else {
        (32, 16)
    };
    let pixels = (w * h) as usize;
    let source_frames = if hd { 400 } else { 240 };
    let playback_readback =
        !(hd && std::env::var("PHOTONIC_TEMPORAL_HD_READBACK").as_deref() == Ok("none"));
    let observation_secs = if hd { 6 } else { 3 };
    let clock_count = if hd {
        std::env::var("PHOTONIC_TEMPORAL_HD_CLOCKS")
            .ok()
            .map(|s| s.parse::<usize>().expect("clock count"))
            .unwrap_or(4)
    } else {
        4
    };
    assert!([1, 2, 4].contains(&clock_count));
    let offsets = &[1i64, 31, 61, 91][..clock_count];
    let preview_quality =
        if hd && std::env::var("PHOTONIC_TEMPORAL_HD_QUALITY").as_deref() == Ok("draft") {
            PreviewQuality::Draft
        } else {
            PreviewQuality::Full
        };
    let tools = locate_for_test().expect("FFmpeg required for temporal playback qualification");
    let retained = std::env::var_os(if hd {
        "PHOTONIC_TEMPORAL_HD_DIR"
    } else if fractional {
        "PHOTONIC_TEMPORAL_FRACTIONAL_DIR"
    } else {
        "PHOTONIC_TEMPORAL_PLAYBACK_DIR"
    })
    .map(std::path::PathBuf::from);
    let dir = retained.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("photonic-temporal-play-{}", uuid::Uuid::new_v4()))
    });
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(dir).unwrap();
    let path = dir.join("source.mkv");
    let video_size = format!("{w}x{h}");
    let mut encoder = std::process::Command::new(&tools.ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pixel_format",
            "yuv444p",
            "-video_size",
            &video_size,
            "-framerate",
            "30",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            "-i",
        ])
        .arg("pipe:0")
        .args([
            "-c:v",
            "ffv1",
            "-g",
            "1",
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
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Avoid a multi-gigabyte raw temporary at1080p. Pipe one plane at a time;
    // verify every encoded/decoded source sample below before using the oracle.
    {
        let mut input = std::io::BufWriter::new(encoder.stdin.take().unwrap());
        let mut plane = vec![0u8; pixels];
        for frame in 0..source_frames {
            for value in source_codes(frame) {
                plane.fill(value as u8);
                input.write_all(&plane).unwrap();
            }
        }
        input.flush().unwrap();
    }
    let encoded = encoder.wait_with_output().unwrap();
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    // Explicit input tags prevent FFmpeg from transforming untagged raw codes
    // while attaching output color metadata. Prove the encoded oracle is exact.
    let mut decoded = std::process::Command::new(&tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv444p", "pipe:1"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut output = decoded.stdout.take().unwrap();
        let mut plane = vec![0u8; pixels];
        for frame in 0..source_frames {
            for value in source_codes(frame) {
                output.read_exact(&mut plane).unwrap();
                assert!(
                    plane.iter().all(|&v| v == value as u8),
                    "lossless fixture source codes changed"
                );
            }
        }
        let mut extra = [0u8; 1];
        assert_eq!(output.read(&mut extra).unwrap(), 0);
    }
    assert!(decoded.wait().unwrap().success());
    let mut asset = MediaAsset::from_file(AssetKind::Video, &path);
    asset.probe = Some(probe_details(&tools, &path).unwrap().probe);
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
    let mut seq = Sequence::new("Four independent clocks", rate, w, h);
    seq.color = SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
    let sequence = seq.id;
    let mut track = Track::new(TrackKind::Video, "Temporal source");
    track.clips.push(Clip::new(
        ClipSource::SolidColor {
            color: Color::BLACK,
        },
        Tick::ZERO,
        Tick(ticks * if hd { 240 } else { 120 }),
    ));
    seq.video_tracks.push(track);
    project.insert_sequence(seq);
    project.active_sequence = Some(sequence);
    let mut graph = NodeGraph::new_project_graph("Four offset mean");
    let source = GraphNode::new(GraphOp::MediaIn {
        asset: id,
        time_source: TimeSource::Sequence,
    });
    let source_id = source.id;
    graph.nodes.insert(source_id, source);
    let mut previous = None;
    for (index, offset) in offsets.iter().copied().enumerate() {
        let shifted = GraphNode::new(GraphOp::TimeOffset {
            offset: Tick(-offset * ticks - if fractional { ticks / 2 } else { 0 }),
        });
        let shifted_id = shifted.id;
        graph.nodes.insert(shifted_id, shifted);
        graph.edges.push(GraphEdge {
            from: (source_id, OutPort(0)),
            to: (shifted_id, InPort(0)),
        });
        previous = Some(if let Some(previous) = previous {
            let mut merge = GraphNode::new(GraphOp::Merge {
                mode: photonic_core::layer::BlendMode::Normal,
            });
            let merge_id = merge.id;
            merge
                .params
                .base
                .0
                .set("params.opacity", PropValue::Float(1. / (index + 1) as f64));
            graph.nodes.insert(merge_id, merge);
            graph.edges.extend([
                GraphEdge {
                    from: (shifted_id, OutPort(0)),
                    to: (merge_id, InPort(0)),
                },
                GraphEdge {
                    from: (previous, OutPort(0)),
                    to: (merge_id, InPort(1)),
                },
            ]);
            merge_id
        } else {
            shifted_id
        });
    }
    graph.edges.push(GraphEdge {
        from: (previous.unwrap(), OutPort(0)),
        to: (graph.output, InPort(0)),
    });
    project.project_graph = Some(graph.id);
    project.graphs.insert(graph.id, graph);
    let mut document = Document::new("Temporal playback", w as f64, h as f64);
    document.timeline = Some(project);
    let before = serde_json::to_value(&document).unwrap();
    std::fs::write(
        dir.join("temporal-playback.photon"),
        document.to_json().unwrap(),
    )
    .unwrap();
    let doc = Arc::new(Mutex::new(document));
    let history = Arc::new(Mutex::new(CommandHistory::new(64)));
    let gpu =
        GpuContext::request_blocking().expect("GPU required for temporal playback qualification");
    let engine = VideoEngine::new(gpu.clone());
    let session = engine.open_session(doc.clone(), history.clone());
    session.send(EngineCmd::SetPreviewQuality(preview_quality));
    session.send(EngineCmd::SetProxyMode(ProxyMode::ForceOriginal));
    session.send(EngineCmd::Seek(Tick::ZERO));
    let deadline = Instant::now() + Duration::from_secs(if fractional && !hd { 5 } else { 30 });
    loop {
        if let Some(frame) = session.latest_frame() {
            assert!(frame.color_errors.is_empty(), "{:?}", frame.color_errors);
            if frame.time == Tick::ZERO
                && frame.preview_quality == preview_quality
                && session.status().readiness.ready == clock_count
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "four source clocks never became ready: {:?}",
            session.status().readiness
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let baseline = session.status();
    let transform = Aces2SdrOutput::shared().unwrap();
    let expected_for = |ordinal: usize| {
        let mut mean = [0f64; 3];
        for &offset in offsets {
            let ap1 = decode_video_sample_to_ap1(
                source_codes(ordinal + offset as usize),
                8,
                true,
                NativeVideoInput::Bt709,
            )
            .unwrap();
            for c in 0..3 {
                mean[c] += ap1[c] / clock_count as f64;
            }
        }
        transform
            .map_premultiplied([mean[0], mean[1], mean[2], 1.])
            .unwrap()
    };
    let paused = session.latest_frame().unwrap();
    let paused_pixels = read_texture_rgba16f(
        &gpu,
        &paused.texture,
        paused.logical_size.0,
        paused.logical_size.1,
    );
    eprintln!(
        "paused frame {:?}: {:?}; expected0 {:?}; expected1 {:?}; expected2 {:?}",
        paused.time,
        paused_pixels[0],
        expected_for(0),
        expected_for(1),
        expected_for(2)
    );
    let paused_expected = expected_for(0);
    for pixel in &paused_pixels {
        for c in 0..4 {
            assert!(
                (pixel[c] as f64 - paused_expected[c]).abs() < 0.012,
                "paused baseline mismatch"
            );
        }
    }
    session.send(EngineCmd::Play);
    let start = Instant::now();
    let mut last_progress = start;
    let mut times = BTreeSet::new();
    let mut largest_error = 0f64;
    let mut largest_gap = Duration::ZERO;
    let mut peak_ring = 0;
    let mut peak_cache = 0;
    while start.elapsed() < Duration::from_secs(observation_secs) {
        if let Some(frame) = session.latest_frame() {
            if times.insert(frame.time.0) {
                let now = Instant::now();
                largest_gap = largest_gap.max(now.duration_since(last_progress));
                last_progress = now;
                assert_eq!(frame.output_encoding, FrameColorEncoding::SrgbDisplay);
                assert!(
                    frame.color_errors.is_empty() && frame.grading_errors.is_empty(),
                    "frame {:?}: color {:?},grading {:?},status {:?}",
                    frame.time,
                    frame.color_errors,
                    frame.grading_errors,
                    session.status().readiness
                );
                assert!(!frame.cached_preview);
                let ordinal = rate.frame_at(frame.time) as usize;
                let mut mean = [0f64; 3];
                for &offset in offsets {
                    let ap1 = decode_video_sample_to_ap1(
                        source_codes(ordinal + offset as usize),
                        8,
                        true,
                        NativeVideoInput::Bt709,
                    )
                    .unwrap();
                    for c in 0..3 {
                        mean[c] += ap1[c] / clock_count as f64;
                    }
                }
                let expected = transform
                    .map_premultiplied([mean[0], mean[1], mean[2], 1.])
                    .unwrap();
                let pixels = if playback_readback {
                    read_texture_rgba16f(
                        &gpu,
                        &frame.texture,
                        if hd { 1 } else { w },
                        if hd { 1 } else { h },
                    )
                } else {
                    Vec::new()
                };
                for pixel in pixels {
                    for c in 0..4 {
                        let error = (pixel[c] as f64 - expected[c]).abs();
                        largest_error = largest_error.max(error);
                        assert!(
                            error < 0.012,
                            "frame {ordinal} channel {c}: {pixel:?} vs {expected:?}; next {:?}",
                            expected_for(ordinal + 1)
                        );
                    }
                }
            }
        }
        let status = session.status();
        peak_ring = peak_ring.max(status.memory.decoded_ring_bytes);
        peak_cache = peak_cache.max(status.memory.gpu_cache_bytes);
        assert!(status.memory.decoded_ring_bytes <= status.memory.decoded_ring_budget_bytes);
        assert!(status.memory.gpu_cache_bytes <= status.memory.gpu_cache_budget_bytes);
        assert!(!status.readiness.capacity_limited && status.readiness.failed_sources == 0);
        assert!(
            last_progress.elapsed() < Duration::from_millis(750),
            "temporal playback stalled: requested {}, ready {}, misses {}",
            status.readiness.requested,
            status.readiness.ready,
            status.evaluation_misses
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    session.send(EngineCmd::Pause);
    let playback_status = session.status();
    let soak_seconds = if hd {
        std::env::var("PHOTONIC_TEMPORAL_HD_SOAK_SECONDS")
            .ok()
            .map(|s| s.parse::<u64>().expect("soak seconds"))
            .unwrap_or(0)
    } else {
        0
    };
    assert!(soak_seconds <= 120, "bounded qualification only");
    let soak_start = Instant::now();
    let mut seeks_checked = 0usize;
    let mut soak_largest_error = 0f64;
    while soak_start.elapsed() < Duration::from_secs(soak_seconds) {
        let ordinal = [0usize, 100, 25, 175][seeks_checked % 4];
        let target = Tick(ordinal as i64 * ticks);
        session.send(EngineCmd::Seek(target));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(frame) = session.latest_frame() {
                if frame.time == target && session.status().readiness.ready == clock_count {
                    assert!(
                        frame.color_errors.is_empty(),
                        "soak source errors:{:?}",
                        frame.color_errors
                    );
                    let pixel = read_texture_rgba16f(&gpu, &frame.texture, 1, 1)[0];
                    let expected = expected_for(ordinal);
                    for c in 0..4 {
                        let error = (pixel[c] as f64 - expected[c]).abs();
                        soak_largest_error = soak_largest_error.max(error);
                        assert!(
                            error < 0.012,
                            "seek{seeks_checked} frame{ordinal}: {pixel:?} vs{expected:?}"
                        );
                    }
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "soak seek{seeks_checked} never became exact"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = session.status();
        peak_ring = peak_ring.max(status.memory.decoded_ring_bytes);
        peak_cache = peak_cache.max(status.memory.gpu_cache_bytes);
        assert!(
            status.memory.decoded_ring_bytes <= status.memory.decoded_ring_budget_bytes
                && status.memory.gpu_cache_bytes <= status.memory.gpu_cache_budget_bytes
        );
        assert!(!status.readiness.capacity_limited && status.readiness.failed_sources == 0);
        seeks_checked += 1;
    }
    if soak_seconds > 0 {
        assert!(seeks_checked >= 8, "insufficient cold/hot seek cycles");
    }
    let final_status = session.status();
    session.shutdown();
    let checks = serde_json::json!({"width":w,"height":h,"preview_quality":format!("{preview_quality:?}"),"output_dimensions":[paused.logical_size.0,paused.logical_size.1],"fps":30,"offset_frames":offsets.iter().map(|v|-v).collect::<Vec<_>>(),"additional_half_frame":fractional,"observation_seconds":observation_secs,"seek_soak_seconds":soak_seconds,"seeks_checked":seeks_checked,"seek_evaluations":final_status.evaluations.saturating_sub(playback_status.evaluations),"seek_soak_largest_pixel_error":soak_largest_error,"distinct_pictures_checked":times.len(),"last_timeline_frame":times.last().copied().unwrap_or(0)/ticks,"largest_pixel_error":if playback_readback{Some(largest_error)}else{None},"largest_observed_gap_ms":largest_gap.as_secs_f64()*1000.,"frames_published":playback_status.frames_published.saturating_sub(baseline.frames_published),"evaluations":playback_status.evaluations.saturating_sub(baseline.evaluations),"last_evaluate_micros":playback_status.last_evaluate_micros,"new_dropped":playback_status.dropped.saturating_sub(baseline.dropped),"new_evaluation_misses":playback_status.evaluation_misses.saturating_sub(baseline.evaluation_misses),"peak_decoded_ring_bytes":peak_ring,"peak_gpu_cache_bytes":peak_cache,"document_and_history_unchanged":true,"high_resolution_throughput_qualified":false,"validation_reads_full_frame":!hd,"playback_probe_pixels":if !playback_readback{0}else if hd{1}else{w*h},"paused_full_readback":true,"minimum_distinct_30_met":times.len()>=30});
    std::fs::write(
        dir.join("checks.json"),
        serde_json::to_vec_pretty(&checks).unwrap(),
    )
    .unwrap();
    eprintln!("{checks}");
    assert!(
        times.len() >= 30,
        "too few distinct pictures: {}",
        times.len()
    );
    let last = *times.last().unwrap();
    assert!(
        last >= ticks * 60,
        "playback did not advance at least two timeline seconds"
    );
    assert_eq!(history.lock().unwrap().revision(), 0);
    assert_eq!(serde_json::to_value(&*doc.lock().unwrap()).unwrap(), before);
    if retained.is_none() {
        std::fs::remove_dir_all(dir).unwrap();
    }
}
