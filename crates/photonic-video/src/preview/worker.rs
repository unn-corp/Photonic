use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{
    ChunkSignature, ChunkState, PreviewCache, PreviewCodec, PreviewError, PreviewStatusSnapshot,
};
use crate::export::job::{resolve_export_job, run_export_snapshot_validated};
use crate::export::presets::{
    Container, ExportPreset, FrameRatePolicy, QualityMode, ResolutionSpec, VideoCodec,
    VideoEncodeSpec,
};
use crate::export::render_loop::{ExportError, ExportEvent};
use crate::graph::eval::GpuContext;
use crate::media::ffmpeg_locate::FfmpegTools;
use crate::session::{ExportJob, PreviewQuality, RenderJobOptions, RenderSnapshot};
use photonic_core::timeline::SequenceId;

const QUEUE_CAPACITY: usize = 4;
const POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PreviewJobId(pub u64);

/// The engine plans a single chunk with its warmed compile providers, then
/// submits this immutable request. Whole marked ranges can be partitioned and
/// planned lazily, avoiding an unbounded queue of graphs or rendered frames.
#[derive(Clone)]
pub struct PreviewChunkRequest {
    pub snapshot: Arc<RenderSnapshot>,
    pub signature: ChunkSignature,
    pub dependencies: Vec<super::SourceDependency>,
}

