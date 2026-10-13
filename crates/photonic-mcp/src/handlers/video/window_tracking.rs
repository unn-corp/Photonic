//! Read-only snapshot analysis followed by a separate explicit history edit.
use super::*;
use photonic_render::window_tracking::TrackingConfig;
use photonic_video::window_tracking::{
    analyze_window, document_fingerprint, validate_request, WindowTrackingProposal,
    WindowTrackingRequest,
};

pub async fn track_grade_window(state: &AppState, args: TrackGradeWindowArgs) -> ToolResult {
    let request = WindowTrackingRequest {
        method: args.method,
        clip: args.clip_id,
        op: args.op_id,
        start: Tick(args.start_ticks),
        end: Tick(args.end_ticks),
        step_frames: args.step_frames.unwrap_or(1),
        format_index: args.format_index,
        config: TrackingConfig {
            search_radius: args.search_radius.unwrap_or(12),
            minimum_score: args.minimum_score.unwrap_or(0.75),
            minimum_margin: args.minimum_margin.unwrap_or(0.05),
        },
    };
    let (snapshot, revision) = {
        let doc = state.document.lock().await;
        if let Err(e) = validate_request(&doc, &request) {
            return ToolResult::error_with_code("TrackingInputInvalid", e);
        }
        (doc.clone(), state.history.lock().await.revision())
    };
    let gpu = match engine_bridge(state) {
        Ok(b) => b.engine().gpu().clone(),
        Err(e) => return e,
    };
    let (job_id, cancel) = match state.video_jobs.lock() {
        Ok(mut jobs) => {
            if jobs.has_active_kind("track_grade_window") {
                return ToolResult::error_with_code(
                    "JobCapacityExceeded",
                    "Only one window tracking job may run at a time",
                );
            }
            match jobs.start("track_grade_window") {
                Ok(job) => job,
                Err(e) => return ToolResult::error_with_code("JobCapacityExceeded", e.to_string()),
            }
        }
        Err(_) => return ToolResult::error("tracking job registry unavailable"),
    };
    let state = state.clone();
    tokio::spawn(async move {
        let jobs = state.video_jobs.clone();
        let worker_jobs = jobs.clone();
        let worker_cancel = cancel.clone();
        set_job_status(
            &jobs,
            job_id,
            JobStatus::Running {
                progress: 0.0,
                message: "Analyzing isolated pre-grade frames".into(),
            },
        );
        let result = tokio::task::spawn_blocking(move || {
            analyze_window(
                snapshot,
                revision,
                request,
                gpu,
                &worker_cancel,
                |progress| {
                    set_job_status(
                        &worker_jobs,
                        job_id,
                        JobStatus::Running {
                            progress,
                            message: "Tracking window translation; document unchanged".into(),
                        },
                    )
                },
            )
        })
        .await;
        let status = if cancel.load(Ordering::Relaxed) {
            JobStatus::Cancelled
        } else {
            match result {
                Ok(Ok(proposal)) => {
                    let doc = state.document.lock().await;
                    let current_revision = state.history.lock().await.revision();
                    if doc.id != proposal.document_id
                        || current_revision != revision
                        || document_fingerprint(&doc).ok().as_ref()
                            != Some(&proposal.document_fingerprint)
                    {
                        JobStatus::Failed {
                            error_code: "RevisionConflict".into(),
                            message: "Document changed during tracking; analyze again".into(),
                        }
                    } else {
                        JobStatus::Done {
                            result: serde_json::to_value(proposal)
                                .expect("finite validated tracking proposal"),
                        }
                    }
                }
                Ok(Err(e)) => JobStatus::Failed {
                    error_code: "TrackingFailed".into(),
                    message: e,
                },
                Err(e) => JobStatus::Failed {
                    error_code: "TrackingWorkerFailed".into(),
                    message: e.to_string(),
                },
            }
        };
        set_job_status(&jobs, job_id, status);
    });
    ToolResult::text("Window analysis queued. Poll get_job_status; apply_window_track is a separate explicit edit.")
        .with_data(json!({"job_id":job_id,"expected_revision":revision}))
}

