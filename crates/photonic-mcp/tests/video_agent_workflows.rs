//! Scripted agent workflows through the same dispatcher used by MCP clients.
//! These verify edit/render outcomes, not a model's reasoning or tool choice.
use photonic_core::timeline::{
    Clip, ClipSource, FrameRate, Sequence, SequenceId, Tick, TimelineProject, Track, TrackKind,
};
use photonic_mcp::{
    dispatch::dispatch_tool,
    protocol::{ContentItem, ToolResult},
    server::AppState,
};
use serde_json::{json, Value};

async fn fixture() -> (AppState, SequenceId, Value) {
    let mut state = AppState::headless_for_test();
    state.path_policy = photonic_core::PathPolicy::test_default();
    let mut project = TimelineProject::new();
    let mut sequence = Sequence::new("Agent outcome", FrameRate::FPS_30, 128, 64);
    let mut track = Track::new(TrackKind::Video, "V1");
    let frame = FrameRate::FPS_30.ticks_per_frame().0;
    for (index, color) in [
        photonic_core::Color::rgb(1.0, 0.0, 0.0),
        photonic_core::Color::rgb(0.0, 1.0, 0.0),
    ]
    .into_iter()
    .enumerate()
    {
        track.clips.push(Clip::new(
            ClipSource::SolidColor { color },
            Tick(index as i64 * frame * 30),
            Tick(frame * 30),
        ));
    }
    let track_id = json!(track.id);
    let id = sequence.id;
    sequence.video_tracks.push(track);
    project.insert_sequence(sequence);
    project.active_sequence = Some(id);
    state.document.lock().await.timeline = Some(project);
    (state, id, track_id)
}
async fn call(state: &AppState, name: &str, args: Value) -> ToolResult {
    dispatch_tool(state, name, args)
        .await
        .expect("registered tool dispatch")
}
fn data(result: &ToolResult) -> &Value {
    result
        .structured_content
        .as_ref()
        .expect("native structured output")
}
fn success(result: &ToolResult) {
    assert_ne!(result.is_error, Some(true), "{result:?}");
}

