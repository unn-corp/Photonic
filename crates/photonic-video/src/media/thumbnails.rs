//! Clip thumbnails & waveform loading (spec 15 — NLE parity gap 10).
//!
//! Reference NLEs draw frame thumbnails along video clips and a min/max
//! waveform along audio clips so a user reads the timeline at a glance. This
//! module is the **engine half** of that: a background-generated, sidecar-cached
//! store the GUI *reads* from. The draw thread never blocks — a cache miss draws
//! the flat clip fill and a thumbnail decode / waveform build happens on a
//! worker thread, landing in the cache for a later frame.
//!
//! ## What lives here
//! - [`ThumbnailCache`] — a bounded in-memory LRU of low-res RGBA thumbnails
//!   ([`RgbaThumb`]), backed by disk in the project sidecar cache dir
//!   (`<project>.photon.cache/`, 01 §9). Keyed by `(AssetId, coarse source-tick
//!   bucket, target height px)`. [`ThumbnailCache::request`] returns a cached
//!   thumbnail immediately or enqueues a background decode and returns `None`
//!   this frame — it **never blocks**.
//! - [`WaveformCache`] — the thin waveform-load helper: returns an asset's
//!   [`WaveformPyramid`](crate::audio::WaveformPyramid) immediately if cached,
//!   otherwise loads/builds it in the background (via [`crate::audio::waveform`]'s
//!   sidecar cache) and returns `None` this frame.
//!
//! ## Egui-free by design (spec 15 §2)
//! Thumbnails are returned as **raw RGBA** ([`RgbaThumb`]); the GUI registers
//! them as `egui` textures on its side. Nothing here touches `egui` or `wgpu`,
//! keeping `photonic-video` a pure engine crate.
//!
//! ## Decode seam
//! The cache is generic over a [`ThumbnailSource`] (the decode provider) so it
//! is unit-testable with a deterministic fake. The production provider,
//! [`DecodeThumbnailSource`], drives a short-lived
//! [`DecodeSource`](crate::decode::DecodeSource) (keyframe seek + one frame),
//! converts YUV→RGBA and box-downscales to ~64 px tall. Likewise
//! [`WaveformSource`] abstracts the audio decode → [`build_pyramid`] the caller
//! owns (this module calls `audio::waveform`'s public API but never decodes
//! audio itself).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};
use photonic_core::timeline::{AssetId, Tick, TICKS_PER_SECOND};

use crate::audio::waveform::{self, WaveformPyramid};

// ── Public data ─────────────────────────────────────────────────────────────

/// A low-resolution, tightly-packed RGBA8 thumbnail (`width * height * 4` bytes,
/// row-major, no padding). The GUI uploads this straight to an `egui` texture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaThumb {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA, row-major.
    pub rgba: Vec<u8>,
}

/// Handle the GUI holds for a cached thumbnail. A cheap `Arc` clone; the same
/// pointer identity is stable while the thumbnail stays resident, so the GUI can
/// key its `egui` texture registry by [`Arc::as_ptr`] and re-upload only when it
/// changes.
pub type ThumbHandle = Arc<RgbaThumb>;

/// The decode provider the [`ThumbnailCache`] pulls frames from (spec 15 §2).
///
/// Both methods are called **only on the background worker thread**, so
/// [`decode_thumbnail`](Self::decode_thumbnail) may block on a decode.
/// [`asset_hash`](Self::asset_hash) must be **cheap** (it gates the sidecar disk
/// lookup done before every decode) — return the asset's already-known
/// content-hash, never re-probe.
pub trait ThumbnailSource: Send + Sync {
    /// Stable content-hash disk-cache identity for `asset` (01 §3/§9), or `None`
    /// if the asset is offline/unknown — such thumbnails are memory-only.
    fn asset_hash(&self, asset: AssetId) -> Option<String>;

    /// Decode one frame at/after `source_tick` for `asset`, converted to RGBA
    /// and scaled so its height is `target_px`. `None` on any decode failure
    /// (offline, no ffmpeg, seek past end). May block — worker thread only.
    fn decode_thumbnail(
        &self,
        asset: AssetId,
        source_tick: Tick,
        target_px: u32,
    ) -> Option<RgbaThumb>;
}

/// The audio provider the [`WaveformCache`] builds pyramids from. Called only on
/// the background worker thread; [`build_pyramid`](Self::build_pyramid) may block
/// on a full audio decode.
pub trait WaveformSource: Send + Sync {
    /// Stable content-hash disk-cache identity for `asset`, or `None` if
    /// offline/unknown (then the pyramid is memory-only, not persisted).
    fn asset_hash(&self, asset: AssetId) -> Option<String>;

    /// Decode `asset`'s audio once and build its peak pyramid, or `None` if the
    /// asset has no decodable audio. May block — worker thread only.
    fn build_pyramid(&self, asset: AssetId) -> Option<WaveformPyramid>;
}

/// Tuning for a [`ThumbnailCache`].
#[derive(Clone, Copy, Debug)]
pub struct ThumbnailConfig {
    /// Max resident thumbnails before LRU eviction (spec 15 §2: ~256).
    pub capacity: usize,
    /// Source-tick width of one thumbnail bucket. Requests whose source ticks
    /// fall in the same bucket share one cached thumbnail (spec 15 §2 "coarse
    /// bucket"). Default ≈ 0.5 s.
    pub bucket_ticks: i64,
    /// After a failed decode, suppress re-enqueue of the same key for this long
    /// so an offline / un-decodable asset can't storm the worker with an ffmpeg
    /// spawn every frame.
    pub retry_cooldown: Duration,
    /// Upper bound on queued + running decode work. This prevents a wide
    /// timeline from turning a scroll/zoom into an unbounded ffmpeg backlog;
    /// visible requests are retried naturally as earlier work finishes.
    pub max_inflight: usize,
}

