//! Media services (02 §1 module map): probe, ffmpeg location, keyframe/pts
//! indices. Pure I/O over the ffmpeg toolchain; no GPU, no engine threads.
//!
//! Probing and index building **block** (they shell out to `ffprobe`); the
//! engine runs them on worker threads. Nothing in this module spawns threads.

/// Atomic file writes for cache/export outputs (37 §2.3): temp-and-rename.
pub mod atomic_write;
/// K-C5 cache-data size summary (project sidecar walk).
pub mod cache_stats;
/// Orphaned ffmpeg-child reaping (37 §2.2): pid/session registry + reaper.
pub mod child_registry;
pub mod ffmpeg_locate;
pub mod keyframe_index;
pub mod motion;
/// Import-ladder L3 poster stills (24-preview-media-load).
pub mod poster;
pub mod probe;
pub mod proxy;
/// Deterministic hysteresis policy for selecting ready preview proxies.
pub mod proxy_policy;
/// Still-image decode sizing + the `(asset, size)`-keyed still cache (26 K-C8).
pub mod stills;
/// Clip thumbnails + waveform loading, sidecar-cached (spec 15, NLE parity 10).
pub mod thumbnails;

pub use cache_stats::{summarize_cache, CacheCategory, CacheReport};

pub use atomic_write::{staging_path, sweep_stale_staging, write_atomic};
pub use child_registry::{ChildRecord, ChildRegistry};
pub use ffmpeg_locate::{locate, FfmpegTools, LocateError, FFMPEG_DIR_ENV};
pub use keyframe_index::{
    cache_dir_for_project, keyframe_cache_path, keyframe_index_ready, IndexError, KeyframeIndex,
    PtsIndex,
};
pub use motion::{
    adapters as motion_adapters, parse_motion, AdapterConfidence, AxisMap, MotionError,
    MotionMetadataAdapter, MotionSeries,
};
pub use poster::{ensure_poster, poster_cache_dir, poster_cache_path, poster_ready, PosterError};
pub use probe::{content_hash, probe_asset, probe_details, ProbeDetails, ProbeError};
pub use proxy::{
    generate_proxy, proxy_cache_dir, proxy_cache_path, resolve_decode_input,
    should_auto_generate_proxy, validate_attach, AttachError, AttachValidation, ProxyError,
};
pub use proxy_policy::{
    AdaptiveProxyPolicy, PreviewMediaChoice, PreviewPressure, PRESSURE_SAMPLES_BEFORE_PROXY,
    RELAXED_BUDGET_DENOMINATOR, RELAXED_BUDGET_NUMERATOR, RELAXED_SAMPLES_BEFORE_ORIGINAL,
};
pub use stills::{resample_linear_premult, still_target_size, StillCache, StillKey};
pub use thumbnails::{
    DecodeThumbnailSource, RgbaThumb, ThumbHandle, ThumbnailCache, ThumbnailConfig,
    ThumbnailDecodeSpec, ThumbnailSource, WaveformCache, WaveformSource,
};
