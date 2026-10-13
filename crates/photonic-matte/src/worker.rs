//! Bounded serial background inference. Polling never waits for inference or
//! model loading. Results are raw subject weights, not source-alpha coverage; callers retain responsibility for presenting pending/error
//! state and must not export a pending placeholder as a finished result.
use crate::{validate_image, CachedMatteModel};
use photonic_core::raster::{image::RasterImage, mask::Mask};
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc,
    },
};

pub const MAX_INPUT_PIXELS: usize = 4 * 1024 * 1024;
pub const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CACHE_ENTRIES: usize = 8;
const MAX_PENDING: usize = 2;

/// The caller's immutable input-content identity, including dimensions, color
/// interpretation and quality. Reusing a key for different pixels is invalid.
pub type MatteKey = u128;
#[derive(Clone, Debug)]
pub enum MatteStatus {
    Pending,
    Ready(Arc<Mask>),
    Failed(Arc<str>),
    Busy,
}
type Outcome = Result<Arc<Mask>, Arc<str>>;
struct Request {
    key: MatteKey,
    epoch: u64,
    prepare: Box<dyn FnOnce() -> Result<Arc<RasterImage>, String> + Send>,
}
struct ResultMessage {
    key: MatteKey,
    epoch: u64,
    outcome: Outcome,
}

