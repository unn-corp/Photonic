//! The single end-to-end export path (02 §7, 10 §6): resolve an
//! [`ExportJob`]'s abstract preset (`ResolutionSpec`/`FrameRatePolicy`) against
//! a frozen [`TimelineProject`] snapshot, drive a **dedicated** headless
//! [`EngineSession`] over that snapshot, and feed one full-quality readback
//! frame per output tick into [`render_loop::export_frames`].
//!
//! Both callers — the GUI (`EngineCmd::Export`, via
//! [`crate::session`]) and the MCP `export_sequence` tool — funnel through
//! [`run_export_job`], so there is exactly ONE render/encode path. SS-3
//! determinism requires this: export always evaluates at full quality with
//! [`ProxyMode::ForceOriginal`], and wall-clock never influences output — the
//! only clock in the loop is a per-frame 30 s deadline that *poisons* the run
//! (surfaces [`ExportError::RenderTimeout`]) rather than substituting a frame.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use photonic_core::history::CommandHistory;
use photonic_core::timeline::{FrameRate, SequenceId, Tick, TimelineProject};
use photonic_core::Document;
use photonic_render::color::Colorimetry;

use super::encoder::{validate_encode_options, AudioStreamSpec};
use super::offline_audio::{self, DEFAULT_EXPORT_SAMPLE_RATE};
use super::presets::{self, ExportPreset, FrameRatePolicy, ResolutionSpec};
use super::render_loop::{self, ExportError, ExportEvent, ExportTarget, Frame, ResolvedExport};
use crate::graph::compile::{fit_long_edge, DRAFT_MAX_LONG_EDGE};
use crate::graph::eval::{read_texture_rgba16f, GpuContext};
use crate::media::ffmpeg_locate::FfmpegTools;
use crate::session::{EngineCmd, EngineFrame, ExportJob, ProxyMode, RenderSnapshot, VideoEngine};

/// Best-effort system sleep inhibitor for export (K-F polish).
///
/// On Linux, spawns `systemd-inhibit --what=idle:sleep --who=Photonic
/// --why=export sleep infinity` and kills it on drop. Other platforms no-op.
struct SleepInhibit {
    child: Option<std::process::Child>,
}