impl Default for ThumbnailConfig {
    fn default() -> Self {
        ThumbnailConfig {
            capacity: 256,
            bucket_ticks: TICKS_PER_SECOND / 2,
            retry_cooldown: Duration::from_secs(2),
            max_inflight: 32,
        }
    }
}

// ── Thumbnail cache ─────────────────────────────────────────────────────────

/// A bounded in-memory LRU of RGBA thumbnails backed by the sidecar disk cache.
///
/// See the [module docs](self). Construct with a [`ThumbnailSource`]; call
/// [`request`](Self::request) from the draw path (never blocks). Drops cleanly:
/// the worker thread exits when the cache is dropped.
pub struct ThumbnailCache {
    store: AsyncStore<ThumbKey, RgbaThumb>,
    bucket_ticks: i64,
    cache_dir: Arc<Mutex<PathBuf>>,
}

/// In-memory + disk cache key. `bucket` = `source_tick / bucket_ticks`; `px` is
/// the target thumbnail height so different zoom-tiers don't collide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ThumbKey {
    asset: AssetId,
    bucket: i64,
    px: u32,
}

impl ThumbnailCache {
    /// Build with default tuning ([`ThumbnailConfig::default`]). `cache_dir` is
    /// the project sidecar dir (`<project>.photon.cache/`,
    /// [`cache_dir_for_project`](crate::media::cache_dir_for_project)).
    pub fn new(source: Arc<dyn ThumbnailSource>, cache_dir: PathBuf) -> Self {
        Self::with_config(source, cache_dir, ThumbnailConfig::default())
    }

    /// Build with explicit tuning.
    pub fn with_config(
        source: Arc<dyn ThumbnailSource>,
        cache_dir: PathBuf,
        cfg: ThumbnailConfig,
    ) -> Self {
        let bucket_ticks = cfg.bucket_ticks.max(1);
        let cache_dir = Arc::new(Mutex::new(cache_dir));
        let producer_cache_dir = Arc::clone(&cache_dir);
        let producer: Producer<ThumbKey, RgbaThumb> = Box::new(move |key: &ThumbKey| {
            let hash = source.asset_hash(key.asset);
            let cache_dir = producer_cache_dir.lock().unwrap().clone();
            // Disk before decode: a sidecar-cached thumbnail survives reopen and
            // is orders of magnitude cheaper than an ffmpeg seek.
            if let Some(h) = &hash {
                if let Some(t) = load_thumb_from_disk(&cache_dir, h, key.bucket, key.px) {
                    return Some(Arc::new(t));
                }
            }
            let tick = Tick(key.bucket.saturating_mul(bucket_ticks));
            let thumb = source.decode_thumbnail(key.asset, tick, key.px)?;
            if let Some(h) = &hash {
                let _ = save_thumb_to_disk(&cache_dir, h, key.bucket, key.px, &thumb);
            }
            Some(Arc::new(thumb))
        });
        ThumbnailCache {
            store: AsyncStore::new(cfg.capacity, cfg.retry_cooldown, cfg.max_inflight, producer),
            bucket_ticks,
            cache_dir,
        }
    }

    /// The thumbnail for `asset` nearest `source_tick` at target height `px`, or
    /// `None` this frame (a background decode is enqueued on the first miss).
    ///
    /// **Never blocks.** A `None` return means "draw the flat fill and ask
    /// again next frame". `source_tick` should already respect the clip's trim
    /// and speed (map slice-x → source tick before calling).
    pub fn request(&self, asset: AssetId, source_tick: Tick, px: u32) -> Option<ThumbHandle> {
        let key = ThumbKey {
            asset,
            bucket: source_tick.0.div_euclid(self.bucket_ticks),
            px,
        };
        self.store.get(key)
    }

    /// Block until every queued/in-flight thumbnail job has finished. For
    /// shutdown, deterministic export, or tests — **never call from the draw
    /// thread** (it blocks on decode).
    pub fn block_until_idle(&self) {
        self.store.block_until_idle();
    }

    /// Number of resident thumbnails (diagnostics / tests).
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Whether the in-memory cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether visible thumbnail requests are still being decoded. UI hosts
    /// use this to request a modest delayed repaint rather than a permanent
    /// polling timer.
    pub fn has_pending(&self) -> bool {
        self.store.has_pending()
    }

    /// Switch the sidecar directory used by future jobs and discard all
    /// resident or failed entries. In-flight jobs from the previous identity
    /// are allowed to finish but their results are discarded by `AsyncStore`.
    pub fn set_cache_dir(&self, cache_dir: PathBuf) {
        *self.cache_dir.lock().unwrap() = cache_dir;
        self.store.invalidate();
    }

    /// Invalidate resident and failed entries without changing the sidecar
    /// directory.
    pub fn invalidate(&self) {
        self.store.invalidate();
    }
}

// ── Waveform cache (thin helper) ────────────────────────────────────────────

/// Max resident waveform pyramids (one per audio asset — far fewer than
/// thumbnails).
const WAVEFORM_CAPACITY: usize = 64;

/// Background loader/builder for asset [`WaveformPyramid`]s.
///
/// [`request`](Self::request) returns a cached pyramid immediately, or `None`
/// while a background load-from-sidecar-or-build runs. The build reuses
/// [`crate::audio::waveform`]'s content-hash sidecar cache so a pyramid survives
/// project reopen.
pub struct WaveformCache {
    store: AsyncStore<AssetId, WaveformPyramid>,
    cache_dir: Arc<Mutex<PathBuf>>,
}

