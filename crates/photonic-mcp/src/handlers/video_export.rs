//! Snapshot export adapters. All outputs are resolved before job admission;
//! compatible outputs share evaluated frames in the video export engine.
use std::sync::{atomic::Ordering, Arc};

use photonic_video::{
    export::{
        job::{resolve_export_job, ResolvedExportJob},
        presets,
        render_loop::{ExportError, ExportEvent},
    },
    ExportJob, RenderSnapshot,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    video::{engine_bridge, find_export_preset, resolve_tick},
    video_jobs::{set_job_status, JobStatus, MAX_ACTIVE_JOBS},
};
use crate::{
    protocol::{ExportSequenceArgs, ToolResult},
    server::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportSequencesArgs {
    pub expected_revision: u64,
    pub outputs: Vec<ExportSequenceArgs>,
}

fn prepare(
    state: &AppState,
    snapshot: &RenderSnapshot,
    args: ExportSequenceArgs,
) -> Result<(ExportJob, ResolvedExportJob), ToolResult> {
    if args
        .expected_revision
        .is_some_and(|expected| expected != snapshot.revision)
    {
        return Err(ToolResult::error_with_code(
            "RevisionConflict",
            "Document changed before export",
        )
        .with_data(
            json!({"actual_revision":snapshot.revision,"expected_revision":args.expected_revision}),
        ));
    }
    let output =
        crate::path_guard::check_path(state, &args.out_path, photonic_core::PathAccess::Write)?;
    let project = snapshot
        .project
        .as_ref()
        .ok_or_else(|| ToolResult::error_with_code("NoProject", "No timeline project"))?;
    let sequence = project.sequences.get(&args.sequence_id).ok_or_else(|| {
        ToolResult::error_with_code("NoSequence", "Requested sequence does not exist")
    })?;
    let format_index = args.format_index.unwrap_or(sequence.active_format);
    if format_index >= sequence.formats.len() {
        return Err(ToolResult::error_with_code(
            "InvalidArguments",
            "format_index is out of range",
        ));
    }
    let range = args
        .range
        .map(|range| {
            Ok::<_, ToolResult>((
                resolve_tick(
                    range.start_ticks,
                    range.start_tc.as_deref(),
                    range.start_seconds,
                    Some(sequence.frame_rate),
                )?,
                resolve_tick(
                    range.end_ticks,
                    range.end_tc.as_deref(),
                    range.end_seconds,
                    Some(sequence.frame_rate),
                )?,
            ))
        })
        .transpose()?;
    let name = args.preset.as_deref().unwrap_or("Web H.264");
    let mut preset = find_export_preset(name).ok_or_else(|| {
        ToolResult::error_with_code(
            "UnknownPreset",
            format!("No export preset named {name:?}; use list_export_presets"),
        )
    })?;
    if let Some(overrides) = args.overrides {
        match (overrides.width, overrides.height) {
            (Some(w), Some(h)) => preset.resolution = presets::ResolutionSpec::Explicit { w, h },
            (None, None) => {}
            _ => {
                return Err(ToolResult::error_with_code(
                    "InvalidArguments",
                    "overrides.width and height must be given together",
                ))
            }
        }
        if let Some(rate) = overrides.frame_rate {
            preset.frame_rate = presets::FrameRatePolicy::Explicit(rate);
        }
    }
    let job = ExportJob {
        sequence: args.sequence_id,
        format_index,
        preset,
        output,
        range,
        options: Default::default(),
    };
    let resolved = resolve_export_job(project, &job)
        .map_err(|error| ToolResult::error_with_code("ExportResolveFailed", error.to_string()))?;
    Ok((job, resolved))
}

async fn snapshot(state: &AppState, expected: Option<u64>) -> Result<RenderSnapshot, ToolResult> {
    let document = state.document.lock().await;
    let revision = state.history.lock().await.revision();
    if expected.is_some_and(|expected| expected != revision) {
        return Err(ToolResult::error_with_code(
            "RevisionConflict",
            "Document changed before export",
        )
        .with_data(json!({"actual_revision":revision,"expected_revision":expected})));
    }
    Ok(RenderSnapshot::from_document(&document, revision))
}

pub async fn export_sequence(state: &AppState, args: ExportSequenceArgs) -> ToolResult {
    let snapshot = match snapshot(state, args.expected_revision).await {
        Ok(snapshot) => snapshot,
        Err(error) => return error,
    };
    let prepared = match prepare(state, &snapshot, args) {
        Ok(prepared) => prepared,
        Err(error) => return error,
    };
    start(state, snapshot, vec![prepared], false)
}

pub async fn export_sequences(state: &AppState, args: ExportSequencesArgs) -> ToolResult {
    if args.outputs.is_empty() || args.outputs.len() > 8 {
        return ToolResult::error_with_code("InvalidArguments", "Export 1–8 outputs per job");
    }
    let snapshot = match snapshot(state, Some(args.expected_revision)).await {
        Ok(snapshot) => snapshot,
        Err(error) => return error,
    };
    let mut prepared = Vec::with_capacity(args.outputs.len());
    for (index, output) in args.outputs.into_iter().enumerate() {
        let pair = match prepare(state, &snapshot, output) {
            Ok(pair) => pair,
            Err(error) => return error.with_data(json!({"output_index":index})),
        };
        if prepared
            .iter()
            .any(|(job, _): &(ExportJob, ResolvedExportJob)| job.output == pair.0.output)
        {
            return ToolResult::error_with_code(
                "DuplicateOutputPath",
                "Each batch output must have a distinct destination",
            )
            .with_data(json!({"output_index":index}));
        }
        prepared.push(pair);
    }
    start(state, snapshot, prepared, true)
}

fn start(
    state: &AppState,
    snapshot: RenderSnapshot,
    prepared: Vec<(ExportJob, ResolvedExportJob)>,
    batch: bool,
) -> ToolResult {
    let tools = match photonic_video::media::ffmpeg_locate::locate() {
        Ok(tools) => tools,
        Err(error) => return ToolResult::error_with_code("FfmpegUnavailable", error.to_string()),
    };
    let bridge = match engine_bridge(state) {
        Ok(bridge) => bridge,
        Err(error) => return error,
    };
    let outputs: Vec<Value> = prepared.iter().map(|(job,resolved)|json!({
        "sequence_id":job.sequence,"output_path":job.output,"total_frames":resolved.total_frames,
        "width":resolved.out_size.0,"height":resolved.out_size.1,"format_index":job.format_index,"preset":job.preset.name,
        "audio":if job.preset.audio.is_some() { "muxed — offline sequence mix (K-0.7)" } else { "none in preset" },
    })).collect();
    let (id, cancel) = match state
        .video_jobs
        .lock()
        .expect("job registry poisoned")
        .start(if batch {
            "export_sequences"
        } else {
            "export_sequence"
        }) {
        Ok(job) => job,
        Err(error) => {
            return ToolResult::error_with_code("JobCapacityExceeded", error.to_string())
                .with_data(json!({"retryable":true,"max_active_jobs":MAX_ACTIVE_JOBS}))
        }
    };
    let jobs = Arc::clone(&state.video_jobs);
    let gpu = bridge.engine().gpu().clone();
    let revision = snapshot.revision;
    let response = if batch {
        json!({"job_id":id,"revision":revision,"outputs":outputs})
    } else {
        let mut response = outputs[0].clone();
        response["job_id"] = json!(id);
        response["revision"] = json!(revision);
        response
    };
    std::thread::spawn(move || {
        set_job_status(
            &jobs,
            id,
            JobStatus::Running {
                progress: 0.0,
                message: "starting frozen export snapshot".into(),
            },
        );
        let export_jobs: Vec<_> = prepared.into_iter().map(|(job, _)| job).collect();
        let mut progress = vec![0.0; export_jobs.len()];
        let result = photonic_video::export::job::run_export_jobs_snapshot(
            gpu,
            &snapshot,
            &export_jobs,
            &tools,
            &cancel,
            |index, event| {
                if let ExportEvent::Progress(frame) = event {
                    progress[index] = if frame.total > 0 {
                        frame.frame as f32 / frame.total as f32
                    } else {
                        0.0
                    };
                    set_job_status(
                        &jobs,
                        id,
                        JobStatus::Running {
                            progress: progress.iter().sum::<f32>() / progress.len() as f32,
                            message: format!(
                                "output {}/{}: {}/{} frames ({:.1} fps)",
                                index + 1,
                                progress.len(),
                                frame.frame,
                                frame.total,
                                frame.fps
                            ),
                        },
                    );
                }
            },
        );
        let status = if cancel.load(Ordering::Relaxed) {
            JobStatus::Cancelled
        } else {
            match result {
                Ok(()) => JobStatus::Done {
                    result: if batch {
                        json!({"revision":revision,"outputs":outputs})
                    } else {
                        let mut output = outputs[0].clone();
                        output["revision"] = json!(revision);
                        output
                    },
                },
                Err(ExportError::RenderTimeout(message)) => JobStatus::Failed {
                    error_code: "RenderTimeout".into(),
                    message,
                },
                Err(error) => JobStatus::Failed {
                    error_code: "ExportFailed".into(),
                    message: error.to_string(),
                },
            }
        };
        set_job_status(&jobs, id, status);
    });
    ToolResult::text("Export job started; poll get_job_status").with_data(response)
}

pub fn tool_schema(available: &[Value]) -> Value {
    let output = available
        .iter()
        .find(|tool| tool["name"] == "export_sequence")
        .expect("single export schema")["inputSchema"]
        .clone();
    crate::schema_gen::tool_definition("export_sequences",
        "Export 1–8 outputs from one revision-pinned snapshot. Resolve every destination/preset before job admission; compatible outputs share evaluated frames in groups of at most four. Poll get_job_status or cancel_job. Each destination publishes atomically; a completed output remains if a later output fails or is cancelled. Full-quality originals and embedded vector content are preserved.",
        json!({"type":"object","properties":{"expected_revision":{"type":"integer","minimum":0},"outputs":{"type":"array","minItems":1,"maxItems":8,"items":output}},"required":["expected_revision","outputs"],"additionalProperties":false}),
        json!({"type":"object","properties":{"job_id":{"type":"string"},"revision":{"type":"integer"},"outputs":{"type":"array","items":{"type":"object"}}},"required":["job_id","revision","outputs"]}),
        crate::schema_gen::ToolBehavior::ExternalMutation)
}