impl SleepInhibit {
    fn acquire() -> Self {
        #[cfg(target_os = "linux")]
        {
            match std::process::Command::new("systemd-inhibit")
                .args([
                    "--what=idle:sleep",
                    "--who=Photonic",
                    "--why=export",
                    "sleep",
                    "infinity",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => return SleepInhibit { child: Some(child) },
                Err(e) => {
                    tracing::debug!(
                        target: "photonic_video::export",
                        "sleep inhibit unavailable: {e}"
                    );
                }
            }
        }
        SleepInhibit { child: None }
    }
}

impl Drop for SleepInhibit {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Everything an [`ExportJob`]'s abstract preset resolves to against a concrete
/// sequence (05 §3.2/§3.3): concrete output/format sizes, the output frame
/// rate, the exact work range, the frame count, and the (unmodified) preset —
/// audio is kept when the preset requests it (K-0.7).
#[derive(Clone, Debug)]
pub struct ResolvedExportJob {
    pub format_index: usize,
    /// The sequence-format size the engine evaluates at (`fw`, `fh`).
    pub format_size: (u32, u32),
    /// The encoded output size (`ow`, `oh`); `<= format_size` (no upscaling).
    pub out_size: (u32, u32),
    pub seq_rate: FrameRate,
    pub out_rate: FrameRate,
    pub start: Tick,
    pub end: Tick,
    pub total_frames: u64,
    /// The preset after resolution overrides. Audio is preserved (K-0.7).
    pub preset: ExportPreset,
    pub out_path: PathBuf,
}

/// Resolve an [`ExportJob`] against its `project` snapshot into concrete
/// numbers (02 §7 / 05 §3.2). Pure and cheap — callers use it up front to
/// surface validation errors synchronously (bad sequence/format, empty range,
/// upscaling refusal, invalid preset) and to report the frame count, then hand
/// the *same* job to [`run_export_job`], which re-resolves identically.
pub fn resolve_export_job(
    project: &TimelineProject,
    job: &ExportJob,
) -> Result<ResolvedExportJob, ExportError> {
    validate_encode_options(job.options.two_pass)?;
    let seq = project
        .sequences
        .get(&job.sequence)
        .ok_or_else(|| ExportError::Resolve(format!("sequence {} not found", job.sequence)))?;
    let seq_rate = seq.frame_rate;
    if seq.formats.is_empty() {
        return Err(ExportError::Resolve(format!(
            "sequence {} has no formats",
            job.sequence
        )));
    }
    let format_index = job.format_index.min(seq.formats.len().saturating_sub(1));
    let (fw, fh) = (
        seq.formats[format_index].width.max(1),
        seq.formats[format_index].height.max(1),
    );

    let content_end = seq
        .video_tracks
        .iter()
        .chain(seq.audio_tracks.iter())
        .flat_map(|t| t.clips.iter())
        .map(|c| c.end())
        .max()
        .unwrap_or(Tick(0));
    let (start, end) = job
        .range
        .unwrap_or_else(|| seq.work_range.unwrap_or((Tick(0), content_end)));
    if end <= start {
        return Err(ExportError::Resolve(
            "export range is empty — sequence has no content and no explicit range was given"
                .into(),
        ));
    }

    let out_rate = match job.preset.frame_rate {
        FrameRatePolicy::Explicit(fr) => fr,
        FrameRatePolicy::MatchSequence => seq_rate,
    };
    let (out_w, out_h) = match job.preset.resolution {
        ResolutionSpec::SourceFormat => (fw, fh),
        ResolutionSpec::Explicit { w, h } => (w.max(1), h.max(1)),
        ResolutionSpec::Scale(s) => (
            ((fw as f32 * s).round() as u32).max(1),
            ((fh as f32 * s).round() as u32).max(1),
        ),
    };
    if out_w > fw || out_h > fh {
        return Err(ExportError::Resolve(format!(
            "export upscaling ({out_w}x{out_h} from a {fw}x{fh} format) is not supported \
             in P3 — the engine evaluates at format size and downscales only"
        )));
    }
    let out_size = if job.options.preview_resolution {
        let (pw, ph) = fit_long_edge(fw, fh, DRAFT_MAX_LONG_EDGE);
        (
            ((out_w as u64 * pw as u64 / fw as u64) as u32).max(1),
            ((out_h as u64 * ph as u64 / fh as u64) as u32).max(1),
        )
    } else {
        (out_w, out_h)
    };

    // Keep the preset's audio spec (K-0.7): offline mix + mux lands in
    // `run_export_job`. Validation still rejects alpha-incompatible containers etc.
    let preset = job.preset.clone();
    presets::validate(&preset)
        .map_err(|e| ExportError::Resolve(format!("preset invalid after overrides: {e}")))?;

    let tpf_out = out_rate.ticks_per_frame().0.max(1);
    let total_frames = ((end.0 - start.0 + tpf_out - 1) / tpf_out).max(1) as u64;

    Ok(ResolvedExportJob {
        format_index,
        format_size: (fw, fh),
        out_size,
        seq_rate,
        out_rate,
        start,
        end,
        total_frames,
        preset,
        out_path: job.output.clone(),
    })
}

/// Run one export job to completion (02 §7, 10 §6). Opens a **dedicated**
/// [`EngineSession`] over a frozen clone of `project` (isolated from any
/// interactive session, so concurrent playback/render can't disturb the
/// seek-then-wait loop), forces full-quality [`ProxyMode::ForceOriginal`], and
/// feeds [`render_loop::export_frames`] one readback frame per output tick.
///
/// Each output tick is snapped to the nearest sequence-grid frame (05 §6.2),
/// seeked, and awaited for a fresh matching frame with a 30 s deadline; a miss
/// poisons the run (cancels between frames, returns
/// [`ExportError::RenderTimeout`]) rather than substituting content. Progress /
/// completion is reported via `on_event`; `cancel` stops the loop between
/// frames.
pub fn run_export_job(
    gpu: GpuContext,
    project: Arc<TimelineProject>,
    job: &ExportJob,
    tools: &FfmpegTools,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(ExportEvent),
) -> Result<(), ExportError> {
    run_export_jobs(
        gpu,
        project,
        std::slice::from_ref(job),
        tools,
        cancel,
        |_, event| on_event(event),
    )
}

/// Export several deliveries from one frozen project. Compatible deliveries
/// share render/readback; different formats, ranges, rates, dimensions, proxy
/// or preview choices get independent render sessions. At most four encoders
/// run together, so a large batch cannot exhaust process/worker resources.
/// Callbacks carry the original index in `jobs`; file publication is per output.
pub fn run_export_jobs(
    gpu: GpuContext,
    project: Arc<TimelineProject>,
    jobs: &[ExportJob],
    tools: &FfmpegTools,
    cancel: &AtomicBool,
    on_event: impl FnMut(usize, ExportEvent),
) -> Result<(), ExportError> {
    run_export_jobs_inner(
        gpu,
        project,
        None,
        jobs,
        tools,
        cancel,
        &mut |_, _, _| Ok(()),
        on_event,
    )
}

/// Export a complete immutable render snapshot, preserving embedded vectors
/// alongside timeline state. Preview cache producers use this same native
/// render path; no cached preview source is attached to the shadow session.
pub fn run_export_snapshot(
    gpu: GpuContext,
    snapshot: &RenderSnapshot,
    job: &ExportJob,
    tools: &FfmpegTools,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(ExportEvent),
) -> Result<(), ExportError> {
    run_export_snapshot_validated(
        gpu,
        snapshot,
        job,
        tools,
        cancel,
        |_, _, _| Ok(()),
        |event| on_event(event),
    )
}

/// Batch equivalent of [`run_export_snapshot`], sharing compatible rendered
/// frames while retaining the source document needed by embedded vectors.
pub fn run_export_jobs_snapshot(
    gpu: GpuContext,
    snapshot: &RenderSnapshot,
    jobs: &[ExportJob],
    tools: &FfmpegTools,
    cancel: &AtomicBool,
    on_event: impl FnMut(usize, ExportEvent),
) -> Result<(), ExportError> {
    let project = snapshot
        .project
        .as_ref()
        .ok_or_else(|| ExportError::Resolve("render snapshot has no timeline".into()))?;
    run_export_jobs_inner(
        gpu,
        Arc::clone(project),
        Some(snapshot),
        jobs,
        tools,
        cancel,
        &mut |_, _, _| Ok(()),
        on_event,
    )
}

/// As [`run_export_snapshot`], with a frame validation callback before encoding.
/// Preview producers use it to verify planned content hashes and codec fidelity
/// constraints against the actual evaluated frame; an error poisons the export.
pub fn run_export_snapshot_validated(
    gpu: GpuContext,
    snapshot: &RenderSnapshot,
    job: &ExportJob,
    tools: &FfmpegTools,
    cancel: &AtomicBool,
    mut validate: impl FnMut(u64, &EngineFrame, &[[f32; 4]]) -> Result<(), ExportError>,
    mut on_event: impl FnMut(ExportEvent),
) -> Result<(), ExportError> {
    let project = snapshot
        .project
        .as_ref()
        .ok_or_else(|| ExportError::Resolve("render snapshot has no timeline".into()))?;
    run_export_jobs_inner(
        gpu,
        Arc::clone(project),
        Some(snapshot),
        std::slice::from_ref(job),
        tools,
        cancel,
        &mut validate,
        |_, event| on_event(event),
    )
}

fn run_export_jobs_inner(
    gpu: GpuContext,
    project: Arc<TimelineProject>,
    snapshot: Option<&RenderSnapshot>,
    jobs: &[ExportJob],
    tools: &FfmpegTools,
    cancel: &AtomicBool,
    validate: &mut dyn FnMut(u64, &EngineFrame, &[[f32; 4]]) -> Result<(), ExportError>,
    mut on_event: impl FnMut(usize, ExportEvent),
) -> Result<(), ExportError> {
    let resolved: Vec<_> = jobs
        .iter()
        .map(|job| resolve_export_job(&project, job))
        .collect::<Result<_, _>>()?;
    for (index, job) in jobs.iter().enumerate() {
        if jobs[..index].iter().any(|other| other.output == job.output) {
            return Err(ExportError::Resolve(
                "batch exports must use distinct output paths".into(),
            ));
        }
    }
    let groups = compatible_groups(jobs, &resolved);
    for indices in groups {
        if cancel.load(Ordering::Relaxed) {
            for index in indices {
                on_event(index, ExportEvent::Cancelled);
            }
            continue;
        }
        run_export_group(
            gpu.clone(),
            Arc::clone(&project),
            snapshot,
            jobs,
            &resolved,
            &indices,
            tools,
            cancel,
            validate,
            |index, event| on_event(indices[index], event),
        )?;
    }
    Ok(())
}

fn compatible_groups(jobs: &[ExportJob], resolved: &[ResolvedExportJob]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, (job, r)) in jobs.iter().zip(resolved).enumerate() {
        let group = groups.iter_mut().find(|indices| {
            let first = indices[0];
            let other = &jobs[first];
            let q = &resolved[first];
            indices.len() < render_loop::MAX_SHARED_OUTPUTS
                && job.sequence == other.sequence
                && r.format_index == q.format_index
                && r.out_size == q.out_size
                && r.out_rate == q.out_rate
                && r.start == q.start
                && r.end == q.end
                && job.options.use_proxies == other.options.use_proxies
                && job.options.preview_resolution == other.options.preview_resolution
        });
        if let Some(group) = group {
            group.push(index);
        } else {
            groups.push(vec![index]);
        }
    }
    groups
}

#[allow(clippy::too_many_arguments)]
fn run_export_group(
    gpu: GpuContext,
    project: Arc<TimelineProject>,
    snapshot: Option<&RenderSnapshot>,
    jobs: &[ExportJob],
    resolutions: &[ResolvedExportJob],
    indices: &[usize],
    tools: &FfmpegTools,
    cancel: &AtomicBool,
    validate: &mut dyn FnMut(u64, &EngineFrame, &[[f32; 4]]) -> Result<(), ExportError>,
    mut on_event: impl FnMut(usize, ExportEvent),
) -> Result<(), ExportError> {
    let job = &jobs[indices[0]];
    let r = &resolutions[indices[0]];

    // Frozen shadow project with the export format forced active.
    let mut frozen = (*project).clone();
    if let Some(s) = frozen.sequences.get_mut(&job.sequence) {
        s.active_format = r.format_index;
    }

    let engine = VideoEngine::new(gpu.clone());
    let shadow_doc = Arc::new(Mutex::new(Document::new("export-shadow", 1.0, 1.0)));
    let history = CommandHistory::new(1);
    let expected_revision = snapshot.map_or(history.revision(), |snapshot| snapshot.revision);
    let shadow_history = Arc::new(Mutex::new(history));
    let session = engine.open_session(shadow_doc, shadow_history);
    let expected_generation = session.publish_snapshot(RenderSnapshot {
        revision: expected_revision,
        project: Some(Arc::new(frozen)),
        document: snapshot.and_then(|snapshot| snapshot.document.clone()),
    });
    let seq_id: SequenceId = job.sequence;
    session.send(EngineCmd::SetActiveSequence(seq_id));
    // K-F4: job options can request proxies / Draft resolution; default remains
    // full quality + original media (02 §7 SS-3 determinism path).
    let proxy = if job.options.use_proxies {
        ProxyMode::ForceProxy
    } else {
        ProxyMode::ForceOriginal
    };
    session.send(EngineCmd::SetProxyMode(proxy));
    let quality = if job.options.preview_resolution {
        crate::session::PreviewQuality::Draft
    } else {
        crate::session::PreviewQuality::Full
    };
    let expected_size = if job.options.preview_resolution {
        fit_long_edge(r.format_size.0, r.format_size.1, DRAFT_MAX_LONG_EDGE)
    } else {
        r.format_size
    };
    session.send(EngineCmd::SetPreviewQuality(quality));

    // Each output keeps its own audio/loudness semantics. Audio mixing is
    // still offline before frame production; this pipeline overlaps video only.
    let mut deliveries = Vec::with_capacity(indices.len());
    for &index in indices {
        if cancel.load(Ordering::Relaxed) {
            session.shutdown();
            for index in 0..indices.len() {
                on_event(index, ExportEvent::Cancelled);
            }
            return Ok(());
        }
        let job = &jobs[index];
        let r = &resolutions[index];
        let (audio, samples) = if r.preset.audio.is_some() {
            let sample_rate = if project.settings.audio_sample_rate > 0 {
                project.settings.audio_sample_rate
            } else {
                DEFAULT_EXPORT_SAMPLE_RATE
            };
            let pcm = offline_audio::render_export_audio(
                &project,
                job.sequence,
                r.start,
                r.end,
                Some(tools),
                r.preset.loudness_target.as_ref(),
            )?;
            (
                Some(AudioStreamSpec {
                    sample_rate,
                    channels: 2,
                }),
                Some(pcm),
            )
        } else {
            (None, None)
        };
        deliveries.push((
            ResolvedExport {
                width: r.out_size.0,
                height: r.out_size.1,
                frame_rate: r.out_rate,
                audio,
                out_path: r.out_path.clone(),
                colorimetry: Colorimetry::BT709_LIMITED,
                prefer_hardware: job.options.prefer_hardware,
                encoder_speed: job.options.encoder_speed.clone(),
                raw_encoder_args: job.options.raw_encoder_args.clone(),
                burn_in_timecode: job.options.burn_in_timecode,
                two_pass: job.options.two_pass,
            },
            samples,
        ));
    }
    let _sleep_guard = indices
        .iter()
        .any(|&index| jobs[index].options.inhibit_sleep)
        .then(SleepInhibit::acquire);
    let (ow, oh) = r.out_size;
    let start = r.start;
    let seq_rate = r.seq_rate;
    let tpf_out = r.out_rate.ticks_per_frame().0.max(1);

    let mut prev = session.latest_frame();
    let mut frame_fail: Option<ExportError> = None;
    let frame_source = |i: u64| -> Frame {
        // Output tick → nearest sequence frame (05 §6.2 retiming: the engine
        // presents exact sequence-grid ticks, so snap the output-grid tick).
        let t = Tick(start.0 + i as i64 * tpf_out);
        let snapped = seq_rate.frame_start(seq_rate.frame_at(t));
        session.send(EngineCmd::Seek(snapped));
        let deadline = Instant::now() + Duration::from_secs(30);
        let frame = loop {
            if cancel.load(Ordering::Relaxed) {
                break None;
            }
            if let Some(f) = session.latest_frame() {
                let fresh = prev.as_ref().map(|q| !Arc::ptr_eq(q, &f)).unwrap_or(true);
                if fresh
                    && f.time == snapped
                    && f.sequence == seq_id
                    && f.doc_revision == expected_revision
                    && f.snapshot_generation == expected_generation
                    && f.preview_quality == quality
                    && f.proxy_mode == proxy
                    && f.logical_size == expected_size
                    && f.preview_asset.is_none()
                {
                    break Some(f);
                }
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        match frame {
            Some(f) => {
                if f.cached_preview {
                    frame_fail = Some(ExportError::Resolve(
                        "native export refused a cached playback preview".into(),
                    ));
                    cancel.store(true, Ordering::Relaxed);
                    return Frame {
                        width: ow,
                        height: oh,
                        rgba_premult: Vec::new(),
                    };
                }
                prev = Some(Arc::clone(&f));
                // Logical region only (bucket-padded texture, see render loop).
                let (render_w, render_h) = f.logical_size;
                let px = read_texture_rgba16f(&gpu, &f.texture, render_w, render_h);
                if let Err(error) = validate(i, &f, &px) {
                    frame_fail = Some(error);
                    cancel.store(true, Ordering::Relaxed);
                    return Frame {
                        width: ow,
                        height: oh,
                        rgba_premult: Vec::new(),
                    };
                }
                let px = if (ow, oh) == (render_w, render_h) {
                    px
                } else {
                    box_downscale(&px, render_w, render_h, ow, oh)
                };
                Frame {
                    width: ow,
                    height: oh,
                    rgba_premult: px.into_flattened(),
                }
            }
            None => {
                // Poison the run: flag the failure, cancel between frames
                // (export_frames checks the flag before the next frame), and
                // hand back a black filler so the closure contract holds.
                if !cancel.load(Ordering::Relaxed) {
                    frame_fail = Some(ExportError::RenderTimeout(format!(
                        "frame {i} (tick {}) was not produced within 30s",
                        snapped.0
                    )));
                    cancel.store(true, Ordering::Relaxed);
                }
                Frame {
                    width: ow,
                    height: oh,
                    rgba_premult: Vec::new(),
                }
            }
        }
    };

    let targets = deliveries
        .iter_mut()
        .zip(indices)
        .map(|((resolved, samples), &index)| ExportTarget {
            preset: &resolutions[index].preset,
            resolved,
            audio_samples: samples.take(),
        })
        .collect();
    let mut completed = false;
    let result = render_loop::export_frames_multi(
        tools,
        targets,
        r.total_frames,
        frame_source,
        cancel,
        |index, event| {
            if matches!(event, ExportEvent::Done) {
                completed = true;
            }
            if !matches!(event, ExportEvent::Done) {
                on_event(index, event);
            }
        },
    );
    session.shutdown();

    match (result, frame_fail) {
        // A frame timeout poisoned the run — surface it even though
        // `export_frames` returned Ok(Cancelled) off the poisoned flag.
        (_, Some(error)) => Err(error),
        (Err(e), None) => Err(e),
        (Ok(()), None) => {
            // Stems are generated only after a successful video/audio encode,
            // never after a cancellation (which is also an Ok return).
            if completed && !cancel.load(Ordering::Relaxed) {
                for (output_index, &index) in indices.iter().enumerate() {
                    let r = &resolutions[index];
                    if r.preset.stems {
                        offline_audio::write_stems_for_export(
                            &project,
                            jobs[index].sequence,
                            r.start,
                            r.end,
                            &r.out_path,
                            Some(tools),
                            r.preset.loudness_target.as_ref(),
                        )?;
                    }
                    on_event(output_index, ExportEvent::Done);
                }
            }
            Ok(())
        }
    }
}

/// Area-average box downscale of a linear-premultiplied RGBA buffer from
/// `w`×`h` to `ow`×`oh` (`ow <= w`, `oh <= h`). The export path only ever
/// downscales (upscaling is refused in [`resolve_export_job`]).
fn box_downscale(src: &[[f32; 4]], w: u32, h: u32, ow: u32, oh: u32) -> Vec<[f32; 4]> {
    let mut out = Vec::with_capacity((ow * oh) as usize);
    for oy in 0..oh {
        let y0 = (oy as u64 * h as u64 / oh as u64) as u32;
        let y1 = (((oy as u64 + 1) * h as u64).div_ceil(oh as u64) as u32).clamp(y0 + 1, h);
        for ox in 0..ow {
            let x0 = (ox as u64 * w as u64 / ow as u64) as u32;
            let x1 = (((ox as u64 + 1) * w as u64).div_ceil(ow as u64) as u32).clamp(x0 + 1, w);
            let mut acc = [0f64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = src[(y * w + x) as usize];
                    for (a, c) in acc.iter_mut().zip(p.iter()) {
                        *a += *c as f64;
                    }
                }
            }
            let n = ((y1 - y0) as f64) * ((x1 - x0) as f64);
            out.push([
                (acc[0] / n) as f32,
                (acc[1] / n) as f32,
                (acc[2] / n) as f32,
                (acc[3] / n) as f32,
            ]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{Sequence, SequenceFormat};

    fn project_and_job() -> (TimelineProject, ExportJob) {
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("export", FrameRate::FPS_30, 1920, 1080);
        // Equal dimensions do not imply equal framing: format-index/reframe
        // identity must still partition shared output groups.
        sequence
            .formats
            .push(SequenceFormat::new("alternate crop", 1920, 1080));
        let id = sequence.id;
        project.insert_sequence(sequence);
        let job = ExportJob {
            sequence: id,
            format_index: 0,
            preset: presets::built_in_presets()
                .into_iter()
                .find(|preset| preset.name == "Web H.264")
                .unwrap(),
            output: PathBuf::from("delivery.mp4"),
            range: Some((Tick(0), Tick::from_seconds(1))),
            options: Default::default(),
        };
        (project, job)
    }

    #[test]
    fn resolve_rejects_two_pass_before_sequence_or_media_work() {
        let (_, mut job) = project_and_job();
        job.options.two_pass = true;
        assert!(matches!(
            resolve_export_job(&TimelineProject::new(), &job),
            Err(ExportError::Encode(
                super::super::encoder::EncodeError::TwoPassUnsupported
            ))
        ));
    }

    #[test]
    fn preview_export_resolves_to_the_same_draft_scale_as_rendering() {
        let (project, mut job) = project_and_job();
        job.options.preview_resolution = true;
        assert_eq!(
            resolve_export_job(&project, &job).unwrap().out_size,
            (960, 540)
        );
        job.preset.resolution = ResolutionSpec::Scale(0.5);
        assert_eq!(
            resolve_export_job(&project, &job).unwrap().out_size,
            (480, 270)
        );
    }

    #[test]
    fn shared_groups_preserve_format_range_cadence_size_and_quality_semantics() {
        let (project, job) = project_and_job();
        let mut jobs = vec![job; 8];
        jobs[1].preset.video.as_mut().unwrap().quality = presets::QualityMode::Crf(30.0);
        jobs[2].format_index = 1;
        jobs[3].range = Some((Tick::from_seconds(1), Tick::from_seconds(2)));
        jobs[4].preset.frame_rate = FrameRatePolicy::Explicit(FrameRate::new(24, 1));
        jobs[5].preset.resolution = ResolutionSpec::Scale(0.5);
        jobs[6].options.use_proxies = true;
        jobs[7].options.preview_resolution = true;
        let resolved: Vec<_> = jobs
            .iter()
            .map(|job| resolve_export_job(&project, job).unwrap())
            .collect();
        assert_eq!(
            compatible_groups(&jobs, &resolved),
            vec![
                vec![0, 1],
                vec![2],
                vec![3],
                vec![4],
                vec![5],
                vec![6],
                vec![7]
            ]
        );
    }

    #[test]
    fn shared_groups_limit_live_encoder_children() {
        let (project, job) = project_and_job();
        let jobs = vec![job; 9];
        let resolved: Vec<_> = jobs
            .iter()
            .map(|job| resolve_export_job(&project, job).unwrap())
            .collect();
        assert_eq!(
            compatible_groups(&jobs, &resolved),
            vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7], vec![8]]
        );
    }
}