impl WaveformCache {
    /// Build with `cache_dir` = the project sidecar dir.
    pub fn new(source: Arc<dyn WaveformSource>, cache_dir: PathBuf) -> Self {
        let cache_dir = Arc::new(Mutex::new(cache_dir));
        let producer_cache_dir = Arc::clone(&cache_dir);
        let producer: Producer<AssetId, WaveformPyramid> = Box::new(move |asset: &AssetId| {
            let asset = *asset;
            let hash = source.asset_hash(asset);
            let cache_dir = producer_cache_dir.lock().unwrap().clone();
            if let Some(h) = &hash {
                if let Ok(Some(p)) = waveform::load_from_dir(&cache_dir, h) {
                    return Some(Arc::new(p));
                }
            }
            let mut pyramid = source.build_pyramid(asset)?;
            if let Some(h) = &hash {
                // Persist under the *disk-cache* hash so the next load hits
                // regardless of what the source stamped on the pyramid.
                pyramid.asset_hash = h.clone();
                let _ = waveform::save_to_dir(&pyramid, &cache_dir);
            }
            Some(Arc::new(pyramid))
        });
        WaveformCache {
            store: AsyncStore::new(WAVEFORM_CAPACITY, Duration::from_secs(2), 4, producer),
            cache_dir,
        }
    }

    /// The pyramid for `asset`, or `None` this frame (a background build is
    /// enqueued on the first miss). **Never blocks.**
    pub fn request(&self, asset: AssetId) -> Option<Arc<WaveformPyramid>> {
        self.store.get(asset)
    }

    /// Block until every queued/in-flight waveform build has finished (shutdown /
    /// export / tests only — blocks).
    pub fn block_until_idle(&self) {
        self.store.block_until_idle();
    }

    /// Whether waveform sidecar loading/building is still in flight.
    pub fn has_pending(&self) -> bool {
        self.store.has_pending()
    }

    /// Switch the sidecar directory used by future jobs and discard all
    /// resident or failed entries.
    pub fn set_cache_dir(&self, cache_dir: PathBuf) {
        *self.cache_dir.lock().unwrap() = cache_dir;
        self.store.invalidate();
    }

    /// Invalidate resident and failed entries without changing the sidecar
    /// directory.
    pub fn invalidate(&self) {
        self.store.invalidate();
    }
}

// ── Decode-backed thumbnail provider (production) ───────────────────────────

/// Everything the [`DecodeThumbnailSource`] needs to open one asset. The caller
/// (which owns the media pool + probes) supplies this cheaply per asset — it
/// must **not** re-probe on each call (see [`ThumbnailSource::asset_hash`]).
pub struct ThumbnailDecodeSpec {
    /// Decode parameters (input path, dims, pix fmt, pts model, keyframe index).
    pub params: crate::decode::scheduler::SourceParams,
    /// The resolved ffmpeg toolchain for this session.
    pub tools: crate::media::ffmpeg_locate::FfmpegTools,
    /// Content hash for the sidecar disk key, if known.
    pub content_hash: Option<String>,
}

type ThumbnailResolver = Box<dyn Fn(AssetId) -> Option<ThumbnailDecodeSpec> + Send + Sync>;

/// The production [`ThumbnailSource`]: seek a short-lived
/// [`DecodeSource`](crate::decode::DecodeSource) to the requested tick, decode
/// one frame, convert YUV→RGBA and box-downscale to the target height.
pub struct DecodeThumbnailSource {
    resolve: ThumbnailResolver,
}

impl DecodeThumbnailSource {
    /// `resolve` maps an [`AssetId`] to its decode spec (or `None` if offline).
    /// It is called on the worker thread and must be cheap (a media-pool lookup,
    /// not a probe).
    pub fn new(
        resolve: impl Fn(AssetId) -> Option<ThumbnailDecodeSpec> + Send + Sync + 'static,
    ) -> Self {
        DecodeThumbnailSource {
            resolve: Box::new(resolve),
        }
    }
}

impl ThumbnailSource for DecodeThumbnailSource {
    fn asset_hash(&self, asset: AssetId) -> Option<String> {
        (self.resolve)(asset)?.content_hash
    }

    fn decode_thumbnail(
        &self,
        asset: AssetId,
        source_tick: Tick,
        target_px: u32,
    ) -> Option<RgbaThumb> {
        let spec = (self.resolve)(asset)?;
        // One-shot scaled ffmpeg: seek + single frame + scale to `target_px`
        // tall, RGBA straight out of the pipe. Avoids the old path of opening a
        // full-res DecodeSource, decoding a 4K YUV frame into the process, then
        // software-box-scaling it — that was multi-second per thumb on 4K60
        // drone footage and starved the playback decode workers.
        let use_cuda = crate::decode::sidecar::cuda_hwaccel_listed(&spec.tools.ffmpeg);
        let decode = |tick| {
            decode_thumbnail_scaled(&spec, tick, target_px, use_cuda).or_else(|| {
                use_cuda
                    .then(|| decode_thumbnail_scaled(&spec, tick, target_px, false))
                    .flatten()
            })
        };
        decode(source_tick).or_else(|| decode(Tick::ZERO))
    }
}

/// Spawn a short-lived ffmpeg that seeks near `source_tick`, grabs one frame,
/// scales it to `target_px` tall (even width), and writes RGBA to stdout.
fn decode_thumbnail_scaled(
    spec: &ThumbnailDecodeSpec,
    source_tick: Tick,
    target_px: u32,
    use_cuda: bool,
) -> Option<RgbaThumb> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let th = target_px.max(16);
    let src_w = spec.params.width.max(1);
    let src_h = spec.params.height.max(1);
    // Same rounding as the old box-downscale path: nearest width that keeps
    // aspect at height `th` (half-up). RGBA has no even-width requirement.
    let tw = ((((src_w as u64 * th as u64) + src_h as u64 / 2) / src_h as u64) as u32).max(1);
    let seek_secs = format!("{:.6}", source_tick.as_seconds_f64().max(0.0));
    let vf = format!("scale={tw}:{th}");

    let mut cmd = Command::new(&spec.tools.ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
    // `-hwaccel cuda` without a cuda filter chain still accelerates the decode
    // then downloads frames to system memory — enough for one thumb frame.
    if use_cuda {
        cmd.args(["-hwaccel", "cuda"]);
    }
    cmd.arg("-ss")
        .arg(&seek_secs)
        .arg("-i")
        .arg(&spec.params.input)
        .args([
            "-frames:v",
            "1",
            "-an",
            "-vf",
            &vf,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
        ])
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::media::child_registry::arm_parent_death_signal(&mut cmd);

    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let need = (tw as usize).checked_mul(th as usize)?.checked_mul(4)?;
    let mut rgba = vec![0u8; need];
    if stdout.read_exact(&mut rgba).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    if !child.wait().ok()?.success() {
        return None;
    }
    Some(RgbaThumb {
        width: tw,
        height: th,
        rgba,
    })
}