struct ActiveRender {
    id: PreviewJobId,
    cancel: Arc<AtomicBool>,
    preempted: Arc<AtomicBool>,
}
#[derive(Default)]
struct PriorityState {
    playing: bool,
    active: Option<ActiveRender>,
}
/// One gate per interactive session, shared with its preview worker. Playback
/// start cancels the current chunk; the worker restarts that chunk after pause.
/// No preview render/decode resources remain active during the pause wait.
#[derive(Clone, Default)]
pub struct PlaybackPriorityGate {
    state: Arc<Mutex<PriorityState>>,
}
impl PlaybackPriorityGate {
    pub fn set_playing(&self, playing: bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.playing = playing;
        if playing {
            if let Some(active) = &state.active {
                active.preempted.store(true, Ordering::Relaxed);
                active.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
    pub fn is_playing(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .playing
    }
    fn begin(&self, id: PreviewJobId) -> Option<RenderPermit> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.playing || state.active.is_some() {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let preempted = Arc::new(AtomicBool::new(false));
        state.active = Some(ActiveRender {
            id,
            cancel: Arc::clone(&cancel),
            preempted: Arc::clone(&preempted),
        });
        Some(RenderPermit {
            gate: self.clone(),
            id,
            cancel,
            preempted,
        })
    }
    fn cancel(&self, id: PreviewJobId) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(active) = &state.active {
            if active.id == id {
                active.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}
struct RenderPermit {
    gate: PlaybackPriorityGate,
    id: PreviewJobId,
    cancel: Arc<AtomicBool>,
    preempted: Arc<AtomicBool>,
}
impl Drop for RenderPermit {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.id == self.id)
        {
            state.active = None;
        }
    }
}
struct JobControl {
    sequence: SequenceId,
    cancel: Arc<AtomicBool>,
}
struct QueuedChunk {
    id: PreviewJobId,
    request: PreviewChunkRequest,
    cancel: Arc<AtomicBool>,
}
struct WorkerShared {
    cache: Arc<PreviewCache>,
    gate: PlaybackPriorityGate,
    jobs: Mutex<HashMap<PreviewJobId, JobControl>>,
    stop: AtomicBool,
    next: AtomicU64,
}
struct WorkerOwner {
    shared: Arc<WorkerShared>,
    sender: crossbeam_channel::Sender<QueuedChunk>,
    join: Mutex<Option<JoinHandle<()>>>,
}
/// Cloning this handle shares the same bounded queue and single worker; it
/// never creates one thread or encoder per caller/export.
#[derive(Clone)]
pub struct PreviewWorker {
    inner: Arc<WorkerOwner>,
}
impl PreviewWorker {
    pub fn new(
        gpu: GpuContext,
        tools: FfmpegTools,
        cache: Arc<PreviewCache>,
        gate: PlaybackPriorityGate,
    ) -> Result<Self, PreviewError> {
        let (sender, receiver) = crossbeam_channel::bounded(QUEUE_CAPACITY);
        let shared = Arc::new(WorkerShared {
            cache,
            gate,
            jobs: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            next: AtomicU64::new(1),
        });
        let worker_shared = Arc::clone(&shared);
        let join = std::thread::Builder::new()
            .name("photonic-preview-render".into())
            .spawn(move || worker_loop(gpu, tools, worker_shared, receiver))?;
        Ok(Self {
            inner: Arc::new(WorkerOwner {
                shared,
                sender,
                join: Mutex::new(Some(join)),
            }),
        })
    }
    pub fn cache(&self) -> Arc<PreviewCache> {
        Arc::clone(&self.inner.shared.cache)
    }
    pub fn gate(&self) -> PlaybackPriorityGate {
        self.inner.shared.gate.clone()
    }
    pub fn status(&self) -> PreviewStatusSnapshot {
        self.inner.shared.cache.status()
    }
    pub fn pending(&self) -> usize {
        self.inner
            .shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
    pub fn submit(&self, request: PreviewChunkRequest) -> Result<PreviewJobId, PreviewError> {
        request.signature.validate()?;
        let shared = &self.inner.shared;
        if shared.stop.load(Ordering::Relaxed) {
            return Err(PreviewError::Stopped);
        }
        let id = PreviewJobId(shared.next.fetch_add(1, Ordering::Relaxed));
        let cancel = Arc::new(AtomicBool::new(false));
        let mut jobs = shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if jobs.len() >= QUEUE_CAPACITY + 1 {
            return Err(PreviewError::QueueFull);
        }
        jobs.insert(
            id,
            JobControl {
                sequence: request.signature.context.sequence,
                cancel: Arc::clone(&cancel),
            },
        );
        // Worker cannot observe a request before its cancellation control exists.
        let signature = request.signature.clone();
        shared
            .cache
            .set_status(&signature, ChunkState::Queued, 0, None);
        match self.inner.sender.try_send(QueuedChunk {
            id,
            request,
            cancel,
        }) {
            Ok(()) => Ok(id),
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                jobs.remove(&id);
                Err(PreviewError::QueueFull)
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                jobs.remove(&id);
                Err(PreviewError::Stopped)
            }
        }
    }
    pub fn cancel(&self, sequence: SequenceId) -> usize {
        let shared = &self.inner.shared;
        let jobs = shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let mut count = 0;
        for (&id, job) in jobs.iter().filter(|(_, job)| job.sequence == sequence) {
            job.cancel.store(true, Ordering::Relaxed);
            shared.gate.cancel(id);
            count += 1;
        }
        count
    }
    pub fn clear(
        &self,
        sequence: SequenceId,
        range: Option<(photonic_core::timeline::Tick, photonic_core::timeline::Tick)>,
    ) -> usize {
        self.cancel(sequence);
        self.inner.shared.cache.clear(sequence, range)
    }
    pub fn shutdown(&self) {
        stop_worker(&self.inner.shared);
        if let Some(join) = self
            .inner
            .join
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = join.join();
        }
    }
}
fn stop_worker(shared: &WorkerShared) {
    shared.stop.store(true, Ordering::Relaxed);
    let jobs = shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    for (&id, job) in jobs.iter() {
        job.cancel.store(true, Ordering::Relaxed);
        shared.gate.cancel(id);
    }
}
impl Drop for WorkerOwner {
    fn drop(&mut self) {
        stop_worker(&self.shared);
        if let Some(join) = self
            .join
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = join.join();
        }
    }
}
fn worker_loop(
    gpu: GpuContext,
    tools: FfmpegTools,
    shared: Arc<WorkerShared>,
    receiver: crossbeam_channel::Receiver<QueuedChunk>,
) {
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        let queued = match receiver.recv_timeout(POLL) {
            Ok(queued) => queued,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        let signature = &queued.request.signature;
        let result = render_chunk(&gpu, &tools, &shared, &queued);
        match result {
            Ok(()) => {}
            Err(PreviewError::Cancelled) => {
                shared
                    .cache
                    .set_status(signature, ChunkState::Cancelled, 0, None)
            }
            Err(error) => {
                shared
                    .cache
                    .set_status(signature, ChunkState::Failed, 0, Some(error.to_string()))
            }
        }
        shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&queued.id);
    }
    for queued in receiver.try_iter() {
        shared
            .cache
            .set_status(&queued.request.signature, ChunkState::Cancelled, 0, None);
        shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&queued.id);
    }
}
fn render_chunk(
    gpu: &GpuContext,
    tools: &FfmpegTools,
    shared: &WorkerShared,
    queued: &QueuedChunk,
) -> Result<(), PreviewError> {
    let request = &queued.request;
    let signature = &request.signature;
    loop {
        if queued.cancel.load(Ordering::Relaxed) || shared.stop.load(Ordering::Relaxed) {
            return Err(PreviewError::Cancelled);
        }
        if shared.cache.lookup(signature).is_some() {
            return Ok(());
        }
        let Some(permit) = shared.gate.begin(queued.id) else {
            shared
                .cache
                .set_status(signature, ChunkState::Paused, 0, None);
            std::thread::sleep(POLL);
            continue;
        };
        let staging = shared.cache.staging(signature.clone())?;
        let job = preview_export_job(
            request,
            staging.path.clone(),
            shared.cache.available_bytes(),
        )?;
        shared
            .cache
            .set_status(signature, ChunkState::Rendering, 0, None);
        let result = run_export_snapshot_validated(
            gpu.clone(),
            &request.snapshot,
            &job,
            tools,
            &permit.cancel,
            |index, frame, pixels| {
                if !request
                    .dependencies
                    .iter()
                    .all(super::SourceDependency::unchanged)
                {
                    return Err(ExportError::Resolve(
                        "preview source changed during rendering".into(),
                    ));
                }
                if signature.frame_hashes.get(index as usize) != Some(&frame.content_hash) {
                    return Err(ExportError::Resolve(
                        "preview render changed after its chunk was prepared".into(),
                    ));
                }
                if signature.context.profile.codec == PreviewCodec::IntraH264
                    && pixels.iter().any(|pixel| pixel[3] < 1.0 - 1e-4)
                {
                    return Err(ExportError::Resolve("H.264 previews cannot preserve transparent frames; select an alpha-capable preview profile".into()));
                }
                Ok(())
            },
            |event| {
                if let ExportEvent::Progress(progress) = event {
                    shared
                        .cache
                        .set_status(signature, ChunkState::Rendering, progress.frame, None);
                }
            },
        );
        if queued.cancel.load(Ordering::Relaxed) || shared.stop.load(Ordering::Relaxed) {
            return Err(PreviewError::Cancelled);
        }
        if permit.preempted.load(Ordering::Relaxed) {
            drop(staging);
            drop(permit);
            continue;
        }
        result?;
        let verified = verify_media(tools, &staging.path, signature, &permit.cancel);
        if permit.preempted.load(Ordering::Relaxed) {
            drop(staging);
            drop(permit);
            continue;
        }
        verified?;
        if !request
            .dependencies
            .iter()
            .all(super::SourceDependency::unchanged)
        {
            return Err(PreviewError::Invalid(
                "preview source changed before publication".into(),
            ));
        }
        let committed = shared.cache.commit(staging, &permit.cancel);
        if permit.preempted.load(Ordering::Relaxed)
            && matches!(committed, Err(PreviewError::Cancelled))
        {
            drop(permit);
            continue;
        }
        committed?;
        return Ok(());
    }
}

