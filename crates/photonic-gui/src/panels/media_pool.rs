//! Media Pool drawer interior (video-editor-module `05-import-export.md` §2,
//! replacing the `video_stubs` shell): import button + window drag-drop →
//! import (with a background ffprobe worker), bins tree, list/grid asset views
//! with probe metadata, offline badge + relink flow, proxy status/toggle, and
//! drag-asset-to-timeline (payload consumed by `app/timeline/mod.rs`, which
//! inserts via the `ops_bridge` path).
//!
//! Import is asynchronous by construction: the click/drop enqueues paths on a
//! worker thread that probes (`photonic_video::media::probe`) and content-
//! hashes each file, then ships a fully-populated `MediaAsset` back over an
//! mpsc channel; the app drains the channel each frame and commits one
//! undoable `AddAsset` per finished asset. `EngineCmd::Probe` is still a P3
//! stub, so probing directly here (same code the engine uses) is the wiring
//! that works today — swap to engine-side probing when the decode-pool story
//! lands.
//!
//! Asset thumbnails are a documented seam (the engine exposes no per-asset
//! decoded-frame access yet); rows use kind glyphs instead.

use super::{PanelAction, PropPanelCtx};
use egui::Ui;
use egui_phosphor::regular as ph;
use photonic_core::timeline::ops::{
    RelinkCandidate, RelinkHashCheck, RelinkMatchKind, RelinkPlanEntry,
};
use photonic_core::timeline::{
    AssetId, AssetKind, AssetSource, BinId, MediaAsset, MediaBin, MediaPool, ProxyRef, ProxyStatus,
    Tick, TICKS_PER_SECOND,
};
use photonic_video::ProxyMode;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

// ── Import worker ─────────────────────────────────────────────────────────────

/// Session-side media pool state: view options, selection, and the background
/// import channel. Lives on `PhotonicApp`.
pub struct MediaPoolUi {
    pub grid_view: bool,
    pub selected: Option<AssetId>,
    /// One-shot: pool click wants single-monitor source peek (24 §3).
    /// Drained by `drive_playback` into `EngineCmd::SetPreviewTarget`.
    pub want_peek: Option<AssetId>,
    /// Bin filter; `None` = pool root (shows unfiled assets).
    pub current_bin: Option<BinId>,
    /// K-C2 filter: only show unused assets (usage == 0).
    pub filter_unused_only: bool,
    /// K-C2 filter: minimum star rating (0 = any).
    pub filter_min_rating: u8,
    pub new_bin_name: String,
    /// L1–L5 jobs still in flight (24-preview-media-load ladder).
    pub importing: usize,
    meta_tx: mpsc::Sender<ImportMetaResult>,
    meta_rx: mpsc::Receiver<ImportMetaResult>,
    /// Proxy transcodes currently running. Separate from importing: ffmpeg
    /// work must never block the editor thread or compete with asset probes.
    pub proxying: usize,
    /// Asset ids with a worker already generating a proxy (session-only).
    /// Prevents auto-L7 and manual "Build proxies" from double-queuing the
    /// same asset into concurrent ffmpeg writes of the same `.part` path.
    proxy_in_flight: std::collections::HashSet<AssetId>,
    proxy_tx: mpsc::Sender<ProxyJobResult>,
    proxy_rx: mpsc::Receiver<ProxyJobResult>,
    /// content_hash → poster PNG for grid/monitor paint (session).
    pub posters: std::collections::HashMap<String, PathBuf>,
    /// content_hash → L4 keyframe index ready.
    pub keyframe_ready: std::collections::HashSet<String>,
    /// content_hash → L5 waveform peak pyramid ready.
    pub waveform_ready: std::collections::HashSet<String>,
    /// Loaded egui textures for poster paths (path string → TextureHandle).
    poster_textures: std::collections::HashMap<String, egui::TextureHandle>,
    /// K-C6 relink: in-flight folder scan (worker → panel), the plan awaiting
    /// the user's confirmation, and the consent flag for byte changes.
    relink_rx: Option<mpsc::Receiver<RelinkScanResult>>,
    pub relink_scanning: bool,
    pub relink_preview: Option<RelinkPreview>,
    pub relink_accept_mismatch: bool,
    /// Cached "how many assets are offline" answer. Recomputed at most every
    /// [`OFFLINE_RECHECK`] because the predicate is a `stat` per asset and the
    /// panel redraws every frame — on a remounting network volume that would be
    /// thousands of syscalls a second.
    offline_count: Option<(std::time::Instant, usize)>,
}

/// How stale the offline-asset count may get before the panel re-`stat`s.
const OFFLINE_RECHECK: std::time::Duration = std::time::Duration::from_millis(1500);

/// A completed background scan of a relink search root.
struct RelinkScanResult {
    root: PathBuf,
    candidates: Vec<RelinkCandidate>,
    truncated: bool,
}

/// The plan the user is being asked to confirm (K-C6). Nothing is committed
/// until they press the button — "never relink silently on hash match alone"
/// (26 §K-C6), and equally never on a *name* match alone.
pub struct RelinkPreview {
    pub root: PathBuf,
    pub entries: Vec<RelinkPlanEntry>,
    /// Offline assets nothing in the scan accounted for.
    pub unmatched: Vec<(AssetId, String)>,
    pub scanned: usize,
    /// The scan hit its depth/count cap — "no match" may just mean "not looked
    /// at", and the UI must not imply otherwise.
    pub truncated: bool,
}

impl RelinkPreview {
    pub fn mismatch_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.hash == RelinkHashCheck::Mismatch)
            .count()
    }

    /// Entries that would actually commit under the current consent flag.
    pub fn committable(&self, accept_mismatch: bool) -> usize {
        self.entries
            .iter()
            .filter(|e| accept_mismatch || e.hash != RelinkHashCheck::Mismatch)
            .count()
    }
}

/// Completed background proxy job, applied by the app through an undoable
/// `SetAssetProxy` command on its next UI frame.
pub struct ProxyJobResult {
    pub asset: AssetId,
    pub proxy: Option<ProxyRef>,
}

/// L1–L5 fill for an already-registered L0 asset (24 §2).
///
/// L1–L4 meta is sent **before** L5 runs so later stages never gate earlier
/// pool metadata (24 §2). A follow-up message may set only
/// [`ImportMetaResult::waveform_only`] after L5 completes.
pub struct ImportMetaResult {
    pub asset: AssetId,
    pub probe: Option<photonic_core::timeline::MediaProbe>,
    pub content_hash: Option<String>,
    pub poster_path: Option<PathBuf>,
    /// L4 keyframe index written (video only).
    pub keyframe_index: bool,
    /// L5 waveform pyramid written (audio / video-with-audio).
    pub waveform_ready: bool,
    /// When true, only session `waveform_ready` is updated — do not apply
    /// `set_asset_meta` (probe/hash already applied by the L1–L4 message).
    pub waveform_only: bool,
}

impl Default for MediaPoolUi {
    fn default() -> Self {
        let (meta_tx, meta_rx) = mpsc::channel();
        let (proxy_tx, proxy_rx) = mpsc::channel();
        MediaPoolUi {
            grid_view: false,
            selected: None,
            want_peek: None,
            current_bin: None,
            filter_unused_only: false,
            filter_min_rating: 0,
            new_bin_name: String::new(),
            importing: 0,
            meta_tx,
            meta_rx,
            proxying: 0,
            proxy_in_flight: std::collections::HashSet::new(),
            proxy_tx,
            proxy_rx,
            posters: std::collections::HashMap::new(),
            keyframe_ready: std::collections::HashSet::new(),
            waveform_ready: std::collections::HashSet::new(),
            poster_textures: std::collections::HashMap::new(),
            relink_rx: None,
            relink_scanning: false,
            relink_preview: None,
            relink_accept_mismatch: false,
            offline_count: None,
        }
    }
}

// ── K-C6: relink scan (GUI side) ─────────────────────────────────────────────
//
// The dir walk and the hash-algorithm dispatch below are deliberate near-twins
// of `photonic-mcp`'s `scan_relink_candidates`/`hash_like`. Their eventual
// single home is `photonic-video/src/media/relink.rs` — the file 02's crate
// layout already names for exactly this — but that crate is owned elsewhere
// this phase, so the duplication is recorded here rather than smuggled in.