// ── YUV → RGBA + box downscale ──────────────────────────────────────────────

/// Convert one decoded frame's planes to full-resolution RGBA8 (BT.601
/// limited-range coefficients — exact colorimetry is immaterial at thumbnail
/// scale). Alpha comes from the `a` plane for 4:4:4:A sources, else opaque.
#[cfg(test)]
fn planes_to_rgba(planes: &crate::decode::DecodedPlanes) -> RgbaThumb {
    use crate::decode::DecodedPlanes;

    let (w, h) = planes.dims();
    let (wu, hu) = (w as usize, h as usize);
    let mut rgba = vec![0u8; wu * hu * 4];
    match planes {
        DecodedPlanes::Yuv420 { .. } => {
            let (y, cb, cr) = (planes.y(), planes.cb(), planes.cr());
            let cw = wu.div_ceil(2);
            for py in 0..hu {
                let crow = (py / 2) * cw;
                for px in 0..wu {
                    let ci = crow + px / 2;
                    let (r, g, b) = yuv_to_rgb(y[py * wu + px], cb[ci], cr[ci]);
                    let o = (py * wu + px) * 4;
                    rgba[o] = r;
                    rgba[o + 1] = g;
                    rgba[o + 2] = b;
                    rgba[o + 3] = 255;
                }
            }
        }
        DecodedPlanes::Yuva444 { .. } => {
            let (y, cb, cr, a) = (
                planes.y(),
                planes.cb(),
                planes.cr(),
                planes.a().expect("YUVA thumbnail frame has alpha"),
            );
            for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
                let (r, g, b) = yuv_to_rgb(y[i], cb[i], cr[i]);
                chunk[0] = r;
                chunk[1] = g;
                chunk[2] = b;
                chunk[3] = a[i];
            }
        }
    }
    RgbaThumb {
        width: w,
        height: h,
        rgba,
    }
}

#[inline]
#[cfg(test)]
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    let yf = (y as f32 - 16.0) * 1.164;
    let uf = u as f32 - 128.0;
    let vf = v as f32 - 128.0;
    (
        clamp8(yf + 1.596 * vf),
        clamp8(yf - 0.813 * vf - 0.391 * uf),
        clamp8(yf + 2.018 * uf),
    )
}

#[inline]
#[cfg(test)]
fn clamp8(x: f32) -> u8 {
    x.clamp(0.0, 255.0) as u8
}

/// Box-filter downscale `src` so its height is `target_h` (width scaled to keep
/// aspect). Never upscales — if the source is already ≤ `target_h` tall it is
/// returned unchanged.
#[cfg(test)]
fn downscale_rgba(src: &RgbaThumb, target_h: u32) -> RgbaThumb {
    let th = target_h.max(1);
    if src.width == 0 || src.height == 0 {
        return RgbaThumb {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
    }
    if th >= src.height {
        return src.clone();
    }
    let (sw, sh) = (src.width as usize, src.height as usize);
    let thh = th as usize;
    let tw = (((src.width as u64 * th as u64) + src.height as u64 / 2) / src.height as u64).max(1)
        as usize;
    let mut out = vec![0u8; tw * thh * 4];
    for dy in 0..thh {
        let sy0 = dy * sh / thh;
        let sy1 = (((dy + 1) * sh) / thh).clamp(sy0 + 1, sh);
        for dx in 0..tw {
            let sx0 = dx * sw / tw;
            let sx1 = (((dx + 1) * sw) / tw).clamp(sx0 + 1, sw);
            let (mut r, mut g, mut b, mut a, mut cnt) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for yy in sy0..sy1 {
                let row = yy * sw;
                for xx in sx0..sx1 {
                    let o = (row + xx) * 4;
                    r += src.rgba[o] as u32;
                    g += src.rgba[o + 1] as u32;
                    b += src.rgba[o + 2] as u32;
                    a += src.rgba[o + 3] as u32;
                    cnt += 1;
                }
            }
            let cnt = cnt.max(1);
            let o = (dy * tw + dx) * 4;
            out[o] = (r / cnt) as u8;
            out[o + 1] = (g / cnt) as u8;
            out[o + 2] = (b / cnt) as u8;
            out[o + 3] = (a / cnt) as u8;
        }
    }
    RgbaThumb {
        width: tw as u32,
        height: th,
        rgba: out,
    }
}

// ── Sidecar disk format ─────────────────────────────────────────────────────

/// Magic + version for the raw thumbnail sidecar file. A tiny raw container
/// (magic, dims, len, RGBA) rather than PNG/JPEG: no codec dependency and a
/// trivially-correct round-trip — the sidecar is rebuildable so the on-disk
/// format is a private detail (spec 15 §4).
const THUMB_MAGIC: &[u8; 4] = b"PTH1";
const THUMB_HEADER_LEN: usize = 16;

fn thumb_disk_path(cache_dir: &Path, hash: &str, bucket: i64, px: u32) -> PathBuf {
    cache_dir.join(format!("{hash}.{bucket}.{px}.thumb"))
}

fn save_thumb_to_disk(
    cache_dir: &Path,
    hash: &str,
    bucket: i64,
    px: u32,
    thumb: &RgbaThumb,
) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let mut buf = Vec::with_capacity(THUMB_HEADER_LEN + thumb.rgba.len());
    buf.extend_from_slice(THUMB_MAGIC);
    buf.extend_from_slice(&thumb.width.to_le_bytes());
    buf.extend_from_slice(&thumb.height.to_le_bytes());
    buf.extend_from_slice(&(thumb.rgba.len() as u32).to_le_bytes());
    buf.extend_from_slice(&thumb.rgba);
    // Temp-and-rename (37 §2.3): a crash mid-write never leaves a truncated
    // thumbnail that a later read would accept as valid.
    crate::media::atomic_write::write_atomic(&thumb_disk_path(cache_dir, hash, bucket, px), &buf)
}

