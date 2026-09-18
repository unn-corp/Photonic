//! Video-engine runtime + async-job infrastructure for the MCP surface
//! (10-mcp-tools.md §2 AppState extension, §6 job pattern).
//!
//! ## Engine bridge (§2, adapted)
//!
//! 10 §2 sketches `VideoEngine` construction in `main.rs` with the winit
//! `GpuContext` shared in GUI mode. That wiring is the app-integration
//! story's seam; this module keeps the MCP crate self-contained instead:
//! the engine is created **lazily on the first engine-backed tool call**
//! via [`photonic_video::VideoEngine::headless`] (own adapter, both run
//! modes). `None` from the adapter request = no GPU — every engine-backed
//! tool then degrades to a structured `EngineUnavailable` error (10 §7's
//! "fails with a clear error rather than blocking the rest of the surface").
//!
//! ## Revisioned snapshot bridge
//!
//! The MCP document/history use Tokio locks; the engine consumes immutable
//! `RenderSnapshot` publications. The bridge checks the real history revision
//! under document → history lock order and clones content only on a change.
//! Unchanged calls reuse the same Arc. Temporary format views receive their
//! own publication generation while retaining the real document revision.
//! A constructor-only std-mutex document pair preserves the legacy session API;
//! published snapshots take precedence and do not copy or reset undo history.
//!
//! Transport callers serialize select/seek/inspect operations and wait for the
//! published generation before relying on sequence state. GUI and MCP sessions
//! remain independent. Embedded vector content is included when required.
//!
//! ## Job registry
//!
//! At most four jobs are active. Completed jobs remain inspectable for up to
//! `JOB_RETENTION`, subject to a bounded retained-entry limit. Cancellation
//! flags a running worker; its terminal status releases the admission slot.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use photonic_core::history::CommandHistory;
use photonic_core::Document;
use photonic_video::{
    EngineCmd, EngineFrame, EngineSession, ProxyMode, RenderSnapshot, VideoEngine,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::server::AppState;

// ─── Engine bridge ───────────────────────────────────────────────────────────

/// Lazily-initialized engine slot held by `AppState` (10 §2's
/// `video_engine` field, lazy-headless variant — see module docs).
#[derive(Default)]
pub struct VideoEngineHandle {
    cell: OnceLock<Option<EngineBridge>>,
    /// Retry receipts are scoped to this application session, independently of
    /// whether a GPU engine has been initialized.
    pub(crate) edit_plans: tokio::sync::Mutex<super::video_edits::EditPlanRegistry>,
}

impl VideoEngineHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// None means not requested; Some(false) records a failed adapter request.
    pub fn initialization_state(&self) -> Option<bool> {
        self.cell.get().map(Option::is_some)
    }

    /// The bridge, creating engine + session on first use. `None` = no GPU
    /// adapter available (the adapter-skip convention).
    pub fn bridge(&self) -> Option<&EngineBridge> {
        self.cell.get_or_init(EngineBridge::create).as_ref()
    }
}

/// One `EngineSession` + the shadow document pair it snapshots from, plus
/// the session-scoped state the MCP layer owns (proxy mode, transport lock).
pub struct EngineBridge {
    next_inspection_id: std::sync::atomic::AtomicU64,
    pub(crate) readback: Arc<StdMutex<super::video_readback::FrameReadback>>,
    engine: VideoEngine,
    session: EngineSession,
    sync_state: StdMutex<BridgeSnapshot>,
    /// Serializes seek-then-wait transactions (`render_frame_at`, transport
    /// tools) so two concurrent calls can't interleave their target ticks.
    transport: tokio::sync::Mutex<()>,
    /// Last mode set via `set_proxy_mode` — `EngineStatus` doesn't echo it,
    /// so `render_frame_at` restores from here after a per-call override.
    proxy_mode: StdMutex<ProxyMode>,
}

#[derive(Default)]
struct BridgeSnapshot {
    cache_path: Option<std::path::PathBuf>,
    base: Option<RenderSnapshot>,
    format_override: Option<(photonic_core::timeline::SequenceId, usize)>,
    generation: u64,
}

