//! Shared render/export job queue (K-F1 / 26 §14).
//!
//! Captures a document snapshot at submission and runs exports serially on a
//! background worker so the GUI can keep editing while jobs complete. Both the
//! interactive app and MCP can drive the same queue; MCP's legacy
//! `JobRegistry` remains for caption/tts jobs that are not export-shaped.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use photonic_core::timeline::{SequenceId, Tick, TimelineProject};

use super::job::{resolve_export_job, run_export_job};
use super::presets::ExportPreset;
use super::render_loop::{ExportError, ExportEvent, ExportProgress};
use crate::graph::eval::GpuContext;
use crate::media::ffmpeg_locate::FfmpegTools;
use crate::session::ExportJob;

/// Opaque job handle returned at enqueue time.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct QueueJobId(pub u64);

/// Lifecycle of one queue entry (K-F1).
#[derive(Clone, Debug, PartialEq)]
pub enum QueueJobStatus {
    Queued,
    Running { frame: u64, total: u64, fps: f32 },
    Done { out_path: PathBuf },
    Failed { message: String },
    Cancelled,
}

/// One frozen export request on the queue.
#[derive(Clone, Debug)]
pub struct QueuedExport {
    pub id: QueueJobId,
    pub label: String,
    pub project: Arc<TimelineProject>,
    pub job: ExportJob,
    pub submitted_at: Instant,
    pub status: QueueJobStatus,
    pub cancel: Arc<AtomicBool>,
}

/// Thread-safe multi-job export queue. Snapshots are frozen at enqueue; a
/// single worker drains the queue FIFO.
pub struct RenderQueue {
    inner: Arc<Mutex<RenderQueueInner>>,
    next_id: AtomicU64,
    /// Wakes the worker when a job is enqueued.
    wake: Arc<(Mutex<()>, std::sync::Condvar)>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct RenderQueueInner {
    pending: VecDeque<QueuedExport>,
    /// Recently finished jobs (retention for UI/MCP polling).
    finished: Vec<QueuedExport>,
    stop: bool,
}

impl RenderQueue {
    pub fn new() -> Self {
        RenderQueue {
            inner: Arc::new(Mutex::new(RenderQueueInner {
                pending: VecDeque::new(),
                finished: Vec::new(),
                stop: false,
            })),
            next_id: AtomicU64::new(1),
            wake: Arc::new((Mutex::new(()), std::sync::Condvar::new())),
            worker: Mutex::new(None),
        }
    }

    /// Ensure the background worker is running. Idempotent.
    pub fn ensure_worker(&self, gpu: GpuContext, tools: FfmpegTools) {
        let mut slot = self.worker.lock().expect("render queue worker lock");
        if slot.is_some() {
            return;
        }
        let inner = Arc::clone(&self.inner);
        let wake = Arc::clone(&self.wake);
        let join = std::thread::Builder::new()
            .name("photonic-render-queue".into())
            .spawn(move || worker_main(inner, wake, gpu, tools))
            .expect("spawn render queue worker");
        *slot = Some(join);
    }