#[tokio::test]
async fn paged_inspection_edit_retry_and_undo_preserve_intended_timeline() {
    let (state, sequence, track) = fixture().await;
    let before = state.document.lock().await.timeline.clone();
    let page = call(
        &state,
        "get_timeline_snapshot",
        json!({"sequence_id":sequence,"limit":1}),
    )
    .await;
    success(&page);
    assert_eq!(data(&page)["clips"].as_array().unwrap().len(), 1);
    assert_eq!(data(&page)["total_clips"], 2);
    let revision = data(&page)["revision"].clone();
    let clip = data(&page)["clips"][0]["clip_id"].clone();
    let continuation = call(
        &state,
        "get_timeline_snapshot",
        json!({"sequence_id":sequence,"limit":1,"offset":1,"expected_revision":revision}),
    )
    .await;
    success(&continuation);
    assert_ne!(data(&continuation)["clips"][0]["clip_id"], clip);
    assert!(data(&continuation)["next_page"].is_null());

    // Create and move a newly returned clip ID within one atomic plan.
    let operations = json!([
        {"tool":"move_clip","arguments":{"clip_id":clip,"new_start_seconds":3.0}},
        {"tool":"insert_clip","arguments":{"track_id":track,"source":{"kind":"solid_color","color":"#0000ff"},"start_seconds":4.0,"duration_ticks":photonic_core::timeline::TICKS_PER_SECOND}},
        {"tool":"move_clip","arguments":{"clip_id":{"$ref":"1.clip_id"},"new_start_seconds":5.0}}
    ]);
    let plan = json!({"sequence_id":sequence,"expected_revision":revision,"request_id":"assemble-outcome","operations":operations});
    let mut dry = plan.clone();
    dry["dry_run"] = json!(true);
    let dry = call(&state, "apply_video_edit_plan", dry).await;
    success(&dry);
    assert_eq!(state.document.lock().await.timeline, before);
    assert_eq!(state.history.lock().await.undo_depth(), 0);
    let applied = call(&state, "apply_video_edit_plan", plan.clone()).await;
    success(&applied);
    assert_eq!(data(&applied)["undo_steps"], 1);
    let retried = call(&state, "apply_video_edit_plan", plan).await;
    success(&retried);
    assert_eq!(data(&retried)["replayed"], true);
    assert_eq!(state.history.lock().await.undo_depth(), 1);
    let after = call(&state,"get_timeline_snapshot",json!({"sequence_id":sequence,"track_ids":[track],"start_ticks":0,"end_ticks":9*photonic_core::timeline::TICKS_PER_SECOND})).await;
    success(&after);
    assert_eq!(data(&after)["total_clips"], 3);
    let starts: Vec<_> = data(&after)["clips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|clip| clip["start_ticks"].as_i64().unwrap())
        .collect();
    let second = photonic_core::timeline::TICKS_PER_SECOND;
    assert_eq!(starts, vec![second, 3 * second, 5 * second]);
    let stale = call(
        &state,
        "get_timeline_snapshot",
        json!({"sequence_id":sequence,"offset":1,"expected_revision":revision}),
    )
    .await;
    assert_eq!(data(&stale)["error_code"], "RevisionConflict");
    let mut document = state.document.lock().await;
    assert!(state.history.lock().await.undo(&mut document));
    assert_eq!(document.timeline, before);
}

#[tokio::test]
async fn invalid_scopes_and_export_batches_fail_before_admitting_work() {
    let (state, sequence, _) = fixture().await;
    for arguments in [
        json!({"sequence_id":sequence,"offset":1}),
        json!({"sequence_id":sequence,"limit":1001}),
        json!({"sequence_id":sequence,"track_ids":[uuid::Uuid::new_v4()]}),
    ] {
        assert_eq!(
            call(&state, "get_timeline_snapshot", arguments)
                .await
                .is_error,
            Some(true)
        );
    }
    let output = std::env::temp_dir().join(format!(
        "photonic-agent-export-{}.mp4",
        uuid::Uuid::new_v4()
    ));
    let args = json!({"expected_revision":0,"outputs":[{"sequence_id":sequence,"out_path":output},{"sequence_id":sequence,"out_path":output}]});
    let duplicate = call(&state, "export_sequences", args).await;
    assert_eq!(data(&duplicate)["error_code"], "DuplicateOutputPath");
    assert!(!output.exists());
    assert!(
        state.video_engine.initialization_state().is_none(),
        "invalid jobs must fail before GPU initialization"
    );
    let stale = call(
        &state,
        "export_sequences",
        json!({"expected_revision":99,"outputs":[{"sequence_id":sequence,"out_path":output}]}),
    )
    .await;
    assert_eq!(data(&stale)["error_code"], "RevisionConflict");
    let malformed = call(
        &state,
        "render_frames_at",
        json!({"sequence_id":sequence,"at_ticks":[0],"columns":0}),
    )
    .await;
    assert_eq!(data(&malformed)["error_code"], "InvalidArguments");
}

#[tokio::test]
async fn contact_sheet_pixels_match_requested_times_and_revision() {
    use base64::Engine as _;
    let (state, sequence, _) = fixture().await;
    let result = call(&state,"render_frames_at",json!({"sequence_id":sequence,"at_ticks":[0,photonic_core::timeline::TICKS_PER_SECOND],"quality":"full","max_long_edge":64,"columns":2,"expected_revision":0})).await;
    if data(&result)["cause"]["error_code"] == "GpuUnavailable"
        || data(&result)["cause"]["error_code"] == "EngineUnavailable"
    {
        assert!(
            std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
            "GPU required: {result:?}"
        );
        eprintln!("GPU unavailable: skipping contact sheet outcome");
        return;
    }
    success(&result);
    assert_eq!(data(&result)["revision"], 0);
    assert_eq!(
        data(&result)["sheet_size"],
        json!({"width":128,"height":32})
    );
    let frames = data(&result)["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 2);
    assert_ne!(
        frames[0]["inspection_request_id"],
        frames[1]["inspection_request_id"]
    );
    for frame in frames {
        assert_eq!(frame["revision"], 0);
        assert_eq!(frame["quality"], "full");
        assert_eq!(frame["processing_quality"], "full");
        assert_eq!(frame["gpu_downscaled"], true);
    }
    let encoded = result
        .content
        .iter()
        .find_map(|content| match content {
            ContentItem::Image { data, .. } => Some(data),
            _ => None,
        })
        .unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let pixels = image::load_from_memory(&bytes).unwrap().to_rgba8();
    let red = pixels.get_pixel(16, 16);
    let green = pixels.get_pixel(80, 16);
    assert!(
        red[0] > 250 && red[1] < 3 && red[2] < 3 && red[3] == 255,
        "{red:?}"
    );
    assert!(
        green[0] < 3 && green[1] > 250 && green[2] < 3 && green[3] == 255,
        "{green:?}"
    );
}

#[tokio::test]
async fn transcript_filler_plan_moves_words_and_video_together() {
    use photonic_core::timeline::{CaptionCue, CaptionTrack, CaptionWord};
    let (state, sequence, track) = fixture().await;
    let mut captions = CaptionTrack::new("Dialogue");
    let caption = captions.id;
    captions.cues.push(CaptionCue::new(
        Tick::ZERO,
        Tick::from_seconds(2),
        vec![
            CaptionWord::new("um", Tick::ZERO, Tick::from_seconds(1)),
            CaptionWord::new("hello", Tick::from_seconds(1), Tick::from_seconds(2)),
        ],
    ));
    state
        .document
        .lock()
        .await
        .timeline
        .as_mut()
        .unwrap()
        .sequences
        .get_mut(&sequence)
        .unwrap()
        .caption_tracks
        .push(captions);
    let before = state.document.lock().await.timeline.clone();
    let transcript = call(
        &state,
        "get_transcript",
        json!({"sequence_id":sequence,"caption_track_id":caption,"limit":1}),
    )
    .await;
    success(&transcript);
    assert_eq!(data(&transcript)["revision"], 0);
    assert_eq!(data(&transcript)["next_offset"], 1);
    let found = call(
        &state,
        "find_filler_words",
        json!({"sequence_id":sequence,"caption_track_id":caption}),
    )
    .await;
    success(&found);
    assert_eq!(data(&found)["count"], 1);
    let plan = json!({"sequence_id":sequence,"expected_revision":0,"request_id":"remove-selected-filler","operations":[
        {"tool":"remove_filler_words","arguments":{"sequence_id":sequence,"caption_track_id":caption,"target_track_ids":[track],"matches":data(&found)["matches"],"ripple":true}}
    ]});
    let applied = call(&state, "apply_video_edit_plan", plan).await;
    success(&applied);
    assert_eq!(data(&applied)["undo_steps"], 1);
    let transcript = call(
        &state,
        "get_transcript",
        json!({"sequence_id":sequence,"caption_track_id":caption}),
    )
    .await;
    success(&transcript);
    assert_eq!(data(&transcript)["total"], 1);
    assert_eq!(data(&transcript)["tokens"][0]["text"], "hello");
    assert_eq!(data(&transcript)["tokens"][0]["start"], 0);
    let stale = call(
        &state,
        "get_transcript",
        json!({"sequence_id":sequence,"caption_track_id":caption,"offset":1,"expected_revision":0}),
    )
    .await;
    assert_eq!(data(&stale)["error_code"], "RevisionConflict");
    let snapshot = call(
        &state,
        "get_timeline_snapshot",
        json!({"sequence_id":sequence}),
    )
    .await;
    assert_eq!(data(&snapshot)["total_clips"], 1);
    assert_eq!(data(&snapshot)["clips"][0]["start_ticks"], 0);
    let mut document = state.document.lock().await;
    assert!(state.history.lock().await.undo(&mut document));
    assert_eq!(document.timeline, before);
}

#[tokio::test]
async fn precision_trim_and_preview_zones_share_one_atomic_undo() {
    let (state, sequence, track) = fixture().await;
    let before = state.document.lock().await.timeline.clone();
    let view = call(
        &state,
        "get_timeline_snapshot",
        json!({"sequence_id":sequence}),
    )
    .await;
    let clips = &data(&view)["clips"];
    let delta = FrameRate::FPS_30.ticks_per_frame().0;
    let plan = json!({"sequence_id":sequence,"expected_revision":0,"request_id":"trim-and-mark","operations":[
        {"tool":"precision_trim","arguments":{"sequence_id":sequence,"track_id":track,"outgoing_clip_id":clips[0]["clip_id"],"incoming_clip_id":clips[1]["clip_id"],"mode":"roll","delta_ticks":delta}},
        {"tool":"set_preview_zones","arguments":{"sequence_id":sequence,"zones":[{"start":1,"end":delta+1}]}}
    ]});
    let edited = call(&state, "apply_video_edit_plan", plan).await;
    success(&edited);
    assert_eq!(data(&edited)["undo_steps"], 1);
    let view = call(
        &state,
        "get_timeline_snapshot",
        json!({"sequence_id":sequence}),
    )
    .await;
    let second = photonic_core::timeline::TICKS_PER_SECOND;
    assert_eq!(data(&view)["clips"][0]["end_ticks"], second + delta);
    assert_eq!(data(&view)["clips"][1]["start_ticks"], second + delta);
    assert_eq!(data(&view)["clips"][1]["end_ticks"], 2 * second);
    assert_eq!(
        data(&view)["preview_zones"],
        json!([{"start":0,"end":2*delta}])
    );
    let mut document = state.document.lock().await;
    assert!(state.history.lock().await.undo(&mut document));
    assert_eq!(document.timeline, before);
}

#[tokio::test]
async fn batch_exports_keep_the_frozen_revision_while_editing_continues() {
    use std::time::{Duration, Instant};
    let (state, sequence, _) = fixture().await;
    let Some(tools) = photonic_video::media::ffmpeg_locate::locate_for_test() else {
        assert!(
            std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
            "FFmpeg required"
        );
        return;
    };
    let dir = std::env::temp_dir().join(format!("photonic-batch-outcome-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let outputs:Vec<_> = (0..2).map(|index|json!({"sequence_id":sequence,"out_path":dir.join(format!("output{index}.mp4")),"range":{"start_ticks":0,"end_ticks":FrameRate::FPS_30.frame_start(2).0}})).collect();
    let started = call(
        &state,
        "export_sequences",
        json!({"expected_revision":0,"outputs":outputs}),
    )
    .await;
    if data(&started)["error_code"] == "EngineUnavailable" {
        assert!(
            std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
            "GPU required"
        );
        return;
    }
    success(&started);
    assert_eq!(data(&started)["revision"], 0);
    let view = call(
        &state,
        "get_timeline_snapshot",
        json!({"sequence_id":sequence}),
    )
    .await;
    success(
        &call(
            &state,
            "move_clip",
            json!({"clip_id":data(&view)["clips"][0]["clip_id"],"new_start_seconds":3.0}),
        )
        .await,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = call(
            &state,
            "get_job_status",
            json!({"job_id":data(&started)["job_id"]}),
        )
        .await;
        success(&status);
        match data(&status)["status"]["state"].as_str().unwrap() {
            "done" => {
                assert_eq!(data(&status)["status"]["result"]["revision"], 0);
                break;
            }
            "failed" | "cancelled" => panic!("batch failed: {status:?}"),
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "batch did not finish: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(state.history.lock().await.revision(), 1);
    for index in 0..2 {
        let output = dir.join(format!("output{index}.mp4"));
        let probe = photonic_video::media::probe::probe_asset(&tools, &output).unwrap();
        let video = probe.video.unwrap();
        assert_eq!((video.width, video.height), (128, 64));
        let decoded = std::process::Command::new(&tools.ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(output)
            .args([
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let pixel = &decoded.stdout[(16 * 128 + 16) * 4..(16 * 128 + 16) * 4 + 4];
        assert!(
            pixel[0] > 240 && pixel[1] < 10 && pixel[2] < 10,
            "export must retain original red clip: {pixel:?}"
        );
    }
}
