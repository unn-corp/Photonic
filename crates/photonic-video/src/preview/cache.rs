use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::SystemTime;

use photonic_core::timeline::{SequenceId, Tick};
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::Xxh3;

use super::{ChunkKey, ChunkSignature, PreviewError};

const MANIFEST_LIMIT: u64 = 128 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct CacheConfig {
    /// Published preview files plus other sidecar data must fit this limit.
    pub max_bytes: u64,
    pub max_entries: usize,
}
impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024 * 1024,
            max_entries: 2048,
        }
    }
}
impl CacheConfig {
    pub fn for_project(limit_mb: Option<u64>) -> Self {
        Self {
            max_bytes: limit_mb
                .map(|mb| mb.saturating_mul(1024 * 1024))
                .unwrap_or(Self::default().max_bytes),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChunkState {
    #[default]
    NotRendered,
    Queued,
    Rendering,
    Paused,
    Rendered,
    Cancelled,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkStatus {
    pub sequence: SequenceId,
    pub format_index: usize,
    pub start: Tick,
    pub end: Tick,
    pub state: ChunkState,
    pub frame: u64,
    pub total: u64,
    pub error: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheStats {
    pub bytes: u64,
    pub external_bytes: u64,
    pub limit_bytes: u64,
    pub entries: usize,
    pub evicted: u64,
    pub rejected: u64,
    pub pressure: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewStatusSnapshot {
    pub doc_revision: u64,
    pub snapshot_generation: u64,
    pub error: Option<String>,
    pub planning_ranges: usize,
    pub profile: super::PreviewProfile,
    pub chunks: Vec<ChunkStatus>,
    pub cache: CacheStats,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}
fn file_stamp(path: &Path) -> std::io::Result<FileStamp> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "preview media must be a regular file",
        ));
    }
    Ok(FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// Holding the Arc returned by lookup pins an immutable chunk while a decoder
/// uses its path. Clear removes it from lookup immediately, but defers deletion
/// until the final reader releases its lease.
#[derive(Debug)]
pub struct PreviewChunk {
    pub signature: ChunkSignature,
    path: PathBuf,
    directory: PathBuf,
    media_stamp: FileStamp,
    bytes: u64,
    retired: AtomicBool,
}
impl PreviewChunk {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn local_time(&self, tick: Tick) -> Option<Tick> {
        self.signature
            .span
            .contains(tick)
            .then(|| tick - self.signature.span.start)
    }
    fn readable(&self) -> bool {
        file_stamp(&self.path).ok() == Some(self.media_stamp)
    }
}
impl Drop for PreviewChunk {
    fn drop(&mut self) {
        if self.retired.load(Ordering::Relaxed) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}
struct Entry {
    chunk: Arc<PreviewChunk>,
    used: u64,
}
type Slot = (SequenceId, usize, Tick);
struct CacheInner {
    entries: HashMap<ChunkKey, Entry>,
    retired: Vec<Arc<PreviewChunk>>,
    current: HashMap<Slot, (u64, ChunkStatus)>,
    zones: HashMap<SequenceId, Vec<(Tick, Tick)>>,
    clock: u64,
    bytes: u64,
    external_bytes: u64,
    evicted: u64,
    rejected: u64,
}

/// One shared index for a project's sidecar directory. Cache mutation never
/// changes the document. Different content variants remain reusable after undo.
pub struct PreviewCache {
    sidecar_root: PathBuf,
    root: PathBuf,
    config: CacheConfig,
    inner: Mutex<CacheInner>,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    signature: ChunkSignature,
    media_bytes: u64,
    media_hash: String,
}

impl PreviewCache {
    pub fn open(sidecar_root: PathBuf, config: CacheConfig) -> Result<Self, PreviewError> {
        if config.max_entries == 0 {
            return Err(PreviewError::Invalid(
                "preview index limit must be positive".into(),
            ));
        }
        let root = sidecar_root.join("preview");
        std::fs::create_dir_all(&root)?;
        let mut cache = Self {
            sidecar_root,
            root,
            config,
            inner: Mutex::new(CacheInner {
                entries: HashMap::new(),
                retired: Vec::new(),
                current: HashMap::new(),
                zones: HashMap::new(),
                clock: 0,
                bytes: 0,
                external_bytes: 0,
                evicted: 0,
                rejected: 0,
            }),
        };
        cache.load_index()?;
        let external = external_cache_bytes(&cache.sidecar_root, &cache.root);
        let inner = cache
            .inner
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        inner.external_bytes = external;
        enforce_budget(inner, config, 0);
        Ok(cache)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config(&self) -> CacheConfig {
        self.config
    }
    pub fn available_bytes(&self) -> u64 {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        self.config.max_bytes.saturating_sub(inner.external_bytes)
    }
    fn directory(&self, key: ChunkKey) -> PathBuf {
        self.root
            .join(key.sequence.to_string())
            .join(key.format_index.to_string())
            .join(format!("{}-{:032x}.chunk", key.start.0, key.hash.0))
    }
    fn load_index(&mut self) -> Result<(), PreviewError> {
        // Only the known sequence/format/chunk depth is traversed. Staging and
        // symlink directories are never treated as ready chunks.
        for sequence in directories(&self.root) {
            for format in directories(&sequence) {
                for directory in directories(&format) {
                    if !directory
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with(".chunk"))
                    {
                        continue;
                    }
                    let mut cleanup = InvalidChunkCleanup(Some(directory.clone()));
                    let manifest_path = directory.join("manifest.json");
                    let Ok(metadata) = std::fs::metadata(&manifest_path) else {
                        continue;
                    };
                    if metadata.len() > MANIFEST_LIMIT {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(&manifest_path) else {
                        continue;
                    };
                    let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else {
                        continue;
                    };
                    if manifest.signature.validate().is_err()
                        || directory != self.directory(manifest.signature.key)
                    {
                        continue;
                    }
                    let path = directory.join(format!(
                        "media.{}",
                        manifest.signature.context.profile.extension()
                    ));
                    let Ok(stamp) = file_stamp(&path) else {
                        continue;
                    };
                    if stamp.len != manifest.media_bytes
                        || hash_file(&path).ok().as_deref() != Some(manifest.media_hash.as_str())
                    {
                        continue;
                    }
                    cleanup.0 = None;
                    let chunk = Arc::new(PreviewChunk {
                        signature: manifest.signature,
                        path,
                        directory,
                        media_stamp: stamp,
                        bytes: stamp.len.saturating_add(metadata.len()),
                        retired: AtomicBool::new(false),
                    });
                    let inner = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
                    inner.clock = inner.clock.wrapping_add(1);
                    inner.bytes = inner.bytes.saturating_add(chunk.bytes);
                    inner.entries.insert(
                        chunk.signature.key,
                        Entry {
                            chunk,
                            used: inner.clock,
                        },
                    );
                    enforce_budget(inner, self.config, 0);
                }
            }
        }
        Ok(())
    }

    /// Caller supplies a full-chunk signature compiled against its current
    /// immutable snapshot. A generation number alone is never a cache key.
    pub fn lookup(&self, signature: &ChunkSignature) -> Option<Arc<PreviewChunk>> {
        let mut inner = match self.inner.try_lock() {
            Ok(inner) => inner,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        collect_retired(&mut inner);
        if inner
            .entries
            .get(&signature.key)
            .is_some_and(|entry| !entry.chunk.readable())
        {
            retire_entry(&mut inner, signature.key);
            collect_retired(&mut inner);
        }
        inner.clock = inner.clock.wrapping_add(1);
        let clock = inner.clock;
        let hit = inner.entries.get_mut(&signature.key).and_then(|entry| {
            if entry.chunk.signature == *signature && entry.chunk.readable() {
                entry.used = clock;
                Some(Arc::clone(&entry.chunk))
            } else {
                None
            }
        });
        put_status(
            &mut inner,
            self.config.max_entries,
            signature,
            if hit.is_some() {
                ChunkState::Rendered
            } else {
                ChunkState::NotRendered
            },
            if hit.is_some() {
                signature.span.frame_count
            } else {
                0
            },
            None,
        );
        hit
    }
    pub fn set_status(
        &self,
        signature: &ChunkSignature,
        state: ChunkState,
        frame: u64,
        error: Option<String>,
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        put_status(
            &mut inner,
            self.config.max_entries,
            signature,
            state,
            frame,
            error,
        );
    }
    /// Marks display status unknown after edits; files remain available for
    /// content-based revalidation, including undo. No eager chunk recompilation.
    pub fn invalidate_sequence(&self, sequence: SequenceId) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        for (_, status) in inner
            .current
            .values_mut()
            .filter(|(_, status)| status.sequence == sequence)
        {
            status.state = ChunkState::NotRendered;
            status.frame = 0;
            status.error = None;
        }
    }
    pub fn set_zones(&self, sequence: SequenceId, zones: &[(Tick, Tick)]) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner
            .zones
            .insert(sequence, zones.iter().copied().take(128).collect());
    }
    pub fn status(&self) -> PreviewStatusSnapshot {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        collect_retired(&mut inner);
        self.status_snapshot(&inner)
    }
    /// Optional UI telemetry must not wait behind publication or filesystem work.
    pub fn try_status(&self) -> Option<PreviewStatusSnapshot> {
        let inner = match self.inner.try_lock() {
            Ok(inner) => inner,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        Some(self.status_snapshot(&inner))
    }
    fn status_snapshot(&self, inner: &CacheInner) -> PreviewStatusSnapshot {
        let mut chunks: Vec<_> = inner
            .current
            .values()
            .map(|(_, status)| status.clone())
            .collect();
        chunks.sort_by_key(|status| (status.sequence.0, status.format_index, status.start));
        PreviewStatusSnapshot {
            doc_revision: 0,
            snapshot_generation: 0,
            error: None,
            planning_ranges: 0,
            profile: super::PreviewProfile::default(),
            chunks,
            cache: CacheStats {
                bytes: inner.bytes,
                external_bytes: inner.external_bytes,
                limit_bytes: self.config.max_bytes,
                entries: inner.entries.len(),
                evicted: inner.evicted,
                rejected: inner.rejected,
                pressure: inner.bytes.saturating_add(inner.external_bytes) >= self.config.max_bytes
                    || inner.rejected > 0,
            },
        }
    }
    pub fn clear(&self, sequence: SequenceId, range: Option<(Tick, Tick)>) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let keys: Vec<_> = inner
            .entries
            .iter()
            .filter(|(_, entry)| {
                let span = entry.chunk.signature.span;
                entry.chunk.signature.context.sequence == sequence
                    && range.is_none_or(|(start, end)| span.start < end && start < span.end)
            })
            .map(|(key, _)| *key)
            .collect();
        for key in &keys {
            if let Some(entry) = inner.entries.remove(key) {
                entry.chunk.retired.store(true, Ordering::Relaxed);
                inner.retired.push(entry.chunk);
            }
        }
        for (_, status) in inner.current.values_mut().filter(|(_, status)| {
            status.sequence == sequence
                && range.is_none_or(|(start, end)| status.start < end && start < status.end)
        }) {
            status.state = ChunkState::NotRendered;
            status.frame = 0;
        }
        collect_retired(&mut inner);
        keys.len()
    }

    /// Stage into an unindexed private directory. A unique directory containing
    /// media + manifest is renamed atomically, so readers never see half a pair.
    pub(crate) fn staging(&self, signature: ChunkSignature) -> Result<ChunkStaging, PreviewError> {
        signature.validate()?;
        if self.available_bytes() == 0 {
            return Err(PreviewError::BudgetExhausted);
        }
        let directory = self.root.join(format!(".staging-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let path = directory.join(format!("media.{}", signature.context.profile.extension()));
        Ok(ChunkStaging {
            signature,
            directory,
            path,
        })
    }
    pub(crate) fn commit(
        &self,
        staging: ChunkStaging,
        cancel: &AtomicBool,
    ) -> Result<Arc<PreviewChunk>, PreviewError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(PreviewError::Cancelled);
        }
        let media_stamp = file_stamp(&staging.path)?;
        let manifest = Manifest {
            signature: staging.signature.clone(),
            media_bytes: media_stamp.len,
            media_hash: hash_file(&staging.path)?,
        };
        let bytes = serde_json::to_vec(&manifest)?;
        let size = media_stamp.len.saturating_add(bytes.len() as u64);
        let external = external_cache_bytes(&self.sidecar_root, &self.root);
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.external_bytes = external;
        collect_retired(&mut inner);
        if let Some(existing) = inner.entries.get(&staging.signature.key) {
            if existing.chunk.signature == staging.signature && existing.chunk.readable() {
                return Ok(Arc::clone(&existing.chunk));
            }
        }
        if size.saturating_add(external) > self.config.max_bytes {
            inner.rejected = inner.rejected.saturating_add(1);
            return Err(PreviewError::BudgetExhausted);
        }
        if inner.entries.contains_key(&staging.signature.key) {
            retire_entry(&mut inner, staging.signature.key);
            collect_retired(&mut inner);
        }
        enforce_budget(&mut inner, self.config, size);
        if inner.bytes.saturating_add(external).saturating_add(size) > self.config.max_bytes
            || inner.entries.len() >= self.config.max_entries
        {
            inner.rejected = inner.rejected.saturating_add(1);
            return Err(PreviewError::BudgetExhausted);
        }
        let manifest_path = staging.directory.join("manifest.json");
        let mut file = std::fs::File::create(&manifest_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::File::open(&staging.path)?.sync_all()?;
        drop(file);
        let destination = self.directory(staging.signature.key);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(PreviewError::Cancelled);
        }
        // Immutable content paths are never overwritten, preserving readers.
        if destination.exists() {
            return Err(PreviewError::Invalid(
                "unindexed preview destination already exists".into(),
            ));
        }
        std::fs::rename(&staging.directory, &destination)?;
        let path = destination.join(
            staging
                .path
                .file_name()
                .ok_or_else(|| PreviewError::Invalid("preview path has no filename".into()))?,
        );
        let chunk = Arc::new(PreviewChunk {
            signature: staging.signature.clone(),
            path,
            directory: destination,
            media_stamp,
            bytes: size,
            retired: AtomicBool::new(false),
        });
        inner.clock = inner.clock.wrapping_add(1);
        let used = inner.clock;
        inner.bytes = inner.bytes.saturating_add(size);
        inner.entries.insert(
            chunk.signature.key,
            Entry {
                chunk: Arc::clone(&chunk),
                used,
            },
        );
        put_status(
            &mut inner,
            self.config.max_entries,
            &chunk.signature,
            ChunkState::Rendered,
            chunk.signature.span.frame_count,
            None,
        );
        Ok(chunk)
    }
}

pub(crate) struct ChunkStaging {
    pub signature: ChunkSignature,
    pub directory: PathBuf,
    pub path: PathBuf,
}
impl Drop for ChunkStaging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn put_status(
    inner: &mut CacheInner,
    capacity: usize,
    signature: &ChunkSignature,
    state: ChunkState,
    frame: u64,
    error: Option<String>,
) {
    inner.clock = inner.clock.wrapping_add(1);
    let key = (
        signature.context.sequence,
        signature.context.format_index,
        signature.span.start,
    );
    if !inner.current.contains_key(&key) && inner.current.len() >= capacity {
        if let Some(oldest) = inner
            .current
            .iter()
            .min_by_key(|(_, (used, _))| used)
            .map(|(key, _)| *key)
        {
            inner.current.remove(&oldest);
        }
    }
    inner.current.insert(
        key,
        (
            inner.clock,
            ChunkStatus {
                sequence: signature.context.sequence,
                format_index: signature.context.format_index,
                start: signature.span.start,
                end: signature.span.end,
                state,
                frame: frame.min(signature.span.frame_count),
                total: signature.span.frame_count,
                error,
            },
        ),
    );
}
fn retire_entry(inner: &mut CacheInner, key: ChunkKey) {
    if let Some(entry) = inner.entries.remove(&key) {
        entry.chunk.retired.store(true, Ordering::Relaxed);
        inner.retired.push(entry.chunk);
    }
}
fn collect_retired(inner: &mut CacheInner) {
    inner.retired.retain(|chunk| {
        if Arc::strong_count(chunk) == 1 {
            inner.bytes = inner.bytes.saturating_sub(chunk.bytes);
            false
        } else {
            true
        }
    });
}
fn enforce_budget(inner: &mut CacheInner, config: CacheConfig, incoming: u64) {
    while inner
        .bytes
        .saturating_add(inner.external_bytes)
        .saturating_add(incoming)
        > config.max_bytes
        || inner
            .entries
            .len()
            .saturating_add(usize::from(incoming > 0))
            > config.max_entries
    {
        let victim = inner
            .entries
            .iter()
            .filter(|(_, entry)| Arc::strong_count(&entry.chunk) == 1)
            .min_by_key(|(_, entry)| {
                let span = entry.chunk.signature.span;
                let protected = inner
                    .zones
                    .get(&entry.chunk.signature.context.sequence)
                    .is_some_and(|zones| {
                        zones
                            .iter()
                            .any(|(start, end)| span.start < *end && *start < span.end)
                    });
                (protected, entry.used)
            })
            .map(|(key, _)| *key);
        let Some(victim) = victim else {
            break;
        };
        if let Some(entry) = inner.entries.remove(&victim) {
            inner.bytes = inner.bytes.saturating_sub(entry.chunk.bytes);
            inner.evicted = inner.evicted.saturating_add(1);
            entry.chunk.retired.store(true, Ordering::Relaxed);
            let slot = (victim.sequence, victim.format_index, victim.start);
            if let Some((_, status)) = inner.current.get_mut(&slot) {
                status.state = ChunkState::NotRendered;
                status.frame = 0;
            }
        }
    }
}
fn directories(root: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir())
                .map(|_| entry.path())
        })
}
fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Xxh3::new();
    let mut block = [0u8; 65536];
    loop {
        let size = file.read(&mut block)?;
        if size == 0 {
            break;
        }
        hash.update(&block[..size]);
    }
    Ok(format!("{:032x}", hash.digest128()))
}
fn external_cache_bytes(root: &Path, preview: &Path) -> u64 {
    let mut bytes = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path == preview {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                bytes = bytes.saturating_add(entry.metadata().map(|meta| meta.len()).unwrap_or(0));
            }
        }
    }
    bytes
}

struct InvalidChunkCleanup(Option<PathBuf>);
impl Drop for InvalidChunkCleanup {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}