/// Files under `root` that could stand in for an offline asset.
///
/// Depth- and count-capped (a user who picks `/` gets an answer, not a
/// traversal) and does not follow directory symlinks. `truncated` is returned so
/// the panel can say "not looked at" instead of implying "not there".
fn scan_relink_candidates(
    root: &Path,
    recursive: bool,
    hash_files: bool,
) -> (Vec<RelinkCandidate>, bool) {
    const MAX_DEPTH: usize = 8;
    const MAX_FILES: usize = 20_000;
    const MAX_HASHED: usize = 4_096;

    let mut out: Vec<RelinkCandidate> = Vec::new();
    let mut truncated = false;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_FILES {
                truncated = true;
                break;
            }
            // `file_type()` does not follow symlinks; `metadata()` would, and a
            // self-referential directory link would then never terminate.
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if recursive && depth < MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                } else if recursive {
                    truncated = true;
                }
            } else if ft.is_file() {
                out.push(RelinkCandidate {
                    path: entry.path(),
                    content_hash: None,
                });
            }
        }
        if out.len() >= MAX_FILES {
            truncated = true;
            break;
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    if hash_files && out.len() <= MAX_HASHED {
        for c in out.iter_mut() {
            c.content_hash = photonic_video::media::probe::content_hash(&c.path).ok();
        }
    }
    (out, truncated)
}

/// Hash `path` with the algorithm that produced `stored`, or `None` when that
/// algorithm is not reproducible here.
///
/// Returning `None` yields `RelinkHashCheck::Unknown`, which is the honest
/// answer — reporting a *mismatch* we did not measure would train the user to
/// tick "relink anyway" past the one guard that catches a wrong-take relink.
/// (`siphash64:` is the retired P2 MCP stopgap; this crate cannot recompute it.)
fn hash_like(stored: Option<&str>, path: &Path) -> Option<String> {
    match stored {
        None => photonic_video::media::probe::content_hash(path).ok(),
        Some(s) if s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit()) => {
            photonic_video::media::probe::content_hash(path).ok()
        }
        Some(_) => None,
    }
}

impl MediaPoolUi {
    /// Start a background scan of `root` for relink candidates (K-C6). The
    /// result is drained by [`draw_media_pool`] and turned into a preview; the
    /// user confirms before anything is committed.
    pub fn spawn_relink_scan(&mut self, root: PathBuf) {
        let (tx, rx) = mpsc::channel();
        self.relink_rx = Some(rx);
        self.relink_scanning = true;
        self.relink_preview = None;
        self.relink_accept_mismatch = false;
        std::thread::spawn(move || {
            let (candidates, truncated) = scan_relink_candidates(&root, true, true);
            let _ = tx.send(RelinkScanResult {
                root,
                candidates,
                truncated,
            });
        });
    }

    /// Drain a finished scan and plan the relink against `project`.
    fn poll_relink_scan(&mut self, project: &photonic_core::timeline::TimelineProject) {
        let Some(rx) = self.relink_rx.as_ref() else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.relink_rx = None;
        self.relink_scanning = false;
        let offline = photonic_core::timeline::ops::offline_assets(project, |p| p.exists());
        let plan = photonic_core::timeline::ops::plan_relink(
            project,
            &offline,
            &result.candidates,
            hash_like,
        );
        let unmatched = plan
            .unmatched
            .iter()
            .filter_map(|id| project.media.assets.get(id))
            .map(|a| (a.id, asset_display_name(a)))
            .collect();
        self.relink_preview = Some(RelinkPreview {
            root: result.root,
            entries: plan.entries,
            unmatched,
            scanned: result.candidates.len(),
            truncated: result.truncated,
        });
        // The pool's online/offline picture is about to change.
        self.offline_count = None;
    }