pub async fn apply_window_track(state: &AppState, args: ApplyWindowTrackArgs) -> ToolResult {
    let proposal = {
        let jobs = match state.video_jobs.lock() {
            Ok(j) => j,
            Err(_) => return ToolResult::error("tracking registry unavailable"),
        };
        let Some(job) = jobs.get(args.job_id) else {
            return ToolResult::error_with_code(
                "TrackingJobMissing",
                "Tracking result expired or does not exist",
            );
        };
        if job.kind != "track_grade_window" {
            return ToolResult::error("job is not a window tracking analysis");
        }
        let JobStatus::Done { result } = &job.status else {
            return ToolResult::error("tracking job has no completed result");
        };
        match serde_json::from_value::<WindowTrackingProposal>(result.clone()) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e.to_string()),
        }
    };
    let mut doc = state.document.lock().await;
    let mut history = state.history.lock().await;
    let command = match proposal.apply_command(
        &doc,
        history.revision(),
        args.replace_center_keys,
        args.allow_partial,
    ) {
        Ok(c) => c,
        Err(e) => return ToolResult::error_with_code("TrackingApplyRefused", e),
    };
    history.execute_discrete(Command::Timeline(command), &mut doc);
    ToolResult::text("Window center keys applied as one undoable edit; size, rotation and softness unchanged")
        .with_data(json!({"job_id":args.job_id,"samples":proposal.samples.len(),"revision":history.revision(),"partial":!proposal.completed}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        Grade, GradeMask, GradeOp, GradeOpKind, GradeOpParams, MediaAsset, WindowShape,
    };
    #[tokio::test]
    async fn window_tracking_mcp_analyzes_then_applies_once_without_hidden_edits() {
        let state = AppState::headless_for_test();
        let root =
            std::env::temp_dir().join(format!("photonic-mcp-tracking-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("pattern.png");
        let img = image::RgbaImage::from_fn(96, 72, |x, y| {
            let v = ((x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663)) % 191 + 32) as u8;
            image::Rgba([v, v, v, 255])
        });
        img.save(&path).unwrap();
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("tracking", FrameRate::FPS_30, 96, 72);
        let mut track = Track::new(photonic_core::timeline::TrackKind::Video, "V1");
        let asset = MediaAsset::from_file(AssetKind::Image, path);
        let mut clip = Clip::new(
            ClipSource::Asset { asset: asset.id },
            Tick::from_seconds(1),
            Tick::from_seconds(2),
        );
        project.media.assets.insert(asset.id, asset);
        let mut op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        op.mask = Some(GradeMask::PowerWindow {
            shape: WindowShape::Ellipse,
            center: [0.5, 0.5],
            size: [0.15, 0.15],
            rotation: 0.0,
            softness: 0.1,
            invert: false,
        });
        let args = json!({"clip_id":clip.id,"op_id":op.id,"start_ticks":clip.start.0,"end_ticks":clip.start.0+seq.frame_rate.ticks_per_frame().0*3});
        clip.grade = Some(Grade {
            ops: vec![op],
            ..Grade::new()
        });
        track.clips.push(clip);
        seq.video_tracks.push(track);
        project.insert_sequence(seq);
        state.document.lock().await.timeline = Some(project);
        let before = document_fingerprint(&*state.document.lock().await).unwrap();
        let result =
            track_grade_window(&state, serde_json::from_value(args.clone()).unwrap()).await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let id: uuid::Uuid =
            serde_json::from_value(result.structured_content.unwrap()["job_id"].clone()).unwrap();
        let second = track_grade_window(&state, serde_json::from_value(args).unwrap()).await;
        assert_eq!(second.is_error, Some(true));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
        loop {
            let status = state
                .video_jobs
                .lock()
                .unwrap()
                .get(id)
                .unwrap()
                .status
                .clone();
            match status {
                JobStatus::Done { result } => {
                    assert_eq!(result["completed"], true);
                    assert_eq!(result["samples"].as_array().unwrap().len(), 4);
                    break;
                }
                JobStatus::Failed { message, .. } => panic!("{message}"),
                _ => assert!(std::time::Instant::now() < deadline, "tracking timed out"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(state.history.lock().await.undo_depth(), 0);
        assert_eq!(
            document_fingerprint(&*state.document.lock().await).unwrap(),
            before
        );
        let apply = || ApplyWindowTrackArgs {
            job_id: id,
            replace_center_keys: false,
            allow_partial: false,
        };
        assert_ne!(
            apply_window_track(&state, apply()).await.is_error,
            Some(true)
        );
        assert_eq!(state.history.lock().await.undo_depth(), 1);
        assert_eq!(
            apply_window_track(&state, apply()).await.is_error,
            Some(true)
        );
        {
            let mut doc = state.document.lock().await;
            assert!(state.history.lock().await.undo(&mut doc));
            assert_eq!(document_fingerprint(&doc).unwrap(), before);
        }
        for name in ["track_grade_window", "apply_window_track"] {
            assert!(crate::schema_gen::tool_list()
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == name));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    async fn checked_tool(state: &AppState, name: &str, args: Value) -> ToolResult {
        let result = Box::pin(crate::dispatch::dispatch_tool(state, name, args))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true), "{name}: {result:?}");
        result
    }
    async fn wait_done(state: &AppState, id: &Value) -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let result = checked_tool(state, "get_job_status", json!({"job_id":id})).await;
            let data = result.structured_content.unwrap();
            match data["status"]["state"].as_str().unwrap() {
                "done" => return data,
                "queued" | "running" => {
                    assert!(std::time::Instant::now() < deadline, "job timeout")
                }
                _ => panic!("job failed: {data}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
    #[tokio::test]
    async fn tracked_native_window_versions_export_and_reimport_metadata() {
        Box::pin(check_tracked_delivery(false)).await;
    }

    #[tokio::test]
    async fn native_balance_match_tracking_versions_archive_export_reimport_workflow() {
        Box::pin(check_tracked_delivery(true)).await;
    }

    #[tokio::test]
    async fn adaptive_native_balance_tracking_versions_archive_export_reimport_workflow() {
        Box::pin(check_tracked_delivery_mode(
            true,
            photonic_render::window_tracking::TrackingMethod::AdaptiveBidirectional,
            false,
        ))
        .await;
    }

    async fn check_tracked_delivery(with_balance: bool) {
        Box::pin(check_tracked_delivery_mode(
            with_balance,
            photonic_render::window_tracking::TrackingMethod::FixedReference,
            false,
        ))
        .await;
    }

    #[tokio::test]
    #[ignore = "requires external OpenCV footage; explicit end-to-end observation, not camera color or tracking ground-truth acceptance"]
    async fn native_real_footage_balance_tracking_delivery_workflow() {
        Box::pin(check_tracked_delivery_mode(
            true,
            photonic_render::window_tracking::TrackingMethod::AdaptiveBidirectional,
            true,
        ))
        .await;
    }

    async fn check_tracked_delivery_mode(
        with_balance: bool,
        method: photonic_render::window_tracking::TrackingMethod,
        real_footage: bool,
    ) {
        use photonic_core::timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
            NativeManagedColorConfig, SequenceColorConfig,
        };
        use photonic_core::timeline::{Interp, Keyframe, PropPath, PropValue, TrackKind};
        let ffmpeg = photonic_video::media::ffmpeg_locate::locate_for_test()
            .expect("FFmpeg required for tracked delivery qualification");
        let state = AppState::headless_for_test();
        // The normal desktop policy admits writes beneath cwd. Keep delivery
        // fixtures there rather than weakening the application's path guard.
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "photonic-tracking-delivery-{}",
                uuid::Uuid::new_v4()
            ));
        let retained = with_balance
            .then(|| {
                std::env::var_os(if real_footage {
                    "PHOTONIC_REAL_WORKFLOW_QUALIFICATION_DIR"
                } else {
                    "PHOTONIC_GRADING_WORKFLOW_QUALIFICATION_DIR"
                })
            })
            .flatten();
        let root = retained
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or(root);
        std::fs::create_dir_all(&root).unwrap();
        let frames = if with_balance { 16 } else { 4 };
        let alpha = if with_balance { 255 } else { 192 };
        *state.document_path.lock().unwrap() = Some(root.join("workflow.photon"));
        let (width, height) = if real_footage { (768, 576) } else { (96, 72) };
        let rate = if real_footage {
            FrameRate::new(10, 1)
        } else {
            FrameRate::FPS_30
        };
        let source = root.join(if real_footage {
            "vtest.avi"
        } else {
            "texture.png"
        });
        let texture = |x: u32, y: u32| {
            ((x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663)) % 160 + 35) as u8
        };
        if real_footage {
            let external = std::path::PathBuf::from(
                std::env::var_os("PHOTONIC_TRACKING_FOOTAGE_DIR")
                    .expect("external footage required"),
            )
            .join("vtest.avi");
            std::fs::copy(external, &source).unwrap();
        } else {
            image::RgbaImage::from_fn(96, 72, |x, y| {
                let v = texture(x, y);
                image::Rgba([v, v, v, alpha])
            })
            .save(&source)
            .unwrap();
        }
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("Tracked delivery", rate, width, height);
        seq.color =
            SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
        let frame = seq.frame_rate.ticks_per_frame().0;
        let sid = seq.id;
        let mut asset = MediaAsset::from_file(
            if real_footage {
                AssetKind::Video
            } else {
                AssetKind::Image
            },
            source.clone(),
        );
        asset.native_input_color = Some(NativeInputColorInterpretation {
            version: 1,
            standard: NativeInputStandard::SrgbDisplay,
            range: InputSignalRange::Full,
            matrix: InputMatrix::Rgb,
            chroma_location: None,
            hlg_peak_nits: None,
            reference_white_nits: None,
        });
        if real_footage {
            asset.probe =
                Some(photonic_video::media::probe::probe_asset(&ffmpeg, &source).unwrap());
            asset.native_input_color = Some(NativeInputColorInterpretation {
                version: 1,
                standard: NativeInputStandard::Bt709Scene,
                range: InputSignalRange::Limited,
                matrix: InputMatrix::Bt709,
                chroma_location: Some(photonic_core::timeline::color::NativeChromaLocation::Left),
                hlg_peak_nits: None,
                reference_white_nits: None,
            });
        }
        let mut clip = Clip::new(
            ClipSource::Asset { asset: asset.id },
            Tick(0),
            Tick(frame * frames),
        );
        let cid = clip.id;
        project.media.assets.insert(asset.id, asset);
        if !real_footage {
            for (at, value) in [
                (Tick(0), 0.0),
                (Tick(frame * (frames - 1)), 2.0 * (frames - 1) as f64),
            ] {
                clip.transform
                    .track_mut(&PropPath::new("transform.x"))
                    .insert_keyframe(Keyframe::new(at, PropValue::Float(value), Interp::Linear));
            }
        }
        let mut op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        let oid = op.id;
        op.mask = Some(GradeMask::PowerWindow {
            shape: WindowShape::Ellipse,
            center: if real_footage {
                [450. / 768., 55. / 576.]
            } else {
                [0.5, 0.5]
            },
            size: if real_footage {
                [0.02, 0.035]
            } else {
                [0.1, 0.15]
            },
            rotation: 0.0,
            softness: 0.1,
            invert: false,
        });
        if !with_balance {
            clip.grade = Some(Grade {
                ops: vec![op.clone()],
                ..Grade::new()
            });
        }
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(clip);
        seq.video_tracks.push(track);
        project.insert_sequence(seq);
        state.document.lock().await.timeline = Some(project);
        if with_balance {
            let captured = checked_tool(&state, "capture_reference_still", json!({"sequence_id":sid,"at_ticks":0,"source_clip_id":cid,"name":"Balanced source"})).await.structured_content.unwrap();
            assert_eq!(captured["native_balance_available"], true);
            let still = captured["still_id"].clone();
            let mut brighter = Grade::new();
            brighter.ops.push(GradeOp::new(
                GradeOpKind::Exposure,
                GradeOpParams::Exposure { stops: 0.5 },
            ));
            checked_tool(&state, "set_grade", json!({"clip_id":cid,"grade":brighter})).await;
            let proposal =
                checked_tool(&state, "compare_reference_still", json!({"still_id":still}))
                    .await
                    .structured_content
                    .unwrap();
            for point in proposal["shot_match_suggestion"]["points"]
                .as_array()
                .unwrap()
            {
                assert!((point.as_f64().unwrap() + 6.0).abs() < 0.03, "{proposal}");
            }
            let before = state.history.lock().await.undo_depth();
            checked_tool(&state, "apply_shot_match", json!({"clip_id":cid,"still_id":still,"at_ticks":0,"expected_revision":proposal["current_revision"]})).await;
            assert_eq!(state.history.lock().await.undo_depth(), before + 1);
            let residual =
                checked_tool(&state, "compare_reference_still", json!({"still_id":still}))
                    .await
                    .structured_content
                    .unwrap();
            for point in residual["shot_match_suggestion"]["points"]
                .as_array()
                .unwrap()
            {
                assert!(point.as_f64().unwrap().abs() < 0.03, "{residual}");
            }
            let matched = serde_json::to_value(&*state.document.lock().await).unwrap();
            checked_tool(&state, "undo", json!({})).await;
            checked_tool(&state, "redo", json!({})).await;
            assert_eq!(
                serde_json::to_value(&*state.document.lock().await).unwrap(),
                matched
            );
            let mut grade = state
                .document
                .lock()
                .await
                .timeline
                .as_ref()
                .unwrap()
                .sequences[&sid]
                .video_tracks[0]
                .clips[0]
                .grade
                .clone()
                .unwrap();
            grade.ops.push(op.clone());
            checked_tool(&state, "set_grade", json!({"clip_id":cid,"grade":grade})).await;
            std::fs::write(
                root.join("balance-proposal.json"),
                serde_json::to_vec_pretty(&proposal).unwrap(),
            )
            .unwrap();
            std::fs::write(
                root.join("balance-residual.json"),
                serde_json::to_vec_pretty(&residual).unwrap(),
            )
            .unwrap();
        }

        let original = checked_tool(
            &state,
            "grade_version",
            json!({"clip_id":cid,"op":"add","name":"Untracked"}),
        )
        .await
        .structured_content
        .unwrap()["active_version"]
            .clone();
        let tracked = checked_tool(
            &state,
            "grade_version",
            json!({"clip_id":cid,"op":"add","name":"Tracked"}),
        )
        .await
        .structured_content
        .unwrap()["active_version"]
            .clone();
        let queued = checked_tool(
            &state,
            "track_grade_window",
            json!({"clip_id":cid,"op_id":oid,"start_ticks":0,"end_ticks":frame*(frames-1),"method":method}),
        )
        .await
        .structured_content
        .unwrap();
        let done = wait_done(&state, &queued["job_id"]).await;
        std::fs::write(
            root.join("tracking-proposal.json"),
            serde_json::to_vec_pretty(&done).unwrap(),
        )
        .unwrap();
        assert_eq!(done["status"]["result"]["completed"], true, "{done}");
        assert_eq!(
            done["status"]["result"]["method"],
            serde_json::to_value(method).unwrap()
        );
        for sample in done["status"]["result"]["samples"].as_array().unwrap() {
            if method == photonic_render::window_tracking::TrackingMethod::AdaptiveBidirectional {
                assert!((0.0..=1.0).contains(&sample["roundtrip_error_pixels"].as_f64().unwrap()));
            } else {
                assert!(sample.get("roundtrip_error_pixels").is_none());
            }
        }

        checked_tool(
            &state,
            "apply_window_track",
            json!({"job_id":queued["job_id"]}),
        )
        .await;
        let tracked_document = serde_json::to_value(&*state.document.lock().await).unwrap();
        let tracked_grade = {
            let doc = state.document.lock().await;
            doc.timeline.as_ref().unwrap().sequences[&sid].video_tracks[0].clips[0]
                .grade
                .clone()
        };
        checked_tool(
            &state,
            "grade_version",
            json!({"clip_id":cid,"op":"activate","version_id":original}),
        )
        .await;
        {
            let doc = state.document.lock().await;
            assert!(
                doc.timeline.as_ref().unwrap().sequences[&sid].video_tracks[0].clips[0]
                    .grade
                    .as_ref()
                    .unwrap()
                    .ops[if with_balance { 2 } else { 0 }]
                .params
                .tracks
                .is_empty()
            );
        }
        checked_tool(
            &state,
            "grade_version",
            json!({"clip_id":cid,"op":"activate","version_id":tracked}),
        )
        .await;
        {
            let doc = state.document.lock().await;
            assert_eq!(
                doc.timeline.as_ref().unwrap().sequences[&sid].video_tracks[0].clips[0].grade,
                tracked_grade
            );
        }
        let restored: photonic_core::Document = serde_json::from_value(tracked_document).unwrap();
        let reload = AppState::headless_for_test();
        *reload.document.lock().await = restored;
        let mut preview_frames = Vec::new();
        for index in 0..frames {
            let result=checked_tool(&reload,"render_frame_at",json!({"sequence_id":sid,"at_ticks":frame*index,"quality":"full","output_format":"png"})).await;
            let data = result
                .content
                .iter()
                .find_map(|c| {
                    if let ContentItem::Image { data, .. } = c {
                        Some(data)
                    } else {
                        None
                    }
                })
                .unwrap();
            let pixels = image::load_from_memory(&general_purpose::STANDARD.decode(data).unwrap())
                .unwrap()
                .to_rgba8();
            assert_eq!(pixels.dimensions(), (width, height));
            let point = if real_footage {
                (450, 55)
            } else {
                (48 + index as u32 * 2, 36)
            };
            assert!(pixels.get_pixel(point.0, point.1).0[3].abs_diff(alpha) <= 1);
            if real_footage {
                preview_frames.push(pixels);
            }
        }
        let before = serde_json::to_value(&*reload.document.lock().await).unwrap();
        if real_footage {
            let document = reload.document.lock().await;
            let project = document.timeline.as_ref().unwrap();
            std::fs::write(
                root.join("before-export-project.json"),
                serde_json::to_vec_pretty(&serde_json::to_value(project).unwrap()).unwrap(),
            )
            .unwrap();
            let round: TimelineProject =
                serde_json::from_slice(&serde_json::to_vec_pretty(project).unwrap()).unwrap();
            std::fs::write(
                root.join("roundtrip-project.json"),
                serde_json::to_vec_pretty(&serde_json::to_value(&round).unwrap()).unwrap(),
            )
            .unwrap();
        }
        let output = root.join("tracked.mov");
        let queued=checked_tool(&reload,"export_sequence",json!({"sequence_id":sid,"out_path":output,"preset":"ProRes Mezzanine","write_manifest":true})).await.structured_content.unwrap();
        assert_eq!(queued["total_frames"], frames);
        wait_done(&reload, &queued["job_id"]).await;
        photonic_video::export::manifest::verify_manifest(
            &photonic_video::export::manifest::manifest_path(&output),
        )
        .unwrap();
        let decoded = std::process::Command::new(&ffmpeg.ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&output)
            .args(["-frames:v", &frames.to_string()])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba64le", "pipe:1"])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert_eq!(
            decoded.stdout.len(),
            frames as usize * width as usize * height as usize * 8
        );
        if real_footage {
            // Compare two deliveries through explicit display/video transfer
            // encodings. This is pipeline consistency, not an independent
            // scene-color oracle for footage of unknown camera provenance.
            let mut squared = 0.0_f64;
            let mut maximum = 0.0_f64;
            let mut count = 0_u64;
            for (index, image) in preview_frames.iter().enumerate() {
                for (x, y, pixel) in image.enumerate_pixels() {
                    let offset = (index * width as usize * height as usize
                        + y as usize * width as usize
                        + x as usize)
                        * 8;
                    let alpha16 = u16::from_le_bytes([
                        decoded.stdout[offset + 6],
                        decoded.stdout[offset + 7],
                    ]);
                    assert!(alpha16.abs_diff(65535) <= 512);
                    for c in 0..3 {
                        let display = f64::from(pixel.0[c]) / 255.0;
                        let linear = if display <= 0.04045 {
                            display / 12.92
                        } else {
                            ((display + 0.055) / 1.055).powf(2.4)
                        };
                        let expected = if linear <= 0.018 {
                            4.5 * linear
                        } else {
                            1.099 * linear.powf(0.45) - 0.099
                        };
                        let actual = f64::from(u16::from_le_bytes([
                            decoded.stdout[offset + c * 2],
                            decoded.stdout[offset + c * 2 + 1],
                        ])) / 65535.0;
                        let error = (actual - expected).abs();
                        maximum = maximum.max(error);
                        squared += error * error;
                        count += 1;
                    }
                }
            }
            let rmse = (squared / count as f64).sqrt();
            std::fs::write(root.join("delivery-consistency.json"),serde_json::to_vec_pretty(&json!({"frames":frames,"size":[width,height],"rgb_samples":count,"maximum_error":maximum,"rmse":rmse,"maximum_limit":0.04,"rmse_limit":0.01,"scope":"PNG sRGB to BT709 transfer compared with decoded ProRes; pipeline consistency only"})).unwrap()).unwrap();
            assert!(
                maximum <= 0.04 && rmse <= 0.01,
                "delivery max={maximum}, rmse={rmse}"
            );
        } else {
            let input = texture(48, 36) as f64 / 255.0;
            let linear =
                photonic_video::color::native::encoded_srgb_to_linear_ap1_display([input; 3])
                    .map(|v| v * 2.0);
            let expected = photonic_video::color::native_output::Aces2SdrOutput::shared()
                .unwrap()
                .map_straight_video(linear)
                .unwrap();
            for index in 0..frames {
                let offset = (index as usize * 96 * 72 + 36 * 96 + 48 + index as usize * 2) * 8;
                for c in 0..3 {
                    let v = u16::from_le_bytes([
                        decoded.stdout[offset + c * 2],
                        decoded.stdout[offset + c * 2 + 1],
                    ]);
                    assert!(
                        v.abs_diff((expected[c] * 65535.0).round() as u16) <= 1600,
                        "frame{index} channel{c}: {v} vs {:?}",
                        expected
                    );
                }
                let a =
                    u16::from_le_bytes([decoded.stdout[offset + 6], decoded.stdout[offset + 7]]);
                assert!(a.abs_diff((f64::from(alpha) / 255.0 * 65535.0_f64).round() as u16) <= 512);
            }
        }
        assert_eq!(
            serde_json::to_value(&*reload.document.lock().await).unwrap(),
            before
        );
        assert_eq!(reload.history.lock().await.undo_depth(), 0);
        // Reimport as delivered media, without pretending an SDR output transform
        // can be inverted into the original scene-referred grading state.
        let imported = checked_tool(&reload, "import_media", json!({"paths":[output]})).await;
        let imported_id = imported.structured_content.unwrap()["assets"][0]["asset_id"].clone();
        let probe = checked_tool(&reload, "probe_media", json!({"asset_id":imported_id}))
            .await
            .structured_content
            .unwrap();
        wait_done(&reload, &probe["job_id"]).await;
        let doc = reload.document.lock().await;
        let imported_key: AssetId = serde_json::from_value(imported_id).unwrap();
        let media = &doc.timeline.as_ref().unwrap().media.assets[&imported_key];
        let probe = media.probe.as_ref().expect("reimport probe persisted");
        let video = probe.video.as_ref().unwrap();
        assert_eq!((video.width, video.height), (width, height));
        assert_eq!(video.frame_rate, rate);
        // ffprobe reports decimal seconds to microsecond precision; decoded
        // frame count above is exact, while the persisted duration can round.
        assert!(
            (probe.duration.0 - frame * frames).abs()
                <= photonic_core::timeline::TICKS_PER_SECOND / 1_000_000 + 1
        );
        assert!(doc.timeline.as_ref().unwrap().media.assets.values().any(|a|matches!(&a.source,photonic_core::timeline::AssetSource::File{path,..} if path==&output)));
        drop(doc);
        if with_balance {
            std::fs::write(
                root.join("workflow.photon"),
                reload.document.lock().await.to_json().unwrap(),
            )
            .unwrap();
            *reload.document_path.lock().unwrap() = Some(root.join("workflow.photon"));
            let destination = root.join(format!("portable-{}", uuid::Uuid::new_v4()));
            let archived = checked_tool(&reload, "archive_project", json!({"path":destination}))
                .await
                .structured_content
                .unwrap();
            let archived_path = std::path::PathBuf::from(archived["path"].as_str().unwrap());
            let mut recovered = photonic_core::Document::from_json(
                &std::fs::read_to_string(&archived_path).unwrap(),
            )
            .unwrap();
            photonic_video::project::archive::resolve_project_relative_assets(
                &mut recovered,
                &archived_path,
            );
            let recovered_project = recovered.timeline.as_ref().unwrap();
            let original = reload.document.lock().await;
            assert_eq!(
                recovered_project.sequences[&sid].reference_stills,
                original.timeline.as_ref().unwrap().sequences[&sid].reference_stills
            );
            drop(original);
            for asset in recovered_project.media.assets.values() {
                if let photonic_core::timeline::AssetSource::File { path, .. } = &asset.source {
                    assert!(path.is_file());
                    assert!(path.starts_with(&destination));
                }
            }
            let cold = AppState::headless_for_test();
            *cold.document.lock().await = recovered;
            *cold.document_path.lock().unwrap() = Some(archived_path);
            checked_tool(&cold,"render_frame_at",json!({"sequence_id":sid,"at_ticks":frame*(frames-1),"quality":"full","output_format":"png"})).await;
            std::fs::write(root.join("workflow-checks.json"),serde_json::to_vec_pretty(&json!({"frames":frames,"native_global_balance":true,"tracked_isolation":true,"version_switching":true,"save_recovery":true,"prores_manifest_and_decode":true,"reimport_probe":true,"portable_archive_reference_identity":true,"cold_archive_render":true,"fixture":if real_footage { "OpenCV vtest first16frames; static architectural target; explicit diagnostic BT709 limited left; unknown source colorimetry, no moving-target ground truth" } else { "synthetic translated texture; not representative footage" }})).unwrap()).unwrap();
        }
        if retained.is_none() {
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[tokio::test]
    #[ignore = "requires external OpenCV vtest footage; native-engine observation, not general tracking acceptance"]
    async fn native_real_footage_tracking_observation_is_readonly() {
        use photonic_core::timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
            NativeManagedColorConfig, SequenceColorConfig,
        };
        use photonic_core::timeline::TrackKind;
        let root = std::path::PathBuf::from(
            std::env::var_os("PHOTONIC_TRACKING_FOOTAGE_DIR")
                .expect("external footage directory required"),
        );
        let tools = photonic_video::media::ffmpeg_locate::locate_for_test().unwrap();
        let mut observations = Vec::new();
        for (name, center, static_control) in [
            ("fixed_window", [450. / 768., 55. / 576.], true),
            ("left_walker", [267. / 768., 257. / 576.], false),
            ("middle_walker", [514. / 768., 190. / 576.], false),
            (
                "sign_with_passing_occluders",
                [428. / 768., 225. / 576.],
                false,
            ),
        ] {
            for method in [
                photonic_render::window_tracking::TrackingMethod::FixedReference,
                photonic_render::window_tracking::TrackingMethod::AdaptiveBidirectional,
            ] {
                let state = AppState::headless_for_test();
                let mut project = TimelineProject::new();
                let mut sequence = Sequence::new(
                    "Real-footage tracking observation",
                    FrameRate::new(10, 1),
                    768,
                    576,
                );
                sequence.color = SequenceColorConfig::NativeManaged(Box::new(
                    NativeManagedColorConfig::sdr_draft(),
                ));
                let sid = sequence.id;
                let tick = sequence.frame_rate.ticks_per_frame().0;
                let source = root.join("vtest.avi");
                let mut asset = MediaAsset::from_file(AssetKind::Video, source.clone());
                asset.probe =
                    Some(photonic_video::media::probe::probe_asset(&tools, &source).unwrap());
                // Explicit diagnostic interpretation; source camera/color metadata
                // is unknown, so this is not footage colorimetry qualification.
                asset.native_input_color = Some(NativeInputColorInterpretation {
                    version: 1,
                    standard: NativeInputStandard::Bt709Scene,
                    range: InputSignalRange::Limited,
                    matrix: InputMatrix::Bt709,
                    chroma_location: Some(
                        photonic_core::timeline::color::NativeChromaLocation::Left,
                    ),
                    hlg_peak_nits: None,
                    reference_white_nits: None,
                });
                let mut clip = Clip::new(
                    ClipSource::Asset { asset: asset.id },
                    Tick::ZERO,
                    Tick(tick * 31),
                );
                let cid = clip.id;
                let mut op = GradeOp::new(
                    GradeOpKind::Exposure,
                    GradeOpParams::Exposure { stops: 0.5 },
                );
                let oid = op.id;
                op.mask = Some(GradeMask::PowerWindow {
                    shape: WindowShape::Ellipse,
                    center,
                    size: [0.02, 0.035],
                    rotation: 0.,
                    softness: 0.1,
                    invert: false,
                });
                clip.grade = Some(Grade {
                    ops: vec![op],
                    ..Grade::new()
                });
                let mut track = Track::new(TrackKind::Video, "V1");
                track.clips.push(clip);
                sequence.video_tracks.push(track);
                project.media.insert(asset);
                project.insert_sequence(sequence);
                project.active_sequence = Some(sid);
                state.document.lock().await.timeline = Some(project);
                let preview = checked_tool(
                    &state,
                    "render_frame_at",
                    json!({"sequence_id":sid,"at_ticks":0,"quality":"full","output_format":"png"}),
                )
                .await;
                eprintln!(
                    "Native footage preview diagnostics: {:?}",
                    preview.structured_content
                );
                let before = serde_json::to_value(&*state.document.lock().await).unwrap();
                let revision = state.history.lock().await.revision();
                let queued = checked_tool(
                &state,
                "track_grade_window",
                json!({"clip_id":cid,"op_id":oid,"start_ticks":0,"end_ticks":30*tick,"method":method}),
            )
            .await
            .structured_content
            .unwrap();
                let result = wait_done(&state, &queued["job_id"]).await;
                let proposal = &result["status"]["result"];
                assert_eq!(proposal["method"], serde_json::to_value(method).unwrap());
                assert_eq!(proposal["frame_size"], json!([768, 576]));
                assert_eq!(proposal["analysis_size"], json!([256, 192]));
                assert_eq!(
                    serde_json::to_value(&*state.document.lock().await).unwrap(),
                    before
                );
                assert_eq!(state.history.lock().await.revision(), revision);
                if static_control {
                    assert_eq!(proposal["completed"], true, "{result}");
                    assert_eq!(proposal["samples"].as_array().unwrap().len(), 31);
                    for sample in proposal["samples"].as_array().unwrap() {
                        assert!(
                            (sample["center"][0].as_f64().unwrap() - f64::from(center[0])).abs()
                                <= 1.0 / 256.0
                        );
                        assert!(
                            (sample["center"][1].as_f64().unwrap() - f64::from(center[1])).abs()
                                <= 1.0 / 192.0
                        );
                    }
                }
                observations.push(
                json!({"name":name,"method":method,"proposal":proposal,"document_and_history_unchanged":true}),
            );
            }
        }
        std::fs::write(root.join("native-engine-observation.json"),serde_json::to_vec_pretty(&json!({"observations":observations,"scope":"Full original video decode and isolated Native AP1 pre-grade tap, default translation tracking thresholds, no apply","input_interpretation":"explicit diagnostic BT.709 limited/left chroma; source colorimetry unknown","limits":"three-second public sample; moving subjects have no annotated ground truth; completion is not accuracy"})).unwrap()).unwrap();
    }
}