fn load_thumb_from_disk(cache_dir: &Path, hash: &str, bucket: i64, px: u32) -> Option<RgbaThumb> {
    let bytes = std::fs::read(thumb_disk_path(cache_dir, hash, bucket, px)).ok()?;
    if bytes.len() < THUMB_HEADER_LEN || &bytes[0..4] != THUMB_MAGIC {
        return None;
    }
    let width = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let len = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
    let rgba = bytes
        .get(THUMB_HEADER_LEN..THUMB_HEADER_LEN + len)?
        .to_vec();
    if rgba.len() != (width as usize) * (height as usize) * 4 {
        return None;
    }
    Some(RgbaThumb {
        width,
        height,
        rgba,
    })
}

// ── Generic background LRU store ────────────────────────────────────────────
//
// Shared machinery behind both caches: a bounded in-memory LRU, single-flight
// dedup of in-progress keys, a background worker running the `producer`, and a
// short negative-cooldown so a failing key can't storm the worker. Factored out
// so the thumbnail and waveform caches don't each re-implement the (subtle)
// threading + eviction.

type Producer<K, V> = Box<dyn Fn(&K) -> Option<Arc<V>> + Send>;

struct AsyncStore<K, V> {
    shared: Arc<Shared<K, V>>,
    cooldown: Duration,
    max_inflight: usize,
    tx: Option<Sender<Job<K>>>,
    worker: Option<JoinHandle<()>>,
}

struct Shared<K, V> {
    state: Mutex<State<K, V>>,
    idle: Condvar,
}

struct State<K, V> {
    lru: Lru<K, V>,
    /// Keys queued or being produced right now (single-flight).
    inflight: HashMap<K, u64>,
    /// Keys whose last production failed, with the time it failed (cooldown).
    failed: HashMap<K, Instant>,
    /// Results from an older cache identity must not repopulate the cache.
    generation: u64,
}

struct Job<K> {
    key: K,
    generation: u64,
}

impl<K, V> AsyncStore<K, V>
where
    K: Eq + std::hash::Hash + Clone + Send + 'static,
    V: Send + Sync + 'static,
{
    fn new(
        capacity: usize,
        cooldown: Duration,
        max_inflight: usize,
        producer: Producer<K, V>,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                lru: Lru::new(capacity),
                inflight: HashMap::new(),
                failed: HashMap::new(),
                generation: 0,
            }),
            idle: Condvar::new(),
        });
        let (tx, rx) = unbounded::<Job<K>>();
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("photonic-thumbnail-worker".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    // Run the (possibly slow) producer OUTSIDE the lock.
                    let produced = producer(&job.key);
                    let mut st = worker_shared.state.lock().unwrap();
                    if st.inflight.get(&job.key).copied() == Some(job.generation) {
                        st.inflight.remove(&job.key);
                    }
                    if job.generation == st.generation {
                        match produced {
                            Some(v) => {
                                st.failed.remove(&job.key);
                                st.lru.insert(job.key, v);
                            }
                            None => {
                                st.failed.insert(job.key, Instant::now());
                            }
                        }
                    }
                    if st.inflight.is_empty() {
                        worker_shared.idle.notify_all();
                    }
                }
            })
            .expect("spawn thumbnail worker thread");
        AsyncStore {
            shared,
            cooldown,
            max_inflight: max_inflight.max(1),
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    /// Memory-cache hit → `Some`; otherwise enqueue a background job (once) and
    /// return `None`. Never blocks on I/O.
    fn get(&self, key: K) -> Option<Arc<V>> {
        let mut st = self.shared.state.lock().unwrap();
        if let Some(v) = st.lru.get(&key) {
            return Some(v);
        }
        if st.inflight.contains_key(&key) {
            return None; // already queued — don't double-enqueue
        }
        if let Some(&at) = st.failed.get(&key) {
            if at.elapsed() < self.cooldown {
                return None; // still cooling down after a failure
            }
            st.failed.remove(&key);
        }
        if st.inflight.len() >= self.max_inflight {
            return None; // bounded background work; a later visible frame retries
        }
        let generation = st.generation;
        st.inflight.insert(key.clone(), generation);
        drop(st);
        // Unbounded channel: send never blocks. If the worker is gone (shutdown),
        // undo the inflight bookkeeping so `block_until_idle` can't hang.
        if let Some(tx) = &self.tx {
            if tx
                .send(Job {
                    key: key.clone(),
                    generation,
                })
                .is_err()
            {
                let mut st = self.shared.state.lock().unwrap();
                if st.inflight.get(&key).copied() == Some(generation) {
                    st.inflight.remove(&key);
                }
                if st.inflight.is_empty() {
                    self.shared.idle.notify_all();
                }
            }
        }
        None
    }

    fn block_until_idle(&self) {
        let mut st = self.shared.state.lock().unwrap();
        while !st.inflight.is_empty() {
            st = self.shared.idle.wait(st).unwrap();
        }
    }

    fn len(&self) -> usize {
        self.shared.state.lock().unwrap().lru.len()
    }

    fn has_pending(&self) -> bool {
        !self.shared.state.lock().unwrap().inflight.is_empty()
    }

    fn invalidate(&self) {
        let mut st = self.shared.state.lock().unwrap();
        st.generation = st.generation.wrapping_add(1);
        st.lru.clear();
        st.failed.clear();
        st.inflight.clear();
        self.shared.idle.notify_all();
    }
}