    /// Offline asset count, re-`stat`ed at most every [`OFFLINE_RECHECK`].
    fn offline_count(&mut self, pool: &MediaPool) -> usize {
        let fresh = self
            .offline_count
            .is_some_and(|(at, _)| at.elapsed() < OFFLINE_RECHECK);
        if !fresh {
            let n = pool.assets.values().filter(|a| asset_is_offline(a)).count();
            self.offline_count = Some((std::time::Instant::now(), n));
        }
        self.offline_count.map(|(_, n)| n).unwrap_or(0)
    }
    /// L0-first import (24 §2): returns stubs for immediate `AddAsset`, then
    /// runs hash → probe → poster → keyframe index → waveform on a worker.
    pub fn spawn_import(
        &mut self,
        paths: Vec<PathBuf>,
        bin: Option<BinId>,
        project_path: Option<PathBuf>,
    ) -> Vec<MediaAsset> {
        // L0 first: stubs are placeable immediately (24 §2). Worker does L1–L5.
        let stubs = l0_register_stubs(&paths, bin);
        if stubs.is_empty() {
            return Vec::new();
        }
        let jobs: Vec<(MediaAsset, PathBuf)> = {
            let mut out = Vec::with_capacity(stubs.len());
            let mut si = 0;
            for path in paths {
                if guess_asset_kind(&path).is_none() {
                    continue;
                }
                if si < stubs.len() {
                    out.push((stubs[si].clone(), path));
                    si += 1;
                }
            }
            out
        };
        self.importing += stubs.len();
        let meta_tx = self.meta_tx.clone();
        std::thread::spawn(move || {
            let tools = photonic_video::media::ffmpeg_locate::locate().ok();
            let cache_dir =
                photonic_video::media::poster::poster_cache_dir(project_path.as_deref());
            for (asset, path) in jobs {
                let asset_id = asset.id;
                let hash = photonic_video::media::probe::content_hash(&path).ok();
                let probe = tools.as_ref().and_then(|t| {
                    match photonic_video::media::probe::probe_asset(t, &path) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            tracing::warn!("media import: probe failed for {path:?}: {e}");
                            None
                        }
                    }
                });
                // L3 poster before L4 keyframe index (24 §2 priority).
                let poster_path = match (tools.as_ref(), hash.as_ref()) {
                    (Some(tools), Some(h))
                        if matches!(asset.kind, AssetKind::Video | AssetKind::Image) =>
                    {
                        let out = photonic_video::media::poster::poster_cache_path(&cache_dir, h);
                        match photonic_video::media::poster::ensure_poster(tools, &path, &out) {
                            Ok(p) => Some(p),
                            Err(e) => {
                                tracing::debug!("media import: poster failed for {path:?}: {e}");
                                None
                            }
                        }
                    }
                    _ => None,
                };
                // L4: keyframe index (video only) — warms scrub seeks.
                let keyframe_index = match (tools.as_ref(), hash.as_ref()) {
                    (Some(tools), Some(h)) if asset.kind == AssetKind::Video => {
                        match photonic_video::media::KeyframeIndex::load_or_build(
                            tools, &path, &cache_dir, h,
                        ) {
                            Ok(_) => true,
                            Err(e) => {
                                tracing::debug!(
                                    "media import: keyframe index failed for {path:?}: {e}"
                                );
                                false
                            }
                        }
                    }
                    _ => false,
                };
                // Send L1–L4 meta *before* L5 so long waveforms never gate
                // pool columns / L7 auto-queue (24 §2).
                if meta_tx
                    .send(ImportMetaResult {
                        asset: asset_id,
                        probe: probe.clone(),
                        content_hash: hash.clone(),
                        poster_path,
                        keyframe_index,
                        waveform_ready: false,
                        waveform_only: false,
                    })
                    .is_err()
                {
                    return;
                }
                // L5: waveform peak pyramid (audio + video-with-audio). Same
                // sidecar dir as timeline WaveformCache so reopen/paint hits.
                // Failures never fail import (24 §2 / D-PM-6).
                let waveform_ready = match (tools.as_ref(), hash.as_ref()) {
                    (Some(tools), Some(h))
                        if matches!(asset.kind, AssetKind::Audio | AssetKind::Video) =>
                    {
                        let try_wave = match asset.kind {
                            AssetKind::Audio => true,
                            AssetKind::Video => match &probe {
                                Some(p) => p.audio.is_some(),
                                None => true,
                            },
                            _ => false,
                        };
                        if !try_wave {
                            false
                        } else if matches!(
                            photonic_video::audio::waveform::load_from_dir(&cache_dir, h),
                            Ok(Some(_))
                        ) {
                            true
                        } else {
                            const WAVEFORM_SAMPLE_RATE: u32 = 48_000;
                            match photonic_video::playback::FfmpegPcmSource::spawn(
                                tools,
                                &path,
                                Tick::ZERO,
                                WAVEFORM_SAMPLE_RATE,
                            ) {
                                Ok(mut src) => {
                                    let pyramid = photonic_video::audio::waveform::build_pyramid(
                                        &mut src,
                                        h.clone(),
                                    );
                                    match photonic_video::audio::waveform::save_to_dir(
                                        &pyramid, &cache_dir,
                                    ) {
                                        Ok(_) => true,
                                        Err(e) => {
                                            tracing::debug!(
                                                "media import: waveform save failed for {path:?}: {e}"
                                            );
                                            false
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "media import: waveform decode failed for {path:?}: {e}"
                                    );
                                    false
                                }
                            }
                        }
                    }
                    _ => false,
                };
                if waveform_ready
                    && meta_tx
                        .send(ImportMetaResult {
                            asset: asset_id,
                            probe: None,
                            content_hash: hash,
                            poster_path: None,
                            keyframe_index: false,
                            waveform_ready: true,
                            waveform_only: true,
                        })
                        .is_err()
                {
                    return;
                }
            }
        });
        stubs
    }

    /// Drain L1–L5 completions. L1–L4 messages need `set_asset_meta`; waveform-only
    /// follow-ups only update session readiness.
    pub fn drain_meta(&mut self) -> Vec<ImportMetaResult> {
        let mut out = Vec::new();
        while let Ok(meta) = self.meta_rx.try_recv() {
            if !meta.waveform_only {
                self.importing = self.importing.saturating_sub(1);
            }
            if let Some(hash) = &meta.content_hash {
                if let Some(path) = &meta.poster_path {
                    self.posters.insert(hash.clone(), path.clone());
                }
                if meta.keyframe_index {
                    self.keyframe_ready.insert(hash.clone());
                }
                if meta.waveform_ready {
                    self.waveform_ready.insert(hash.clone());
                }
            }
            out.push(meta);
        }
        out
    }

    /// Ensure a poster texture is loaded for `hash` (if a path is known).
    fn poster_texture(&mut self, ctx: &egui::Context, hash: &str) -> Option<egui::TextureHandle> {
        if let Some(tex) = self.poster_textures.get(hash) {
            return Some(tex.clone());
        }
        let path = self.posters.get(hash)?;
        let bytes = std::fs::read(path).ok()?;
        let img = photonic_core::raster::image::RasterImage::from_encoded(&bytes).ok()?;
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [img.width as usize, img.height as usize],
            &img.pixels,
        );
        let name = format!("media_poster_{hash}");
        let handle = ctx.load_texture(name, color, egui::TextureOptions::LINEAR);
        self.poster_textures
            .insert(hash.to_string(), handle.clone());
        Some(handle)
    }

    /// True while a background proxy job is already covering this asset.
    pub fn proxy_job_in_flight(&self, asset: AssetId) -> bool {
        self.proxy_in_flight.contains(&asset)
    }

    /// Build all supplied file-backed video proxies sequentially on one worker.
    /// The worker yields CPU priority inside `generate_proxy`; completion is
    /// handed back to the UI so document/history mutation stays on the main
    /// thread. Existing ready cache files are reused without re-encoding.
    ///
    /// Skips assets already in flight (session) so auto-L7 and manual Build
    /// cannot double-write the same `.part` proxy path.
    pub fn spawn_proxy_generation(
        &mut self,
        assets: Vec<MediaAsset>,
        project_path: Option<PathBuf>,
    ) {
        let assets: Vec<MediaAsset> = assets
            .into_iter()
            .filter(|asset| {
                asset.kind == AssetKind::Video
                    && matches!(asset.source, AssetSource::File { .. })
                    && !self.proxy_in_flight.contains(&asset.id)
                    // Never overwrite a user-attached proxy with a generated one
                    // (G-15A; in-flight generate completion must not clobber Attach).
                    && !matches!(
                        asset.proxy.as_ref(),
                        Some(p) if p.origin == photonic_core::timeline::ProxyOrigin::Attached
                    )
            })
            .collect();
        if assets.is_empty() {
            return;
        }
        for a in &assets {
            self.proxy_in_flight.insert(a.id);
        }
        self.proxying += assets.len();
        let tx = self.proxy_tx.clone();
        std::thread::spawn(move || {
            let tools = photonic_video::media::ffmpeg_locate::locate().ok();
            let cache_dir = photonic_video::media::proxy::proxy_cache_dir(project_path.as_deref());
            for asset in assets {
                let AssetSource::File { path, .. } = &asset.source else {
                    let _ = tx.send(ProxyJobResult {
                        asset: asset.id,
                        proxy: asset.proxy.clone(),
                    });
                    continue;
                };
                let hash = asset
                    .content_hash
                    .clone()
                    .or_else(|| photonic_video::media::probe::content_hash(path).ok());
                let proxy = match (tools.as_ref(), hash) {
                    (Some(tools), Some(hash)) => {
                        let output =
                            photonic_video::media::proxy::proxy_cache_path(&cache_dir, &hash);
                        let status = if output.is_file()
                            || photonic_video::media::proxy::generate_proxy(
                                tools,
                                path,
                                &output,
                                &|| false,
                            )
                            .is_ok()
                        {
                            ProxyStatus::Ready
                        } else {
                            ProxyStatus::Failed
                        };
                        Some(ProxyRef {
                            path: output,
                            status,
                            origin: photonic_core::timeline::ProxyOrigin::Generated,
                        })
                    }
                    // A missing toolchain/hash must not detach a previously
                    // ready proxy; preserve the asset's existing attachment.
                    _ => asset.proxy.clone(),
                };
                if tx
                    .send(ProxyJobResult {
                        asset: asset.id,
                        proxy,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
    }

    /// Drain proxy completions. The caller applies them through timeline
    /// commands so undo/redo and the engine mirror stay coherent.
    /// Each completion is a single document update (Ready/Failed) — no
    /// intermediate Pending history entries (G-15C).
    pub fn drain_finished_proxies(&mut self) -> Vec<ProxyJobResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.proxy_rx.try_recv() {
            self.proxying = self.proxying.saturating_sub(1);
            self.proxy_in_flight.remove(&result.asset);
            out.push(result);
        }
        out
    }
}

/// Drag payload for pool-row → timeline drops (consumed in
/// `app/timeline/mod.rs`).
#[derive(Clone, Copy, Debug)]
pub struct AssetDrag {
    pub asset: AssetId,
}

// ── Pure helpers (unit-tested below) ─────────────────────────────────────────

/// L0 register only: build placeable stubs with **no** probe/hash (24 §2).
/// Pure and synchronous — the same path `spawn_import` uses before the worker.
/// Multi-select import must return N stubs before any L1–L2 completes.
pub fn l0_register_stubs(paths: &[PathBuf], bin: Option<BinId>) -> Vec<MediaAsset> {
    paths
        .iter()
        .filter_map(|path| {
            let kind = guess_asset_kind(path)?;
            let mut asset = MediaAsset::from_file(kind, path);
            asset.bin = bin;
            // Contract: L0 has no probe/hash yet.
            debug_assert!(asset.probe.is_none());
            debug_assert!(asset.content_hash.is_none());
            Some(asset)
        })
        .collect()
}

/// Extension → asset kind (mirrors 05 §1's accepted-format table).
pub fn guess_asset_kind(path: &Path) -> Option<AssetKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" | "mts" | "mxf" => AssetKind::Video,
        "mp3" | "wav" | "aac" | "flac" | "ogg" | "m4a" | "opus" => AssetKind::Audio,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "exr" => {
            AssetKind::Image
        }
        "svg" | "photon" => AssetKind::VectorDoc,
        "cube" => AssetKind::Lut3d,
        _ => return None,
    })
}

/// Display name: the file stem for file-backed assets, a fixed label for
/// embedded vector refs.
pub fn asset_display_name(asset: &MediaAsset) -> String {
    match &asset.source {
        AssetSource::File { path, .. } => path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        AssetSource::EmbeddedVector { .. } => "Embedded vector".to_string(),
    }
}

/// Is the file-backed source currently reachable? Embedded sources are always
/// online.
pub fn asset_is_offline(asset: &MediaAsset) -> bool {
    match &asset.source {
        AssetSource::File { path, .. } => !path.exists(),
        AssetSource::EmbeddedVector { .. } => false,
    }
}

/// `M:SS.d` duration readout for list columns.
pub fn format_duration(t: Tick) -> String {
    let total_ds = (t.0.max(0) * 10) / TICKS_PER_SECOND; // deciseconds
    let m = total_ds / 600;
    let s = (total_ds % 600) / 10;
    let d = total_ds % 10;
    format!("{m}:{s:02}.{d}")
}