impl EngineBridge {
    fn create() -> Option<EngineBridge> {
        let engine = VideoEngine::headless()?;
        let shadow_doc = Arc::new(StdMutex::new(Document::new(
            "video-engine-shadow",
            1.0,
            1.0,
        )));
        let shadow_history = Arc::new(StdMutex::new(CommandHistory::new(1)));
        let session = engine.open_session(Arc::clone(&shadow_doc), Arc::clone(&shadow_history));
        Some(EngineBridge {
            next_inspection_id: std::sync::atomic::AtomicU64::new(1),
            readback: Arc::new(StdMutex::new(
                super::video_readback::FrameReadback::default(),
            )),
            engine,
            session,
            sync_state: StdMutex::new(BridgeSnapshot::default()),
            transport: tokio::sync::Mutex::new(()),
            proxy_mode: StdMutex::new(ProxyMode::default()),
        })
    }

    pub fn engine(&self) -> &VideoEngine {
        &self.engine
    }

    pub fn session(&self) -> &EngineSession {
        &self.session
    }

    pub fn next_inspection_id(&self) -> u64 {
        self.next_inspection_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Serialize a seek-then-wait transaction (held across the whole tool
    /// call for `render_frame_at` and the transport tools).
    pub async fn lock_transport(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.transport.lock().await
    }

    pub fn proxy_mode(&self) -> ProxyMode {
        *self.proxy_mode.lock().expect("proxy_mode poisoned")
    }

    pub fn set_proxy_mode(&self, mode: ProxyMode) {
        *self.proxy_mode.lock().expect("proxy_mode poisoned") = mode;
        self.session.send(EngineCmd::SetProxyMode(mode));
    }

    /// Reuse the immutable revisioned snapshot when the document is unchanged.
    /// Callers serialize transport so a concurrent status read cannot replace a
    /// render's temporary format view halfway through its frame request.
    pub async fn sync(&self, state: &AppState) {
        self.sync_with_format(state, None).await;
    }

    pub async fn sync_with_format(
        &self,
        state: &AppState,
        format_override: Option<(photonic_core::timeline::SequenceId, usize)>,
    ) {
        let document = state.document.lock().await;
        let history = state.history.lock().await;
        let revision = history.revision();
        let mut current = self.sync_state.lock().expect("snapshot state poisoned");
        let document_path = state
            .document_path
            .lock()
            .expect("document path poisoned")
            .clone();
        let cache_path = photonic_video::media::proxy_cache_dir(document_path.as_deref());
        if current.cache_path.as_ref() != Some(&cache_path)
            && self.session.send(EngineCmd::SetPreviewCacheDir {
                path: cache_path.clone(),
            })
        {
            current.cache_path = Some(cache_path);
        }
        let unchanged = current
            .base
            .as_ref()
            .is_some_and(|base| base.revision == revision);
        if unchanged && current.format_override == format_override {
            return;
        }
        if !unchanged {
            current.base = Some(RenderSnapshot::from_document(&document, revision));
        }
        let mut view = current.base.as_ref().expect("snapshot initialized").clone();
        if let (Some((sequence, index)), Some(project)) = (format_override, view.project.as_mut()) {
            if project
                .sequences
                .get(&sequence)
                .is_some_and(|seq| seq.active_format != index)
            {
                if let Some(sequence) = Arc::make_mut(project).sequences.get_mut(&sequence) {
                    sequence.active_format = index;
                }
            }
        }
        current.generation = self.session.publish_snapshot(view);
        current.format_override = format_override;
    }

    pub fn shadow_revision(&self) -> u64 {
        self.sync_state
            .lock()
            .expect("snapshot state poisoned")
            .base
            .as_ref()
            .map_or(0, |base| base.revision)
    }

    pub fn snapshot_generation(&self) -> u64 {
        self.sync_state
            .lock()
            .expect("snapshot state poisoned")
            .generation
    }

    /// Wait until the engine consumes this bridge's publication generation.
    pub async fn wait_engine_synced(&self, timeout: Duration) -> bool {
        let want = self.snapshot_generation();
        let deadline = Instant::now() + timeout;
        loop {
            if self.session.status().snapshot_generation >= want {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Wait for a frame **newer than** `prev` (pointer-compared) matching
    /// `pred`. Pair with: sync → `wait_engine_synced` → capture `prev` →
    /// send `Seek` → this — so a stale pre-seek frame can never satisfy it.
    pub async fn wait_fresh_frame(
        &self,
        prev: Option<Arc<EngineFrame>>,
        timeout: Duration,
        pred: impl Fn(&EngineFrame) -> bool,
    ) -> Option<Arc<EngineFrame>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.session.latest_frame() {
                let fresh = match &prev {
                    Some(p) => !Arc::ptr_eq(p, &frame),
                    None => true,
                };
                if fresh && pred(&frame) {
                    return Some(frame);
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

// ─── Job registry (10 §6) ────────────────────────────────────────────────────

pub type JobId = Uuid;

/// Terminal-state retention before GC eviction (10 §6: "retained 10 minutes
/// ... comfortably exceeds the debounced-checkpoint window").
pub const JOB_RETENTION: Duration = Duration::from_secs(600);

/// 10 §6's status enum. `Failed` carries the §8-style structured code so
/// agents can branch on error kind when polling.
#[derive(Clone, Debug)]
pub enum JobStatus {
    Queued,
    Running { progress: f32, message: String },
    Done { result: Value },
    Failed { error_code: String, message: String },
    Cancelled,
}

impl JobStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobStatus::Done { .. } | JobStatus::Failed { .. } | JobStatus::Cancelled
        )
    }

    pub fn to_json(&self) -> Value {
        match self {
            JobStatus::Queued => json!({ "state": "queued" }),
            JobStatus::Running { progress, message } => {
                json!({ "state": "running", "progress": progress, "message": message })
            }
            JobStatus::Done { result } => json!({ "state": "done", "result": result }),
            JobStatus::Failed {
                error_code,
                message,
            } => json!({ "state": "failed", "error_code": error_code, "message": message }),
            JobStatus::Cancelled => json!({ "state": "cancelled" }),
        }
    }
}

pub struct JobHandle {
    pub kind: String,
    pub status: JobStatus,
    /// Cooperative cancel flag — workers poll it between units of work
    /// (`export_frames` checks between frames, 02 §7).
    pub cancel: Arc<AtomicBool>,
    pub created: Instant,
    /// Set when `status` turned terminal; drives GC retention.
    pub finished: Option<Instant>,
}

pub const MAX_ACTIVE_JOBS: usize = 4;
const MAX_RETAINED_JOBS: usize = 256;
#[derive(Debug, thiserror::Error)]
#[error("All {MAX_ACTIVE_JOBS} video job slots are occupied; poll or cancel an existing job before retrying")]
pub struct JobAdmissionError;

#[derive(Default)]
pub struct JobRegistry {
    jobs: HashMap<JobId, JobHandle>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a queued job; returns its id and the shared cancel flag the
    /// worker should poll.
    pub fn start(
        &mut self,
        kind: impl Into<String>,
    ) -> Result<(JobId, Arc<AtomicBool>), JobAdmissionError> {
        self.gc(JOB_RETENTION);
        if self
            .jobs
            .values()
            .filter(|job| !job.status.is_terminal())
            .count()
            >= MAX_ACTIVE_JOBS
        {
            return Err(JobAdmissionError);
        }
        if self.jobs.len() >= MAX_RETAINED_JOBS {
            if let Some(oldest) = self
                .jobs
                .iter()
                .filter(|(_, job)| job.status.is_terminal())
                .min_by_key(|(_, job)| job.finished)
                .map(|(id, _)| *id)
            {
                self.jobs.remove(&oldest);
            }
        }
        let id = Uuid::new_v4();
        let cancel = Arc::new(AtomicBool::new(false));
        self.jobs.insert(
            id,
            JobHandle {
                kind: kind.into(),
                status: JobStatus::Queued,
                cancel: Arc::clone(&cancel),
                created: Instant::now(),
                finished: None,
            },
        );
        Ok((id, cancel))
    }

    /// Update a job's status; stamps `finished` on the terminal transition.
    /// Silently ignores unknown ids (a worker may outlive a GC'd entry).
    pub fn set_status(&mut self, id: JobId, status: JobStatus) {
        if let Some(job) = self.jobs.get_mut(&id) {
            if status.is_terminal() && job.finished.is_none() {
                job.finished = Some(Instant::now());
            }
            job.status = status;
        }
    }

    pub fn get(&self, id: JobId) -> Option<&JobHandle> {
        self.jobs.get(&id)
    }

    /// Set the cancel flag. Returns `None` for unknown ids; `Some(true)` if
    /// the job was still live (worker will observe the flag), `Some(false)`
    /// if it had already reached a terminal state.
    pub fn request_cancel(&mut self, id: JobId) -> Option<bool> {
        let job = self.jobs.get_mut(&id)?;
        job.cancel.store(true, Ordering::Relaxed);
        Some(!job.status.is_terminal())
    }

    /// Evict terminal jobs older than `retention` (10 §6 lifetime/GC).
    pub fn gc(&mut self, retention: Duration) {
        self.jobs.retain(|_, job| match job.finished {
            Some(at) => at.elapsed() < retention,
            None => true,
        });
    }

    pub fn status_json(&self, id: JobId) -> Option<Value> {
        self.get(id).map(|job| {
            json!({
                "job_id": id,
                "kind": job.kind,
                "status": job.status.to_json(),
            })
        })
    }
}

/// Convenience for workers: update status through the `AppState` handle
/// without each call site re-spelling the lock/poison dance.
pub fn set_job_status(jobs: &Arc<StdMutex<JobRegistry>>, id: JobId, status: JobStatus) {
    if let Ok(mut reg) = jobs.lock() {
        reg.set_status(id, status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_admission_is_bounded_and_terminal_jobs_release_capacity() {
        let mut registry = JobRegistry::new();
        let ids: Vec<_> = (0..MAX_ACTIVE_JOBS)
            .map(|_| registry.start("fake").unwrap().0)
            .collect();
        assert!(registry.start("excess").is_err());
        registry.set_status(ids[0], JobStatus::Done { result: json!({}) });
        assert!(registry.start("next").is_ok());
    }

    #[tokio::test]
    async fn unchanged_document_reuses_snapshot_generation() {
        let state = AppState::headless_for_test();
        let Some(bridge) = state.video_engine.bridge() else {
            return;
        };
        bridge.sync(&state).await;
        let first = bridge.snapshot_generation();
        bridge.sync(&state).await;
        assert_eq!(first, bridge.snapshot_generation());
        state.history.lock().await.reset();
        bridge.sync(&state).await;
        assert!(bridge.snapshot_generation() > first);
        assert_eq!(
            bridge.shadow_revision(),
            state.history.lock().await.revision()
        );
    }

    #[test]
    fn job_registry_lifecycle_and_gc() {
        let mut reg = JobRegistry::new();
        let (id, cancel) = reg.start("fake").unwrap();
        assert!(matches!(reg.get(id).unwrap().status, JobStatus::Queued));

        reg.set_status(
            id,
            JobStatus::Running {
                progress: 0.5,
                message: "halfway".into(),
            },
        );
        assert!(reg.get(id).unwrap().finished.is_none());

        // Cancellation of a live job flags the worker's AtomicBool.
        assert_eq!(reg.request_cancel(id), Some(true));
        assert!(cancel.load(Ordering::Relaxed));

        reg.set_status(id, JobStatus::Cancelled);
        assert!(reg.get(id).unwrap().finished.is_some());
        // Cancelling a terminal job reports "already finished".
        assert_eq!(reg.request_cancel(id), Some(false));

        // GC: terminal + retention elapsed ⇒ evicted (zero retention here).
        reg.gc(Duration::from_secs(0));
        assert!(reg.get(id).is_none(), "terminal job must be GC'd");
        assert_eq!(reg.request_cancel(id), None, "evicted ⇒ JobNotFound");

        // Live jobs are never GC'd regardless of age.
        let (live, _) = reg.start("fake").unwrap();
        reg.gc(Duration::from_secs(0));
        assert!(reg.get(live).is_some(), "non-terminal job must survive GC");
    }

    #[test]
    fn job_status_json_shapes() {
        let s = JobStatus::Failed {
            error_code: "AssetOffline".into(),
            message: "gone".into(),
        }
        .to_json();
        assert_eq!(s["state"], "failed");
        assert_eq!(s["error_code"], "AssetOffline");
        let s = JobStatus::Done {
            result: json!({"output_path": "/tmp/x.mp4"}),
        }
        .to_json();
        assert_eq!(s["state"], "done");
        assert_eq!(s["result"]["output_path"], "/tmp/x.mp4");
    }
}