pub struct MatteWorker {
    sender: SyncSender<Request>,
    receiver: Receiver<ResultMessage>,
    pending: HashSet<MatteKey>,
    cache: VecDeque<(MatteKey, Outcome)>,
    cache_bytes: usize,
    generation: u64,
    epoch: u64,
}
impl MatteWorker {
    pub fn new() -> std::io::Result<Self> {
        let mut model: Option<Result<CachedMatteModel, String>> = None;
        let mut model_epoch = 0;
        Self::with_inference_epoch(move |image, epoch| {
            if epoch != model_epoch {
                if model.as_ref().is_some_and(Result::is_err) {
                    model = None;
                }
                model_epoch = epoch;
            }
            let model = model.get_or_insert_with(|| {
                CachedMatteModel::load_cached().map_err(|e| format!("{e:#}"))
            });
            match model {
                Ok(model) => model.matte(image).map_err(|e| format!("{e:#}")),
                Err(error) => Err(error.clone()),
            }
        })
    }
    #[cfg(test)]
    fn with_inference(
        mut infer: impl FnMut(&RasterImage) -> Result<Mask, String> + Send + 'static,
    ) -> std::io::Result<Self> {
        Self::with_inference_epoch(move |image, _| infer(image))
    }
    fn with_inference_epoch(
        mut infer: impl FnMut(&RasterImage, u64) -> Result<Mask, String> + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, requests) = mpsc::sync_channel::<Request>(1);
        let (results, receiver) = mpsc::sync_channel(2);
        std::thread::Builder::new()
            .name("photonic-matte".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let outcome = (request.prepare)()
                        .and_then(|image| {
                            validate_image(&image).map_err(|e| e.to_string())?;
                            if image.width as usize * image.height as usize > MAX_INPUT_PIXELS {
                                return Err(
                                    "asynchronous matting input exceeds four-megapixel budget"
                                        .into(),
                                );
                            }
                            infer(&image, request.epoch).and_then(|mask| {
                                if mask.width != image.width
                                    || mask.height != image.height
                                    || mask.data.len() != mask.width as usize * mask.height as usize
                                {
                                    Err("matting worker returned mismatched mask geometry".into())
                                } else {
                                    Ok(mask)
                                }
                            })
                        })
                        .map(Arc::new)
                        .map_err(Arc::<str>::from);
                    if results
                        .send(ResultMessage {
                            key: request.key,
                            epoch: request.epoch,
                            outcome,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender,
            receiver,
            pending: HashSet::new(),
            cache: VecDeque::new(),
            cache_bytes: 0,
            generation: 0,
            epoch: 0,
        })
    }
    fn drain(&mut self) {
        loop {
            match self.receiver.try_recv() {
                Ok(result) => {
                    self.pending.remove(&result.key);
                    if result.epoch != self.epoch {
                        continue;
                    }
                    self.generation = self.generation.wrapping_add(1);
                    self.cache_bytes += result.outcome.as_ref().map_or(0, |mask| mask.data.len());
                    self.cache.push_back((result.key, result.outcome));
                    while self.cache.len() > MAX_CACHE_ENTRIES || self.cache_bytes > MAX_CACHE_BYTES
                    {
                        if let Some((_, old)) = self.cache.pop_front() {
                            self.cache_bytes -= old.as_ref().map_or(0, |mask| mask.data.len());
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    for key in self.pending.drain() {
                        self.generation = self.generation.wrapping_add(1);
                        self.cache
                            .push_back((key, Err(Arc::from("matting worker stopped"))));
                    }
                    while self.cache.len() > MAX_CACHE_ENTRIES {
                        if let Some((_, old)) = self.cache.pop_front() {
                            self.cache_bytes -= old.as_ref().map_or(0, |mask| mask.data.len());
                        }
                    }
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
    }
    /// Discard cached/stale results without spawning an overlapping worker.
    pub fn invalidate(&mut self) {
        self.cache.clear();
        self.cache_bytes = 0;
        self.epoch = self.epoch.wrapping_add(1);
        self.generation = self.generation.wrapping_add(1);
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn poll(&mut self) -> u64 {
        self.drain();
        self.generation
    }
    pub fn status(&mut self, key: MatteKey) -> Option<MatteStatus> {
        self.drain();
        if let Some(index) = self.cache.iter().position(|(k, _)| *k == key) {
            let entry = self.cache.remove(index).unwrap();
            let status = match &entry.1 {
                Ok(mask) => MatteStatus::Ready(mask.clone()),
                Err(error) => MatteStatus::Failed(error.clone()),
            };
            self.cache.push_back(entry);
            return Some(status);
        }
        self.pending.contains(&key).then_some(MatteStatus::Pending)
    }
    pub fn request(&mut self, key: MatteKey, image: Arc<RasterImage>) -> MatteStatus {
        if let Some(status) = self.status(key) {
            return status;
        }
        if let Err(error) = validate_image(&image) {
            return MatteStatus::Failed(Arc::from(error.to_string()));
        }
        if image.width as usize * image.height as usize > MAX_INPUT_PIXELS {
            return MatteStatus::Failed(Arc::from(
                "asynchronous matting input exceeds four-megapixel budget",
            ));
        }
        self.request_prepared(key, move || Ok(image))
    }
    /// Defer pixel preparation/readback onto the same serial worker. The closure
    /// must own immutable source data and honor the input budget before allocating.
    /// Duplicate/cache-hit keys never invoke it. No network access is performed
    /// by the supplied inference implementation.
    pub fn request_prepared(
        &mut self,
        key: MatteKey,
        prepare: impl FnOnce() -> Result<Arc<RasterImage>, String> + Send + 'static,
    ) -> MatteStatus {
        if let Some(status) = self.status(key) {
            return status;
        }
        if self.pending.len() >= MAX_PENDING {
            return MatteStatus::Busy;
        }
        match self.sender.try_send(Request {
            key,
            epoch: self.epoch,
            prepare: Box::new(prepare),
        }) {
            Ok(()) => {
                self.pending.insert(key);
                MatteStatus::Pending
            }
            Err(TrySendError::Full(_)) => MatteStatus::Busy,
            Err(TrySendError::Disconnected(_)) => {
                MatteStatus::Failed(Arc::from("matting worker stopped"))
            }
        }
    }
    pub fn cached_bytes(&self) -> usize {
        self.cache_bytes
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Condvar, Mutex,
    };
    fn await_result(worker: &mut MatteWorker, key: MatteKey) -> MatteStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(status) = worker.status(key) {
                if !matches!(status, MatteStatus::Pending) {
                    return status;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not finish"
            );
            std::thread::yield_now();
        }
    }
    #[test]
    fn bounded_queue_dedup_cache_and_nonblocking_poll() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        let (started, ready) = mpsc::channel();
        let mut worker = MatteWorker::with_inference(move |image| {
            count.fetch_add(1, Ordering::SeqCst);
            started.send(()).unwrap();
            let (lock, cv) = &*g;
            let guard = lock.lock().unwrap();
            drop(cv.wait_while(guard, |open| !*open).unwrap());
            Ok(Mask::full(image.width, image.height))
        })
        .unwrap();
        let image = Arc::new(RasterImage::filled(2, 2, [1, 2, 3, 255]));
        assert!(matches!(
            worker.request(1, image.clone()),
            MatteStatus::Pending
        ));
        ready
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(matches!(
            worker.request(1, image.clone()),
            MatteStatus::Pending
        ));
        assert!(matches!(
            worker.request(2, image.clone()),
            MatteStatus::Pending
        ));
        assert!(matches!(
            worker.request(3, image.clone()),
            MatteStatus::Busy
        ));
        assert_eq!(worker.pending_count(), 2);
        for _ in 0..100 {
            assert!(matches!(worker.status(1), Some(MatteStatus::Pending)));
        }
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(matches!(
            await_result(&mut worker, 1),
            MatteStatus::Ready(_)
        ));
        assert!(matches!(
            await_result(&mut worker, 2),
            MatteStatus::Ready(_)
        ));
        assert!(matches!(worker.request(1, image), MatteStatus::Ready(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(worker.poll(), 2);
        assert_eq!(worker.poll(), 2);
        assert_eq!(worker.cached_bytes(), 8);
    }
    #[test]
    fn failures_geometry_and_lru_are_bounded() {
        let mut worker = MatteWorker::with_inference(|image| {
            if image.pixels[0] == 0 {
                Err("fixture failure".into())
            } else if image.pixels[0] == 1 {
                Ok(Mask::full(1, 1))
            } else {
                Ok(Mask::full(image.width, image.height))
            }
        })
        .unwrap();
        for key in 0..12 {
            let image = Arc::new(RasterImage::filled(2, 2, [key as u8, 0, 0, 255]));
            assert!(matches!(worker.request(key, image), MatteStatus::Pending));
            let result = await_result(&mut worker, key);
            if key < 2 {
                assert!(matches!(result, MatteStatus::Failed(_)));
            } else {
                assert!(matches!(result, MatteStatus::Ready(_)));
            }
        }
        assert!(worker.status(0).is_none());
        assert_eq!(worker.cache.len(), 8);
        assert_eq!(worker.cached_bytes(), 32);
        let invalid = Arc::new(RasterImage {
            width: 2,
            height: 2,
            pixels: vec![],
        });
        assert!(matches!(
            worker.request(20, invalid),
            MatteStatus::Failed(_)
        ));
    }
    #[test]
    fn cache_byte_budget_and_large_input_refusal() {
        let mut worker =
            MatteWorker::with_inference(|image| Ok(Mask::full(image.width, image.height))).unwrap();
        let image = Arc::new(RasterImage::filled(2048, 2048, [0, 0, 0, 255]));
        for key in 0..6 {
            assert!(matches!(
                worker.request(key, image.clone()),
                MatteStatus::Pending
            ));
            assert!(matches!(
                await_result(&mut worker, key),
                MatteStatus::Ready(_)
            ));
            assert!(worker.cached_bytes() <= MAX_CACHE_BYTES);
        }
        assert_eq!(worker.cache.len(), 4);
        assert!(worker.status(0).is_none());
        let oversized = Arc::new(RasterImage::filled(2049, 2048, [0, 0, 0, 255]));
        assert!(matches!(
            worker.request(100, oversized),
            MatteStatus::Failed(_)
        ));
        assert_eq!(worker.pending_count(), 0);
    }

    #[test]
    fn worker_disconnect_resolves_pending_instead_of_hanging() {
        let mut worker =
            MatteWorker::with_inference(|_| panic!("simulated inference panic")).unwrap();
        let image = Arc::new(RasterImage::filled(1, 1, [0, 0, 0, 255]));
        assert!(matches!(worker.request(5, image), MatteStatus::Pending));
        assert!(matches!(
            await_result(&mut worker, 5),
            MatteStatus::Failed(_)
        ));
        assert_eq!(worker.pending_count(), 0);
    }
    #[test]
    fn preparation_is_off_caller_thread_and_errors_are_cached() {
        let caller = std::thread::current().id();
        let mut worker =
            MatteWorker::with_inference(|image| Ok(Mask::full(image.width, image.height))).unwrap();
        assert!(matches!(
            worker.request_prepared(40, move || {
                assert_ne!(std::thread::current().id(), caller);
                Err("readback fixture failure".into())
            }),
            MatteStatus::Pending
        ));
        assert!(matches!(
            await_result(&mut worker, 40),
            MatteStatus::Failed(_)
        ));
        assert!(matches!(
            worker.request_prepared(40, || panic!("cached preparation reran")),
            MatteStatus::Failed(_)
        ));
    }

    #[test]
    #[ignore = "requires explicitly provisioned pinned local model"]
    fn actual_cached_model_runs_on_worker_and_caches_output() {
        let mut worker = MatteWorker::new().unwrap();
        let mut image = RasterImage::filled(96, 96, [12, 12, 20, 255]);
        for y in 30..66 {
            for x in 30..66 {
                image.set_pixel(x, y, [230, 220, 60, 255]);
            }
        }
        let image = Arc::new(image);
        assert!(matches!(
            worker.request(60, image.clone()),
            MatteStatus::Pending
        ));
        let MatteStatus::Ready(mask) = await_result(&mut worker, 60) else {
            panic!("real model failed")
        };
        assert!(mask.get(48, 48) as u32 > mask.get(3, 3) as u32 + 40);
        let MatteStatus::Ready(cached) = worker.request(60, image) else {
            panic!("cache missing")
        };
        assert!(Arc::ptr_eq(&mask, &cached));
    }
    #[test]
    fn invalidation_discards_inflight_results_without_another_worker() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        let threads = Arc::new(Mutex::new(Vec::new()));
        let seen = threads.clone();
        let (started, ready) = mpsc::channel();
        let mut worker = MatteWorker::with_inference(move |image| {
            seen.lock().unwrap().push(std::thread::current().id());
            started.send(()).unwrap();
            let guard = g.0.lock().unwrap();
            drop(g.1.wait_while(guard, |open| !*open).unwrap());
            Ok(Mask {
                width: 1,
                height: 1,
                data: vec![image.pixels[0]],
            })
        })
        .unwrap();
        assert!(matches!(
            worker.request(1, Arc::new(RasterImage::filled(1, 1, [10, 0, 0, 255]))),
            MatteStatus::Pending
        ));
        ready
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        worker.invalidate();
        assert!(matches!(
            worker.request(2, Arc::new(RasterImage::filled(1, 1, [20, 0, 0, 255]))),
            MatteStatus::Pending
        ));
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let MatteStatus::Ready(mask) = await_result(&mut worker, 2) else {
            panic!("new result unavailable")
        };
        assert_eq!(mask.data, [20]);
        assert!(worker.status(1).is_none());
        let threads = threads.lock().unwrap();
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0], threads[1]);
    }
    #[test]
    fn invalidation_retries_cached_failure_in_new_epoch() {
        let mut worker = MatteWorker::with_inference_epoch(|image, epoch| {
            if epoch == 0 {
                Err("cached loader failure".into())
            } else {
                Ok(Mask::full(image.width, image.height))
            }
        })
        .unwrap();
        let image = Arc::new(RasterImage::filled(1, 1, [0, 0, 0, 255]));
        worker.request(5, image.clone());
        assert!(matches!(
            await_result(&mut worker, 5),
            MatteStatus::Failed(_)
        ));
        worker.invalidate();
        assert!(matches!(worker.request(5, image), MatteStatus::Pending));
        assert!(matches!(
            await_result(&mut worker, 5),
            MatteStatus::Ready(_)
        ));
    }
}