/// One-line probe summary (dimensions / fps / audio) for the metadata column.
pub fn probe_summary(asset: &MediaAsset) -> String {
    let Some(probe) = &asset.probe else {
        return "—".to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    parts.push(format_duration(probe.duration));
    if let Some(v) = &probe.video {
        let fps = v.frame_rate.num as f64 / v.frame_rate.den.max(1) as f64;
        parts.push(format!("{}x{}", v.width, v.height));
        parts.push(format!("{fps:.3}fps").replace(".000fps", "fps"));
        // K-G6: surface interlaced scan so the pool shows what probe found.
        match v.scan {
            photonic_core::timeline::ScanType::InterlacedTopFirst => {
                parts.push("interlaced (TFF)".into());
            }
            photonic_core::timeline::ScanType::InterlacedBottomFirst => {
                parts.push("interlaced (BFF)".into());
            }
            photonic_core::timeline::ScanType::Progressive
            | photonic_core::timeline::ScanType::Unknown => {}
        }
    }
    if let Some(a) = &probe.audio {
        parts.push(format!("{}ch {}Hz", a.channels, a.sample_rate));
    }
    parts.push(probe.codec.clone());
    parts.join(" · ")
}

/// Child bins of `parent`, in name order.
pub fn bin_children(bins: &[MediaBin], parent: Option<BinId>) -> Vec<&MediaBin> {
    let mut out: Vec<&MediaBin> = bins.iter().filter(|b| b.parent == parent).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Assets filed under `bin` (root = unfiled), sorted by display name for a
/// stable list.
pub fn assets_in_bin(pool: &MediaPool, bin: Option<BinId>) -> Vec<&MediaAsset> {
    let mut out: Vec<&MediaAsset> = pool.assets.values().filter(|a| a.bin == bin).collect();
    out.sort_by_key(|a| asset_display_name(a));
    out
}

/// K-C2: how many timeline clips reference `asset` across all sequences
/// (derived query — not stored). Pure over the project graph.
///
/// This is the `×N` / `ON TL` badge count — **clip** uses only. It is
/// deliberately *not* the "is this asset unused" predicate: a LUT is referenced
/// by a grade op and a graph node, never by a clip, so it scores 0 here while
/// being very much in use. Use [`photonic_core::timeline::ops::unused_assets`]
/// for anything that deletes.
pub fn asset_usage_count(
    project: &photonic_core::timeline::TimelineProject,
    asset: AssetId,
) -> usize {
    let mut n = 0usize;
    for seq in project.sequences.values() {
        for track in seq.tracks() {
            for clip in &track.clips {
                // `source.asset()` covers `Asset` *and* `Vector` — a vector-doc
                // clip is a timeline use of that asset just as much as a video
                // one, and matching `Asset` alone under-counted it.
                if clip.source.asset() == Some(asset) {
                    n += 1;
                }
            }
        }
    }
    n
}

fn kind_glyph(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Video => ph::FILM_STRIP,
        AssetKind::Audio => ph::SPEAKER_HIGH,
        AssetKind::Image => ph::IMAGE,
        AssetKind::VectorDoc => ph::BEZIER_CURVE,
        AssetKind::Lut3d => ph::PALETTE,
    }
}

// ── Drawer UI ─────────────────────────────────────────────────────────────────

/// `DrawerGroup::MediaPool` interior (04 §4.1 / 05 §2).
pub(crate) fn draw_media_pool(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    let Some(project) = ctx.doc.timeline.as_ref() else {
        ui.label("No timeline project yet — enter video mode to create one.");
        return;
    };
    let pool = &project.media;

    // ── Toolbar: import, new bin, view toggle ───────────────────────────────
    ui.horizontal(|ui| {
        if ui
            .button(format!("{} Import…", ph::DOWNLOAD_SIMPLE))
            .on_hover_text("Import media files (or drop files on the window)")
            .clicked()
        {
            ctx.action = Some(PanelAction::MediaImportDialog {
                bin: ctx.media_ui.current_bin,
            });
        }
        let grid = ctx.media_ui.grid_view;
        if ui
            .selectable_label(!grid, ph::LIST)
            .on_hover_text("List view")
            .clicked()
        {
            ctx.media_ui.grid_view = false;
        }
        if ui
            .selectable_label(grid, ph::SQUARES_FOUR)
            .on_hover_text("Grid view")
            .clicked()
        {
            ctx.media_ui.grid_view = true;
        }
    });

    // ── Proxy playback mode (engine-wide toggle, 05 §4) + L7 ingest (G-15C) ─
    ui.horizontal(|ui| {
        ui.label("Proxies:");
        let mut mode = ctx.proxy_mode;
        egui::ComboBox::from_id_salt("media_proxy_mode")
            .selected_text(match mode {
                ProxyMode::Auto => "Auto",
                ProxyMode::ForceProxy => "Force proxy",
                ProxyMode::ForceOriginal => "Force original",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut mode, ProxyMode::Auto, "Auto");
                ui.selectable_value(&mut mode, ProxyMode::ForceProxy, "Force proxy");
                ui.selectable_value(&mut mode, ProxyMode::ForceOriginal, "Force original");
            });
        if mode != ctx.proxy_mode {
            ctx.action = Some(PanelAction::MediaSetProxyMode { mode });
        }
        let generate_on_import = ctx
            .doc
            .timeline
            .as_ref()
            .map(|p| p.settings.generate_proxies)
            .unwrap_or(false);
        let mut gen = generate_on_import;
        if ui
            .checkbox(&mut gen, "On import")
            .on_hover_text(
                "Automatically build half-res editing proxies after import (L7). \
                 Playback uses them when Proxy mode is Auto or Force proxy.",
            )
            .changed()
        {
            ctx.action = Some(PanelAction::MediaSetGenerateProxiesOnImport { enabled: gen });
        }
        if ui
            .add_enabled(
                ctx.media_ui.proxying == 0,
                egui::Button::new("Build proxies"),
            )
            .on_hover_text("Create reusable low-resolution editing proxies in the background")
            .clicked()
        {
            ctx.action = Some(PanelAction::MediaGenerateProxies);
        }
        if ctx.media_ui.proxying > 0 {
            ui.label(
                egui::RichText::new(format!("{} building…", ctx.media_ui.proxying))
                    .weak()
                    .small(),
            );
        }
        if !ctx.engine_online {
            ui.label(egui::RichText::new("(engine offline)").weak().small());
        }
    });

    // ── K-C2 filters + K-C5 remove-unused ───────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Filter:");
        ui.checkbox(&mut ctx.media_ui.filter_unused_only, "Unused only")
            .on_hover_text("Show only assets not placed on any sequence");
        ui.label("Min ★");
        for r in 0u8..=5 {
            let label = if r == 0 {
                "Any".to_string()
            } else {
                format!("{r}")
            };
            if ui
                .selectable_label(ctx.media_ui.filter_min_rating == r, label)
                .clicked()
            {
                ctx.media_ui.filter_min_rating = r;
            }
        }
        ui.separator();
        // Must be the same predicate the action applies, or the label lies about
        // how many assets the button will delete.
        let unused_n = photonic_core::timeline::ops::unused_assets(project).len();
        if ui
            .add_enabled(
                unused_n > 0,
                egui::Button::new(format!("Remove unused ({unused_n})")),
            )
            .on_hover_text("Delete every media pool asset with zero timeline references (undoable)")
            .clicked()
        {
            ctx.action = Some(PanelAction::MediaRemoveUnused);
        }
        // K-C5: cache size report (read-only).
        if ui
            .button("Cache…")
            .on_hover_text("Show project sidecar cache sizes (proxies, posters, …)")
            .clicked()
        {
            // Project path is not on PropPanelCtx; global proxy cache + any
            // open project's cache is enough for a size readout (K-C5 slice).
            let report = photonic_video::media::summarize_cache(None);
            let mut lines = vec![format!(
                "Cache root: {}\n{:.2} MB total",
                report.root.display(),
                report.total_mb()
            )];
            for c in &report.categories {
                if c.files > 0 {
                    lines.push(format!(
                        "  {}: {:.2} MB ({} files)",
                        c.name,
                        c.bytes as f64 / (1024.0 * 1024.0),
                        c.files
                    ));
                }
            }
            // Surface via the same status path as imports.
            // PropPanelCtx may not have set_import_status — use action-less toast via selection of label.
            ui.memory_mut(|m| {
                m.data
                    .insert_temp(egui::Id::new("media_cache_report"), lines.join("\n"));
            });
        }
    });
    if let Some(report) = ui.memory(|m| {
        m.data
            .get_temp::<String>(egui::Id::new("media_cache_report"))
    }) {
        ui.collapsing("Cache report", |ui| {
            ui.monospace(&report);
            if ui.button("Dismiss").clicked() {
                ui.memory_mut(|m| {
                    m.data.remove::<String>(egui::Id::new("media_cache_report"));
                });
            }
        });
    }

    // ── K-C6: offline media + batch relink ──────────────────────────────────
    ctx.media_ui.poll_relink_scan(project);
    draw_relink_section(ui, ctx, project);

    ui.separator();

    // ── Bins tree (flat with parent refs → indented tree) ───────────────────
    ui.horizontal(|ui| {
        let root_selected = ctx.media_ui.current_bin.is_none();
        if ui
            .selectable_label(root_selected, format!("{} Media", ph::HOUSE))
            .clicked()
        {
            ctx.media_ui.current_bin = None;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let name_ok = !ctx.media_ui.new_bin_name.trim().is_empty();
            if ui
                .add_enabled(name_ok, egui::Button::new(ph::FOLDER_PLUS))
                .on_hover_text("New bin")
                .clicked()
            {
                ctx.action = Some(PanelAction::MediaCreateBin {
                    name: std::mem::take(&mut ctx.media_ui.new_bin_name),
                    parent: ctx.media_ui.current_bin,
                });
            }
            ui.add(
                egui::TextEdit::singleline(&mut ctx.media_ui.new_bin_name)
                    .hint_text("New bin…")
                    .desired_width(110.0),
            );
        });
    });
    draw_bin_tree(ui, ctx, &pool.bins, None, 0);

    ui.separator();

    // ── Pending imports ─────────────────────────────────────────────────────
    if ctx.media_ui.importing > 0 {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(format!(
                "Importing {} file{}…",
                ctx.media_ui.importing,
                if ctx.media_ui.importing == 1 { "" } else { "s" }
            ));
        });
    }

    // ── Asset list / grid ───────────────────────────────────────────────────
    let mut assets = assets_in_bin(pool, ctx.media_ui.current_bin);
    // K-C2 usage counts: pure derived query over timeline clips.
    let usage: std::collections::HashMap<AssetId, usize> = assets
        .iter()
        .map(|a| (a.id, asset_usage_count(project, a.id)))
        .collect();
    // Apply K-C2 filters (unused-only / min rating). "Unused" here means the
    // same full reference scan Remove-unused deletes on — not the clip-only
    // badge count, which would list every in-use LUT as unused.
    let min_r = ctx.media_ui.filter_min_rating;
    let unused_only = ctx.media_ui.filter_unused_only;
    let unused_set: std::collections::HashSet<AssetId> = if unused_only {
        photonic_core::timeline::ops::unused_assets(project)
            .into_iter()
            .collect()
    } else {
        std::collections::HashSet::new()
    };
    assets.retain(|a| {
        if unused_only && !unused_set.contains(&a.id) {
            return false;
        }
        if min_r > 0 {
            match a.rating {
                Some(r) if r >= min_r => {}
                _ => return false,
            }
        }
        true
    });
    if assets.is_empty() && ctx.media_ui.importing == 0 {
        ui.label(
            egui::RichText::new("No media here yet. Import files or drop them on the window.")
                .weak(),
        );
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if ctx.media_ui.grid_view {
                ui.horizontal_wrapped(|ui| {
                    for asset in &assets {
                        let n = usage.get(&asset.id).copied().unwrap_or(0);
                        draw_asset_cell(ui, ctx, asset, true, &pool.bins, n);
                    }
                });
            } else {
                for asset in &assets {
                    let n = usage.get(&asset.id).copied().unwrap_or(0);
                    draw_asset_cell(ui, ctx, asset, false, &pool.bins, n);
                }
            }
        });
}