fn preview_export_job(
    request: &PreviewChunkRequest,
    output: std::path::PathBuf,
    limit: u64,
) -> Result<ExportJob, PreviewError> {
    let context = &request.signature.context;
    let project = request
        .snapshot
        .project
        .as_ref()
        .ok_or_else(|| PreviewError::Invalid("preview snapshot has no timeline".into()))?;
    let sequence = project
        .sequences
        .get(&context.sequence)
        .ok_or_else(|| PreviewError::Invalid("preview sequence not found".into()))?;
    if context.frame_rate != sequence.frame_rate {
        return Err(PreviewError::Invalid(
            "preview cadence differs from its snapshot".into(),
        ));
    }
    if context.quality == PreviewQuality::Full && context.use_proxy {
        return Err(PreviewError::Invalid(
            "Full preview rendering requires original media".into(),
        ));
    }
    let (container, codec, quality, alpha) = match context.profile.codec {
        PreviewCodec::IntraH264 => (
            Container::Mp4,
            VideoCodec::H264,
            QualityMode::Crf(context.profile.quality as f32),
            false,
        ),
        PreviewCodec::IntraProResLike => (
            Container::Mov,
            VideoCodec::ProResLikeMezzanine,
            QualityMode::Lossless,
            true,
        ),
        PreviewCodec::Lossless => (
            Container::WebM,
            VideoCodec::Vp9,
            QualityMode::Lossless,
            true,
        ),
    };
    if limit < 128 * 1024 {
        return Err(PreviewError::BudgetExhausted);
    }
    let size = context.profile.output_size(context.width, context.height);
    // Resolve the normal export at source format, then apply profile scale to
    // the encoded dimensions. Draft applies its own established canvas scale.
    let mut job = ExportJob {
        sequence: context.sequence,
        format_index: context.format_index,
        output,
        range: Some((request.signature.span.start, request.signature.span.end)),
        preset: ExportPreset {
            name: "Timeline preview".into(),
            container,
            video: Some(VideoEncodeSpec { codec, quality }),
            audio: None,
            resolution: ResolutionSpec::SourceFormat,
            frame_rate: FrameRatePolicy::MatchSequence,
            alpha,
            faststart: false,
            loudness_target: None,
            stems: false,
        },
        options: RenderJobOptions {
            use_proxies: context.use_proxy,
            preview_resolution: context.quality == PreviewQuality::Draft,
            raw_encoder_args: vec![
                "-g".into(),
                "1".into(),
                "-fs".into(),
                limit.saturating_sub(64 * 1024).to_string(),
            ],
            ..Default::default()
        },
    };
    let resolved = resolve_export_job(project, &job)?;
    if resolved.out_size != (context.width, context.height) {
        return Err(PreviewError::Invalid(
            "preview dimensions differ from the prepared render canvas".into(),
        ));
    }
    if size != resolved.out_size {
        // Export resolution is authored against full format, before its Draft
        // scaling, so derive the corresponding source-format dimensions.
        let format = &sequence.formats[context.format_index];
        job.preset.resolution = ResolutionSpec::Explicit {
            w: (format.width / 2).max(1),
            h: (format.height / 2).max(1),
        };
    }
    Ok(job)
}