    /// Enqueue an export of `project` with `job`. Returns a stable id.
    /// The project is cloned into an `Arc` so later edits do not affect the job.
    pub fn enqueue(
        &self,
        label: impl Into<String>,
        project: TimelineProject,
        job: ExportJob,
    ) -> Result<QueueJobId, ExportError> {
        // Validate up front so the caller gets a synchronous error.
        resolve_export_job(&project, &job)?;
        let id = QueueJobId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let entry = QueuedExport {
            id,
            label: label.into(),
            project: Arc::new(project),
            job,
            submitted_at: Instant::now(),
            status: QueueJobStatus::Queued,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        {
            let mut g = self.inner.lock().expect("render queue lock");
            if g.stop {
                return Err(ExportError::Resolve(
                    "render queue has been shut down".into(),
                ));
            }
            g.pending.push_back(entry);
        }
        self.wake.1.notify_one();
        Ok(id)
    }

    /// Convenience: enqueue a marker-zone / range multi-export (K-F2 foundation)
    /// — one job per `(label, range)` pair sharing the same preset and format.
    pub fn enqueue_multi(
        &self,
        project: &TimelineProject,
        sequence: SequenceId,
        format_index: usize,
        preset: ExportPreset,
        segments: impl IntoIterator<Item = (String, PathBuf, Option<(Tick, Tick)>)>,
    ) -> Result<Vec<QueueJobId>, ExportError> {
        let mut ids = Vec::new();
        for (label, output, range) in segments {
            let job = ExportJob {
                sequence,
                format_index,
                preset: preset.clone(),
                output,
                range,
                options: Default::default(),
            };
            ids.push(self.enqueue(label, project.clone(), job)?);
        }
        Ok(ids)
    }

    pub fn cancel(&self, id: QueueJobId) -> bool {
        let mut g = self.inner.lock().expect("render queue lock");
        // Running jobs live in `finished` with Running status — poison cancel.
        if let Some(j) = g.finished.iter_mut().find(|j| j.id == id) {
            if !matches!(j.status, QueueJobStatus::Running { .. }) {
                return false;
            }
            j.cancel.store(true, Ordering::Relaxed);
            return true;
        }
        // Still-queued: remove and mark cancelled.
        if let Some(pos) = g.pending.iter().position(|j| j.id == id) {
            let mut j = g.pending.remove(pos).expect("position just found");
            j.cancel.store(true, Ordering::Relaxed);
            j.status = QueueJobStatus::Cancelled;
            g.finished.push(j);
            return true;
        }
        false
    }

    pub fn status(&self, id: QueueJobId) -> Option<QueueJobStatus> {
        let g = self.inner.lock().expect("render queue lock");
        g.pending
            .iter()
            .chain(g.finished.iter())
            .find(|j| j.id == id)
            .map(|j| j.status.clone())
    }

    pub fn list(&self) -> Vec<QueuedExport> {
        let g = self.inner.lock().expect("render queue lock");
        g.pending.iter().chain(g.finished.iter()).cloned().collect()
    }

    /// Stop the worker (best-effort). Used in tests / shutdown.
    pub fn shutdown(&self) {
        {
            let mut g = self.inner.lock().expect("render queue lock");
            g.stop = true;
            for j in &g.finished {
                if matches!(j.status, QueueJobStatus::Running { .. }) {
                    j.cancel.store(true, Ordering::Relaxed);
                }
            }
            while let Some(mut j) = g.pending.pop_front() {
                j.cancel.store(true, Ordering::Relaxed);
                j.status = QueueJobStatus::Cancelled;
                g.finished.push(j);
            }
        }
        self.wake.1.notify_one();
        if let Some(join) = self.worker.lock().expect("worker lock").take() {
            let _ = join.join();
        }
    }
}

impl Default for RenderQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RenderQueue {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_main(
    inner: Arc<Mutex<RenderQueueInner>>,
    wake: Arc<(Mutex<()>, std::sync::Condvar)>,
    gpu: GpuContext,
    tools: FfmpegTools,
) {
    loop {
        let job = {
            let mut g = inner.lock().expect("render queue lock");
            loop {
                if g.stop {
                    return;
                }
                if let Some(j) = g.pending.pop_front() {
                    // Publish running under the same lock as removal, so
                    // shutdown/cancel cannot miss a job between queues.
                    let mut running = j.clone();
                    running.status = QueueJobStatus::Running {
                        frame: 0,
                        total: 0,
                        fps: 0.0,
                    };
                    g.finished.push(running);
                    break j;
                }
                // Release and wait.
                drop(g);
                let (lock, cv) = &*wake;
                let guard = lock.lock().expect("wake lock");
                let _ = cv.wait_timeout(guard, std::time::Duration::from_millis(500));
                g = inner.lock().expect("render queue lock");
            }
        };

        if job.cancel.load(Ordering::Relaxed) || matches!(job.status, QueueJobStatus::Cancelled) {
            let mut g = inner.lock().expect("render queue lock");
            if let Some(done) = g.finished.iter_mut().find(|entry| entry.id == job.id) {
                done.status = QueueJobStatus::Cancelled;
            }
            continue;
        }

        let cancel = Arc::clone(&job.cancel);
        let id = job.id;
        let out_path = job.job.output.clone();
        let project = Arc::clone(&job.project);
        let export_job = job.job.clone();

        let progress_id = id;
        let progress_inner = Arc::clone(&inner);
        let result = run_export_job(
            gpu.clone(),
            project,
            &export_job,
            &tools,
            &cancel,
            move |ev| {
                if let ExportEvent::Progress(ExportProgress {
                    frame, total, fps, ..
                }) = ev
                {
                    if let Ok(mut g) = progress_inner.lock() {
                        if let Some(j) = g.finished.iter_mut().find(|j| j.id == progress_id) {
                            j.status = QueueJobStatus::Running { frame, total, fps };
                        }
                    }
                }
            },
        );

        let mut g = inner.lock().expect("render queue lock");
        if let Some(j) = g.finished.iter_mut().find(|j| j.id == id) {
            j.status = match result {
                Ok(()) if cancel.load(Ordering::Relaxed) => QueueJobStatus::Cancelled,
                Ok(()) => QueueJobStatus::Done {
                    out_path: out_path.clone(),
                },
                Err(ExportError::RenderTimeout(m)) => QueueJobStatus::Failed { message: m },
                Err(e) => QueueJobStatus::Failed {
                    message: e.to_string(),
                },
            };
        }
        // Cap finished retention.
        const MAX_FINISHED: usize = 64;
        if g.finished.len() > MAX_FINISHED {
            let drop_n = g.finished.len() - MAX_FINISHED;
            g.finished.drain(0..drop_n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::presets::{
        Container, FrameRatePolicy, QualityMode, ResolutionSpec, VideoCodec, VideoEncodeSpec,
    };
    use photonic_core::timeline::{FrameRate, Sequence, Track, TrackKind};

    fn video_only_preset() -> ExportPreset {
        ExportPreset {
            name: "test".into(),
            container: Container::Mp4,
            video: Some(VideoEncodeSpec {
                codec: VideoCodec::H264,
                quality: QualityMode::Crf(23.0),
            }),
            audio: None,
            resolution: ResolutionSpec::SourceFormat,
            frame_rate: FrameRatePolicy::MatchSequence,
            alpha: false,
            faststart: true,
            loudness_target: None,
            stems: false,
        }
    }

    fn tiny_project() -> TimelineProject {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("s", FrameRate::FPS_30, 64, 64);
        let mut t = Track::new(TrackKind::Video, "V1");
        t.clips.push(photonic_core::timeline::Clip::new(
            photonic_core::timeline::ClipSource::SolidColor {
                color: photonic_core::Color {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
            Tick(0),
            Tick::from_seconds(1),
        ));
        seq.video_tracks.push(t);
        project.insert_sequence(seq);
        project
    }

    #[test]
    fn enqueue_assigns_monotonic_ids_and_lists() {
        let q = RenderQueue::new();
        let project = tiny_project();
        let seq = *project.sequences.keys().next().unwrap();
        let job = ExportJob {
            sequence: seq,
            format_index: 0,
            preset: video_only_preset(),
            output: PathBuf::from("/tmp/photonic-queue-test.mp4"),
            range: Some((Tick(0), Tick::from_seconds(1))),
            options: Default::default(),
        };
        let a = q.enqueue("a", project.clone(), job.clone()).unwrap();
        let b = q.enqueue("b", project, job).unwrap();
        assert_ne!(a, b);
        let list = q.list();
        assert_eq!(list.len(), 2);
        assert!(matches!(list[0].status, QueueJobStatus::Queued));
        assert!(q.cancel(a));
        assert!(matches!(q.status(a), Some(QueueJobStatus::Cancelled)));
        q.shutdown();
    }

    #[test]
    fn enqueue_multi_creates_one_job_per_segment() {
        let q = RenderQueue::new();
        let project = tiny_project();
        let seq = *project.sequences.keys().next().unwrap();
        let ids = q
            .enqueue_multi(
                &project,
                seq,
                0,
                video_only_preset(),
                [
                    (
                        "seg-a".into(),
                        PathBuf::from("/tmp/a.mp4"),
                        Some((Tick(0), Tick::from_seconds(1))),
                    ),
                    (
                        "seg-b".into(),
                        PathBuf::from("/tmp/b.mp4"),
                        Some((Tick(0), Tick::from_seconds(1))),
                    ),
                ],
            )
            .unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(q.list().len(), 2);
        q.shutdown();
    }

    #[test]
    fn shutdown_cancels_running_and_pending_jobs_and_refuses_new_work() {
        let queue = RenderQueue::new();
        let project = tiny_project();
        let sequence = *project.sequences.keys().next().unwrap();
        let job = ExportJob {
            sequence,
            format_index: 0,
            preset: video_only_preset(),
            output: PathBuf::from("shutdown.mp4"),
            range: None,
            options: Default::default(),
        };
        let running = queue
            .enqueue("running", project.clone(), job.clone())
            .unwrap();
        let pending = queue
            .enqueue("pending", project.clone(), job.clone())
            .unwrap();
        let running_cancel = {
            let mut inner = queue.inner.lock().unwrap();
            let mut entry = inner.pending.pop_front().unwrap();
            entry.status = QueueJobStatus::Running {
                frame: 0,
                total: 30,
                fps: 0.0,
            };
            let cancel = Arc::clone(&entry.cancel);
            inner.finished.push(entry);
            cancel
        };
        queue.shutdown();
        assert!(running_cancel.load(Ordering::Relaxed));
        assert_eq!(queue.status(pending), Some(QueueJobStatus::Cancelled));
        assert!(!queue.cancel(pending));
        assert!(queue.status(running).is_some());
        assert!(queue.enqueue("late", project, job).is_err());
    }
}