/// K-C6: the offline row, the folder-scan trigger, and the preview the user
/// confirms before anything is rebound.
///
/// Nothing here mutates the document. The confirm button hands a `Vec<
/// TimelineCmd>` up as [`PanelAction::ClipEditBatch`] — the panel layer's
/// generic "N commands, ONE undo step" carrier (`app/panel_actions.rs` wraps it
/// in `Command::Batch` and calls `execute_discrete`), so a 200-clip folder move
/// is one undo (DoD 4).
fn draw_relink_section(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    project: &photonic_core::timeline::TimelineProject,
) {
    let offline_n = ctx.media_ui.offline_count(&project.media);
    if offline_n == 0 && ctx.media_ui.relink_preview.is_none() && !ctx.media_ui.relink_scanning {
        return;
    }

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{} {offline_n} offline", ph::LINK_BREAK))
                .strong()
                .color(egui::Color32::from_rgb(235, 100, 90)),
        )
        .on_hover_text(
            "Assets whose file is not where the project says it is — a moved \
             folder, a renamed drive, or an unmounted volume.",
        );
        if ui
            .add_enabled(
                offline_n > 0 && !ctx.media_ui.relink_scanning,
                egui::Button::new("Relink offline…"),
            )
            .on_hover_text(
                "Pick the folder the media moved to. Every offline asset it can \
                 account for is matched (same bytes, then same name) and shown \
                 for confirmation before anything is rebound.",
            )
            .clicked()
        {
            let dialog =
                rfd::FileDialog::new().set_title("Relink: choose the folder the media moved to");
            if let Some(dir) = crate::app::run_file_dialog(move || dialog.pick_folder()) {
                ctx.media_ui.spawn_relink_scan(dir);
            }
        }
        if ctx.media_ui.relink_scanning {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(egui::RichText::new("Scanning…").weak().small());
        }
    });

    // Taken out (and put back below unless the user resolved it) so the closure
    // can own the `ctx` borrow it needs to raise a `PanelAction`.
    let Some(preview) = ctx.media_ui.relink_preview.take() else {
        return;
    };
    let mut keep = true;
    let mut accept = ctx.media_ui.relink_accept_mismatch;
    let mismatches = preview.mismatch_count();
    let committable = preview.committable(accept);
    let total = preview.entries.len();
    let unmatched = preview.unmatched.len();

    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(
            egui::RichText::new(format!(
                "Relink preview — {} ({} file(s) scanned)",
                preview.root.display(),
                preview.scanned
            ))
            .strong(),
        );
        if preview.truncated {
            ui.label(
                egui::RichText::new(format!(
                    "{} Scan hit its depth/size cap — an unmatched asset may just \
                     not have been looked at.",
                    ph::WARNING
                ))
                .small()
                .color(egui::Color32::from_rgb(235, 170, 80)),
            );
        }
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .auto_shrink([false, true])
            .id_salt("relink_preview_rows")
            .show(ui, |ui| {
                for e in &preview.entries {
                    let name = project
                        .media
                        .assets
                        .get(&e.asset)
                        .map(asset_display_name)
                        .unwrap_or_else(|| "(gone)".to_string());
                    ui.horizontal(|ui| {
                        let (glyph, color, tip) = match e.hash {
                            RelinkHashCheck::Match => (
                                ph::SEAL_CHECK,
                                egui::Color32::from_rgb(120, 200, 140),
                                "Same bytes as the original — verified by content hash.",
                            ),
                            RelinkHashCheck::Mismatch => (
                                ph::WARNING,
                                egui::Color32::from_rgb(235, 100, 90),
                                "DIFFERENT BYTES. This file is not the take the project \
                                 recorded — relinking to it rebinds every clip to other \
                                 media, and nothing downstream will tell you.",
                            ),
                            RelinkHashCheck::Unknown => (
                                ph::MAGNIFYING_GLASS,
                                egui::Color32::from_rgb(180, 180, 190),
                                "Unverified: this asset has no comparable content hash, \
                                 so the bytes could not be checked.",
                            ),
                        };
                        ui.label(egui::RichText::new(glyph).color(color))
                            .on_hover_text(tip);
                        ui.label(egui::RichText::new(&name).small().strong());
                        ui.label(egui::RichText::new(ph::ARROW_RIGHT).weak().small());
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(e.new_path.display().to_string()).small(),
                            )
                            .truncate(),
                        )
                        .on_hover_text(format!(
                            "{}\nmatched by: {}{}",
                            e.new_path.display(),
                            match e.matched_by {
                                RelinkMatchKind::ContentHash => "content hash (same bytes)",
                                RelinkMatchKind::ExactName => "file name",
                                RelinkMatchKind::CaseInsensitiveName => "file name, ignoring case",
                            },
                            if e.ambiguous {
                                "\nseveral files matched this rule — the first was chosen"
                            } else {
                                ""
                            }
                        ));
                    });
                }
                for (_, name) in &preview.unmatched {
                    ui.label(
                        egui::RichText::new(format!("{} {name} — no candidate found", ph::X))
                            .small()
                            .weak(),
                    );
                }
            });

        if mismatches > 0 {
            ui.checkbox(
                &mut accept,
                format!("Relink {mismatches} asset(s) whose bytes differ"),
            )
            .on_hover_text(
                "Those files are not the takes this project recorded. Accepting \
                 records the new content hash and clears the stale probe, so the \
                 pool stops claiming metadata for media that is no longer there.",
            );
        }

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    committable > 0,
                    egui::Button::new(format!("Relink {committable} asset(s)")),
                )
                .on_hover_text("Commits as ONE undo step")
                .clicked()
            {
                let cmds = photonic_core::timeline::ops::relink_plan_commands(
                    project,
                    &preview.entries,
                    accept,
                );
                if !cmds.is_empty() {
                    ctx.action = Some(PanelAction::ClipEditBatch(cmds));
                }
                keep = false;
            }
            if ui.button("Cancel").clicked() {
                keep = false;
            }
            ui.label(
                egui::RichText::new(format!(
                    "{total} matched · {mismatches} byte change(s) · {unmatched} unmatched"
                ))
                .weak()
                .small(),
            );
        });
    });

    ctx.media_ui.relink_accept_mismatch = accept;
    if keep {
        ctx.media_ui.relink_preview = Some(preview);
    } else {
        // Committed or dismissed: the online/offline picture just changed.
        ctx.media_ui.offline_count = None;
    }
}