fn verify_media(
    tools: &FfmpegTools,
    path: &std::path::Path,
    signature: &ChunkSignature,
    cancel: &AtomicBool,
) -> Result<(), PreviewError> {
    let mut command = Command::new(&tools.ffprobe);
    command
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,nb_read_frames,codec_name",
            "-of",
            "json",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::media::child_registry::arm_parent_death_signal(&mut command);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if cancel.load(Ordering::Relaxed) {
                PreviewError::Cancelled
            } else {
                PreviewError::Invalid("preview media verification timed out".into())
            });
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
        std::thread::sleep(POLL);
    }
    let output = child.wait_with_output()?;
    if !output.status.success() || output.stdout.len() > 16 * 1024 {
        return Err(PreviewError::Invalid("preview media probe failed".into()));
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let stream = &json["streams"][0];
    let size = signature
        .context
        .profile
        .output_size(signature.context.width, signature.context.height);
    let frames = stream["nb_read_frames"]
        .as_str()
        .and_then(|frames| frames.parse::<u64>().ok());
    let codec = match signature.context.profile.codec {
        PreviewCodec::IntraH264 => "h264",
        PreviewCodec::IntraProResLike => "prores",
        PreviewCodec::Lossless => "vp9",
    };
    if stream["width"].as_u64() != Some(size.0 as u64)
        || stream["height"].as_u64() != Some(size.1 as u64)
        || frames != Some(signature.span.frame_count)
        || stream["codec_name"].as_str() != Some(codec)
    {
        return Err(PreviewError::Invalid(
            "preview output is truncated or disagrees with its chunk manifest".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn playback_preempts_active_render_and_releases_before_restart() {
        let gate = PlaybackPriorityGate::default();
        let permit = gate.begin(PreviewJobId(1)).unwrap();
        assert!(gate.begin(PreviewJobId(2)).is_none());
        gate.set_playing(true);
        assert!(permit.cancel.load(Ordering::Relaxed));
        assert!(permit.preempted.load(Ordering::Relaxed));
        assert!(gate.begin(PreviewJobId(2)).is_none());
        drop(permit);
        assert!(gate.begin(PreviewJobId(2)).is_none());
        gate.set_playing(false);
        let restarted = gate.begin(PreviewJobId(1)).unwrap();
        assert!(!restarted.cancel.load(Ordering::Relaxed));
        gate.cancel(PreviewJobId(1));
        assert!(restarted.cancel.load(Ordering::Relaxed));
        assert!(!restarted.preempted.load(Ordering::Relaxed));
    }
}