impl<K, V> Drop for AsyncStore<K, V> {
    fn drop(&mut self) {
        // Drop the sender first so the worker's `recv` returns `Err` and it
        // exits, then join it.
        self.tx.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// A bounded LRU map. Recency is a monotonic counter stamped on each entry;
/// eviction scans for the smallest stamp (O(capacity), only on insert-over-cap).
struct Lru<K, V> {
    map: HashMap<K, (Arc<V>, u64)>,
    cap: usize,
    clock: u64,
}

impl<K: Eq + std::hash::Hash + Clone, V> Lru<K, V> {
    fn new(cap: usize) -> Self {
        Lru {
            map: HashMap::new(),
            cap: cap.max(1),
            clock: 0,
        }
    }

    /// Return the value and mark it most-recently-used.
    fn get(&mut self, key: &K) -> Option<Arc<V>> {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.map.get_mut(key)?;
        entry.1 = clock;
        Some(Arc::clone(&entry.0))
    }

    fn insert(&mut self, key: K, value: Arc<V>) {
        self.clock += 1;
        if !self.map.contains_key(&key) && self.map.len() >= self.cap {
            if let Some(victim) = self
                .map
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&victim);
            }
        }
        self.map.insert(key, (value, self.clock));
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn clear(&mut self) {
        self.map.clear();
    }

    #[cfg(test)]
    fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "photonic-thumbs-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    // ── Fake sources ────────────────────────────────────────────────────────

    struct FakeThumbSource {
        decodes: Arc<AtomicUsize>,
        hash: Option<String>,
    }
    impl ThumbnailSource for FakeThumbSource {
        fn asset_hash(&self, _asset: AssetId) -> Option<String> {
            self.hash.clone()
        }
        fn decode_thumbnail(&self, _asset: AssetId, tick: Tick, px: u32) -> Option<RgbaThumb> {
            self.decodes.fetch_add(1, Ordering::SeqCst);
            let side = px.max(1);
            let byte = (tick.0 & 0xff) as u8;
            Some(RgbaThumb {
                width: side,
                height: side,
                rgba: vec![byte; (side * side * 4) as usize],
            })
        }
    }

    struct BlockingThumbSource {
        started: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
        decodes: Arc<AtomicUsize>,
    }

    impl ThumbnailSource for BlockingThumbSource {
        fn asset_hash(&self, _asset: AssetId) -> Option<String> {
            None
        }

        fn decode_thumbnail(&self, _asset: AssetId, _tick: Tick, px: u32) -> Option<RgbaThumb> {
            self.started.store(true, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            self.decodes.fetch_add(1, Ordering::SeqCst);
            Some(RgbaThumb {
                width: px,
                height: px,
                rgba: vec![0; (px * px * 4) as usize],
            })
        }
    }

    /// A source whose `decode_thumbnail` must never be called (asserts the disk
    /// or memory path served the request).
    struct PanicThumbSource {
        hash: Option<String>,
    }
    impl ThumbnailSource for PanicThumbSource {
        fn asset_hash(&self, _asset: AssetId) -> Option<String> {
            self.hash.clone()
        }
        fn decode_thumbnail(&self, _a: AssetId, _t: Tick, _p: u32) -> Option<RgbaThumb> {
            panic!("decode_thumbnail must not run — should have hit the disk cache");
        }
    }

    // ── Cache behaviour ───────────────────────────────────────────────────────

    #[test]
    fn request_miss_enqueues_then_hits() {
        let decodes = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(FakeThumbSource {
            decodes: Arc::clone(&decodes),
            hash: None, // memory-only, isolate from disk
        });
        let cache = ThumbnailCache::new(source, temp_dir("miss-hit"));
        let asset = AssetId::new();

        // First look: miss → enqueue, no thumbnail this frame.
        assert!(cache.request(asset, Tick(1_000), 64).is_none());
        cache.block_until_idle();

        // Now resident.
        let handle = cache
            .request(asset, Tick(1_000), 64)
            .expect("cached after background decode");
        assert_eq!(handle.height, 64);
        assert_eq!(handle.width, 64);
        assert_eq!(decodes.load(Ordering::SeqCst), 1);

        // A second tick in the SAME bucket reuses the one decode.
        let same_bucket = Tick(1_000 + super::TICKS_PER_SECOND / 4);
        assert!(cache.request(asset, same_bucket, 64).is_some());
        assert_eq!(
            decodes.load(Ordering::SeqCst),
            1,
            "same bucket, no re-decode"
        );
    }

    #[test]
    fn invalidate_discards_resident_thumbnail_and_allows_redecode() {
        let decodes = Arc::new(AtomicUsize::new(0));
        let cache = ThumbnailCache::new(
            Arc::new(FakeThumbSource {
                decodes: Arc::clone(&decodes),
                hash: None,
            }),
            temp_dir("invalidate"),
        );
        let asset = AssetId::new();

        cache.request(asset, Tick(1_000), 64);
        cache.block_until_idle();
        assert!(cache.request(asset, Tick(1_000), 64).is_some());
        assert_eq!(decodes.load(Ordering::SeqCst), 1);

        cache.invalidate();
        assert!(cache.request(asset, Tick(1_000), 64).is_none());
        cache.block_until_idle();
        assert!(cache.request(asset, Tick(1_000), 64).is_some());
        assert_eq!(decodes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn invalidate_discards_an_inflight_old_generation_result() {
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let decodes = Arc::new(AtomicUsize::new(0));
        let cache = ThumbnailCache::new(
            Arc::new(BlockingThumbSource {
                started: Arc::clone(&started),
                release: Arc::clone(&release),
                decodes: Arc::clone(&decodes),
            }),
            temp_dir("invalidate-inflight"),
        );
        let asset = AssetId::new();

        cache.request(asset, Tick(1_000), 8);
        while !started.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        cache.invalidate();
        release.store(true, Ordering::SeqCst);
        while decodes.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }

        assert!(
            cache.request(asset, Tick(1_000), 8).is_none(),
            "a completed job from the old generation must not be resident"
        );
        cache.block_until_idle();
        assert!(cache.request(asset, Tick(1_000), 8).is_some());
        assert_eq!(decodes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn distinct_buckets_and_heights_decode_separately() {
        let decodes = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(FakeThumbSource {
            decodes: Arc::clone(&decodes),
            hash: None,
        });
        let cache = ThumbnailCache::new(source, temp_dir("distinct"));
        let asset = AssetId::new();

        cache.request(asset, Tick(0), 64);
        cache.request(asset, Tick(10 * super::TICKS_PER_SECOND), 64); // different bucket
        cache.request(asset, Tick(0), 32); // different height
        cache.block_until_idle();
        assert_eq!(decodes.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn disk_round_trip_survives_a_fresh_cache() {
        let dir = temp_dir("disk-rt");
        let asset = AssetId::new();

        // Cache A decodes once and persists to the sidecar dir.
        let decodes_a = Arc::new(AtomicUsize::new(0));
        {
            let cache_a = ThumbnailCache::new(
                Arc::new(FakeThumbSource {
                    decodes: Arc::clone(&decodes_a),
                    hash: Some("hashA".into()),
                }),
                dir.clone(),
            );
            cache_a.request(asset, Tick(0), 64);
            cache_a.block_until_idle();
            assert_eq!(decodes_a.load(Ordering::SeqCst), 1);
        }
        assert!(
            thumb_disk_path(&dir, "hashA", 0, 64).exists(),
            "thumbnail persisted to the sidecar dir"
        );

        // Cache B (fresh memory) with a source that panics if decode runs: the
        // request must be served from disk, not re-decoded.
        let cache_b = ThumbnailCache::new(
            Arc::new(PanicThumbSource {
                hash: Some("hashA".into()),
            }),
            dir.clone(),
        );
        assert!(cache_b.request(asset, Tick(0), 64).is_none());
        cache_b.block_until_idle();
        let handle = cache_b
            .request(asset, Tick(0), 64)
            .expect("loaded from disk on cache B");
        assert_eq!((handle.width, handle.height), (64, 64));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn raw_disk_helpers_round_trip() {
        let dir = temp_dir("raw-disk");
        let thumb = RgbaThumb {
            width: 3,
            height: 2,
            rgba: (0..24).collect(),
        };
        save_thumb_to_disk(&dir, "h", 7, 64, &thumb).unwrap();
        let back = load_thumb_from_disk(&dir, "h", 7, 64).expect("round-trip");
        assert_eq!(back, thumb);
        assert!(load_thumb_from_disk(&dir, "h", 99, 64).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── LRU ───────────────────────────────────────────────────────────────────

    #[test]
    fn lru_evicts_least_recently_used() {
        let mut lru: Lru<u32, u32> = Lru::new(2);
        lru.insert(1, Arc::new(10));
        lru.insert(2, Arc::new(20));
        // Touch 1 so 2 becomes the eviction victim.
        assert_eq!(lru.get(&1).map(|v| *v), Some(10));
        lru.insert(3, Arc::new(30));

        assert_eq!(lru.len(), 2);
        assert!(lru.contains(&1), "recently used survives");
        assert!(!lru.contains(&2), "least recently used evicted");
        assert!(lru.contains(&3));
    }

    #[test]
    fn lru_reinsert_of_existing_key_does_not_evict() {
        let mut lru: Lru<u32, u32> = Lru::new(2);
        lru.insert(1, Arc::new(10));
        lru.insert(2, Arc::new(20));
        lru.insert(1, Arc::new(11)); // replace, not grow
        assert_eq!(lru.len(), 2);
        assert!(lru.contains(&1) && lru.contains(&2));
        assert_eq!(lru.get(&1).map(|v| *v), Some(11));
    }

    #[test]
    fn cache_capacity_is_bounded() {
        let source = Arc::new(FakeThumbSource {
            decodes: Arc::new(AtomicUsize::new(0)),
            hash: None,
        });
        let cache = ThumbnailCache::with_config(
            source,
            temp_dir("bounded"),
            ThumbnailConfig {
                capacity: 4,
                bucket_ticks: super::TICKS_PER_SECOND / 2,
                retry_cooldown: Duration::from_secs(2),
                max_inflight: 32,
            },
        );
        let asset = AssetId::new();
        for i in 0..20 {
            cache.request(asset, Tick(i * 10 * super::TICKS_PER_SECOND), 64);
        }
        cache.block_until_idle();
        assert!(
            cache.len() <= 4,
            "LRU keeps the cache bounded (got {})",
            cache.len()
        );
    }

    // ── Colour conversion + downscale ────────────────────────────────────────

    #[test]
    fn yuv_black_and_white_convert() {
        // Limited-range black (Y=16) and white (Y=235), neutral chroma.
        let (r, g, b) = yuv_to_rgb(16, 128, 128);
        assert!(r < 4 && g < 4 && b < 4, "black ~ (0,0,0), got {r},{g},{b}");
        let (r, g, b) = yuv_to_rgb(235, 128, 128);
        assert!(
            r > 250 && g > 250 && b > 250,
            "white ~ (255,..), got {r},{g},{b}"
        );
    }

    #[test]
    fn downscale_halves_and_averages() {
        // 2x2 → target height 1 → 1x1 averaging the four pixels.
        let src = RgbaThumb {
            width: 2,
            height: 2,
            rgba: vec![
                0, 0, 0, 255, // (0,0)
                100, 100, 100, 255, // (1,0)
                100, 100, 100, 255, // (0,1)
                200, 200, 200, 255, // (1,1)
            ],
        };
        let out = downscale_rgba(&src, 1);
        assert_eq!((out.width, out.height), (1, 1));
        // mean of {0,100,100,200} = 100.
        assert_eq!(&out.rgba, &[100, 100, 100, 255]);
    }

    #[test]
    fn downscale_never_upscales() {
        let src = RgbaThumb {
            width: 4,
            height: 4,
            rgba: vec![7; 4 * 4 * 4],
        };
        let out = downscale_rgba(&src, 64);
        assert_eq!((out.width, out.height), (4, 4), "source returned unscaled");
    }

    #[test]
    fn planes_to_rgba_maps_dims_and_alpha() {
        use crate::decode::DecodedPlanes;
        let planes = DecodedPlanes::yuva444(2, 1, vec![16, 235, 128, 128, 128, 128, 10, 250]);
        let img = planes_to_rgba(&planes);
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.rgba.len(), 2 * 4); // 2 px * 1 row * 4 bytes
        assert_eq!(img.rgba[3], 10, "pixel 0 alpha from a-plane");
        assert_eq!(img.rgba[7], 250, "pixel 1 alpha from a-plane");
    }

    // ── Waveform cache ───────────────────────────────────────────────────────

    struct FakeWaveSource {
        builds: Arc<AtomicUsize>,
        hash: Option<String>,
    }
    impl FakeWaveSource {
        fn make_pyramid() -> WaveformPyramid {
            WaveformPyramid {
                asset_hash: "unstamped".into(),
                channels: 1,
                source_sample_rate: 48_000,
                total_frames: 256,
                levels: vec![waveform::WaveformLevel {
                    samples_per_bucket: 256,
                    channels: vec![vec![waveform::PeakBucket {
                        min: -0.5,
                        max: 0.5,
                        rms: 0.25,
                    }]],
                }],
            }
        }
    }
    impl WaveformSource for FakeWaveSource {
        fn asset_hash(&self, _asset: AssetId) -> Option<String> {
            self.hash.clone()
        }
        fn build_pyramid(&self, _asset: AssetId) -> Option<WaveformPyramid> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            Some(Self::make_pyramid())
        }
    }

    #[test]
    fn waveform_request_miss_enqueues_then_hits() {
        let builds = Arc::new(AtomicUsize::new(0));
        let cache = WaveformCache::new(
            Arc::new(FakeWaveSource {
                builds: Arc::clone(&builds),
                hash: None,
            }),
            temp_dir("wave-miss"),
        );
        let asset = AssetId::new();
        assert!(cache.request(asset).is_none());
        cache.block_until_idle();
        let p = cache.request(asset).expect("built in background");
        assert_eq!(p.channels, 1);
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(cache.request(asset).is_some());
        assert_eq!(builds.load(Ordering::SeqCst), 1, "cached, no rebuild");
    }

    #[test]
    fn waveform_disk_cache_survives_fresh_cache() {
        let dir = temp_dir("wave-disk");
        let asset = AssetId::new();

        let builds_a = Arc::new(AtomicUsize::new(0));
        {
            let cache_a = WaveformCache::new(
                Arc::new(FakeWaveSource {
                    builds: Arc::clone(&builds_a),
                    hash: Some("waveA".into()),
                }),
                dir.clone(),
            );
            cache_a.request(asset);
            cache_a.block_until_idle();
            assert_eq!(builds_a.load(Ordering::SeqCst), 1);
        }
        // A fresh cache with a source that never builds must load from the
        // sidecar the first build wrote.
        let builds_b = Arc::new(AtomicUsize::new(0));
        let cache_b = WaveformCache::new(
            Arc::new(FakeWaveSource {
                builds: Arc::clone(&builds_b),
                hash: Some("waveA".into()),
            }),
            dir.clone(),
        );
        cache_b.request(asset);
        cache_b.block_until_idle();
        assert!(cache_b.request(asset).is_some());
        assert_eq!(
            builds_b.load(Ordering::SeqCst),
            0,
            "loaded from disk, not rebuilt"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Decode-backed provider (skips without ffmpeg) ────────────────────────

    #[test]
    fn decode_thumbnail_source_real_frame() {
        use crate::decode::scheduler::{PtsKind, SourceParams};
        use crate::decode::PixFmt;
        use crate::media::ffmpeg_locate::locate_for_test;
        use crate::media::keyframe_index::KeyframeIndex;
        use photonic_core::timeline::FrameRate;

        let Some(tools) = locate_for_test() else {
            eprintln!("ffmpeg/ffprobe not found — skipping decode_thumbnail_source_real_frame");
            return;
        };
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/counter.mp4");
        let Ok(keyframes) = KeyframeIndex::build(&tools, &fixture) else {
            eprintln!("keyframe index build failed — skipping");
            return;
        };
        let rate = FrameRate { num: 30, den: 1 };
        let spec_tools = tools.clone();
        let source = Arc::new(DecodeThumbnailSource::new(move |_asset: AssetId| {
            Some(ThumbnailDecodeSpec {
                params: SourceParams {
                    input: fixture.clone(),
                    width: 320,
                    height: 180,
                    pix_fmt: PixFmt::Yuv420p,
                    pts_kind: PtsKind::Cfr(rate),
                    keyframes: keyframes.clone(),
                },
                tools: spec_tools.clone(),
                content_hash: Some("counter".into()),
            })
        }));
        let dir = temp_dir("decode-real");
        let cache = ThumbnailCache::new(source, dir.clone());
        let asset = AssetId::new();

        // Frame 75 = 2.5s.
        let tick = Tick(75 * rate.ticks_per_frame().0);
        assert!(cache.request(asset, tick, 64).is_none());
        cache.block_until_idle();
        let handle = cache
            .request(asset, tick, 64)
            .expect("decoded a real thumbnail");

        assert_eq!(handle.height, 64);
        // 320x180 → height 64 → width round(320*64/180) = 114.
        assert_eq!(handle.width, 114);
        assert_eq!(
            handle.rgba.len(),
            (handle.width * handle.height * 4) as usize
        );
        // counter.mp4 has a burned-in frame number, so the thumbnail is not a
        // single flat colour.
        let first = &handle.rgba[0..4];
        assert!(
            handle.rgba.chunks_exact(4).any(|px| px != first),
            "decoded thumbnail should not be uniform"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