fn draw_bin_tree(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    bins: &[MediaBin],
    parent: Option<BinId>,
    depth: usize,
) {
    if depth > 16 {
        return; // corrupt parent cycles shouldn't hang the UI
    }
    // Collect ids up front: `ctx` is mutably borrowed inside the loop.
    let children: Vec<(BinId, String)> = bin_children(bins, parent)
        .into_iter()
        .map(|b| (b.id, b.name.clone()))
        .collect();
    for (id, name) in children {
        ui.horizontal(|ui| {
            ui.add_space(12.0 * (depth + 1) as f32);
            let selected = ctx.media_ui.current_bin == Some(id);
            let resp = ui.selectable_label(selected, format!("{} {name}", ph::FOLDER));
            if resp.clicked() {
                ctx.media_ui.current_bin = Some(id);
            }
            resp.context_menu(|ui| {
                if ui.button("Delete bin").clicked() {
                    ctx.action = Some(PanelAction::MediaRemoveBin { bin: id });
                    ui.close_menu();
                }
            });
        });
        draw_bin_tree(ui, ctx, bins, Some(id), depth + 1);
    }
}

/// One asset row (list) or tile (grid): kind glyph (thumbnail seam), name,
/// probe metadata, offline/proxy badges, drag source + context menu.
/// `usage` is the K-C2 clip-reference count (0 = unused).
fn draw_asset_cell(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    asset: &MediaAsset,
    grid: bool,
    bins: &[MediaBin],
    usage: usize,
) {
    let name = asset_display_name(asset);
    let offline = asset_is_offline(asset);
    let selected = ctx.media_ui.selected == Some(asset.id);
    let drag_id = ui.id().with(("media_asset", asset.id));

    let response = ui
        .dnd_drag_source(drag_id, AssetDrag { asset: asset.id }, |ui| {
            let frame = egui::Frame::none()
                .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                .rounding(4.0)
                .fill(if selected {
                    ui.visuals().selection.bg_fill.gamma_multiply(0.4)
                } else {
                    egui::Color32::TRANSPARENT
                });
            frame.show(ui, |ui| {
                if grid {
                    ui.set_width(96.0);
                    ui.vertical_centered(|ui| {
                        // L3 poster when ready; kind glyph otherwise (24 §2).
                        let poster = asset
                            .content_hash
                            .as_deref()
                            .and_then(|h| ctx.media_ui.poster_texture(ui.ctx(), h));
                        if let Some(tex) = poster {
                            let size = egui::vec2(88.0, 50.0);
                            ui.add(egui::Image::new((tex.id(), size)).maintain_aspect_ratio(true));
                        } else {
                            ui.label(egui::RichText::new(kind_glyph(asset.kind)).size(28.0));
                        }
                        ui.label(egui::RichText::new(&name).small());
                        badges(ui, asset, offline, usage);
                    });
                } else {
                    ui.horizontal(|ui| {
                        let poster = asset
                            .content_hash
                            .as_deref()
                            .and_then(|h| ctx.media_ui.poster_texture(ui.ctx(), h));
                        if let Some(tex) = poster {
                            ui.add(
                                egui::Image::new((tex.id(), egui::vec2(40.0, 24.0)))
                                    .maintain_aspect_ratio(true),
                            );
                        } else {
                            ui.label(kind_glyph(asset.kind));
                        }
                        // Right cluster first (stats + badges hug the right
                        // edge); the name then fills the remaining left space and
                        // truncates, so a long name never overruns into the stats
                        // (they used to draw into the same pixels).
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(probe_summary(asset)).weak().small());
                            badges(ui, asset, offline, usage);
                            ui.add(egui::Label::new(&name).truncate());
                        });
                    });
                }
            });
        })
        .response;

    if response.clicked() {
        ctx.media_ui.selected = Some(asset.id);
        // Single-monitor source peek (24 §3) — play-wins still enforced engine-side.
        ctx.media_ui.want_peek = Some(asset.id);
    }
    if response.double_clicked() {
        ctx.action = Some(PanelAction::MediaInsertAtPlayhead { asset: asset.id });
    }
    response
        .on_hover_text(match &asset.source {
            AssetSource::File { path, .. } => path.display().to_string(),
            AssetSource::EmbeddedVector { .. } => "Embedded vector content".to_string(),
        })
        .context_menu(|ui| {
            if ui.button("Insert at playhead").clicked() {
                ctx.action = Some(PanelAction::MediaInsertAtPlayhead { asset: asset.id });
                ui.close_menu();
            }
            // K-A8: subclip from first half of media (or 0–1s) — a full zone UI
            // is follow-up; this lands the verb on the real create_subclip path.
            if !asset.is_subclip()
                && matches!(asset.kind, AssetKind::Video | AssetKind::Audio)
                && ui
                    .button("Create subclip (first half)…")
                    .on_hover_text("K-A8: zone-bounded pool entry sharing the parent cache")
                    .clicked()
            {
                let (rin, rout) = asset
                    .probe
                    .as_ref()
                    .map(|p| {
                        let half = (p.duration.0 / 2).max(1);
                        (0i64, half)
                    })
                    .unwrap_or((0, photonic_core::timeline::TICKS_PER_SECOND));
                ctx.action = Some(PanelAction::MediaCreateSubclip {
                    asset: asset.id,
                    in_ticks: rin,
                    out_ticks: rout,
                    name: None,
                });
                ui.close_menu();
            }
            if offline && ui.button("Relink…").clicked() {
                ctx.action = Some(PanelAction::MediaRelink { asset: asset.id });
                ui.close_menu();
            }
            // K-C2 star rating.
            ui.menu_button("Rating", |ui| {
                if ui
                    .selectable_label(asset.rating.is_none(), "Unrated")
                    .clicked()
                {
                    ctx.action = Some(PanelAction::MediaSetRating {
                        asset: asset.id,
                        rating: None,
                    });
                    ui.close_menu();
                }
                for r in 1u8..=5 {
                    let stars = "★".repeat(r as usize);
                    if ui
                        .selectable_label(asset.rating == Some(r), format!("{stars} ({r})"))
                        .clicked()
                    {
                        ctx.action = Some(PanelAction::MediaSetRating {
                            asset: asset.id,
                            rating: Some(r),
                        });
                        ui.close_menu();
                    }
                }
            });
            // K-C2 tags → project TagId registry via set_asset_tags_resolved.
            ui.menu_button("Tags", |ui| {
                if ui.button("Clear tags").clicked() {
                    ctx.action = Some(PanelAction::MediaSetTags {
                        asset: asset.id,
                        tags: Vec::new(),
                    });
                    ui.close_menu();
                }
                for preset in ["Hero", "B-roll", "Selects", "Audio"] {
                    let on = asset.tags.iter().any(|t| t.eq_ignore_ascii_case(preset));
                    if ui.selectable_label(on, preset).clicked() {
                        let mut tags = asset.tags.clone();
                        if on {
                            tags.retain(|t| !t.eq_ignore_ascii_case(preset));
                        } else {
                            tags.push(preset.to_string());
                        }
                        ctx.action = Some(PanelAction::MediaSetTags {
                            asset: asset.id,
                            tags,
                        });
                        ui.close_menu();
                    }
                }
            });
            // G-15A: attach a user-owned proxy without re-encoding; detach never
            // deletes Attached files (handler / set_asset_proxy only clears ref).
            if asset.kind == AssetKind::Video
                && matches!(asset.source, AssetSource::File { .. })
                && ui.button("Attach Proxy…").clicked()
            {
                ctx.action = Some(PanelAction::MediaAttachProxy { asset: asset.id });
                ui.close_menu();
            }
            if asset.proxy.is_some() && ui.button("Detach Proxy").clicked() {
                ctx.action = Some(PanelAction::MediaDetachProxy { asset: asset.id });
                ui.close_menu();
            }
            ui.menu_button("Move to bin", |ui| {
                if ui.button("(root)").clicked() {
                    ctx.action = Some(PanelAction::MediaAssignBin {
                        asset: asset.id,
                        bin: None,
                    });
                    ui.close_menu();
                }
                for b in bin_children(bins, None) {
                    move_to_bin_items(ui, ctx, asset.id, bins, b, 0);
                }
            });
            if ui.button("Remove from pool").clicked() {
                ctx.action = Some(PanelAction::MediaRemoveAsset { asset: asset.id });
                ui.close_menu();
            }
        });
}

fn move_to_bin_items(
    ui: &mut Ui,
    ctx: &mut PropPanelCtx,
    asset: AssetId,
    bins: &[MediaBin],
    bin: &MediaBin,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    if ui
        .button(format!("{}{}", "  ".repeat(depth), bin.name))
        .clicked()
    {
        ctx.action = Some(PanelAction::MediaAssignBin {
            asset,
            bin: Some(bin.id),
        });
        ui.close_menu();
    }
    for child in bin_children(bins, Some(bin.id)) {
        move_to_bin_items(ui, ctx, asset, bins, child, depth + 1);
    }
}

fn badges(ui: &mut Ui, asset: &MediaAsset, offline: bool, usage: usize) {
    if !asset.tags.is_empty() {
        let label = if asset.tags.len() == 1 {
            asset.tags[0].clone()
        } else {
            format!("{}+{}", asset.tags[0], asset.tags.len() - 1)
        };
        ui.label(egui::RichText::new(label).small().weak());
    }
    if let Some(r) = asset.rating {
        ui.label(
            egui::RichText::new(format!("{}★", r.min(5)))
                .small()
                .color(egui::Color32::from_rgb(235, 200, 80)),
        )
        .on_hover_text(format!("Rating: {r}/5"));
    }
    if usage > 0 {
        let label = if usage == 1 {
            "ON TL".to_string()
        } else {
            format!("×{usage}")
        };
        ui.label(
            egui::RichText::new(label)
                .small()
                .strong()
                .color(egui::Color32::from_rgb(120, 200, 140)),
        )
        .on_hover_text(format!(
            "Used on the timeline ({usage} clip{})",
            if usage == 1 { "" } else { "s" }
        ));
    }
    if offline {
        ui.label(
            egui::RichText::new("OFFLINE")
                .small()
                .strong()
                .color(egui::Color32::from_rgb(235, 100, 90)),
        );
    }
    // K-C7 / K-G6: import triage badges (VFR info, interlaced warn, …).
    if let Some(probe) = asset.probe.as_ref() {
        let findings = photonic_core::timeline::triage_probe(probe);
        for f in &findings {
            let (label, color) = match f.severity {
                photonic_core::timeline::TriageSeverity::Info => (
                    f.code.to_ascii_uppercase(),
                    egui::Color32::from_rgb(140, 160, 200),
                ),
                photonic_core::timeline::TriageSeverity::Warn => (
                    f.code.to_ascii_uppercase(),
                    egui::Color32::from_rgb(235, 180, 90),
                ),
                photonic_core::timeline::TriageSeverity::Action => (
                    f.code.to_ascii_uppercase(),
                    egui::Color32::from_rgb(235, 100, 90),
                ),
            };
            let tip = match &f.remedy {
                Some(r) => format!("{}\n{}\nRemedy: {r}", f.summary, f.consequence),
                None => format!("{}\n{}", f.summary, f.consequence),
            };
            ui.label(egui::RichText::new(label).small().strong().color(color))
                .on_hover_text(tip);
        }
    }
    if let Some(proxy) = &asset.proxy {
        use photonic_core::timeline::ProxyStatus;
        let (txt, color) = match proxy.status {
            ProxyStatus::Ready => ("PROXY", egui::Color32::from_rgb(120, 200, 140)),
            ProxyStatus::Pending => ("PROXY…", egui::Color32::from_gray(150)),
            ProxyStatus::Failed => ("PROXY FAILED", egui::Color32::from_rgb(235, 150, 90)),
        };
        ui.label(egui::RichText::new(txt).small().color(color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guess_asset_kind_covers_the_format_table() {
        assert_eq!(
            guess_asset_kind(Path::new("/a/clip.MP4")),
            Some(AssetKind::Video)
        );
        assert_eq!(
            guess_asset_kind(Path::new("music.flac")),
            Some(AssetKind::Audio)
        );
        assert_eq!(
            guess_asset_kind(Path::new("frame.JPeG")),
            Some(AssetKind::Image)
        );
        assert_eq!(
            guess_asset_kind(Path::new("art.photon")),
            Some(AssetKind::VectorDoc)
        );
        assert_eq!(
            guess_asset_kind(Path::new("look.cube")),
            Some(AssetKind::Lut3d)
        );
        assert_eq!(guess_asset_kind(Path::new("notes.txt")), None);
        assert_eq!(guess_asset_kind(Path::new("no_extension")), None);
    }

    #[test]
    fn display_name_is_file_stem() {
        let a = MediaAsset::from_file(AssetKind::Video, "/some/dir/My Clip.final.mp4");
        assert_eq!(asset_display_name(&a), "My Clip.final");
    }

    #[test]
    fn offline_is_missing_file() {
        let a = MediaAsset::from_file(AssetKind::Video, "/definitely/not/here.mp4");
        assert!(asset_is_offline(&a));
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(Tick(0)), "0:00.0");
        assert_eq!(format_duration(Tick(TICKS_PER_SECOND * 65)), "1:05.0");
        assert_eq!(
            format_duration(Tick(TICKS_PER_SECOND * 3 + TICKS_PER_SECOND / 2)),
            "0:03.5"
        );
        // Negative (corrupt) durations clamp rather than underflow.
        assert_eq!(format_duration(Tick(-5)), "0:00.0");
    }

    #[test]
    fn usage_count_counts_clip_refs() {
        use photonic_core::timeline::{
            Clip, ClipSource, FrameRate, Sequence, Tick, TimelineProject, Track, TrackKind,
        };
        let mut project = TimelineProject::new();
        let id = AssetId::new();
        let other = AssetId::new();
        let mut seq = Sequence::new("S", FrameRate::FPS_30, 320, 180);
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(Clip::new(
            ClipSource::Asset { asset: id },
            Tick::ZERO,
            Tick(100),
        ));
        track.clips.push(Clip::new(
            ClipSource::Asset { asset: id },
            Tick(100),
            Tick(100),
        ));
        track.clips.push(Clip::new(
            ClipSource::Asset { asset: other },
            Tick(200),
            Tick(100),
        ));
        seq.video_tracks.push(track);
        let sid = seq.id;
        project.sequences.insert(sid, seq);
        assert_eq!(asset_usage_count(&project, id), 2);
        assert_eq!(asset_usage_count(&project, other), 1);
        assert_eq!(asset_usage_count(&project, AssetId::new()), 0);
    }

    #[test]
    fn probe_summary_surfaces_interlaced_scan() {
        use photonic_core::timeline::{
            FrameRate, MediaProbe, ProbedColor, ScanType, VideoStreamInfo,
        };
        let mut asset = MediaAsset::new(
            AssetKind::Video,
            AssetSource::File {
                path: PathBuf::from("/tmp/interlaced.mov"),
                rel_path: None,
            },
        );
        asset.probe = Some(MediaProbe {
            duration: Tick(TICKS_PER_SECOND),
            video: Some(VideoStreamInfo {
                width: 720,
                height: 480,
                frame_rate: FrameRate::new(30, 1),
                pixel_aspect: 1.0,
                color: ProbedColor::default(),
                keyframe_index_cached: false,
                scan: ScanType::InterlacedTopFirst,
            }),
            audio: None,
            container: "mov".into(),
            codec: "prores".into(),
            is_vfr: false,
            pixel_format: None,
            has_alpha: false,
        });
        let s = probe_summary(&asset);
        assert!(
            s.contains("interlaced (TFF)"),
            "probe_summary should surface K-G6 scan type: {s}"
        );
        assert!(v_scan_is_interlaced(&asset));
    }

    fn v_scan_is_interlaced(asset: &MediaAsset) -> bool {
        asset
            .probe
            .as_ref()
            .and_then(|p| p.video.as_ref())
            .is_some_and(|v| v.scan.is_interlaced())
    }

    #[test]
    fn bins_and_assets_filter_and_sort() {
        let mut pool = MediaPool::new();
        let root_bin = MediaBin::new("B", None);
        let child = MediaBin::new("A-child", Some(root_bin.id));
        let bins = vec![root_bin.clone(), child.clone(), MediaBin::new("A", None)];

        let kids = bin_children(&bins, None);
        assert_eq!(
            kids.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
            vec!["A", "B"]
        );
        assert_eq!(bin_children(&bins, Some(root_bin.id)).len(), 1);

        let mut filed = MediaAsset::from_file(AssetKind::Video, "/x/zzz.mp4");
        filed.bin = Some(root_bin.id);
        let unfiled_b = MediaAsset::from_file(AssetKind::Audio, "/x/bbb.wav");
        let unfiled_a = MediaAsset::from_file(AssetKind::Audio, "/x/aaa.wav");
        pool.insert(filed.clone());
        pool.insert(unfiled_b);
        pool.insert(unfiled_a);

        let root_assets = assets_in_bin(&pool, None);
        assert_eq!(
            root_assets
                .iter()
                .map(|a| asset_display_name(a))
                .collect::<Vec<_>>(),
            vec!["aaa", "bbb"]
        );
        let binned = assets_in_bin(&pool, Some(root_bin.id));
        assert_eq!(binned.len(), 1);
        assert_eq!(binned[0].id, filed.id);
    }

    #[test]
    fn probe_summary_handles_missing_probe() {
        let a = MediaAsset::from_file(AssetKind::Video, "/x/clip.mp4");
        assert_eq!(probe_summary(&a), "—");
    }

    /// 24 §2 / checklist §11.8: multi-select import yields **N L0 stubs** with
    /// no probe/hash before any L1–L2 work runs.
    #[test]
    fn l0_register_n_stubs_before_any_probe() {
        let t0 = std::time::Instant::now();
        let paths = vec![
            PathBuf::from("/media/a.mp4"),
            PathBuf::from("/media/b.mov"),
            PathBuf::from("/media/skip.txt"), // not a media kind
            PathBuf::from("/media/c.wav"),
        ];
        let stubs = l0_register_stubs(&paths, None);
        let l0_ms = t0.elapsed().as_millis();
        assert_eq!(stubs.len(), 3, "three media paths → three L0 rows");
        assert!(stubs.iter().all(|a| a.probe.is_none()));
        assert!(stubs.iter().all(|a| a.content_hash.is_none()));
        assert!(
            l0_ms < 100,
            "L0 multi-register must be UI-cheap (got {l0_ms} ms)"
        );
        // Distinct ids so concurrent AddAsset is safe.
        let mut ids: Vec<_> = stubs.iter().map(|a| a.id).collect();
        ids.sort_by_key(|i| i.0);
        ids.dedup();
        assert_eq!(ids.len(), 3);
    }

    // ── K-C6 relink flow ────────────────────────────────────────────────────

    fn kc6_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "photonic_gui_kc6_{tag}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The scan is deterministic, recursive by request, and hashes what it finds
    /// so a *renamed* file can still be matched by content.
    #[test]
    fn relink_scan_is_sorted_recursive_and_hashed() {
        let dir = kc6_dir("scan");
        std::fs::write(dir.join("b.mp4"), b"bee").unwrap();
        std::fs::write(dir.join("a.mp4"), b"ay").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("c.mp4"), b"see").unwrap();

        let (flat, _) = scan_relink_candidates(&dir, false, false);
        assert_eq!(flat.len(), 2, "non-recursive must not descend");
        assert!(flat.iter().all(|c| c.content_hash.is_none()));

        let (deep, truncated) = scan_relink_candidates(&dir, true, true);
        assert!(!truncated);
        let names: Vec<String> = deep
            .iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["a.mp4", "b.mp4", "c.mp4"],
            "sorted by full path"
        );
        assert!(
            deep.iter().all(|c| c.content_hash.is_some()),
            "a hashed scan is what makes by-content matching possible"
        );
        // Distinct bytes → distinct hashes (a scan that hashed everything to the
        // same value would match everything to everything).
        let mut hashes: Vec<&String> = deep
            .iter()
            .filter_map(|c| c.content_hash.as_ref())
            .collect();
        hashes.sort();
        hashes.dedup();
        assert_eq!(hashes.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The panel must never *claim* a byte mismatch it cannot measure: the
    /// retired MCP `siphash64:` identity is not reproducible here, so it reads
    /// Unknown, while an xxh3 identity is recomputed and compared for real.
    #[test]
    fn hash_like_reports_unknown_rather_than_a_false_mismatch() {
        let dir = kc6_dir("hash");
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"some bytes").unwrap();
        let real = photonic_video::media::probe::content_hash(&file).unwrap();

        assert_eq!(
            hash_like(Some(&real), &file).as_deref(),
            Some(real.as_str())
        );
        assert_eq!(hash_like(None, &file).as_deref(), Some(real.as_str()));
        assert_eq!(
            hash_like(Some("siphash64:0011223344556677"), &file),
            None,
            "an algorithm this crate cannot compute must not be reported as a mismatch"
        );
        // Sensitivity: a genuinely different file does produce a different hash,
        // so the `Some(..)` arms above are not trivially equal.
        let other = dir.join("b.mp4");
        std::fs::write(&other, b"other bytes entirely").unwrap();
        assert_ne!(
            hash_like(Some(&real), &other).as_deref(),
            Some(real.as_str())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The confirm button's count and the commands it produces agree: a byte
    /// change is excluded until the user opts in.
    #[test]
    fn preview_commits_only_verified_entries_until_mismatch_is_accepted() {
        use photonic_core::timeline::ops;
        use photonic_core::timeline::TimelineProject;

        let mut project = TimelineProject::new();
        let mut good = MediaAsset::from_file(AssetKind::Video, "/old/good.mp4");
        good.content_hash = Some("aaaaaaaaaaaaaaaa".into());
        let mut bad = MediaAsset::from_file(AssetKind::Video, "/old/bad.mp4");
        bad.content_hash = Some("bbbbbbbbbbbbbbbb".into());
        let (good_id, bad_id) = (good.id, bad.id);
        project.media.insert(good);
        project.media.insert(bad);

        // Fake disk: good.mp4 kept its bytes, bad.mp4 is a different take.
        let plan = ops::plan_relink(
            &project,
            &[good_id, bad_id],
            &[
                RelinkCandidate {
                    path: PathBuf::from("/new/good.mp4"),
                    content_hash: Some("aaaaaaaaaaaaaaaa".into()),
                },
                RelinkCandidate {
                    path: PathBuf::from("/new/bad.mp4"),
                    content_hash: Some("cccccccccccccccc".into()),
                },
            ],
            |stored, path| {
                let _ = stored;
                match path.file_name().unwrap().to_string_lossy().as_ref() {
                    "good.mp4" => Some("aaaaaaaaaaaaaaaa".into()),
                    _ => Some("cccccccccccccccc".into()),
                }
            },
        );
        let preview = RelinkPreview {
            root: PathBuf::from("/new"),
            entries: plan.entries,
            unmatched: Vec::new(),
            scanned: 2,
            truncated: false,
        };
        assert_eq!(preview.entries.len(), 2);
        assert_eq!(preview.mismatch_count(), 1);
        assert_eq!(
            preview.committable(false),
            1,
            "button label must exclude it"
        );
        assert_eq!(preview.committable(true), 2);

        // …and the commands the button builds match the label.
        let cmds = ops::relink_plan_commands(&project, &preview.entries, false);
        assert_eq!(cmds.len(), 1);
        let accepted = ops::relink_plan_commands(&project, &preview.entries, true);
        assert_eq!(
            accepted.len(),
            3,
            "two relinks plus the re-identification of the changed asset"
        );
    }
}
